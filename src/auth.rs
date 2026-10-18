use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gcp_auth::{CustomServiceAccount, TokenProvider};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::models::ConnectionParams;

const BIGQUERY_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/bigquery",
    "https://www.googleapis.com/auth/cloud-platform",
];

#[derive(Clone)]
pub enum AuthProvider {
    Google(Arc<dyn TokenProvider>),
    AuthorizedUser(Arc<AuthorizedUserProvider>),
    AccessToken(String),
}

pub struct AuthorizedUserProvider {
    http: reqwest::Client,
    credentials: AuthorizedUserCredentials,
    cached: Mutex<Option<CachedToken>>,
}

#[derive(Clone, Deserialize)]
struct AuthorizedUserCredentials {
    #[serde(rename = "type")]
    credential_type: String,
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

impl AuthProvider {
    pub async fn from_params(params: &ConnectionParams) -> Result<Self, String> {
        match params.auth_method() {
            "adc" => gcp_auth::provider()
                .await
                .map(Self::Google)
                .map_err(|error| format!("Application Default Credentials are unavailable: {error}")),
            "service_account" => {
                let credential = params
                    .credential()
                    .ok_or_else(|| {
                        "Service account authentication requires a JSON key or file path"
                            .to_string()
                    })?;
                let account = if credential.starts_with('{') {
                    CustomServiceAccount::from_json(credential)
                } else {
                    CustomServiceAccount::from_file(Path::new(credential))
                }
                .map_err(|error| format!("Invalid service account credential: {error}"))?;
                Ok(Self::Google(Arc::new(account)))
            }
            "authorized_user" => {
                let credential = read_json_credential(params).await?;
                let credentials: AuthorizedUserCredentials = serde_json::from_str(&credential)
                    .map_err(|error| format!("Invalid authorized user credential JSON: {error}"))?;
                if credentials.credential_type != "authorized_user" {
                    return Err("OAuth credential JSON must have type 'authorized_user'".to_string());
                }
                if credentials.client_id.trim().is_empty()
                    || credentials.client_secret.trim().is_empty()
                    || credentials.refresh_token.trim().is_empty()
                {
                    return Err(
                        "OAuth credential JSON must contain client_id, client_secret and refresh_token"
                            .to_string(),
                    );
                }
                let http = reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(20))
                    .timeout(Duration::from_secs(30))
                    .build()
                    .map_err(|error| format!("Unable to initialize OAuth client: {error}"))?;
                Ok(Self::AuthorizedUser(Arc::new(AuthorizedUserProvider {
                    http,
                    credentials,
                    cached: Mutex::new(None),
                })))
            }
            "access_token" => params
                .credential()
                .map(|value| Self::AccessToken(value.to_string()))
                .ok_or_else(|| "Access token authentication requires a token".to_string()),
            method => Err(format!(
                "Unsupported authentication method '{method}'. Use adc, service_account, authorized_user, or access_token"
            )),
        }
    }

    pub async fn access_token(&self) -> Result<String, String> {
        match self {
            Self::Google(provider) => provider
                .token(BIGQUERY_SCOPES)
                .await
                .map(|token| token.as_str().to_string())
                .map_err(|error| format!("Unable to obtain a Google access token: {error}")),
            Self::AuthorizedUser(provider) => provider.access_token().await,
            Self::AccessToken(token) => Ok(token.clone()),
        }
    }
}

impl AuthorizedUserProvider {
    async fn access_token(&self) -> Result<String, String> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref() {
            if token.expires_at > Instant::now() + Duration::from_secs(30) {
                return Ok(token.value.clone());
            }
        }

        let response = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.credentials.client_id.as_str()),
                ("client_secret", self.credentials.client_secret.as_str()),
                ("refresh_token", self.credentials.refresh_token.as_str()),
            ])
            .send()
            .await
            .map_err(|error| format!("OAuth token refresh failed: {error}"))?;
        let status = response.status();
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|error| format!("Invalid OAuth token response: {error}"))?;
        if !status.is_success() {
            let message = payload
                .get("error_description")
                .or_else(|| payload.get("error"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown OAuth error");
            return Err(format!("OAuth token refresh failed: {message}"));
        }
        let value = payload
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "OAuth token response did not contain access_token".to_string())?
            .to_string();
        let expires_in = payload
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(3600);
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(expires_in),
        });
        Ok(value)
    }
}

async fn read_json_credential(params: &ConnectionParams) -> Result<String, String> {
    let credential = params
        .credential()
        .ok_or_else(|| "Authentication requires credential JSON or a file path".to_string())?;
    if credential.starts_with('{') {
        Ok(credential.to_string())
    } else {
        tokio::fs::read_to_string(Path::new(credential))
            .await
            .map_err(|error| format!("Unable to read credential file '{credential}': {error}"))
    }
}
