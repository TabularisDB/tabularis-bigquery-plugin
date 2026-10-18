//! Query, session and explain conformance: RPC results must deserialize into
//! the structures Tabularis uses (copied from `src-tauri/src/models.rs` and
//! the response readers in `src-tauri/src/plugins/driver.rs`). Metadata, DDL
//! and routine shapes are covered by `conformance_metadata.rs`.

mod support;

use serde::Deserialize;
use serde_json::{json, Value};
use support::{call, complete, mock_connection};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Pagination {
    page: u32,
    page_size: u32,
    total_rows: Option<u64>,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    affected_rows: u64,
    #[serde(default)]
    truncated: bool,
    pagination: Option<Pagination>,
    #[serde(default)]
    additional_results: Option<Vec<QueryResult>>,
    #[serde(default)]
    server_time_ms: Option<f64>,
    #[serde(default)]
    fetch_time_ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BatchStatementResult {
    result: Option<QueryResult>,
    error: Option<String>,
    execution_time_ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawExplainOutput {
    engine: String,
    format: String,
    payload: String,
    original_query: String,
}

/// `parse_query_response` in the host.
fn parse_query_response(value: Value) -> (QueryResult, bool) {
    if let Some(object) = value.as_object() {
        if let (Some(result), Some(flag)) = (object.get("result"), object.get("in_transaction")) {
            return (
                serde_json::from_value(result.clone()).unwrap(),
                flag.as_bool().unwrap_or(false),
            );
        }
    }
    (serde_json::from_value(value).unwrap(), false)
}

/// `parse_session_error` in the host.
fn parse_session_error(value: &Value) -> Option<(String, bool)> {
    let object = value.as_object()?;
    let error = object.get("error")?.as_str()?;
    let in_transaction = object.get("in_transaction")?.as_bool().unwrap_or(false);
    Some((error.to_string(), in_transaction))
}

/// `parse_batch_response` in the host.
fn parse_batch_response(value: Value) -> (Vec<BatchStatementResult>, bool) {
    if let Some(object) = value.as_object() {
        if let Some(results) = object.get("results") {
            return (
                serde_json::from_value(results.clone()).unwrap(),
                object
                    .get("in_transaction")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
        }
    }
    (serde_json::from_value(value).unwrap(), false)
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bigquery/v2/projects/p/queries"))
        .and(body_partial_json(json!({"query": "SELECT nope"})))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": 400, "message": "Unrecognized name: nope", "errors": [{"reason": "invalidQuery", "message": "Unrecognized name: nope"}]}
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bigquery/v2/projects/p/queries"))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut response = complete("c", &[1, 2], 2, None);
            response["sessionInfo"] = json!({"sessionId": "s"});
            response
        }))
        .with_priority(5)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bigquery/v2/projects/p/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "statistics": {"query": {"totalBytesProcessed": "10"}}
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn execute_query_without_session_is_a_bare_query_result() {
    let server = server().await;
    let response = call(
        "execute_query",
        json!({"params": mock_connection(&server), "query": "SELECT 1", "limit": 100, "page": 1, "schema": null}),
    )
    .await;
    // `execute_query` without a session deserializes the result directly.
    let result: QueryResult = serde_json::from_value(response["result"].clone()).unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.pagination.unwrap().page_size, 100);

    // The host's count query sends no limit.
    let response = call(
        "execute_query",
        json!({"params": mock_connection(&server), "query": "SELECT COUNT(*) FROM (SELECT 1) as count_wrapper", "limit": null, "page": 1, "schema": null}),
    )
    .await;
    let _: QueryResult = serde_json::from_value(response["result"].clone()).unwrap();
}

#[tokio::test]
async fn execute_query_in_session_uses_the_wrapped_shapes() {
    let server = server().await;
    let request = |sql: &str| json!({"params": mock_connection(&server), "query": sql, "limit": 10, "page": 1, "schema": null, "session_id": "conformance-tab"});
    let response = call("execute_query", request("BEGIN")).await;
    let value = response["result"].clone();
    assert!(parse_session_error(&value).is_none());
    let (result, in_transaction) = parse_query_response(value);
    assert_eq!(result.columns, vec!["n"]);
    assert!(in_transaction);

    let response = call("execute_query", request("SELECT nope")).await;
    let (error, in_transaction) = parse_session_error(&response["result"]).unwrap();
    assert!(error.contains("Unrecognized name: nope"));
    assert!(in_transaction);

    let response = call("release_session", json!({"session_id": "conformance-tab"})).await;
    assert_eq!(response, json!({"jsonrpc": "2.0", "result": null, "id": 1}));
}

#[tokio::test]
async fn execute_query_batch_uses_bare_and_wrapped_shapes() {
    let server = server().await;
    let response = call(
        "execute_query_batch",
        json!({"params": mock_connection(&server), "queries": ["SELECT 1", "SELECT nope"], "limit": 10, "page": 1, "schema": null}),
    )
    .await;
    // Without a session the host reads a bare array.
    let results: Vec<BatchStatementResult> =
        serde_json::from_value(response["result"].clone()).unwrap();
    assert!(results[0].result.is_some() && results[0].error.is_none());
    assert!(results[1].result.is_none() && results[1].error.is_some());

    let response = call(
        "execute_query_batch",
        json!({"params": mock_connection(&server), "queries": ["BEGIN TRANSACTION", "SELECT 1"], "limit": 10, "page": 1, "schema": null, "session_id": "batch-tab"}),
    )
    .await;
    let (results, in_transaction) = parse_batch_response(response["result"].clone());
    assert_eq!(results.len(), 2);
    assert!(in_transaction);
    call("release_session", json!({"session_id": "batch-tab"})).await;
}

#[tokio::test]
async fn explain_query_returns_the_raw_shape() {
    let server = server().await;
    let response = call(
        "explain_query",
        json!({"params": mock_connection(&server), "query": "SELECT 1", "analyze": false, "schema": null}),
    )
    .await;
    let raw: RawExplainOutput = serde_json::from_value(response["result"].clone()).unwrap();
    assert_eq!(raw.engine, "bigquery");
    assert_eq!(raw.format, "bigquery-job-json");
    assert_eq!(raw.original_query, "SELECT 1");
    let payload: Value = serde_json::from_str(&raw.payload).unwrap();
    assert_eq!(payload["version"], 1);

    let manifest: Value = serde_json::from_str(include_str!("../.tabularium")).unwrap();
    let parsers = manifest["explain_parsers"].as_array().unwrap();
    assert!(parsers
        .iter()
        .any(|parser| parser["engine"] == raw.engine && parser["format"] == raw.format));
}

#[tokio::test]
async fn errors_use_json_rpc_error_objects() {
    let server = server().await;
    let response = call(
        "execute_query",
        json!({"params": mock_connection(&server), "query": "SELECT nope", "limit": 10, "page": 1}),
    )
    .await;
    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"].is_string());
    assert_eq!(response["id"], 1);
}
