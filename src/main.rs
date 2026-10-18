use tokio::io::BufReader;

#[tokio::main]
async fn main() {
    bigquery_plugin::server::serve(BufReader::new(tokio::io::stdin()), tokio::io::stdout()).await;
}
