//! Query runtime behaviour against a mocked BigQuery REST API.

mod support;

use std::time::Duration;

use serde_json::{json, Value};
use support::{bodies, call, complete, field, mock_connection, queries, query_value, result};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const QUERIES: &str = "/bigquery/v2/projects/p/queries";
const JOBS: &str = "/bigquery/v2/projects/p/jobs";

fn query_params(server: &MockServer, sql: &str, limit: u32, page: u32) -> Value {
    json!({
        "params": mock_connection(server),
        "query": sql,
        "limit": limit,
        "page": page,
        "schema": "ds"
    })
}

#[tokio::test]
async fn follows_page_tokens_until_the_page_is_full() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete(
            "j1",
            &[1, 2],
            5,
            Some("t1"),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{QUERIES}/j1")))
        .and(query_param("pageToken", "t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("j1", &[3, 4, 5], 5, None)))
        .mount(&server)
        .await;

    let response = call(
        "execute_query",
        query_params(&server, "SELECT n FROM t", 5, 1),
    )
    .await;
    let value = result(&response);
    assert_eq!(value["rows"], json!([[1], [2], [3], [4], [5]]));
    assert_eq!(value["pagination"]["has_more"], false);
    assert_eq!(value["pagination"]["total_rows"], 5);

    let gets = queries(&server, "GET", &format!("{QUERIES}/j1")).await;
    assert_eq!(gets.len(), 1);
    assert_eq!(query_value(&gets[0], "maxResults"), Some("3"));
    assert_eq!(query_value(&gets[0], "location"), Some("US"));
}

#[tokio::test]
async fn sends_idempotent_labelled_jobs_and_reuses_them_for_later_pages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete(
            "j2",
            &[1, 2],
            6,
            Some("t"),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{QUERIES}/j2")))
        .and(query_param("startIndex", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete(
            "j2",
            &[3, 4],
            6,
            Some("u"),
        )))
        .mount(&server)
        .await;

    let sql = "SELECT n FROM t ORDER BY n";
    let first = call("execute_query", query_params(&server, sql, 2, 1)).await;
    assert_eq!(result(&first)["rows"], json!([[1], [2]]));
    let second = call("execute_query", query_params(&server, sql, 2, 2)).await;
    assert_eq!(result(&second)["rows"], json!([[3], [4]]));
    assert_eq!(result(&second)["pagination"]["has_more"], true);

    let posts = bodies(&server, "POST", QUERIES).await;
    assert_eq!(posts.len(), 1, "page 2 must not run the SQL again");
    let body = &posts[0];
    assert_eq!(body["labels"], json!({"client": "tabularis"}));
    assert_eq!(body["jobTimeoutMs"], "110000");
    assert_eq!(body["formatOptions"], json!({"useInt64Timestamp": true}));
    assert_eq!(body["maxResults"], 2);
    assert_eq!(
        body["defaultDataset"],
        json!({"projectId": "p", "datasetId": "ds"})
    );
    let request_id = body["requestId"].as_str().unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(request_id).unwrap().get_version_num(),
        4
    );
}

#[tokio::test]
async fn expired_results_of_side_effecting_statements_are_not_rerun() {
    let server = MockServer::start().await;
    let response = call(
        "execute_query",
        query_params(&server, "DELETE FROM t WHERE true", 10, 2),
    )
    .await;
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("expired"));
    assert!(bodies(&server, "POST", QUERIES).await.is_empty());
}

#[tokio::test]
async fn retries_transient_failures_with_the_same_request_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"code": 503, "message": "backend", "errors": [{"reason": "backendError", "message": "backend"}]}
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "message": "slow down", "errors": [{"reason": "rateLimitExceeded", "message": "slow down"}]}
        })))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("j3", &[7], 1, None)))
        .with_priority(3)
        .mount(&server)
        .await;

    let response = call("execute_query", query_params(&server, "SELECT 7", 10, 1)).await;
    assert_eq!(result(&response)["rows"], json!([[7]]));
    let posts = bodies(&server, "POST", QUERIES).await;
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[0]["requestId"], posts[1]["requestId"]);
    assert_eq!(posts[1]["requestId"], posts[2]["requestId"]);
}

