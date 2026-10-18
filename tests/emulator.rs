//! Integration tests against a BigQuery emulator.
//!
//! Skipped unless `BIGQUERY_EMULATOR_HOST` is set, for example:
//!
//! ```bash
//! docker run -d -p 9050:9050 ghcr.io/goccy/bigquery-emulator:latest --project=test --dataset=ds
//! BIGQUERY_EMULATOR_HOST=localhost:9050 cargo test --test emulator
//! ```
//!
//! Metadata, DDL and CRUD round trips live in `emulator_metadata.rs`.
//!
//! The emulator does not implement sessions, dry runs, DML row counts,
//! child jobs or `tabledata.list` paging, so those paths are covered by the
//! mocked tests and only checked here for graceful behaviour.

mod support;

use bigquery_plugin::client::{self, JobRef};
use bigquery_plugin::models::ConnectionParams;
use serde_json::{json, Value};
use support::{call, result, unique_name};

const PROJECT: &str = support::EMULATOR_PROJECT;
const DATASET: &str = support::EMULATOR_DATASET;

async fn query(params: &Value, sql: &str, limit: u32, page: u32) -> Value {
    call(
        "execute_query",
        json!({"params": params, "query": sql, "limit": limit, "page": page, "schema": DATASET}),
    )
    .await
}

async fn create_numbers(params: &Value, table: &str, rows: u32) {
    let response = query(
        params,
        &format!(
            "CREATE TABLE `{DATASET}.{table}` AS SELECT n, CONCAT('row-', CAST(n AS STRING)) AS label FROM UNNEST(GENERATE_ARRAY(1, {rows})) AS n"
        ),
        10,
        1,
    )
    .await;
    result(&response);
}

#[tokio::test]
async fn connects_and_lists_datasets_and_tables() {
    let params = emulator!();
    assert_eq!(
        call("test_connection", json!({"params": params})).await["result"],
        Value::Null
    );
    assert_eq!(
        call("ping", json!({"params": params})).await["result"],
        Value::Null
    );

    let schemas = call("get_schemas", json!({"params": params})).await;
    assert!(result(&schemas)
        .as_array()
        .unwrap()
        .iter()
        .any(|schema| schema == DATASET));

    let table = unique_name("listing");
    create_numbers(&params, &table, 3).await;
    let tables = call("get_tables", json!({"params": params, "schema": DATASET})).await;
    assert!(result(&tables).to_string().contains(&table), "{tables}");
}

