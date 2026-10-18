use serde_json::Value;

/// A BigQuery error decoded from an HTTP error body or a job `errorResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: Option<u16>,
    pub reason: Option<String>,
    pub message: String,
    pub location: Option<String>,
}

impl ApiError {
    /// Decode the `{"error": {...}}` body of a failed REST call.
    pub fn from_response(status: u16, payload: &Value) -> Self {
        let error = payload.get("error").unwrap_or(payload);
        let first = error
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|errors| errors.first());
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| {
                first
                    .and_then(|first| first.get("message"))
                    .and_then(Value::as_str)
            })
            .or_else(|| payload.get("raw").and_then(Value::as_str))
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("BigQuery request failed with HTTP {status}"));
        Self {
            status: Some(status),
            reason: first
                .and_then(|first| first.get("reason"))
                .or_else(|| error.get("status"))
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned),
            message,
            location: first.and_then(location_of),
        }
    }

    /// Decode a job's `status.errorResult` (or one entry of `errors`).
    pub fn from_error_proto(error: &Value) -> Self {
        Self {
            status: None,
            reason: error
                .get("reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("BigQuery job failed")
                .to_string(),
            location: location_of(error),
        }
    }

    pub fn reason(&self) -> &str {
        self.reason.as_deref().unwrap_or_default()
    }

    /// Whether the same request may succeed when sent again after a delay.
    pub fn is_retryable(&self) -> bool {
        match self.status {
            Some(429 | 500 | 502 | 503 | 504) => true,
            Some(403) => matches!(self.reason(), "rateLimitExceeded" | "backendError"),
            _ => matches!(self.reason(), "backendError" | "internalError"),
        }
    }

    /// A user-facing message with an actionable hint when one is known.
    pub fn describe(&self) -> String {
        let mut text = self.message.clone();
        if let Some(position) = self.position() {
            text.push_str(&format!(" (line {}, column {})", position.0, position.1));
        }
        let suffix = match (self.reason.as_deref(), self.status) {
            (Some(reason), Some(status)) => format!(" [{reason}, HTTP {status}]"),
            (Some(reason), None) => format!(" [{reason}]"),
            (None, Some(status)) if !self.message.ends_with(&format!("HTTP {status}")) => {
                format!(" [HTTP {status}]")
            }
            (None, _) => String::new(),
        };
        text.push_str(&suffix);
        if let Some(hint) = self.hint() {
            text.push_str("\nHint: ");
            text.push_str(hint);
        }
        text
    }

    fn position(&self) -> Option<(u32, u32)> {
        if self.reason() != "invalidQuery" || self.message.contains("[at ") {
            return None;
        }
        let location = self.location.as_deref()?;
        let (line, column) = location.split_once(':')?;
        Some((line.trim().parse().ok()?, column.trim().parse().ok()?))
    }

    fn hint(&self) -> Option<&'static str> {
        let message = self.message.to_ascii_lowercase();
        if message.contains("partition elimination") || message.contains("without a filter over") {
            return Some("The table requires a partition filter. Add a WHERE condition on the partitioning column.");
        }
        if message.contains("streaming buffer") {
            return Some("Rows still in the streaming buffer cannot be modified by DML. Wait until the buffer is flushed (up to 90 minutes) or restrict the statement to older rows.");
        }
        match self.reason() {
            "accessDenied" => Some("Grant the account BigQuery Job User on the billing project and BigQuery Data Viewer (or Editor for writes) on the dataset."),
            "notFound" => Some("Check the project, dataset and table names, and that the connection Location matches the dataset location."),
            "quotaExceeded" => Some("A BigQuery quota was exceeded. Wait and retry, or review the project quotas in the Google Cloud console."),
            "rateLimitExceeded" => Some("Too many requests in a short time. Retry in a few seconds."),
            "bytesBilledLimitExceeded" => Some("The query would bill more than the connection's Maximum bytes billed. Raise the limit or reduce scanned data with partition filters and fewer columns."),
            "responseTooLarge" => Some("The result is too large to return. Add LIMIT, select fewer columns, or write the result to a destination table."),
            "billingTierLimitExceeded" => Some("The query needs more compute per byte than the on-demand tier allows. Simplify joins or use reservations."),
            "invalidQuery" => Some("Fix the GoogleSQL statement at the reported position."),
            "stopped" | "jobBackendError" => None,
            "duplicate" => Some("The object already exists. Use CREATE OR REPLACE or IF NOT EXISTS."),
            _ if self.status == Some(401) => Some("The credential was rejected or has expired. Refresh the credential in the connection settings."),
            _ => None,
        }
    }
}

fn location_of(error: &Value) -> Option<String> {
    error
        .get("location")
        .and_then(Value::as_str)
        .filter(|location| !location.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ApiError;

    fn error(status: u16, reason: &str, message: &str) -> ApiError {
        ApiError::from_response(
            status,
            &json!({"error": {"code": status, "message": message, "errors": [{"reason": reason, "message": message}]}}),
        )
    }

    #[test]
    fn maps_common_reasons_to_hints() {
        for (reason, fragment) in [
            ("accessDenied", "BigQuery Job User"),
            ("notFound", "Location"),
            ("quotaExceeded", "quota"),
            ("rateLimitExceeded", "Retry"),
            ("bytesBilledLimitExceeded", "Maximum bytes billed"),
            ("responseTooLarge", "LIMIT"),
            ("billingTierLimitExceeded", "reservations"),
        ] {
            let text = error(400, reason, "failed").describe();
            assert!(text.contains(reason), "{text}");
            assert!(text.contains(fragment), "{reason}: {text}");
        }
    }

    #[test]
    fn recognizes_partition_and_streaming_buffer_errors_by_message() {
        let text = error(
            400,
            "invalidQuery",
            "Cannot query over table 'd.t' without a filter over column(s) 'day' that can be used for partition elimination",
        )
        .describe();
        assert!(text.contains("partition filter"));
        let text = error(
            400,
            "invalidQuery",
            "UPDATE or DELETE statement over table d.t would affect rows in the streaming buffer, which is not supported",
        )
        .describe();
        assert!(text.contains("streaming buffer cannot be modified"));
    }

    #[test]
    fn reports_invalid_query_positions() {
        let job_error = ApiError::from_error_proto(
            &json!({"reason": "invalidQuery", "location": "3:15", "message": "Unrecognized name: foo"}),
        );
        assert!(job_error
            .describe()
            .starts_with("Unrecognized name: foo (line 3, column 15)"));
        let inline = error(
            400,
            "invalidQuery",
            "Syntax error: Unexpected keyword [at 1:8]",
        );
        assert!(!inline.describe().contains("(line"));
    }

    #[test]
    fn classifies_retryable_errors() {
        assert!(error(429, "rateLimitExceeded", "x").is_retryable());
        assert!(error(503, "backendError", "x").is_retryable());
        assert!(error(403, "rateLimitExceeded", "x").is_retryable());
        assert!(error(403, "backendError", "x").is_retryable());
        assert!(!error(403, "accessDenied", "x").is_retryable());
        assert!(!error(403, "quotaExceeded", "x").is_retryable());
        assert!(!error(400, "invalidQuery", "x").is_retryable());
    }

    #[test]
    fn falls_back_to_raw_bodies() {
        let parsed = ApiError::from_response(502, &json!({"raw": "Bad gateway"}));
        assert_eq!(parsed.message, "Bad gateway");
        assert!(parsed.is_retryable());
    }
}
