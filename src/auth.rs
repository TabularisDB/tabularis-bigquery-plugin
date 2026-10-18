use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gcp_auth::{CustomServiceAccount, TokenProvider};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::endpoint::ApiEndpoint;
use crate::models::ConnectionParams;

const BIGQUERY_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/bigquery",
    "https://www.googleapis.com/auth/cloud-platform",
];
const IAM_CREDENTIALS_BASE: &str = "https://iamcredentials.googleapis.com/v1/";

type ProviderFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<AuthProvider, String>> + Send + 'a>>;

type GoogleFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Arc<dyn TokenProvider>, String>> + Send>,
>;
/// Builds a fresh gcp_auth provider, so a rejected token is not served again
/// from the old provider's internal cache.
type GoogleBuild = Arc<dyn Fn() -> GoogleFuture + Send + Sync>;

#[derive(Clone)]
pub enum AuthProvider {
    Google(Arc<GoogleProvider>),
    AuthorizedUser(Arc<AuthorizedUserProvider>),
    Impersonated(Arc<ImpersonatedProvider>),
    AccessToken(String),
    /// No Authorization header; only allowed for loopback emulator endpoints.
    None,
}

/// A gcp_auth provider (service account key, metadata server, gcloud CLI).
/// gcp_auth caches tokens internally without a way to drop them, so after a
/// 401 the provider is rebuilt instead.
pub struct GoogleProvider {
    current: Mutex<Arc<dyn TokenProvider>>,
    build: GoogleBuild,
}

impl GoogleProvider {
    async fn new(build: GoogleBuild) -> Result<Self, String> {
        let current = build().await?;
        Ok(Self {
            current: Mutex::new(current),
            build,
        })
    }

    async fn access_token(&self) -> Result<String, String> {
        let provider = self.current.lock().await.clone();
        provider
            .token(BIGQUERY_SCOPES)
            .await
            .map(|token| token.as_str().to_string())
            .map_err(|error| format!("Unable to obtain a Google access token: {error}"))
    }

    async fn rebuild(&self) {
        match (self.build)().await {
            Ok(provider) => *self.current.lock().await = provider,
            Err(error) => {
                eprintln!("bigquery-plugin: unable to refresh Google credentials: {error}")
            }
        }
    }
}

pub struct AuthorizedUserProvider {
    http: reqwest::Client,
    credentials: AuthorizedUserCredentials,
    cached: Mutex<Option<CachedToken>>,
}

/// Exchanges a source credential for a short-lived token of another service
/// account through the IAM Credentials `generateAccessToken` method.
pub struct ImpersonatedProvider {
    http: reqwest::Client,
    source: AuthProvider,
    url: Url,
    delegates: Vec<String>,
    cached: Mutex<Option<CachedToken>>,
}

#[derive(Clone, Deserialize)]
struct AuthorizedUserCredentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

impl CachedToken {
    fn fresh(&self) -> Option<String> {
        (self.expires_at > Instant::now() + Duration::from_secs(60)).then(|| self.value.clone())
    }
}

impl AuthProvider {
    pub async fn from_params(
        params: &ConnectionParams,
        endpoint: &ApiEndpoint,
    ) -> Result<Self, String> {
        let base = match params.auth_method() {
            "adc" => application_default().await?,
            "service_account" => {
                let credential = params.credential().ok_or_else(|| {
                    "Service account authentication requires a JSON key or file path".to_string()
                })?;
                let text = load_json_credential(credential).await?;
                from_credential_json(&text, &["service_account", "impersonated_service_account"])
                    .await?
            }
            "authorized_user" => {
                let credential = params.credential().ok_or_else(|| {
                    "Authentication requires credential JSON or a file path".to_string()
                })?;
                let text = load_json_credential(credential).await?;
                from_credential_json(&text, &["authorized_user", "impersonated_service_account"])
                    .await?
            }
            "access_token" => params
                .credential()
                .map(|value| Self::AccessToken(strip_bom(value).trim().to_string()))
                .ok_or_else(|| "Access token authentication requires a token".to_string())?,
            "none" => {
                if !endpoint.local {
                    return Err(
                        "Authentication 'none' is only allowed with a localhost API endpoint, such as a BigQuery emulator"
                            .to_string(),
                    );
                }
                Self::None
            }
            method => {
                return Err(format!(
                    "Unsupported authentication method '{method}'. Use adc, service_account, authorized_user, access_token or none"
                ))
            }
        };

        match params.impersonate_service_account() {
            Some(target) => {
                if matches!(base, Self::None) {
                    return Err(
                        "Service account impersonation requires an authentication method other than 'none'"
                            .to_string(),
                    );
                }
                let url = impersonation_url_for(target)?;
                Ok(Self::Impersonated(Arc::new(ImpersonatedProvider::new(
                    base,
                    url,
                    Vec::new(),
                )?)))
            }
            None => Ok(base),
        }
    }