#[tokio::test]
async fn does_not_retry_permanent_errors_and_explains_them() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "message": "Access Denied: Project p", "errors": [{"reason": "accessDenied", "message": "Access Denied: Project p"}]}
        })))
        .mount(&server)
        .await;
    let response = call("execute_query", query_params(&server, "SELECT 1", 10, 1)).await;
    let message = response["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("Access Denied: Project p [accessDenied, HTTP 403]"),
        "{message}"
    );
    assert!(
        message.contains("Hint: Grant the account BigQuery Job User"),
        "{message}"
    );
    assert_eq!(bodies(&server, "POST", QUERIES).await.len(), 1);
}

#[tokio::test]
async fn refreshes_credentials_once_after_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": 401, "message": "Request had invalid authentication credentials."}
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("j4", &[1], 1, None)))
        .with_priority(2)
        .mount(&server)
        .await;
    let response = call("execute_query", query_params(&server, "SELECT 1", 10, 1)).await;
    assert_eq!(result(&response)["rows"], json!([[1]]));
    assert_eq!(bodies(&server, "POST", QUERIES).await.len(), 2);
}

#[tokio::test]
async fn judges_failure_by_the_job_error_result() {
    let server = MockServer::start().await;
    let mut warning = complete("j5", &[1], 1, None);
    warning["errors"] = json!([{"reason": "invalid", "message": "A warning"}]);
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .and(body_partial_json(json!({"query": "SELECT 1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(warning))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{JOBS}/j5")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": {"state": "DONE"}})),
        )
        .mount(&server)
        .await;
    let response = call("execute_query", query_params(&server, "SELECT 1", 10, 1)).await;
    assert_eq!(result(&response)["rows"], json!([[1]]));

    let mut failed = complete("j6", &[], 0, None);
    failed["errors"] = json!([{"reason": "invalidQuery", "message": "Unrecognized name: x"}]);
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .and(body_partial_json(json!({"query": "SELECT x"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(failed))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{JOBS}/j6")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": {"state": "DONE", "errorResult": {"reason": "invalidQuery", "location": "1:8", "message": "Unrecognized name: x"}}
        })))
        .mount(&server)
        .await;
    let response = call("execute_query", query_params(&server, "SELECT x", 10, 1)).await;
    let message = response["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("Unrecognized name: x (line 1, column 8) [invalidQuery]"),
        "{message}"
    );
}

#[tokio::test]
async fn polls_running_jobs_until_complete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jobReference": {"projectId": "p", "jobId": "slow", "location": "EU"},
            "jobComplete": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{QUERIES}/slow")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobComplete": false})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{QUERIES}/slow")))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("slow", &[1, 2], 2, None)))
        .with_priority(2)
        .mount(&server)
        .await;
    let response = call(
        "execute_query",
        query_params(&server, "SELECT n FROM big", 10, 1),
    )
    .await;
    assert_eq!(result(&response)["rows"], json!([[1], [2]]));
    let gets = queries(&server, "GET", &format!("{QUERIES}/slow")).await;
    assert_eq!(gets.len(), 2);
    assert!(gets
        .iter()
        .all(|query| query_value(query, "location") == Some("EU")));
}

#[tokio::test]
async fn browses_native_tables_through_tabledata_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bigquery/v2/projects/p/datasets/ds/tables/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "TABLE",
            "numRows": "10",
            "schema": {"fields": [field("n", "INT64"), field("at", "TIMESTAMP")]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/bigquery/v2/projects/p/datasets/ds/tables/events/data",
        ))
        .and(query_param("startIndex", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRows": "10",
            "pageToken": "next",
            "rows": [{"f": [{"v": "5"}, {"v": "1700000000123457"}]}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/bigquery/v2/projects/p/datasets/ds/tables/events/data",
        ))
        .and(query_param("pageToken", "next"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRows": "10",
            "rows": [{"f": [{"v": "6"}, {"v": null}]}]
        })))
        .mount(&server)
        .await;

    // Host table tabs send `SELECT * FROM `ds`.`events` LIMIT n` and page.
    let response = call(
        "execute_query",
        query_params(&server, "SELECT * FROM `ds`.`events` LIMIT 7", 5, 2),
    )
    .await;
    let value = result(&response);
    assert_eq!(
        value["rows"],
        json!([[5, "2023-11-14T22:13:20.123457Z"], [6, null]])
    );
    assert_eq!(value["pagination"]["total_rows"], 7);
    assert_eq!(value["pagination"]["has_more"], false);
    assert_eq!(value["metadata"]["source"], "tabledata.list");
    assert!(bodies(&server, "POST", QUERIES).await.is_empty());
    let pages = queries(
        &server,
        "GET",
        "/bigquery/v2/projects/p/datasets/ds/tables/events/data",
    )
    .await;
    assert_eq!(query_value(&pages[0], "maxResults"), Some("2"));
    assert_eq!(query_value(&pages[1], "maxResults"), Some("1"));
}

