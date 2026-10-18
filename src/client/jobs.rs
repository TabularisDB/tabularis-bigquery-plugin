//! Query job lifecycle: start, wait, cancel, page through results and reuse
//! a finished job for later pages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::{sleep_until, Instant};

use super::transport::CallError;
use super::BigQueryClient;
use crate::bigquery::{fields_from_response, rows_from_response, QueryOutput};
use crate::cancel::{self, JobCanceller};
use crate::errors::ApiError;
use crate::session::SessionState;
use crate::statements;

/// How long one jobs.query / getQueryResults call waits server-side.
const WAIT_SLICE_MS: u64 = 10_000;
/// Finished results stay addressable for later pages for this long.
const RESULT_TTL: Duration = Duration::from_secs(10 * 60);
const RESULT_CACHE_CAPACITY: usize = 256;

pub struct QueryRequest<'a> {
    pub sql: &'a str,
    pub dataset: Option<&'a str>,
    pub page_size: u32,
    pub page: u32,
    /// False when only the job statistics matter (EXPLAIN ANALYZE).
    pub fetch_rows: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRef {
    pub project: String,
    pub job_id: String,
    pub location: Option<String>,
}

struct CachedResult {
    job: JobRef,
    affected_rows: u64,
    stored: std::time::Instant,
}

static RESULTS: OnceLock<Mutex<HashMap<String, CachedResult>>> = OnceLock::new();

fn results() -> &'static Mutex<HashMap<String, CachedResult>> {
    RESULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lookup_result(key: &str) -> Option<(JobRef, u64)> {
    let cache = results().lock().ok()?;
    cache
        .get(key)
        .filter(|entry| entry.stored.elapsed() < RESULT_TTL)
        .map(|entry| (entry.job.clone(), entry.affected_rows))
}

fn store_result(key: String, job: JobRef, affected_rows: u64) {
    let Ok(mut cache) = results().lock() else {
        return;
    };
    cache.retain(|_, entry| entry.stored.elapsed() < RESULT_TTL);
    if cache.len() >= RESULT_CACHE_CAPACITY {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.stored)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        key,
        CachedResult {
            job,
            affected_rows,
            stored: std::time::Instant::now(),
        },
    );
}

/// Rows read for one page, before value conversion.
struct PageData {
    schema: Value,
    rows: Vec<Value>,
    total_rows: Option<u64>,
    page_token: Option<String>,
}

impl PageData {
    fn into_output(self, offset: u64, page_size: u32) -> QueryOutput {
        let response = json!({"schema": self.schema, "rows": self.rows});
        let fields = fields_from_response(&response).unwrap_or_default();
        let mut rows = rows_from_response(&response, &fields);
        rows.truncate(page_size as usize);
        let has_more = match self.total_rows {
            Some(total) => offset.saturating_add(rows.len() as u64) < total,
            None => self.page_token.is_some(),
        };
        QueryOutput {
            columns: fields.iter().map(|field| field.name.clone()).collect(),
            fields,
            rows,
            affected_rows: 0,
            total_rows: self.total_rows,
            has_more,
            total_bytes_processed: None,
            cache_hit: None,
            job_id: None,
            source: None,
        }
    }
}

fn string_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| match value {
        Value::String(text) => text.parse().ok(),
        other => other.as_u64(),
    })
}