    /// A bearer token, or `None` when the connection is unauthenticated.
    pub async fn access_token(&self) -> Result<Option<String>, String> {
        match self {
            Self::Google(provider) => provider.access_token().await.map(Some),
            Self::AuthorizedUser(provider) => provider.access_token().await.map(Some),
            Self::Impersonated(provider) => provider.access_token().await.map(Some),
            Self::AccessToken(token) => Ok(Some(token.clone())),
            Self::None => Ok(None),
        }
    }

    /// Drop cached tokens after the API rejected one with HTTP 401, so the
    /// next request fetches a new one. A fixed access token cannot be
    /// refreshed.
    pub async fn invalidate(&self) {
        match self {
            Self::Google(provider) => provider.rebuild().await,
            Self::AuthorizedUser(provider) => *provider.cached.lock().await = None,
            Self::Impersonated(provider) => {
                *provider.cached.lock().await = None;
                Box::pin(provider.source.invalidate()).await;
            }
            Self::AccessToken(_) | Self::None => {}
        }
    }
}

impl AuthorizedUserProvider {
    async fn access_token(&self) -> Result<String, String> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref().and_then(CachedToken::fresh) {
            return Ok(token);
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
        let payload: Value = response
            .json()
            .await
            .map_err(|error| format!("Invalid OAuth token response: {error}"))?;
        if !status.is_success() {
            let message = payload
                .get("error_description")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("unknown OAuth error");
            return Err(format!("OAuth token refresh failed: {message}"));
        }
        let value = payload
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| "OAuth token response did not contain access_token".to_string())?
            .to_string();
        let expires_in = payload
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(3600);
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(expires_in),
        });
        Ok(value)
    }
}

impl ImpersonatedProvider {
    pub(crate) fn new(
        source: AuthProvider,
        url: Url,
        delegates: Vec<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            http: oauth_http()?,
            source,
            url,
            delegates: delegates
                .into_iter()
                .map(|delegate| {
                    if delegate.starts_with("projects/") {
                        delegate
                    } else {
                        format!("projects/-/serviceAccounts/{delegate}")
                    }
                })
                .collect(),
            cached: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, String> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref().and_then(CachedToken::fresh) {
            return Ok(token);
        }
        let source = Box::pin(self.source.access_token()).await?.ok_or_else(|| {
            "Impersonation requires an authenticated source credential".to_string()
        })?;
        let response = self
            .http
            .post(self.url.clone())
            .bearer_auth(source)
            .json(&json!({
                "scope": BIGQUERY_SCOPES,
                "delegates": self.delegates,
                "lifetime": "3600s"
            }))
            .send()
            .await
            .map_err(|error| format!("Service account impersonation failed: {error}"))?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|error| format!("Invalid impersonation response: {error}"))?;
        if !status.is_success() {
            let message = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown IAM Credentials error");
            return Err(format!(
                "Service account impersonation failed ({status}): {message}. The source account needs roles/iam.serviceAccountTokenCreator on the target"
            ));
        }
        let value = payload
            .get("accessToken")
            .and_then(Value::as_str)
            .ok_or_else(|| "Impersonation response did not contain accessToken".to_string())?
            .to_string();
        let lifetime = payload
            .get("expireTime")
            .and_then(Value::as_str)
            .and_then(|expiry| chrono::DateTime::parse_from_rfc3339(expiry).ok())
            .and_then(|expiry| {
                (expiry.with_timezone(&chrono::Utc) - chrono::Utc::now())
                    .to_std()
                    .ok()
            })
            .unwrap_or(Duration::from_secs(3600));
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + lifetime,
        });
        Ok(value)
    }
}

fn oauth_http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("Unable to initialize OAuth client: {error}"))
}

