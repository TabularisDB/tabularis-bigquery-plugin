#![allow(dead_code)]

use bigquery_plugin::rpc::handle_request;
use serde_json::{json, Value};
use wiremock::MockServer;

/// Connection params pointing the plugin at a local mock or emulator.
pub fn connection(endpoint: &str, project: &str) -> Value {
    json!({
        "driver": "bigquery",
        "database": project,
        "extra": {"auth_method": "none", "api_endpoint": endpoint}
    })
}

pub fn mock_connection(server: &MockServer) -> Value {
    connection(&server.uri(), "p")
}

/// Project and dataset created by the documented emulator command:
/// `bigquery-emulator --project=test --dataset=ds`.
pub const EMULATOR_PROJECT: &str = "test";
pub const EMULATOR_DATASET: &str = "ds";

/// Connection params for the emulator named by `BIGQUERY_EMULATOR_HOST`, or
/// `None` when emulator tests should be skipped.
pub fn emulator_connection() -> Option<Value> {
    let host = std::env::var("BIGQUERY_EMULATOR_HOST").ok()?;
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let endpoint = if host.starts_with("http") {
        host.to_string()
    } else {
        format!("http://{host}")
    };
    Some(connection(&endpoint, EMULATOR_PROJECT))
}

/// Skip the current test unless an emulator is configured.
#[macro_export]
macro_rules! emulator {
    () => {
        match support::emulator_connection() {
            Some(params) => params,
            None => {
                eprintln!("BIGQUERY_EMULATOR_HOST is not set; skipping emulator test");
                return;
            }
        }
    };
}

pub fn unique_name(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

pub async fn call(method: &str, params: Value) -> Value {
    handle_request(json!({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})).await
}

pub fn result(response: &Value) -> &Value {
    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    &response["result"]
}

pub fn field(name: &str, field_type: &str) -> Value {
    json!({"name": name, "type": field_type, "mode": "NULLABLE"})
}

pub fn rows(values: &[i64]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| json!({"f": [{"v": value.to_string()}]}))
            .collect(),
    )
}

/// A completed jobs.query / getQueryResults response with one INT64 column.
pub fn complete(job_id: &str, values: &[i64], total: u64, page_token: Option<&str>) -> Value {
    let mut response = json!({
        "jobReference": {"projectId": "p", "jobId": job_id, "location": "US"},
        "jobComplete": true,
        "schema": {"fields": [field("n", "INT64")]},
        "rows": rows(values),
        "totalRows": total.to_string()
    });
    if let Some(token) = page_token {
        response["pageToken"] = json!(token);
    }
    response
}

pub async fn bodies(server: &MockServer, method: &str, path: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.method.as_str() == method && request.url.path() == path)
        .map(|request| serde_json::from_slice(&request.body).unwrap_or(Value::Null))
        .collect()
}

pub async fn queries(server: &MockServer, method: &str, path: &str) -> Vec<Vec<(String, String)>> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.method.as_str() == method && request.url.path() == path)
        .map(|request| {
            request
                .url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .collect()
}

pub fn query_value<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}
