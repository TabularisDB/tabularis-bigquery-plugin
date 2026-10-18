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
        let project_id = params.billing_project_id()?.to_string();
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
                ..QueryRequest::default()
            },
            None,
        )
        .await
    }
}

/// Every setting that changes how the client talks to BigQuery must be part
/// of the key, or an edited connection would keep using a stale client.
fn cache_key(params: &ConnectionParams) -> String {
    let timeout = params.timeout_seconds().to_string();
    let mut hasher = Sha256::new();
    for part in [
        params.billing_project_id().unwrap_or_default(),
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

// ---------------------------------------------------------------------------
// Catalog REST calls and parameterized statements used by the metadata, DDL,
// CRUD and routine handlers. Every call takes the target project explicitly so
// multi-project browsing never depends on the billing project.

/// One GoogleSQL statement or script with optional named query parameters.
#[derive(Debug, Clone, Default)]
pub struct Statement {
    pub sql: String,
    /// Complete `QueryParameter` resources (`name`, `parameterType`,
    /// `parameterValue`). Non-empty switches the job to named parameters.
    pub parameters: Vec<Value>,
    /// `(project, dataset)` used to resolve unqualified names.
    pub default_dataset: Option<(String, String)>,
    /// Overrides the connection location, e.g. with the dataset location.
    pub location: Option<String>,
    /// Rows to return; 0 runs the statement for its side effects only.
    pub max_results: u32,
}

impl Statement {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            ..Self::default()
        }
    }

    pub fn with_parameters(mut self, parameters: Vec<Value>) -> Self {
        self.parameters = parameters;
        self
    }

    pub fn in_dataset(mut self, project: &str, dataset: &str) -> Self {
        self.default_dataset = Some((project.to_string(), dataset.to_string()));
        self
    }

    pub fn at_location(mut self, location: Option<String>) -> Self {
        self.location = location;
        self
    }

    pub fn returning(mut self, max_results: u32) -> Self {
        self.max_results = max_results;
        self
    }

    /// The job request for this statement, run through the same job path as
    /// user queries (requestId, retries, jobTimeoutMs, labels, cancellation).
    pub fn request(&self) -> QueryRequest<'_> {
        let (project, dataset) = match &self.default_dataset {
            Some((project, dataset)) => (Some(project.as_str()), Some(dataset.as_str())),
            None => (None, None),
        };
        QueryRequest {
            sql: &self.sql,
            dataset,
            project,
            location: self.location.as_deref(),
            parameters: &self.parameters,
            page_size: self.max_results,
            page: 1,
            fetch_rows: self.max_results > 0,
        }
    }
}

impl BigQueryClient {
    async fn list_paged(&self, segments: &[&str], key: &str) -> Result<Vec<Value>, String> {
        let mut page_token: Option<String> = None;
        let mut items = Vec::new();
        loop {
            let mut query = vec![("maxResults".to_string(), "1000".to_string())];
            if let Some(token) = &page_token {
                query.push(("pageToken".to_string(), token.clone()));
            }
            let response = self.get(segments, &query).await.map_err(String::from)?;
            items.extend(
                response
                    .get(key)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            page_token = response
                .get("nextPageToken")
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())
                .map(str::to_owned);
            if page_token.is_none() {
                return Ok(items);
            }
        }
    }

    /// Projects visible to the credential (`GET projects`).
    pub async fn list_projects(&self) -> Result<Vec<String>, String> {
        let projects = self.list_paged(&["projects"], "projects").await?;
        Ok(projects
            .iter()
            .filter_map(|project| {
                project
                    .pointer("/projectReference/projectId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect())
    }

    pub async fn list_datasets_in(&self, project: &str) -> Result<Vec<String>, String> {
        let datasets = self
            .list_paged(&["projects", project, "datasets"], "datasets")
            .await?;
        let mut names = datasets
            .iter()
            .filter_map(|dataset| {
                dataset
                    .pointer("/datasetReference/datasetId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }

    pub async fn get_dataset(&self, project: &str, dataset: &str) -> Result<Value, String> {
        self.get(&["projects", project, "datasets", dataset], &[])
            .await
            .map_err(String::from)
    }

    pub async fn list_tables_in(&self, project: &str, dataset: &str) -> Result<Vec<Value>, String> {
        self.list_paged(
            &["projects", project, "datasets", dataset, "tables"],
            "tables",
        )
        .await
    }

    pub async fn get_table_in(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
    ) -> Result<Value, String> {
        self.get(
            &["projects", project, "datasets", dataset, "tables", table],
            &[],
        )
        .await
        .map_err(String::from)
    }

    pub async fn list_routines(&self, project: &str, dataset: &str) -> Result<Vec<Value>, String> {
        self.list_paged(
            &["projects", project, "datasets", dataset, "routines"],
            "routines",
        )
        .await
    }

    pub async fn get_routine(
        &self,
        project: &str,
        dataset: &str,
        routine: &str,
    ) -> Result<Value, String> {
        self.get(
            &[
                "projects", project, "datasets", dataset, "routines", routine,
            ],
            &[],
        )
        .await
        .map_err(String::from)
    }

    /// Runs a statement in the billing project, with optional named
    /// parameters, and waits for it to finish. Uses the query job runtime,
    /// so it is retried, labelled, bounded by the query timeout and
    /// cancellable like any user query.
    pub async fn run_statement(&self, statement: &Statement) -> Result<QueryOutput, String> {
        self.run_query(&statement.request(), None).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{cache_key, Statement};
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
            ("billing_project", "billing"),
        ] {
            let mut extra = json!({"credential": "a"});
            extra[field] = json!(value);
            assert_ne!(base, key(extra), "{field}");
        }
    }

    #[test]
    fn statement_request_carries_dataset_location_and_parameters() {
        let parameter = json!({"name": "id", "parameterType": {"type": "INT64"}, "parameterValue": {"value": "1"}});
        let statement = Statement::new("SELECT @id")
            .with_parameters(vec![parameter.clone()])
            .in_dataset("data", "ds")
            .at_location(Some("EU".into()))
            .returning(5);
        let request = statement.request();
        assert_eq!(request.project, Some("data"));
        assert_eq!(request.dataset, Some("ds"));
        assert_eq!(request.location, Some("EU"));
        assert_eq!(request.parameters, &[parameter]);
        assert_eq!(request.page_size, 5);
        assert!(request.fetch_rows);
        assert!(!Statement::new("DROP TABLE t").request().fetch_rows);
    }
}
