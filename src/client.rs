use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};

use crate::auth::AuthProvider;
use crate::bigquery::{
    api_error, fields_from_response, rows_from_response, FieldSchema, QueryOutput,
};
use crate::models::ConnectionParams;

const API_BASE: &str = "https://bigquery.googleapis.com/bigquery/v2/";
const DEFAULT_PAGE_SIZE: u32 = 10_000;

static CLIENTS: OnceLock<Mutex<HashMap<String, Arc<BigQueryClient>>>> = OnceLock::new();

pub struct BigQueryClient {
    http: reqwest::Client,
    auth: AuthProvider,
    pub project_id: String,
    location: Option<String>,
    max_bytes_billed: Option<String>,
    timeout: Duration,
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
        Ok(Self {
            http,
            auth: AuthProvider::from_params(params).await?,
            project_id,
            location: params.location().map(str::to_owned),
            max_bytes_billed,
            timeout,
        })
    }

    fn url(&self, segments: &[&str]) -> Result<Url, String> {
        let mut url = Url::parse(API_BASE).map_err(|error| error.to_string())?;
        url.path_segments_mut()
            .map_err(|_| "Invalid BigQuery API base URL".to_string())?
            .extend(segments);
        Ok(url)
    }

    async fn request_json(
        &self,
        method: Method,
        url: Url,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let mut last_error = String::new();
        for attempt in 0..4u32 {
            let token = self.auth.access_token().await?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(token)
                .query(query);
            if let Some(body) = body {
                request = request.json(body);
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.map_err(|error| {
                        format!("Unable to read the BigQuery response: {error}")
                    })?;
                    let payload: Value =
                        serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
                    if status.is_success() {
                        return Ok(payload);
                    }
                    let message = api_error(&payload).unwrap_or_else(|| format!("HTTP {status}"));
                    last_error = format!("BigQuery API request failed ({status}): {message}");
                    if !is_retryable(status) || attempt == 3 {
                        return Err(last_error);
                    }
                }
                Err(error) => {
                    last_error = format!("BigQuery API request failed: {error}");
                    if attempt == 3 {
                        return Err(last_error);
                    }
                }
            }
            sleep(Duration::from_millis(250 * 2u64.pow(attempt))).await;
        }
        Err(last_error)
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
                .request_json(Method::GET, url.clone(), &query, None)
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
                .request_json(Method::GET, url.clone(), &query, None)
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
        self.request_json(Method::GET, url, &[], None).await
    }

    pub async fn execute_query(
        &self,
        sql: &str,
        dataset: Option<&str>,
        limit: Option<u32>,
        page: u32,
    ) -> Result<QueryOutput, String> {
        self.run_query(sql, dataset, limit, page, false).await
    }

    pub async fn dry_run(&self, sql: &str, dataset: Option<&str>) -> Result<Value, String> {
        let output = self.run_query(sql, dataset, Some(0), 1, true).await?;
        Ok(json!({
            "kind": "bigquery_dry_run",
            "total_bytes_processed": output.total_bytes_processed,
            "job_id": output.job_id
        }))
    }

    async fn run_query(
        &self,
        sql: &str,
        dataset: Option<&str>,
        limit: Option<u32>,
        page: u32,
        dry_run: bool,
    ) -> Result<QueryOutput, String> {
        if sql.trim().is_empty() {
            return Err("Query must not be empty".to_string());
        }
        let url = self.url(&["projects", &self.project_id, "queries"])?;
        let requested_size = limit.unwrap_or(DEFAULT_PAGE_SIZE);
        let initial_timeout_ms = self.timeout.as_millis().min(120_000) as u64;
        let mut body = json!({
            "query": sql,
            "useLegacySql": false,
            "timeoutMs": initial_timeout_ms,
            "maxResults": if dry_run { 0 } else { requested_size.saturating_add(1) },
            "dryRun": dry_run
        });
        if let Some(location) = &self.location {
            body["location"] = Value::String(location.clone());
        }
        if let Some(maximum) = &self.max_bytes_billed {
            body["maximumBytesBilled"] = Value::String(maximum.clone());
        }
        if let Some(dataset) = dataset.filter(|value| !value.is_empty()) {
            body["defaultDataset"] = json!({"projectId": self.project_id, "datasetId": dataset});
        }

        let initial = self
            .request_json(Method::POST, url, &[], Some(&body))
            .await?;
        if let Some(error) = api_error(&initial) {
            return Err(error);
        }
        let job_id = initial
            .pointer("/jobReference/jobId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if dry_run {
            return Ok(output_from_response(initial, requested_size, job_id));
        }

        let job_id = job_id.ok_or_else(|| "BigQuery did not return a query job ID".to_string())?;
        let location = initial
            .pointer("/jobReference/location")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.location.clone());
        let offset = u64::from(page.saturating_sub(1)) * u64::from(requested_size);
        let final_response = self
            .wait_for_results(
                &job_id,
                location.as_deref(),
                offset,
                requested_size.saturating_add(1),
            )
            .await?;
        let mut output = output_from_response(final_response, requested_size, Some(job_id));
        if output.rows.len() > requested_size as usize {
            output.rows.truncate(requested_size as usize);
            output.has_more = true;
        }
        if let Some(total) = output.total_rows {
            output.has_more = offset.saturating_add(output.rows.len() as u64) < total;
        }
        Ok(output)
    }

    async fn wait_for_results(
        &self,
        job_id: &str,
        location: Option<&str>,
        start_index: u64,
        max_results: u32,
    ) -> Result<Value, String> {
        let url = self.url(&["projects", &self.project_id, "queries", job_id])?;
        let started = Instant::now();
        loop {
            let mut query = vec![
                ("timeoutMs".to_string(), "10000".to_string()),
                ("startIndex".to_string(), start_index.to_string()),
                ("maxResults".to_string(), max_results.to_string()),
            ];
            if let Some(location) = location {
                query.push(("location".to_string(), location.to_string()));
            }
            let response = self
                .request_json(Method::GET, url.clone(), &query, None)
                .await?;
            if let Some(error) = api_error(&response) {
                return Err(error);
            }
            if response
                .get("jobComplete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(response);
            }
            if started.elapsed() >= self.timeout {
                return Err(format!(
                    "BigQuery query timed out after {} seconds",
                    self.timeout.as_secs()
                ));
            }
            sleep(Duration::from_millis(250)).await;
        }
    }
}