#[tokio::test]
async fn views_fall_back_to_query_jobs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bigquery/v2/projects/p/datasets/ds/tables/v"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"type": "VIEW"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("view", &[1], 1, None)))
        .mount(&server)
        .await;
    let response = call(
        "execute_query",
        query_params(&server, "SELECT * FROM `ds`.`v`", 10, 1),
    )
    .await;
    assert_eq!(result(&response)["metadata"]["source"], "job");
    assert_eq!(bodies(&server, "POST", QUERIES).await.len(), 1);
}

#[tokio::test]
async fn scripts_return_the_last_select_and_sum_dml_rows() {
    let server = MockServer::start().await;
    let mut parent = complete("script", &[], 0, None);
    parent["schema"] = Value::Null;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(parent))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{JOBS}/script")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "statistics": {"query": {"statementType": "SCRIPT"}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(JOBS))
        .and(query_param("parentJobId", "script"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobs": [
            {"jobReference": {"projectId": "p", "jobId": "c3", "location": "US"},
             "statistics": {"creationTime": "3", "query": {"statementType": "DELETE", "numDmlAffectedRows": "4"}}},
            {"jobReference": {"projectId": "p", "jobId": "c2", "location": "US"},
             "statistics": {"creationTime": "2", "query": {"statementType": "SELECT"}, "scriptStatistics": {"evaluationKind": "STATEMENT"}}},
            {"jobReference": {"projectId": "p", "jobId": "c1", "location": "US"},
             "statistics": {"creationTime": "1", "query": {"statementType": "INSERT", "numDmlAffectedRows": "3"}}}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{QUERIES}/c2")))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("c2", &[42], 1, None)))
        .mount(&server)
        .await;

    let sql = "INSERT INTO t VALUES (1); SELECT 42 AS n; DELETE FROM t WHERE true";
    let response = call("execute_query", query_params(&server, sql, 10, 1)).await;
    let value = result(&response);
    assert_eq!(value["rows"], json!([[42]]));
    assert_eq!(value["affected_rows"], 7);
    assert_eq!(bodies(&server, "POST", QUERIES).await[0]["maxResults"], 0);
}

async fn session_mocks(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .and(body_partial_json(json!({"createSession": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut response = complete("s1", &[1], 1, None);
            response["sessionInfo"] = json!({"sessionId": "bq-session"});
            response
        }))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .and(body_partial_json(json!({"query": "SELECT broken"})))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": 400, "message": "Unrecognized name: broken", "errors": [{"reason": "invalidQuery", "message": "Unrecognized name: broken"}]}
        })))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{QUERIES}/s1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("s1", &[1], 1, None)))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("s2", &[2], 1, None)))
        .with_priority(5)
        .mount(server)
        .await;
}

#[tokio::test]
async fn host_sessions_map_to_bigquery_sessions_and_track_transactions() {
    let server = MockServer::start().await;
    session_mocks(&server).await;
    let run = |sql: &str| {
        let mut params = query_params(&server, sql, 10, 1);
        params["session_id"] = json!("tab-1");
        call("execute_query", params)
    };

    let begin = run("BEGIN TRANSACTION").await;
    assert_eq!(result(&begin)["in_transaction"], true);
    assert_eq!(result(&begin)["result"]["rows"], json!([[1]]));

    let failed = run("SELECT broken").await;
    let failed = result(&failed);
    assert!(failed["error"]
        .as_str()
        .unwrap()
        .contains("Unrecognized name"));
    assert_eq!(failed["in_transaction"], true);

    let savepoint = run("SAVEPOINT tabularis_read").await;
    assert!(result(&savepoint)["error"]
        .as_str()
        .unwrap()
        .contains("savepoints"));

    let commit = run("COMMIT").await;
    assert_eq!(result(&commit)["in_transaction"], false);

    let posts = bodies(&server, "POST", QUERIES).await;
    assert_eq!(posts.len(), 3, "the savepoint is never sent");
    assert_eq!(posts[0]["createSession"], true);
    for joined in &posts[1..] {
        assert_eq!(
            joined["connectionProperties"],
            json!([{"key": "session_id", "value": "bq-session"}])
        );
        assert_eq!(joined["location"], "US");
        assert!(joined.get("createSession").is_none());
    }

    let released = call("release_session", json!({"session_id": "tab-1"})).await;
    assert_eq!(released["result"], Value::Null);
    let posts = bodies(&server, "POST", QUERIES).await;
    assert_eq!(
        posts.last().unwrap()["query"],
        "CALL BQ.ABORT_SESSION('bq-session')"
    );
    let unknown = call("release_session", json!({"session_id": "never-used"})).await;
    assert_eq!(unknown["result"], Value::Null);
}