/// Resolve Application Default Credentials. Credential files are parsed here
/// so `impersonated_service_account` and `authorized_user` files referenced by
/// GOOGLE_APPLICATION_CREDENTIALS work; everything else uses gcp_auth's chain
/// (metadata server, gcloud CLI).
async fn application_default() -> Result<AuthProvider, String> {
    if let Some(path) = adc_file_path() {
        let text = tokio::fs::read_to_string(&path).await.map_err(|error| {
            format!(
                "Unable to read Application Default Credentials file '{}': {error}",
                path.display()
            )
        })?;
        let text = strip_bom(&text);
        let credential_type = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned));
        if matches!(
            credential_type.as_deref(),
            Some("service_account" | "authorized_user" | "impersonated_service_account")
        ) {
            return from_credential_json(
                text,
                &[
                    "service_account",
                    "authorized_user",
                    "impersonated_service_account",
                ],
            )
            .await;
        }
    }
    let build: GoogleBuild = Arc::new(|| {
        Box::pin(async {
            gcp_auth::provider().await.map_err(|error| {
                format!("Application Default Credentials are unavailable: {error}")
            })
        })
    });
    GoogleProvider::new(build)
        .await
        .map(|provider| AuthProvider::Google(Arc::new(provider)))
}

fn adc_file_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return Some(path);
    }
    let directory = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join("gcloud")
    } else {
        PathBuf::from(std::env::var_os("HOME")?)
            .join(".config")
            .join("gcloud")
    };
    let path = directory.join("application_default_credentials.json");
    path.is_file().then_some(path)
}

/// Build a provider from a Google credential JSON document of an allowed type.
fn from_credential_json<'a>(text: &'a str, allowed: &'a [&'a str]) -> ProviderFuture<'a> {
    Box::pin(async move {
        let text = strip_bom(text);
        let value: Value = serde_json::from_str(text)
            .map_err(|error| format!("Invalid credential JSON: {error}"))?;
        let credential_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !allowed.contains(&credential_type.as_str()) {
            return Err(format!(
                "Credential JSON has type '{credential_type}', expected {}",
                allowed.join(" or ")
            ));
        }
        match credential_type.as_str() {
            "service_account" => {
                validate_token_uri(&value)?;
                let key: Arc<str> = Arc::from(text);
                let build: GoogleBuild = Arc::new(move || {
                    let key = key.clone();
                    Box::pin(async move {
                        CustomServiceAccount::from_json(&key)
                            .map(|account| Arc::new(account) as Arc<dyn TokenProvider>)
                            .map_err(|error| format!("Invalid service account credential: {error}"))
                    })
                });
                GoogleProvider::new(build)
                    .await
                    .map(|provider| AuthProvider::Google(Arc::new(provider)))
            }
            "authorized_user" => {
                let credentials: AuthorizedUserCredentials = serde_json::from_value(value)
                    .map_err(|error| format!("Invalid authorized user credential JSON: {error}"))?;
                if credentials.client_id.trim().is_empty()
                    || credentials.client_secret.trim().is_empty()
                    || credentials.refresh_token.trim().is_empty()
                {
                    return Err(
                        "OAuth credential JSON must contain client_id, client_secret and refresh_token"
                            .to_string(),
                    );
                }
                Ok(AuthProvider::AuthorizedUser(Arc::new(
                    AuthorizedUserProvider {
                        http: oauth_http()?,
                        credentials,
                        cached: Mutex::new(None),
                    },
                )))
            }
            _ => {
                let url = value
                    .get("service_account_impersonation_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "Impersonated credential JSON must contain service_account_impersonation_url"
                            .to_string()
                    })
                    .and_then(validate_impersonation_url)?;
                let source = value
                    .get("source_credentials")
                    .ok_or_else(|| {
                        "Impersonated credential JSON must contain source_credentials".to_string()
                    })?
                    .to_string();
                let source =
                    from_credential_json(&source, &["service_account", "authorized_user"]).await?;
                let delegates = value
                    .get("delegates")
                    .and_then(Value::as_array)
                    .map(|delegates| {
                        delegates
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(AuthProvider::Impersonated(Arc::new(
                    ImpersonatedProvider::new(source, url, delegates)?,
                )))
            }
        }
    })
}

