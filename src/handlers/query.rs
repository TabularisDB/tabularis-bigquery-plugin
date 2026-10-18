use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};

use crate::bigquery::QueryOutput;
use crate::client::{self, BigQueryClient, QueryRequest};
use crate::explain;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::session::{self, SessionState};
use crate::statements::{self, TransactionControl};

const DEFAULT_PAGE_SIZE: u32 = 10_000;

struct Paging<'a> {
    schema: Option<&'a str>,
    page_size: u32,
    page: u32,
}

impl<'a> Paging<'a> {
    fn from_params(params: &'a Value) -> Self {
        let page_size = params
            .get("limit")
            .or_else(|| params.get("page_size"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_PAGE_SIZE);
        let page = params
            .get("page")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(1)
            .max(1);
        Self {
            schema: params
                .get("schema")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty()),
            page_size,
            page,
        }
    }

    fn request<'s>(&self, sql: &'s str) -> QueryRequest<'s>
    where
        'a: 's,
    {
        QueryRequest {
            sql,
            dataset: self.schema,
            page_size: self.page_size,
            page: self.page,
            fetch_rows: true,
            ..QueryRequest::default()
        }
    }

    fn render(&self, output: &QueryOutput, started: Instant) -> Value {
        let mut value = output.to_tabularis(self.page, self.page_size);
        value["execution_time_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        value
    }
}

fn session_id(params: &Value) -> Option<&str> {
    params
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Run one statement, inside a session when one is given, and keep the
/// session's transaction flag in step with BEGIN / COMMIT / ROLLBACK.
async fn run_statement(
    client: &Arc<BigQueryClient>,
    state: Option<&mut SessionState>,
    request: &QueryRequest<'_>,
) -> Result<QueryOutput, String> {
    if statements::split(request.sql).len() == 1
        && statements::transaction_control(request.sql) == TransactionControl::Savepoint
    {
        return Err(
            "BigQuery does not support savepoints; the statement was not sent to BigQuery"
                .to_string(),
        );
    }
    let Some(state) = state else {
        return client.run_query(request, None).await;
    };
    state.client = Some(client.clone());
    let result = client.run_query(request, Some(&mut *state)).await;
    match &result {
        Ok(_) => {
            state.in_transaction =
                statements::transaction_state_after(request.sql, state.in_transaction)
        }
        Err(_) if statements::ends_transaction(request.sql) => state.in_transaction = false,
        Err(_) => {}
    }
    result
}

pub async fn execute_query(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let Some(query) = params.get("query").and_then(Value::as_str) else {
        return error_response(id, -32602, "Invalid params: missing 'query'");
    };
    let paging = Paging::from_params(params);
    let started = Instant::now();
    let client = match client::client_for(&connection).await {
        Ok(client) => client,
        Err(error) => return error_response(id, -32603, &error),
    };
    let request = paging.request(query);

    let Some(host_session) = session_id(params) else {
        return match run_statement(&client, None, &request).await {
            Ok(output) => ok_response(id, paging.render(&output, started)),
            Err(error) => error_response(id, -32603, &error),
        };
    };

    // Session-aware hosts accept `{result, in_transaction}` and report a
    // failed statement as `{error, in_transaction}` so the tab's transaction
    // badge stays correct.
    let shared = session::get_or_create(host_session).await;
    let mut state = shared.lock().await;
    let result = run_statement(&client, Some(&mut state), &request).await;
    let in_transaction = state.in_transaction;
    match result {
        Ok(output) => ok_response(
            id,
            json!({"result": paging.render(&output, started), "in_transaction": in_transaction}),
        ),
        Err(error) => ok_response(
            id,
            json!({"error": error, "in_transaction": in_transaction}),
        ),
    }
}

pub async fn execute_query_batch(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let queries = params
        .get("queries")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let paging = Paging::from_params(params);
    let client = match client::client_for(&connection).await {
        Ok(client) => client,
        Err(error) => return error_response(id, -32603, &error),
    };

    if let Some(host_session) = session_id(params) {
        let shared = session::get_or_create(host_session).await;
        let mut state = shared.lock().await;
        let results = run_batch(&client, &paging, &queries, Some(&mut state)).await;
        let in_transaction = state.in_transaction;
        return ok_response(
            id,
            json!({"results": results, "in_transaction": in_transaction}),
        );
    }

    // Without a host session, statements that leave state behind (DECLARE,
    // SET, temporary tables, transactions) still need to see each other, so
    // the batch runs in a short-lived BigQuery session.
    if queries.len() > 1 && queries.iter().any(|query| statements::needs_session(query)) {
        let mut state = SessionState::default();
        let results = run_batch(&client, &paging, &queries, Some(&mut state)).await;
        if let Err(error) = session::abort(&mut state).await {
            eprintln!("bigquery-plugin: unable to abort batch session: {error}");
        }
        return ok_response(id, json!(results));
    }
    let results = run_batch(&client, &paging, &queries, None).await;
    ok_response(id, json!(results))
}

async fn run_batch(
    client: &Arc<BigQueryClient>,
    paging: &Paging<'_>,
    queries: &[&str],
    mut state: Option<&mut SessionState>,
) -> Vec<Value> {
    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        let started = Instant::now();
        let request = paging.request(query);
        match run_statement(client, state.as_deref_mut(), &request).await {
            Ok(output) => results.push(json!({
                "result": paging.render(&output, started),
                "error": null,
                "execution_time_ms": started.elapsed().as_secs_f64() * 1000.0
            })),
            Err(error) => results.push(json!({
                "result": null,
                "error": error,
                "execution_time_ms": started.elapsed().as_secs_f64() * 1000.0
            })),
        }
    }
    results
}

pub async fn release_session(id: Value, params: &Value) -> Value {
    if let Some(host_session) = session_id(params) {
        if let Err(error) = session::release(host_session).await {
            eprintln!("bigquery-plugin: unable to abort session: {error}");
        }
    }
    ok_response(id, Value::Null)
}

pub async fn explain_query(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let Some(query) = params.get("query").and_then(Value::as_str) else {
        return error_response(id, -32602, "Invalid params: missing 'query'");
    };
    let analyze = params
        .get("analyze")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let schema = params
        .get("schema")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let result = async {
        let client = client::client_for(&connection).await?;
        if analyze {
            let job = client.run_for_statistics(query, schema, None).await?;
            Ok::<_, String>(explain::payload("analyze", &job))
        } else {
            let job = client.dry_run_job(query, schema, None).await?;
            Ok(explain::payload("estimate", &job))
        }
    }
    .await;
    match result {
        Ok(payload) => ok_response(id, explain::raw_output(&payload, query)),
        Err(error) => error_response(id, -32603, &error),
    }
}
