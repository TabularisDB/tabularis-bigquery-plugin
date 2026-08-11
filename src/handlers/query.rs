use std::time::Instant;

use serde_json::{json, Value};

use crate::client;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};

pub async fn execute_query(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let Some(query) = params.get("query").and_then(Value::as_str) else {
        return error_response(id, -32602, "Invalid params: missing 'query'");
    };
    let limit = params
        .get("limit")
        .or_else(|| params.get("page_size"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let page = params
        .get("page")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1)
        .max(1);
    let schema = params.get("schema").and_then(Value::as_str);
    let started = Instant::now();

    let result = async {
        let client = client::client_for(&connection).await?;
        client.execute_query(query, schema, limit, page).await
    }
    .await;

    match result {
        Ok(output) => {
            let mut value = output.to_tabularis(page, limit.unwrap_or(10_000));
            value["execution_time_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            ok_response(id, value)
        }
        Err(error) => error_response(id, -32603, &error),
    }
}

pub async fn execute_query_batch(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let queries = params
        .get("queries")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let page = params
        .get("page")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1)
        .max(1);
    let schema = params.get("schema").and_then(Value::as_str);
    let client = match client::client_for(&connection).await {
        Ok(client) => client,
        Err(error) => return error_response(id, -32603, &error),
    };

    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        let started = Instant::now();
        match client.execute_query(query, schema, limit, page).await {
            Ok(output) => results.push(json!({
                "result": output.to_tabularis(page, limit.unwrap_or(10_000)),
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
    ok_response(id, json!(results))
}

pub async fn explain_query(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let Some(query) = params.get("query").and_then(Value::as_str) else {
        return error_response(id, -32602, "Invalid params: missing 'query'");
    };
    let schema = params.get("schema").and_then(Value::as_str);
    let result = async {
        let client = client::client_for(&connection).await?;
        client.dry_run(query, schema).await
    }
    .await;
    match result {
        Ok(output) => {
            let total_bytes = output
                .get("total_bytes_processed")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<u64>().ok());
            let raw_output = serde_json::to_string_pretty(&output).ok();
            ok_response(
                id,
                json!({
                    "root": {
                        "id": "bigquery-dry-run",
                        "node_type": "BigQuery dry run",
                        "relation": null,
                        "startup_cost": null,
                        "total_cost": null,
                        "plan_rows": null,
                        "actual_rows": null,
                        "actual_time_ms": null,
                        "actual_loops": null,
                        "buffers_hit": null,
                        "buffers_read": null,
                        "filter": null,
                        "index_condition": null,
                        "join_type": null,
                        "hash_condition": null,
                        "extra": {
                            "total_bytes_processed": output.get("total_bytes_processed").cloned().unwrap_or(Value::Null),
                            "estimated_gibibytes_processed": total_bytes.map(|bytes| bytes as f64 / 1_073_741_824.0),
                            "job_id": output.get("job_id").cloned().unwrap_or(Value::Null)
                        },
                        "children": []
                    },
                    "planning_time_ms": null,
                    "execution_time_ms": null,
                    "original_query": query,
                    "driver": "bigquery",
                    "has_analyze_data": false,
                    "raw_output": raw_output
                }),
            )
        }
        Err(error) => error_response(id, -32603, &error),
    }
}
