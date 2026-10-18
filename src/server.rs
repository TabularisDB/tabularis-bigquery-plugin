//! Concurrent JSON-RPC dispatch over newline-delimited stdio.
//!
//! Each request runs in its own task, so a long query never blocks metadata
//! calls or the host's `cancel` notification. A single writer task owns the
//! output stream, so response lines never interleave. Notifications (messages
//! without an `id`) are never answered. A handler that panics is logged and
//! its request still gets a JSON-RPC internal error, so the host never waits
//! for a reply that cannot come.

use std::collections::HashMap;
use std::future::Future;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::{Id, JoinError, JoinSet};

use crate::{cancel, rpc};

pub async fn serve<R, W>(reader: R, writer: W)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    serve_with(reader, writer, rpc::handle_request).await;
}

/// [`serve`] with a custom request handler, so tests can inject one.
pub async fn serve_with<R, W, H, F>(reader: R, writer: W, handler: H)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Fn(Value) -> F,
    F: Future<Output = Value> + Send + 'static,
{
    let (sender, mut receiver) = mpsc::unbounded_channel::<Value>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(response) = receiver.recv().await {
            let mut encoded = match serde_json::to_vec(&response) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("failed to serialize JSON-RPC response: {error}");
                    continue;
                }
            };
            encoded.push(b'\n');
            if writer.write_all(&encoded).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });

    let mut tasks = JoinSet::new();
    // Request id of each running task, to answer it if the task panics.
    let mut pending: HashMap<Id, Value> = HashMap::new();
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        while let Some(joined) = tasks.try_join_next_with_id() {
            settle(joined, &mut pending, &sender);
        }
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let _ = sender.send(rpc::error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
                continue;
            }
        };
        let Some(id) = message.get("id").cloned() else {
            handle_notification(&message);
            continue;
        };
        let task_sender = sender.clone();
        let request = handler(message);
        let request_id = id.clone();
        let handle = tasks.spawn(async move {
            let response = cancel::scope(request_id, request).await;
            let _ = task_sender.send(response);
        });
        pending.insert(handle.id(), id);
    }

    // Input closed: let running requests answer, then flush the writer.
    while let Some(joined) = tasks.join_next_with_id().await {
        settle(joined, &mut pending, &sender);
    }
    drop(sender);
    let _ = writer_task.await;
}

/// Forget a finished task; if it panicked, log it and answer its request.
fn settle(
    joined: Result<(Id, ()), JoinError>,
    pending: &mut HashMap<Id, Value>,
    sender: &mpsc::UnboundedSender<Value>,
) {
    match joined {
        Ok((task, ())) => {
            pending.remove(&task);
        }
        Err(error) => {
            let id = pending.remove(&error.id());
            eprintln!(
                "bigquery-plugin: request {} failed: {error}",
                id.as_ref()
                    .map_or_else(|| "?".to_string(), Value::to_string)
            );
            if let Some(id) = id {
                let _ = sender.send(rpc::error_response(
                    id,
                    -32603,
                    "Internal error: the BigQuery plugin failed while handling this request",
                ));
            }
        }
    }
}

fn handle_notification(message: &Value) {
    if message.get("method").and_then(Value::as_str) == Some("cancel") {
        if let Some(id) = message.pointer("/params/id") {
            cancel::cancel(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn panicking_handlers_still_answer_with_an_internal_error() {
        let (plugin_read, mut host_write) = tokio::io::duplex(4096);
        let (host_read, plugin_write) = tokio::io::duplex(4096);
        let serving = tokio::spawn(serve_with(
            BufReader::new(plugin_read),
            plugin_write,
            |message: Value| async move {
                if message["method"] == "boom" {
                    panic!("handler exploded");
                }
                rpc::error_response(message["id"].clone(), 1, "ok")
            },
        ));
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"boom\",\"id\":7}\n")
            .await
            .unwrap();
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"fine\",\"id\":8}\n")
            .await
            .unwrap();
        drop(host_write);
        serving.await.unwrap();

        let mut lines = BufReader::new(host_read).lines();
        let mut responses = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            responses.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        responses.sort_by_key(|response| response["id"].as_i64());
        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(responses[0]["id"], json!(7));
        assert_eq!(responses[0]["error"]["code"], json!(-32603));
        assert_eq!(responses[1]["id"], json!(8));
    }
}