/// A service account key must only send its signed assertion to Google.
fn validate_token_uri(value: &Value) -> Result<(), String> {
    let Some(token_uri) = value.get("token_uri").and_then(Value::as_str) else {
        return Err("Service account JSON must contain token_uri".to_string());
    };
    let url = Url::parse(token_uri)
        .map_err(|_| format!("Service account token_uri '{token_uri}' is not a valid URL"))?;
    if url.scheme() == "https" && is_googleapis_host(&url) {
        Ok(())
    } else {
        Err(format!(
            "Service account token_uri '{token_uri}' is not a googleapis.com HTTPS endpoint"
        ))
    }
}

fn validate_impersonation_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value)
        .map_err(|_| format!("Invalid service_account_impersonation_url '{value}'"))?;
    if url.scheme() == "https"
        && is_googleapis_host(&url)
        && url.path().ends_with(":generateAccessToken")
    {
        Ok(url)
    } else {
        Err(format!(
            "service_account_impersonation_url '{value}' must be a googleapis.com generateAccessToken URL"
        ))
    }
}

fn impersonation_url_for(target: &str) -> Result<Url, String> {
    let valid = target.contains('@')
        && target
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._-".contains(character));
    if !valid {
        return Err(format!(
            "Impersonated service account '{target}' must be a service account email"
        ));
    }
    Url::parse(&format!(
        "{IAM_CREDENTIALS_BASE}projects/-/serviceAccounts/{target}:generateAccessToken"
    ))
    .map_err(|error| format!("Invalid impersonation target: {error}"))
}

fn is_googleapis_host(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "googleapis.com" || host.ends_with(".googleapis.com"))
}

fn strip_bom(value: &str) -> &str {
    value.strip_prefix('\u{feff}').unwrap_or(value)
}

