use reqwest::Url;

pub const DEFAULT_API_BASE: &str = "https://bigquery.googleapis.com/bigquery/v2/";

/// The BigQuery REST base URL used for every API call of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEndpoint {
    pub base: Url,
    /// True when the endpoint is a loopback address, such as a local emulator.
    pub local: bool,
}

impl ApiEndpoint {
    pub fn default_endpoint() -> Self {
        Self {
            base: Url::parse(DEFAULT_API_BASE).expect("valid default BigQuery API base"),
            local: false,
        }
    }
}

/// Resolve the optional `api_endpoint` override. HTTPS is required except for
/// loopback hosts, so credentials are never sent in clear text over a network.
pub fn resolve(value: Option<&str>) -> Result<ApiEndpoint, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(ApiEndpoint::default_endpoint());
    };
    let mut url =
        Url::parse(value).map_err(|error| format!("Invalid API endpoint '{value}': {error}"))?;
    let local = is_loopback(&url);
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        "http" => {
            return Err(
                "API endpoint must use https unless it points to localhost or 127.0.0.1"
                    .to_string(),
            )
        }
        scheme => return Err(format!("Unsupported API endpoint scheme '{scheme}'")),
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("API endpoint must include a host".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("API endpoint must not contain user information".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("API endpoint must not contain a query string or fragment".to_string());
    }

    let path = url.path().trim_end_matches('/').to_string();
    let path = if path.ends_with("/bigquery/v2") {
        format!("{path}/")
    } else {
        format!("{path}/bigquery/v2/")
    };
    url.set_path(&path);
    Ok(ApiEndpoint { base: url, local })
}

fn is_loopback(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]")
    )
}

#[cfg(test)]
mod tests {
    use super::{resolve, DEFAULT_API_BASE};

    #[test]
    fn defaults_to_the_public_api() {
        let endpoint = resolve(None).unwrap();
        assert_eq!(endpoint.base.as_str(), DEFAULT_API_BASE);
        assert!(!endpoint.local);
        assert_eq!(resolve(Some("  ")).unwrap().base.as_str(), DEFAULT_API_BASE);
    }

    #[test]
    fn appends_the_rest_path_once() {
        assert_eq!(
            resolve(Some("http://localhost:9050"))
                .unwrap()
                .base
                .as_str(),
            "http://localhost:9050/bigquery/v2/"
        );
        assert_eq!(
            resolve(Some("https://bigquery-psc.p.googleapis.com/bigquery/v2/"))
                .unwrap()
                .base
                .as_str(),
            "https://bigquery-psc.p.googleapis.com/bigquery/v2/"
        );
    }

    #[test]
    fn allows_plain_http_only_on_loopback() {
        assert!(resolve(Some("http://127.0.0.1:9050")).unwrap().local);
        assert!(resolve(Some("http://[::1]:9050")).unwrap().local);
        assert!(resolve(Some("http://bigquery.internal:9050")).is_err());
        assert!(!resolve(Some("https://bigquery.internal")).unwrap().local);
    }

    #[test]
    fn rejects_unsafe_urls() {
        assert!(resolve(Some("ftp://localhost")).is_err());
        assert!(resolve(Some("https://user:secret@example.com")).is_err());
        assert!(resolve(Some("https://example.com/?key=1")).is_err());
        assert!(resolve(Some("not a url")).is_err());
    }
}
