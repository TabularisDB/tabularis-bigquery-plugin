use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::Url;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::auth::AuthProvider;
use crate::bigquery::{FieldSchema, QueryOutput};
use crate::models::ConnectionParams;

mod jobs;
mod tabledata;
mod transport;

pub use jobs::{JobRef, QueryRequest};

const DEFAULT_PAGE_SIZE: u32 = 10_000;

static CLIENTS: OnceLock<Mutex<HashMap<String, Arc<BigQueryClient>>>> = OnceLock::new();

#[derive(Clone)]
pub struct BigQueryClient {
    http: reqwest::Client,
    auth: AuthProvider,
    pub project_id: String,
    location: Option<String>,
    max_bytes_billed: Option<String>,
    timeout: Duration,
    api_base: Url,
    /// Fingerprint of the connection settings, used to scope cached results.
    key: String,
}

impl BigQueryClient {
    async fn new(params: &ConnectionParams) -> Result<Self, String> {
        let timeout = Duration::from_secs(params.timeout_seconds());
        let project_id = params.project_id()?.to_string();
        let max_bytes_billed = params
            .max_bytes_billed()
            .map(|value| {
                value
                    .parse::<u64>()
                    .map(|_| value.to_string())
                    .map_err(|_| "Maximum bytes billed must be a non-negative integer".to_string())
            })
            .transpose()?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(timeout + Duration::from_secs(30))
            .user_agent(concat!(
                "tabularis-bigquery-plugin/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|error| format!("Unable to initialize the HTTP client: {error}"))?;
        let endpoint = crate::endpoint::resolve(params.api_endpoint())?;
        Ok(Self {
            http,
            auth: AuthProvider::from_params(params, &endpoint).await?,
            project_id,
            location: params.location().map(str::to_owned),
            max_bytes_billed,
            timeout,
            api_base: endpoint.base,
            key: cache_key(params),
        })
    }

    fn url(&self, segments: &[&str]) -> Result<Url, String> {
        let mut url = self.api_base.clone();
        url.path_segments_mut()
            .map_err(|_| "Invalid BigQuery API base URL".to_string())?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    async fn request_json(
        &self,
        method: reqwest::Method,
        url: Url,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, String> {
        self.call(method, url, query, body)
            .await
            .map_err(String::from)
    }

    pub async fn list_datasets(&self) -> Result<Vec<String>, String> {
        let url = self.url(&["projects", &self.project_id, "datasets"])?;
        let mut page_token: Option<String> = None;
        let mut datasets = Vec::new();
        loop {
            let mut query = vec![("maxResults".to_string(), "1000".to_string())];
            if let Some(token) = &page_token {
                query.push(("pageToken".to_string(), token.clone()));
            }
            let response = self
                .request_json(reqwest::Method::GET, url.clone(), &query, None)
                .await?;
            if let Some(entries) = response.get("datasets").and_then(Value::as_array) {
                datasets.extend(entries.iter().filter_map(|entry| {
                    entry
                        .pointer("/datasetReference/datasetId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }));
            }
            page_token = response
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if page_token.is_none() {
                break;
            }
        }
        datasets.sort();
        Ok(datasets)
    }

    pub async fn list_tables(&self, dataset: &str) -> Result<Vec<Value>, String> {
        let url = self.url(&["projects", &self.project_id, "datasets", dataset, "tables"])?;
        let mut page_token: Option<String> = None;
        let mut tables = Vec::new();
        loop {
            let mut query = vec![("maxResults".to_string(), "1000".to_string())];
            if let Some(token) = &page_token {
                query.push(("pageToken".to_string(), token.clone()));
            }
            let response = self
                .request_json(reqwest::Method::GET, url.clone(), &query, None)
                .await?;
            tables.extend(
                response
                    .get("tables")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            page_token = response
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if page_token.is_none() {
                break;
            }
        }
        Ok(tables)
    }

    pub async fn get_table(&self, dataset: &str, table: &str) -> Result<Value, String> {
        let url = self.url(&[
            "projects",
            &self.project_id,
            "datasets",
            dataset,
            "tables",
            table,
        ])?;
        self.request_json(reqwest::Method::GET, url, &[], None)
            .await
    }

    pub async fn execute_query(
        &self,
        sql: &str,
        dataset: Option<&str>,
        limit: Option<u32>,
        page: u32,
    ) -> Result<QueryOutput, String> {
        self.run_query(
            &QueryRequest {
                sql,
                dataset,
                page_size: limit.unwrap_or(DEFAULT_PAGE_SIZE),
                page: page.max(1),
                fetch_rows: true,
            },
            None,
        )
        .await
    }

    pub async fn dry_run(&self, sql: &str, dataset: Option<&str>) -> Result<Value, String> {
        let job = self.dry_run_job(sql, dataset, None).await?;
        Ok(json!({
            "kind": "bigquery_dry_run",
            "total_bytes_processed": job.pointer("/statistics/query/totalBytesProcessed").cloned().unwrap_or(Value::Null),
            "job_id": job.pointer("/jobReference/jobId").cloned().unwrap_or(Value::Null)
        }))
    }
}

/// Every setting that changes how the client talks to BigQuery must be part
/// of the key, or an edited connection would keep using a stale client.
fn cache_key(params: &ConnectionParams) -> String {
    let timeout = params.timeout_seconds().to_string();
    let mut hasher = Sha256::new();
    for part in [
        params.project_id.as_deref().unwrap_or_default(),
        params.auth_method(),
        params.credential().unwrap_or_default(),
        params.location().unwrap_or_default(),
        params.max_bytes_billed().unwrap_or_default(),
        timeout.as_str(),
        params.api_endpoint().unwrap_or_default(),
        params.impersonate_service_account().unwrap_or_default(),
    ] {
        hasher.update(part);
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

pub async fn client_for(params: &ConnectionParams) -> Result<Arc<BigQueryClient>, String> {
    let fingerprint = cache_key(params);
    let key = params
        .connection_id
        .as_deref()
        .map(|connection_id| format!("{connection_id}:{fingerprint}"))
        .unwrap_or(fingerprint);
    let cache = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(client) = cache.lock().await.get(&key).cloned() {
        return Ok(client);
    }
    let client = Arc::new(BigQueryClient::new(params).await?);
    cache.lock().await.insert(key, client.clone());
    Ok(client)
}

pub async fn clear_cache() {
    if let Some(cache) = CLIENTS.get() {
        cache.lock().await.clear();
    }
}

pub fn table_fields(table: &Value) -> Result<Vec<FieldSchema>, String> {
    let fields = table
        .pointer("/schema/fields")
        .cloned()
        .unwrap_or_else(|| json!([]));
    serde_json::from_value(fields)
        .map_err(|error| format!("Invalid BigQuery table schema: {error}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::cache_key;
    use crate::models::ConnectionParams;

    fn key(extra: serde_json::Value) -> String {
        cache_key(&ConnectionParams::from_value(
            &json!({"database": "p", "password": "legacy", "extra": extra}),
        ))
    }

    #[test]
    fn cache_key_tracks_every_client_setting() {
        let base = key(json!({"credential": "a"}));
        assert_ne!(base, key(json!({"credential": "b"})));
        for (field, value) in [
            ("query_timeout_seconds", "600"),
            ("location", "EU"),
            ("max_bytes_billed", "1000"),
            ("api_endpoint", "http://localhost:9050"),
            (
                "impersonate_service_account",
                "sa@p.iam.gserviceaccount.com",
            ),
            ("auth_method", "access_token"),
        ] {
            let mut extra = json!({"credential": "a"});
            extra[field] = json!(value);
            assert_ne!(base, key(extra), "{field}");
        }
    }
}