#[tokio::test]
async fn stateful_batches_share_a_short_lived_session() {
    let server = MockServer::start().await;
    session_mocks(&server).await;
    let response = call(
        "execute_query_batch",
        json!({
            "params": mock_connection(&server),
            "queries": ["DECLARE x INT64 DEFAULT 1", "SELECT x"],
            "limit": 10,
            "page": 1
        }),
    )
    .await;
    let results = result(&response)
        .as_array()
        .expect("bare array without session_id");
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|item| item["error"].is_null()),
        "{results:?}"
    );
    let posts = bodies(&server, "POST", QUERIES).await;
    assert_eq!(posts[0]["createSession"], true);
    assert_eq!(posts[1]["connectionProperties"][0]["value"], "bq-session");
    assert_eq!(
        posts.last().unwrap()["query"],
        "CALL BQ.ABORT_SESSION('bq-session')"
    );
}

#[tokio::test]
async fn explain_returns_a_dry_run_estimate_without_running_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(JOBS))
        .and(body_partial_json(json!({"configuration": {"dryRun": true}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jobReference": {"projectId": "p", "location": "US"},
            "statistics": {"query": {"statementType": "SELECT", "totalBytesProcessed": "1099511627776", "cacheHit": false}}
        })))
        .mount(&server)
        .await;
    let response = call(
        "explain_query",
        json!({"params": mock_connection(&server), "query": "SELECT * FROM ds.t", "analyze": false, "schema": "ds"}),
    )
    .await;
    let raw = result(&response);
    assert_eq!(raw["engine"], "bigquery");
    assert_eq!(raw["format"], "bigquery-job-json");
    let payload: Value = serde_json::from_str(raw["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["mode"], "estimate");
    assert_eq!(payload["estimated_cost_usd"], 6.25);
    assert!(bodies(&server, "POST", QUERIES).await.is_empty());
}

#[tokio::test]
async fn explain_analyze_reads_the_executed_query_plan() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(QUERIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete("an", &[], 0, None)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{JOBS}/an")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jobReference": {"projectId": "p", "jobId": "an"},
            "statistics": {"startTime": "1", "endTime": "11", "query": {
                "totalBytesBilled": "10485760", "totalSlotMs": "120",
                "queryPlan": [{"id": "0", "name": "S00: Input", "slotMs": "100"}, {"id": "1", "name": "S01: Output", "inputStages": ["0"], "slotMs": "20"}]
            }}
        })))
        .mount(&server)
        .await;
    let response = call(
        "explain_query",
        json!({"params": mock_connection(&server), "query": "SELECT 1", "analyze": true}),
    )
    .await;
    let payload: Value =
        serde_json::from_str(result(&response)["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["mode"], "analyze");
    assert_eq!(payload["query_plan"].as_array().unwrap().len(), 2);
    assert_eq!(payload["total_slot_ms"], 120);
    assert_eq!(payload["elapsed_ms"], 10);
}

#[tokio::test]
async fn ping_lists_a_single_dataset() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bigquery/v2/projects/p/datasets"))
        .and(query_param("maxResults", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"datasets": []})))
        .expect(1)
        .mount(&server)
        .await;
    let response = call("ping", json!({"params": mock_connection(&server)})).await;
    assert_eq!(response["result"], Value::Null);
    tokio::time::sleep(Duration::from_millis(10)).await;
}