#[tokio::test]
async fn pages_through_large_results_without_rerunning() {
    let params = emulator!();
    let table = unique_name("paging");
    create_numbers(&params, &table, 2500).await;
    let sql = format!("SELECT n, label FROM `{DATASET}.{table}` ORDER BY n");

    let mut seen = Vec::new();
    for page in 1..=3 {
        let response = query(&params, &sql, 1000, page).await;
        let value = result(&response);
        assert_eq!(value["pagination"]["total_rows"], 2500);
        assert_eq!(value["pagination"]["has_more"], page < 3);
        seen.extend(
            value["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[0].as_i64().unwrap()),
        );
    }
    assert_eq!(seen, (1..=2500).collect::<Vec<_>>());
}

#[tokio::test]
async fn browses_tables_like_a_table_tab() {
    let params = emulator!();
    let table = unique_name("browse");
    create_numbers(&params, &table, 30).await;
    let sql = format!("SELECT * FROM `{DATASET}`.`{table}` LIMIT 25");
    let first = query(&params, &sql, 20, 1).await;
    let second = query(&params, &sql, 20, 2).await;
    assert_eq!(result(&first)["rows"].as_array().unwrap().len(), 20);
    assert_eq!(result(&first)["columns"], json!(["n", "label"]));
    assert_eq!(result(&second)["rows"].as_array().unwrap().len(), 5);
    assert_eq!(result(&second)["pagination"]["has_more"], false);
}

#[tokio::test]
async fn runs_dml_and_reports_success() {
    let params = emulator!();
    let table = unique_name("dml");
    create_numbers(&params, &table, 10).await;
    let response = query(
        &params,
        &format!("UPDATE `{DATASET}.{table}` SET label = 'x' WHERE n <= 4"),
        10,
        1,
    )
    .await;
    let affected = result(&response)["affected_rows"].as_u64().unwrap();
    // The emulator omits numDmlAffectedRows; BigQuery reports 4.
    assert!(affected == 0 || affected == 4, "{affected}");
    let check = query(
        &params,
        &format!("SELECT COUNT(*) AS c FROM `{DATASET}.{table}` WHERE label = 'x'"),
        10,
        1,
    )
    .await;
    assert_eq!(result(&check)["rows"], json!([[4]]));
}

#[tokio::test]
async fn formats_timestamps_exactly() {
    let params = emulator!();
    let response = query(
        &params,
        "SELECT TIMESTAMP_MICROS(1700000000123457) AS created, TIMESTAMP '1969-12-31 23:59:59.5 UTC' AS before_epoch",
        10,
        1,
    )
    .await;
    assert_eq!(
        result(&response)["rows"],
        json!([["2023-11-14T22:13:20.123457Z", "1969-12-31T23:59:59.500000Z"]])
    );
}

#[tokio::test]
async fn returns_the_last_result_of_a_script() {
    let params = emulator!();
    let response = query(&params, "SELECT 1 AS a; SELECT 2 AS b", 10, 1).await;
    assert_eq!(result(&response)["rows"], json!([[2]]));
}

#[tokio::test]
async fn reports_invalid_queries() {
    let params = emulator!();
    let response = query(
        &params,
        "SELECT * FROM missing_dataset.missing_table",
        10,
        1,
    )
    .await;
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("missing_table"));
}

#[tokio::test]
async fn cancels_jobs_through_the_rest_api() {
    let params = emulator!();
    let response = query(&params, "SELECT 1 AS n", 10, 1).await;
    let job_id = result(&response)["metadata"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let client = client::client_for(&ConnectionParams::from_value(&params))
        .await
        .unwrap();
    // The job has finished; BigQuery and the emulator accept a late cancel.
    client
        .cancel_job(&JobRef {
            project: PROJECT.to_string(),
            job_id,
            location: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn sessions_keep_state_when_supported() {
    let params = emulator!();
    let run = |sql: &str| {
        call(
            "execute_query",
            json!({"params": params, "query": sql, "limit": 10, "page": 1, "schema": DATASET, "session_id": "emulator-tab"}),
        )
    };
    let declared = run("CREATE TEMP TABLE session_numbers AS SELECT 1 AS n").await;
    let declared = result(&declared);
    if declared.get("error").is_some() {
        eprintln!("emulator does not support sessions: {}", declared["error"]);
        call("release_session", json!({"session_id": "emulator-tab"})).await;
        return;
    }
    let read = run("SELECT n FROM session_numbers").await;
    let read = result(&read);
    match read.get("result") {
        Some(value) => assert_eq!(value["rows"], json!([[1]])),
        None => eprintln!("emulator does not keep session state: {}", read["error"]),
    }
    assert_eq!(read["in_transaction"], false);
    let released = call("release_session", json!({"session_id": "emulator-tab"})).await;
    assert_eq!(released["result"], Value::Null);
}

#[tokio::test]
async fn explain_returns_a_raw_plan() {
    let params = emulator!();
    for analyze in [false, true] {
        let response = call(
            "explain_query",
            json!({"params": params, "query": "SELECT 1 AS n", "analyze": analyze, "schema": DATASET}),
        )
        .await;
        let raw = result(&response);
        assert_eq!(raw["format"], "bigquery-job-json");
        let payload: Value = serde_json::from_str(raw["payload"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["mode"],
            if analyze { "analyze" } else { "estimate" }
        );
    }
}