fn rows_of(response: &Value) -> Vec<Value> {
    response
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn page_token_of(response: &Value) -> Option<String> {
    response
        .get("pageToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

impl BigQueryClient {
    // Project selection for jobs and unqualified names lives only in these two
    // helpers, so a separate billing project can be wired in here alone.

    /// Project that runs and bills query jobs.
    fn job_project(&self) -> &str {
        &self.project_id
    }

    /// Project of unqualified table names (`defaultDataset.projectId`) and of
    /// table browsing.
    pub(crate) fn data_project(&self) -> &str {
        &self.project_id
    }

    /// Run a statement and return one page of its result.
    pub async fn run_query(
        &self,
        request: &QueryRequest<'_>,
        session: Option<&mut SessionState>,
    ) -> Result<QueryOutput, String> {
        self.run_job(request, session)
            .await
            .map(|(output, _)| output)
    }

    /// Execute a statement and return its finished job resource, for
    /// EXPLAIN ANALYZE.
    pub async fn run_for_statistics(
        &self,
        sql: &str,
        dataset: Option<&str>,
        session: Option<&mut SessionState>,
    ) -> Result<Value, String> {
        let request = QueryRequest {
            sql,
            dataset,
            page_size: 0,
            page: 1,
            fetch_rows: false,
        };
        let (_, job) = self.run_job(&request, session).await?;
        let job = job.ok_or_else(|| "BigQuery did not return a query job".to_string())?;
        self.get_job(&job).await.map_err(String::from)
    }

    async fn run_job(
        &self,
        request: &QueryRequest<'_>,
        mut session: Option<&mut SessionState>,
    ) -> Result<(QueryOutput, Option<JobRef>), String> {
        if request.sql.trim().is_empty() {
            return Err("Query must not be empty".to_string());
        }
        let page_size = request.page_size;
        let offset = u64::from(request.page.saturating_sub(1)) * u64::from(page_size);
        let session_key = session.as_ref().and_then(|state| state.bigquery_id.clone());
        let cache_key = self.result_key(request.sql, request.dataset, session_key.as_deref());

        if request.page > 1 && request.fetch_rows {
            if let Some((job, affected_rows)) = lookup_result(&cache_key) {
                let page = self.fetch_page(&job, offset, page_size).await?;
                let mut output = page.into_output(offset, page_size);
                output.affected_rows = affected_rows;
                output.job_id = Some(job.job_id.clone());
                output.source = Some("job".to_string());
                return Ok((output, Some(job)));
            }
            if !statements::is_read_only_query(request.sql) {
                return Err(
                    "The result of this statement has expired. Run it again to fetch more rows."
                        .to_string(),
                );
            }
        }

        if session.is_none() && request.fetch_rows {
            if let Some(target) = crate::browse::parse(request.sql, request.dataset) {
                if let Ok(Some(output)) = self.browse_table(&target, request.page, page_size).await
                {
                    return Ok((output, None));
                }
            }
        }

        let script = statements::may_be_script(request.sql);
        let rows_requested = request.fetch_rows && offset == 0 && !script;
        let max_results = if rows_requested { page_size } else { 0 };
        let deadline = Instant::now() + self.timeout;

        let (job, initial) = self
            .start_query(request, session.as_deref_mut(), max_results)
            .await?;
        let canceller = self.canceller(job.clone());
        cancel::register_job(canceller);
        let outcome = async {
            let done = self
                .wait_for_completion(&job, initial.clone(), max_results, deadline)
                .await?;
            self.finish(
                request,
                &job,
                &initial,
                done,
                session,
                rows_requested,
                offset,
                &cache_key,
            )
            .await
        }
        .await;
        cancel::clear_job();
        outcome.map(|output| (output, Some(job)))
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        request: &QueryRequest<'_>,
        job: &JobRef,
        initial: &Value,
        done: Value,
        session: Option<&mut SessionState>,
        rows_requested: bool,
        offset: u64,
        cache_key: &str,
    ) -> Result<QueryOutput, String> {
        let mut resource: Option<Value> = None;
        let has_errors = done
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty());
        if has_errors {
            let job_resource = self.get_job(job).await?;
            if let Some(error) = job_resource.pointer("/status/errorResult") {
                return Err(ApiError::from_error_proto(error).describe());
            }
            resource = Some(job_resource);
        }

        if let Some(state) = session {
            if state.bigquery_id.is_none() {
                let mut id = initial
                    .pointer("/sessionInfo/sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if id.is_none() {
                    if resource.is_none() {
                        resource = self.get_job(job).await.ok();
                    }
                    id = resource
                        .as_ref()
                        .and_then(|value| value.pointer("/statistics/sessionInfo/sessionId"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                if id.is_some() {
                    state.bigquery_id = id;
                    state.location = job.location.clone();
                }
            }
        }

        let mut result_job = Some(job.clone());
        let mut affected_rows = string_u64(done.get("numDmlAffectedRows")).unwrap_or_default();
        let mut use_initial_rows = rows_requested;
        if statements::may_be_script(request.sql) {
            if resource.is_none() {
                resource = self.get_job(job).await.ok();
            }
            let statement_type = resource
                .as_ref()
                .and_then(|value| value.pointer("/statistics/query/statementType"))
                .and_then(Value::as_str);
            if statement_type == Some("SCRIPT") {
                let children = self.list_child_jobs(job).await?;
                let (child, affected) = script_result(&children, job);
                result_job = child;
                affected_rows = affected;
                use_initial_rows = false;
            }
        }

        let mut output = match (&result_job, request.fetch_rows) {
            (Some(result_job), true) => {
                let page = if use_initial_rows && result_job == job {
                    self.complete_page(job, done.clone(), request.page_size)
                        .await?
                } else {
                    self.fetch_page(result_job, offset, request.page_size)
                        .await?
                };
                page.into_output(offset, request.page_size)
            }
            _ => PageData {
                schema: Value::Null,
                rows: Vec::new(),
                total_rows: None,
                page_token: None,
            }
            .into_output(offset, request.page_size),
        };
        output.affected_rows = affected_rows;
        output.total_bytes_processed = done
            .get("totalBytesProcessed")
            .and_then(Value::as_str)
            .map(str::to_owned);
        output.cache_hit = done.get("cacheHit").and_then(Value::as_bool);
        output.job_id = Some(job.job_id.clone());
        output.source = Some("job".to_string());

        if let (Some(result_job), true) = (result_job, request.fetch_rows) {
            if !output.fields.is_empty() {
                store_result(cache_key.to_string(), result_job, affected_rows);
            }
        }
        Ok(output)
    }

    async fn start_query(
        &self,
        request: &QueryRequest<'_>,
        session: Option<&mut SessionState>,
        max_results: u32,
    ) -> Result<(JobRef, Value), String> {
        let mut body = json!({
            "query": request.sql,
            "useLegacySql": false,
            "requestId": uuid::Uuid::new_v4().to_string(),
            "timeoutMs": WAIT_SLICE_MS.min(self.timeout.as_millis() as u64),
            "maxResults": max_results,
            "jobTimeoutMs": self.timeout.as_millis().to_string(),
            "labels": {"client": "tabularis"},
            "formatOptions": {"useInt64Timestamp": true}
        });
        let mut location = self.location.clone();
        if let Some(state) = session {
            match &state.bigquery_id {
                Some(id) => {
                    body["connectionProperties"] = json!([{"key": "session_id", "value": id}]);
                    if state.location.is_some() {
                        location = state.location.clone();
                    }
                }
                None => body["createSession"] = Value::Bool(true),
            }
        }
        if let Some(location) = &location {
            body["location"] = Value::String(location.clone());
        }
        if let Some(maximum) = &self.max_bytes_billed {
            body["maximumBytesBilled"] = Value::String(maximum.clone());
        }
        if let Some(dataset) = request.dataset.filter(|value| !value.is_empty()) {
            body["defaultDataset"] =
                json!({"projectId": self.data_project(), "datasetId": dataset});
        }

        let response = self
            .post(&["projects", self.job_project(), "queries"], &[], &body)
            .await
            .map_err(String::from)?;
        let job_id = response
            .pointer("/jobReference/jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| "BigQuery did not return a query job ID".to_string())?;
        let job = JobRef {
            project: response
                .pointer("/jobReference/projectId")
                .and_then(Value::as_str)
                .unwrap_or(self.job_project())
                .to_string(),
            job_id: job_id.to_string(),
            location: response
                .pointer("/jobReference/location")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(location),
        };
        Ok((job, response))
    }

    async fn wait_for_completion(
        &self,
        job: &JobRef,
        mut response: Value,
        max_results: u32,
        deadline: Instant,
    ) -> Result<Value, String> {
        loop {
            if response
                .get("jobComplete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(response);
            }
            if cancel::is_cancelled() {
                return Err("Query cancelled. The BigQuery job was asked to stop.".to_string());
            }
            tokio::select! {
                biased;
                _ = cancel::cancelled() => {
                    return Err("Query cancelled. The BigQuery job was asked to stop.".to_string());
                }
                _ = sleep_until(deadline) => {
                    let _ = self.cancel_job(job).await;
                    return Err(format!(
                        "BigQuery query exceeded the {} second query timeout and was cancelled.\nHint: raise Query timeout in the connection settings; above 120 seconds also raise the Tabularis plugin call timeout.",
                        self.timeout.as_secs()
                    ));
                }
                next = self.get_query_results(job, Some(0), max_results, None) => {
                    response = next.map_err(String::from)?;
                }
            }
        }
    }

    /// Fill a page from a completed response, following `pageToken` when the
    /// server returned fewer rows than requested (responses are size capped).
    async fn complete_page(
        &self,
        job: &JobRef,
        first: Value,
        page_size: u32,
    ) -> Result<PageData, String> {
        let mut rows = rows_of(&first);
        let mut page_token = page_token_of(&first);
        let total_rows = string_u64(first.get("totalRows"));
        let schema = first.get("schema").cloned().unwrap_or(Value::Null);
        while rows.len() < page_size as usize {
            let Some(token) = page_token.clone() else {
                break;
            };
            let remaining = page_size - rows.len() as u32;
            let next = self
                .get_query_results(job, None, remaining, Some(&token))
                .await
                .map_err(String::from)?;
            let batch = rows_of(&next);
            page_token = page_token_of(&next);
            if batch.is_empty() {
                break;
            }
            rows.extend(batch);
        }
        Ok(PageData {
            schema,
            rows,
            total_rows,
            page_token,
        })
    }

    /// Read rows `offset..offset + page_size` of a finished job.
    async fn fetch_page(
        &self,
        job: &JobRef,
        offset: u64,
        page_size: u32,
    ) -> Result<PageData, String> {
        let first = self
            .get_query_results(job, Some(offset), page_size, None)
            .await
            .map_err(String::from)?;
        if !first
            .get("jobComplete")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Err("BigQuery job is still running".to_string());
        }
        self.complete_page(job, first, page_size).await
    }

    async fn get_query_results(
        &self,
        job: &JobRef,
        start_index: Option<u64>,
        max_results: u32,
        page_token: Option<&str>,
    ) -> Result<Value, CallError> {
        let mut query = vec![
            ("timeoutMs".to_string(), WAIT_SLICE_MS.to_string()),
            ("maxResults".to_string(), max_results.to_string()),
            (
                "formatOptions.useInt64Timestamp".to_string(),
                "true".to_string(),
            ),
        ];
        if let Some(token) = page_token {
            query.push(("pageToken".to_string(), token.to_string()));
        } else if let Some(start) = start_index {
            query.push(("startIndex".to_string(), start.to_string()));
        }
        if let Some(location) = &job.location {
            query.push(("location".to_string(), location.clone()));
        }
        self.get(&["projects", &job.project, "queries", &job.job_id], &query)
            .await
    }

    pub async fn get_job(&self, job: &JobRef) -> Result<Value, CallError> {
        let mut query = Vec::new();
        if let Some(location) = &job.location {
            query.push(("location".to_string(), location.clone()));
        }
        self.get(&["projects", &job.project, "jobs", &job.job_id], &query)
            .await
    }

    async fn list_child_jobs(&self, job: &JobRef) -> Result<Vec<Value>, String> {
        let mut children = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![
                ("parentJobId".to_string(), job.job_id.clone()),
                ("maxResults".to_string(), "1000".to_string()),
            ];
            if let Some(token) = &page_token {
                query.push(("pageToken".to_string(), token.clone()));
            }
            let response = self
                .get(&["projects", &job.project, "jobs"], &query)
                .await
                .map_err(String::from)?;
            children.extend(
                response
                    .get("jobs")
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
        Ok(children)
    }

    /// Ask BigQuery to stop a job. Errors are ignored by callers: the job may
    /// already have finished.
    pub async fn cancel_job(&self, job: &JobRef) -> Result<(), String> {
        let url = self.url(&["projects", &job.project, "jobs", &job.job_id, "cancel"])?;
        let mut query = Vec::new();
        if let Some(location) = &job.location {
            query.push(("location".to_string(), location.clone()));
        }
        self.call(reqwest::Method::POST, url, &query, None)
            .await
            .map(|_| ())
            .map_err(String::from)
    }

    fn canceller(&self, job: JobRef) -> JobCanceller {
        let client = self.clone();
        Arc::new(move || {
            let client = client.clone();
            let job = job.clone();
            Box::pin(async move {
                if let Err(error) = client.cancel_job(&job).await {
                    eprintln!(
                        "bigquery-plugin: unable to cancel job {}: {error}",
                        job.job_id
                    );
                }
            })
        })
    }

    /// Abort a BigQuery session from outside it.
    pub async fn abort_session(
        &self,
        session_id: &str,
        location: Option<&str>,
    ) -> Result<(), String> {
        let escaped = session_id.replace('\\', "\\\\").replace('\'', "\\'");
        let mut body = json!({
            "query": format!("CALL BQ.ABORT_SESSION('{escaped}')"),
            "useLegacySql": false,
            "requestId": uuid::Uuid::new_v4().to_string(),
            "timeoutMs": WAIT_SLICE_MS,
            "labels": {"client": "tabularis"}
        });
        if let Some(location) = location.or(self.location.as_deref()) {
            body["location"] = Value::String(location.to_string());
        }
        self.post(&["projects", self.job_project(), "queries"], &[], &body)
            .await
            .map(|_| ())
            .map_err(String::from)
    }

    /// Dry-run a statement and return the job resource with its estimate.
    pub async fn dry_run_job(
        &self,
        sql: &str,
        dataset: Option<&str>,
        session: Option<&SessionState>,
    ) -> Result<Value, String> {
        let mut query = json!({"query": sql, "useLegacySql": false});
        if let Some(dataset) = dataset.filter(|value| !value.is_empty()) {
            query["defaultDataset"] =
                json!({"projectId": self.data_project(), "datasetId": dataset});
        }
        if let Some(maximum) = &self.max_bytes_billed {
            query["maximumBytesBilled"] = Value::String(maximum.clone());
        }
        let mut location = self.location.clone();
        if let Some(state) = session {
            if let Some(id) = &state.bigquery_id {
                query["connectionProperties"] = json!([{"key": "session_id", "value": id}]);
                location = state.location.clone().or(location);
            }
        }
        let mut reference = json!({"projectId": self.job_project()});
        if let Some(location) = location {
            reference["location"] = Value::String(location);
        }
        let body = json!({
            "jobReference": reference,
            "configuration": {"dryRun": true, "query": query, "labels": {"client": "tabularis"}}
        });
        self.post(&["projects", self.job_project(), "jobs"], &[], &body)
            .await
            .map_err(String::from)
    }

    fn result_key(&self, sql: &str, dataset: Option<&str>, session: Option<&str>) -> String {
        let mut hasher = Sha256::new();
        for part in [
            self.key.as_str(),
            sql,
            dataset.unwrap_or_default(),
            session.unwrap_or_default(),
        ] {
            hasher.update(part);
            hasher.update([0]);
        }
        format!("{:x}", hasher.finalize())
    }
}

/// Pick the child job whose rows a script returns and sum DML counts.
fn script_result(children: &[Value], parent: &JobRef) -> (Option<JobRef>, u64) {
    let mut ordered: Vec<&Value> = children.iter().collect();
    ordered.sort_by_key(|child| {
        (
            string_u64(child.pointer("/statistics/creationTime")).unwrap_or_default(),
            string_u64(child.pointer("/statistics/startTime")).unwrap_or_default(),
        )
    });
    let affected = ordered
        .iter()
        .filter_map(|child| string_u64(child.pointer("/statistics/query/numDmlAffectedRows")))
        .sum();
    let result = ordered
        .iter()
        .rev()
        .find(|child| {
            child
                .pointer("/statistics/query/statementType")
                .and_then(Value::as_str)
                == Some("SELECT")
                && child
                    .pointer("/statistics/scriptStatistics/evaluationKind")
                    .and_then(Value::as_str)
                    != Some("EXPRESSION")
                && child.pointer("/status/errorResult").is_none()
        })
        .and_then(|child| {
            Some(JobRef {
                project: child
                    .pointer("/jobReference/projectId")
                    .and_then(Value::as_str)
                    .unwrap_or(&parent.project)
                    .to_string(),
                job_id: child.pointer("/jobReference/jobId")?.as_str()?.to_string(),
                location: child
                    .pointer("/jobReference/location")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| parent.location.clone()),
            })
        });
    (result, affected)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{script_result, JobRef};

    #[test]
    fn script_returns_last_select_statement_and_sums_dml() {
        let parent = JobRef {
            project: "p".into(),
            job_id: "parent".into(),
            location: Some("EU".into()),
        };
        let child =
            |id: &str, created: &str, kind: &str, statement: &str, affected: Option<&str>| {
                let mut value = json!({
                    "jobReference": {"projectId": "p", "jobId": id},
                    "statistics": {
                        "creationTime": created,
                        "query": {"statementType": statement},
                        "scriptStatistics": {"evaluationKind": kind}
                    }
                });
                if let Some(affected) = affected {
                    value["statistics"]["query"]["numDmlAffectedRows"] = json!(affected);
                }
                value
            };
        // jobs.list returns newest first.
        let children = vec![
            child("declare", "5", "EXPRESSION", "SELECT", None),
            child("update", "4", "STATEMENT", "UPDATE", Some("3")),
            child("second", "3", "STATEMENT", "SELECT", None),
            child("insert", "2", "STATEMENT", "INSERT", Some("2")),
            child("first", "1", "STATEMENT", "SELECT", None),
        ];
        let (job, affected) = script_result(&children, &parent);
        let job = job.unwrap();
        assert_eq!(job.job_id, "second");
        assert_eq!(job.location.as_deref(), Some("EU"));
        assert_eq!(affected, 5);
        assert_eq!(script_result(&[], &parent), (None, 0));
    }
}
