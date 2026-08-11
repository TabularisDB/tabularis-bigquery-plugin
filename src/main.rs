use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        let response = bigquery_plugin::rpc::handle_line(&line).await;
        let mut encoded = match serde_json::to_vec(&response) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("failed to serialize JSON-RPC response: {error}");
                continue;
            }
        };
        encoded.push(b'\n');

        if stdout.write_all(&encoded).await.is_err() || stdout.flush().await.is_err() {
            break;
        }
    }
}
