//! Concurrent stdio dispatch and host `cancel` notifications.

mod support;

use std::time::Duration;

use bigquery_plugin::server::serve;
use serde_json::{json, Value};
use support::mock_connection;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn read_line(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
) -> Option<Value> {
    tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("response within 10 s")
        .unwrap()
        .map(|line| serde_json::from_str(&line).unwrap())
}

#[tokio::test]
async fn cancel_notification_stops_the_job_without_a_reply() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bigquery/v2/projects/p/queries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jobReference": {"projectId": "p", "jobId": "long", "location": "EU"},
            "jobComplete": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/bigquery/v2/projects/p/queries/long"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"jobComplete": false}))
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bigquery/v2/projects/p/jobs/long/cancel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"job": {}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/bigquery/v2/projects/p/datasets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"datasets": []})))
        .mount(&server)
        .await;

    let (host, plugin) = tokio::io::duplex(64 * 1024);
    let (plugin_read, plugin_write) = tokio::io::split(plugin);
    let serving = tokio::spawn(serve(BufReader::new(plugin_read), plugin_write));
    let (host_read, mut host_write) = tokio::io::split(host);
    let mut lines = BufReader::new(host_read).lines();

    let request = json!({
        "jsonrpc": "2.0", "method": "execute_query", "id": 7,
        "params": {"params": mock_connection(&server), "query": "SELECT * FROM huge", "limit": 10, "page": 1}
    });
    let ping = json!({
        "jsonrpc": "2.0", "method": "ping", "id": 8,
        "params": {"params": mock_connection(&server)}
    });
    for message in [request, ping] {
        host_write
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }

    // The ping answers while the query is still running.
    let first = read_line(&mut lines).await.unwrap();
    assert_eq!(first["id"], 8);
    assert_eq!(first["result"], Value::Null);

    for notification in [
        json!({"jsonrpc": "2.0", "method": "cancel", "params": {"id": 7}}),
        json!({"jsonrpc": "2.0", "method": "cancel", "params": {"id": 12345}}),
        json!({"jsonrpc": "2.0", "method": "progress", "params": {}}),
    ] {
        host_write
            .write_all(format!("{notification}\n").as_bytes())
            .await
            .unwrap();
    }

    let cancelled = read_line(&mut lines).await.unwrap();
    assert_eq!(cancelled["id"], 7);
    assert!(cancelled["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cancelled"));

    host_write.shutdown().await.unwrap();
    drop(host_write);
    assert!(
        read_line(&mut lines).await.is_none(),
        "notifications must never be answered"
    );
    serving.await.unwrap();
    // Give the spawned jobs.cancel call time to arrive before verifying.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let cancels: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path().ends_with("/jobs/long/cancel"))
        .collect();
    assert_eq!(cancels.len(), 1);
    assert_eq!(
        cancels[0].url.query(),
        Some("location=EU"),
        "jobs.cancel must carry the job location"
    );
}

#[tokio::test]
async fn parse_errors_are_reported_with_a_null_id() {
    let (host, plugin) = tokio::io::duplex(4096);
    let (plugin_read, plugin_write) = tokio::io::split(plugin);
    let serving = tokio::spawn(serve(BufReader::new(plugin_read), plugin_write));
    let (host_read, mut host_write) = tokio::io::split(host);
    let mut lines = BufReader::new(host_read).lines();
    host_write.write_all(b"{not json\n\n").await.unwrap();
    host_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"id\":3}\n")
        .await
        .unwrap();
    let parse_error = read_line(&mut lines).await.unwrap();
    assert_eq!(parse_error["error"]["code"], -32700);
    assert_eq!(parse_error["id"], Value::Null);
    let initialized = read_line(&mut lines).await.unwrap();
    assert_eq!(
        initialized,
        json!({"jsonrpc": "2.0", "result": null, "id": 3})
    );
    host_write.shutdown().await.unwrap();
    serving.await.unwrap();
}
