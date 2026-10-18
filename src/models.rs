use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

/// Stays below the host's default 120 s plugin call timeout, so a timed-out
/// query is reported and cancelled by the plugin rather than abandoned.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 110;

#[derive(Debug, Clone, Default)]
pub struct ConnectionParams {
    pub driver: Option<String>,
    pub project_id: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub connection_id: Option<String>,
    pub extra: HashMap<String, String>,
}

impl ConnectionParams {
    pub fn from_value(value: &Value) -> Self {
        let object = value.as_object();
        let string = |key: &str| {
            object
                .and_then(|fields| fields.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };

        let project_id = object
            .and_then(|fields| fields.get("database"))
            .and_then(|database| match database {
                Value::String(value) => Some(value.clone()),
                Value::Array(values) => values.first().and_then(Value::as_str).map(str::to_owned),
                _ => None,
            });

        let extra = object
            .and_then(|fields| fields.get("extra"))
            .and_then(Value::as_object)
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Self {
            driver: string("driver"),
            project_id,
            username: string("username"),
            password: string("password"),
            connection_id: string("connection_id"),
            extra,
        }
    }

    pub fn project_id(&self) -> Result<&str, String> {
        self.project_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Project ID is required in the Database field".to_string())
    }

    /// Project that owns and pays for query jobs. Multi-project browsing
    /// sends the selected project in `database`, so an explicit
    /// `billing_project` extra keeps jobs in the configured project.
    pub fn billing_project_id(&self) -> Result<&str, String> {
        match self.extra_value("billing_project") {
            Some(project) => Ok(project),
            None => self.project_id(),
        }
    }

    /// Project whose datasets are browsed: the selected database, or the
    /// billing project when the host sends no database ("all databases").
    pub fn data_project_id(&self) -> Result<&str, String> {
        self.project_id().or_else(|_| self.billing_project_id())
    }

    /// Projects listed by `get_databases` in addition to the projects the
    /// credential can see, e.g. `bigquery-public-data`.
    pub fn additional_projects(&self) -> Vec<String> {
        self.extra_value("additional_projects")
            .map(|value| {
                value
                    .split([',', '\n', ' '])
                    .map(str::trim)
                    .filter(|project| !project.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn extra_value(&self, key: &str) -> Option<&str> {
        self.extra
            .get(key)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn auth_method(&self) -> &str {
        self.extra_value("auth_method").unwrap_or("adc")
    }

    pub fn credential(&self) -> Option<&str> {
        self.extra_value("credential").or_else(|| {
            self.password
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
    }

    pub fn location(&self) -> Option<&str> {
        self.extra_value("location")
    }

    pub fn max_bytes_billed(&self) -> Option<&str> {
        self.extra_value("max_bytes_billed")
    }

    pub fn timeout_seconds(&self) -> u64 {
        self.extra_value("query_timeout_seconds")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(30, 3_600))
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
    }

    pub fn api_endpoint(&self) -> Option<&str> {
        self.extra_value("api_endpoint")
    }

    pub fn impersonate_service_account(&self) -> Option<&str> {
        self.extra_value("impersonate_service_account")
    }
}

pub fn inner_params(value: &Value) -> &Value {
    value.get("params").unwrap_or(value)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    #[serde(default)]
    pub is_pk: bool,
    #[serde(default)]
    pub is_auto_increment: bool,
    #[serde(default)]
    pub default_value: Option<String>,
    /// Column description. Current hosts do not send it; accepted so the
    /// DDL builders can emit `OPTIONS(description = ...)` when they do.
    #[serde(default, alias = "description")]
    pub comment: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ConnectionParams;

    #[test]
    fn parses_project_and_string_extras() {
        let value = json!({
            "driver": "bigquery",
            "database": "billing-project",
            "extra": {"auth_method": "adc", "ignored": true}
        });
        let params = ConnectionParams::from_value(&value);
        assert_eq!(params.project_id().unwrap(), "billing-project");
        assert_eq!(params.auth_method(), "adc");
        assert!(!params.extra.contains_key("ignored"));
    }

    #[test]
    fn billing_project_defaults_to_database_and_honors_override() {
        let value = json!({"database": "public-data"});
        let params = ConnectionParams::from_value(&value);
        assert_eq!(params.billing_project_id().unwrap(), "public-data");
        let value = json!({
            "database": "bigquery-public-data",
            "extra": {"billing_project": "my-billing", "additional_projects": "a-1, b-2\nc"}
        });
        let params = ConnectionParams::from_value(&value);
        assert_eq!(params.project_id().unwrap(), "bigquery-public-data");
        assert_eq!(params.billing_project_id().unwrap(), "my-billing");
        assert_eq!(params.additional_projects(), vec!["a-1", "b-2", "c"]);
        let value = json!({"database": "", "extra": {"billing_project": "my-billing"}});
        let params = ConnectionParams::from_value(&value);
        assert_eq!(params.data_project_id().unwrap(), "my-billing");
    }

    #[test]
    fn clamps_timeout() {
        let value = json!({"database": "p", "extra": {"query_timeout_seconds": "2"}});
        assert_eq!(ConnectionParams::from_value(&value).timeout_seconds(), 30);
    }

    #[test]
    fn secure_plugin_credential_takes_priority_over_legacy_password() {
        let value = json!({
            "database": "p",
            "password": "legacy",
            "extra": {"credential": "secure"}
        });
        assert_eq!(
            ConnectionParams::from_value(&value).credential(),
            Some("secure")
        );
    }

    #[test]
    fn legacy_password_remains_a_credential_fallback() {
        let value = json!({"database": "p", "password": "legacy"});
        assert_eq!(
            ConnectionParams::from_value(&value).credential(),
            Some("legacy")
        );
    }
}
