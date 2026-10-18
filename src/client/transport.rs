use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde_json::{json, Value};
use tokio::time::sleep;

use super::BigQueryClient;
use crate::errors::ApiError;

const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF_MS: u64 = 250;
const MAX_BACKOFF_MS: u64 = 8_000;

/// Failure of one REST call: either a decoded API error or a local problem.
#[derive(Debug, Clone)]
pub enum CallError {
    Api(ApiError),
    Other(String),
}

impl CallError {
    pub fn describe(&self) -> String {
        match self {
            Self::Api(error) => error.describe(),
            Self::Other(message) => message.clone(),
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(error) => error.status,
            Self::Other(_) => None,
        }
    }
}

impl From<CallError> for String {
    fn from(error: CallError) -> Self {
        error.describe()
    }
}

impl BigQueryClient {
    /// Send a REST call with bearer auth. Transient failures (429, 5xx and 403
    /// rate-limit or backend errors, transport errors) are retried with
    /// exponential backoff and jitter; a 401 refreshes the token once. Every
    /// POST this plugin sends is idempotent (jobs.query carries a requestId),
    /// so retrying is safe.
    pub(crate) async fn call(
        &self,
        method: Method,
        url: Url,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, CallError> {
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let token = self.auth.access_token().await.map_err(CallError::Other)?;
            let mut request = self.http.request(method.clone(), url.clone()).query(query);
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            if let Some(body) = body {
                request = request.json(body);
            }

            let error = match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.map_err(|error| {
                        CallError::Other(format!("Unable to read the BigQuery response: {error}"))
                    })?;
                    if status.is_success() {
                        return serde_json::from_str(&text).map_err(|error| {
                            CallError::Other(format!("Invalid BigQuery response: {error}"))
                        });
                    }
                    let payload: Value =
                        serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
                    let error = ApiError::from_response(status.as_u16(), &payload);
                    if status == StatusCode::UNAUTHORIZED && !refreshed {
                        refreshed = true;
                        self.auth.invalidate().await;
                        continue;
                    }
                    if !error.is_retryable() {
                        return Err(CallError::Api(error));
                    }
                    CallError::Api(error)
                }
                Err(error) => CallError::Other(format!("BigQuery API request failed: {error}")),
            };
            attempt += 1;
            if attempt >= MAX_ATTEMPTS {
                return Err(error);
            }
            sleep(backoff(attempt)).await;
        }
    }

    pub(crate) async fn get(
        &self,
        segments: &[&str],
        query: &[(String, String)],
    ) -> Result<Value, CallError> {
        let url = self.url(segments).map_err(CallError::Other)?;
        self.call(Method::GET, url, query, None).await
    }

    pub(crate) async fn post(
        &self,
        segments: &[&str],
        query: &[(String, String)],
        body: &Value,
    ) -> Result<Value, CallError> {
        let url = self.url(segments).map_err(CallError::Other)?;
        self.call(Method::POST, url, query, Some(body)).await
    }

    /// Cheapest authenticated call: one dataset of the project.
    pub async fn ping(&self) -> Result<(), String> {
        self.get(
            &["projects", self.data_project(), "datasets"],
            &[("maxResults".to_string(), "1".to_string())],
        )
        .await
        .map(|_| ())
        .map_err(String::from)
    }
}

/// Exponential backoff with up to 50 % random jitter.
fn backoff(attempt: u32) -> Duration {
    let base = (BASE_BACKOFF_MS << attempt.saturating_sub(1).min(6)).min(MAX_BACKOFF_MS);
    let jitter = (uuid::Uuid::new_v4().as_u128() % 1_000) as u64 * base / 2_000;
    Duration::from_millis(base + jitter)
}

#[cfg(test)]
mod tests {
    use super::backoff;

    #[test]
    fn backoff_grows_and_stays_bounded() {
        for attempt in 1..10 {
            let delay = backoff(attempt).as_millis() as u64;
            let base = (250u64 << (attempt - 1).min(6)).min(8_000);
            assert!(
                delay >= base && delay <= base + base / 2,
                "{attempt}: {delay}"
            );
        }
    }
}