/// Inline JSON is used as-is; anything else is read as a file path.
async fn load_json_credential(credential: &str) -> Result<String, String> {
    let credential = strip_bom(credential).trim();
    if credential.starts_with('{') {
        return Ok(credential.to_string());
    }
    let text = tokio::fs::read_to_string(Path::new(credential))
        .await
        .map_err(|error| format!("Unable to read credential file '{credential}': {error}"))?;
    Ok(strip_bom(&text).to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        from_credential_json, impersonation_url_for, load_json_credential, validate_token_uri,
        AuthProvider, GoogleBuild, GoogleProvider, ImpersonatedProvider,
    };
    use crate::endpoint;
    use crate::models::ConnectionParams;

    fn service_account(token_uri: &str) -> serde_json::Value {
        json!({
            "type": "service_account",
            "project_id": "p",
            "private_key_id": "k",
            "private_key": "-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n",
            "client_email": "sa@p.iam.gserviceaccount.com",
            "client_id": "1",
            "token_uri": token_uri
        })
    }

    #[test]
    fn accepts_only_googleapis_token_endpoints() {
        assert!(
            validate_token_uri(&service_account("https://oauth2.googleapis.com/token")).is_ok()
        );
        assert!(validate_token_uri(&service_account("https://evil.example.com/token")).is_err());
        assert!(
            validate_token_uri(&service_account("http://oauth2.googleapis.com/token")).is_err()
        );
        assert!(validate_token_uri(&service_account("https://googleapis.com.evil.io/t")).is_err());
        assert!(validate_token_uri(&json!({"type": "service_account"})).is_err());
    }

    #[tokio::test]
    async fn rejects_service_accounts_with_foreign_token_uri() {
        let text = service_account("https://attacker.example/token").to_string();
        let error = from_credential_json(&text, &["service_account"])
            .await
            .err()
            .unwrap();
        assert!(error.contains("googleapis.com"), "{error}");
    }

    #[tokio::test]
    async fn tolerates_a_byte_order_mark() {
        let text = format!(
            "\u{feff}{}",
            json!({"type": "authorized_user", "client_id": "c", "client_secret": "s", "refresh_token": "r"})
        );
        assert!(load_json_credential(&text).await.unwrap().starts_with('{'));
        let provider = from_credential_json(&text, &["authorized_user"])
            .await
            .unwrap();
        assert!(matches!(provider, AuthProvider::AuthorizedUser(_)));
    }

    #[tokio::test]
    async fn parses_impersonated_adc_files() {
        let text = json!({
            "type": "impersonated_service_account",
            "service_account_impersonation_url": "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/target@p.iam.gserviceaccount.com:generateAccessToken",
            "delegates": ["middle@p.iam.gserviceaccount.com"],
            "source_credentials": {"type": "authorized_user", "client_id": "c", "client_secret": "s", "refresh_token": "r"}
        })
        .to_string();
        let provider = from_credential_json(&text, &["impersonated_service_account"])
            .await
            .unwrap();
        let AuthProvider::Impersonated(provider) = provider else {
            panic!("expected an impersonated provider");
        };
        assert_eq!(
            provider.delegates,
            vec!["projects/-/serviceAccounts/middle@p.iam.gserviceaccount.com"]
        );

        let foreign = text.replace("iamcredentials.googleapis.com", "iam.example.com");
        assert!(
            from_credential_json(&foreign, &["impersonated_service_account"])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn impersonation_exchanges_the_source_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/-/serviceAccounts/t@p.iam.gserviceaccount.com:generateAccessToken",
            ))
            .and(header("authorization", "Bearer source-token"))
            .and(body_partial_json(json!({"lifetime": "3600s"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accessToken": "impersonated-token",
                "expireTime": "2999-01-01T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let url = format!(
            "{}/v1/projects/-/serviceAccounts/t@p.iam.gserviceaccount.com:generateAccessToken",
            server.uri()
        )
        .parse()
        .unwrap();
        let provider = ImpersonatedProvider::new(
            AuthProvider::AccessToken("source-token".into()),
            url,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(provider.access_token().await.unwrap(), "impersonated-token");
        // The second call is served from the cache; `expect(1)` verifies it.
        assert_eq!(provider.access_token().await.unwrap(), "impersonated-token");
    }

    struct FixedToken(&'static str);

    #[async_trait::async_trait]
    impl gcp_auth::TokenProvider for FixedToken {
        async fn token(&self, _scopes: &[&str]) -> Result<Arc<gcp_auth::Token>, gcp_auth::Error> {
            Ok(Arc::new(
                serde_json::from_value(json!({"access_token": self.0, "expires_in": 3600}))
                    .unwrap(),
            ))
        }

        async fn project_id(&self) -> Result<Arc<str>, gcp_auth::Error> {
            Ok(Arc::from("p"))
        }
    }

    #[tokio::test]
    async fn invalidating_google_credentials_rebuilds_the_provider() {
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = builds.clone();
        let build: GoogleBuild = Arc::new(move || {
            let token = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                "first"
            } else {
                "second"
            };
            Box::pin(
                async move { Ok(Arc::new(FixedToken(token)) as Arc<dyn gcp_auth::TokenProvider>) },
            )
        });
        let provider = AuthProvider::Google(Arc::new(GoogleProvider::new(build).await.unwrap()));
        assert_eq!(
            provider.access_token().await.unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            provider.access_token().await.unwrap().as_deref(),
            Some("first")
        );
        provider.invalidate().await;
        assert_eq!(
            provider.access_token().await.unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn validates_impersonation_targets() {
        assert!(impersonation_url_for("reader@p.iam.gserviceaccount.com").is_ok());
        assert!(impersonation_url_for("not-an-email").is_err());
        assert!(impersonation_url_for("a@b/../../x").is_err());
    }

    #[tokio::test]
    async fn none_requires_a_local_endpoint() {
        let params = ConnectionParams::from_value(&json!({
            "database": "p",
            "extra": {"auth_method": "none"}
        }));
        let public = endpoint::resolve(None).unwrap();
        assert!(AuthProvider::from_params(&params, &public).await.is_err());
        let local = endpoint::resolve(Some("http://localhost:9050")).unwrap();
        let provider = AuthProvider::from_params(&params, &local).await.unwrap();
        assert_eq!(provider.access_token().await.unwrap(), None);
    }

    #[tokio::test]
    async fn impersonation_wraps_any_authenticated_mode() {
        let params = ConnectionParams::from_value(&json!({
            "database": "p",
            "extra": {
                "auth_method": "access_token",
                "credential": "token",
                "impersonate_service_account": "reader@p.iam.gserviceaccount.com"
            }
        }));
        let provider = AuthProvider::from_params(&params, &endpoint::resolve(None).unwrap())
            .await
            .unwrap();
        assert!(matches!(provider, AuthProvider::Impersonated(_)));

        let params = ConnectionParams::from_value(&json!({
            "database": "p",
            "extra": {
                "auth_method": "none",
                "impersonate_service_account": "reader@p.iam.gserviceaccount.com"
            }
        }));
        let local = endpoint::resolve(Some("http://localhost:9050")).unwrap();
        assert!(AuthProvider::from_params(&params, &local).await.is_err());
    }
}