fn output_from_response(response: Value, page_size: u32, job_id: Option<String>) -> QueryOutput {
    let fields = fields_from_response(&response).unwrap_or_default();
    let columns = fields.iter().map(|field| field.name.clone()).collect();
    let rows = rows_from_response(&response, &fields);
    let affected_rows = response
        .get("numDmlAffectedRows")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    let total_rows = response
        .get("totalRows")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok());
    let has_more = response.get("pageToken").is_some() || rows.len() > page_size as usize;
    QueryOutput {
        columns,
        fields,
        rows,
        affected_rows,
        total_rows,
        has_more,
        total_bytes_processed: response
            .get("totalBytesProcessed")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cache_hit: response.get("cacheHit").and_then(Value::as_bool),
        job_id,
    }
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::INTERNAL_SERVER_ERROR
        || status == StatusCode::BAD_GATEWAY
        || status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::GATEWAY_TIMEOUT
}

fn cache_key(params: &ConnectionParams) -> String {
    let mut hasher = Sha256::new();
    hasher.update(params.project_id.as_deref().unwrap_or_default());
    hasher.update([0]);
    hasher.update(params.auth_method());
    hasher.update([0]);
    hasher.update(params.password.as_deref().unwrap_or_default());
    hasher.update([0]);
    hasher.update(params.location().unwrap_or_default());
    hasher.update([0]);
    hasher.update(params.max_bytes_billed().unwrap_or_default());
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
