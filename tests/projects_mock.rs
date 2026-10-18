//! Billing and data projects and metadata statements on the job runtime,
//! against a mocked BigQuery REST API.

mod support;

use serde_json::{json, Value};
use support::{bodies, call, complete, connection, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BILLING_QUERIES: &str = "/bigquery/v2/projects/bill/queries";
const TABLE: &str = "/bigquery/v2/projects/data/datasets/ds/tables/items";

/// A connection browsing `data` whose jobs run in the `bill` project.
fn params(server: &MockServer) -> Value {
    let mut params = connection(&server.uri(), "data");
    params["extra"]["billing_project"] = json!("bill");
    params
}

async fn mount_table(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(TABLE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "TABLE",
            "tableReference": {"projectId": "data", "datasetId": "ds", "tableId": "items"},
            "schema": {"fields": [
                {"name": "id", "type": "INT64", "mode": "REQUIRED"},
                {"name": "name", "type": "STRING", "mode": "NULLABLE"}
            ]},
            "tableConstraints": {"primaryKey": {"columns": ["id"]}}
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/bigquery/v2/projects/data/datasets/ds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"location": "EU"})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn jobs_run_in_the_billing_project_and_resolve_names_in_the_data_project() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BILLING_QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("j", &[1], 1, None)))
        .mount(&server)
        .await;

    let response = call(
        "execute_query",
        json!({"params": params(&server), "query": "SELECT n FROM items", "limit": 10, "page": 1, "schema": "ds"}),
    )
    .await;
    assert_eq!(result(&response)["rows"], json!([[1]]));
    let sent = bodies(&server, "POST", BILLING_QUERIES).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["defaultDataset"],
        json!({"projectId": "data", "datasetId": "ds"})
    );
}

#[tokio::test]
async fn row_edits_use_the_job_runtime_with_typed_parameters() {
    let server = MockServer::start().await;
    mount_table(&server).await;
    Mock::given(method("POST"))
        .and(path(BILLING_QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jobReference": {"projectId": "bill", "jobId": "dml", "location": "EU"},
            "jobComplete": true,
            "numDmlAffectedRows": "1"
        })))
        .mount(&server)
        .await;

    let response = call(
        "insert_record",
        json!({"params": params(&server), "schema": "ds", "table": "items", "data": {"id": 7, "name": "O'Reilly"}}),
    )
    .await;
    assert_eq!(result(&response), &json!(1));

    let sent = bodies(&server, "POST", BILLING_QUERIES).await;
    assert_eq!(sent.len(), 1);
    let body = &sent[0];
    assert!(body["requestId"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(body["labels"], json!({"client": "tabularis"}));
    assert!(body["jobTimeoutMs"].is_string());
    assert_eq!(body["location"], "EU");
    assert_eq!(
        body["defaultDataset"],
        json!({"projectId": "data", "datasetId": "ds"})
    );
    assert_eq!(body["parameterMode"], "NAMED");
    let parameters = body["queryParameters"].as_array().unwrap();
    assert_eq!(parameters.len(), 2);
    assert!(parameters
        .iter()
        .any(|parameter| parameter["parameterValue"]["value"] == "O'Reilly"));
    assert!(!body["query"].as_str().unwrap().contains("O'Reilly"));
}
