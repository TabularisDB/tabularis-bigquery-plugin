//! Concurrent JSON-RPC dispatch over newline-delimited stdio.
//!
//! Each request runs in its own task, so a long query never blocks metadata
//! calls or the host's `cancel` notification. A single writer task owns the
//! output stream, so response lines never interleave. Notifications (messages
//! without an `id`) are never answered.

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::{cancel, rpc};

pub async fn serve<R, W>(reader: R, writer: W)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
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
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        while tasks.try_join_next().is_some() {}
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
        let sender = sender.clone();
        tasks.spawn(async move {
            let response = cancel::scope(id, rpc::handle_request(message)).await;
            let _ = sender.send(response);
        });
    }

    // Input closed: let running requests answer, then flush the writer.
    while tasks.join_next().await.is_some() {}
    drop(sender);
    let _ = writer_task.await;
}

fn handle_notification(message: &Value) {
    if message.get("method").and_then(Value::as_str) == Some("cancel") {
        if let Some(id) = message.pointer("/params/id") {
            cancel::cancel(id);
        }
    }
}
