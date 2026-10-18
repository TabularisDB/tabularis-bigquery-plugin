use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const JS_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FieldSchema {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldSchema>,
    #[serde(default)]
    pub default_value_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_element_type: Option<RangeElementType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct RangeElementType {
    #[serde(rename = "type")]
    pub element_type: String,
}

impl FieldSchema {
    pub fn display_type(&self) -> String {
        let base = match self.field_type.to_ascii_uppercase().as_str() {
            "RECORD" | "STRUCT" => {
                let fields = self
                    .fields
                    .iter()
                    .map(|field| format!("{} {}", field.name, field.display_type()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("STRUCT<{fields}>")
            }
            other => other.to_string(),
        };
        if self.mode.as_deref() == Some("REPEATED") {
            format!("ARRAY<{base}>")
        } else {
            base
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueryOutput {
    pub columns: Vec<String>,
    pub fields: Vec<FieldSchema>,
    pub rows: Vec<Vec<Value>>,
    pub affected_rows: u64,
    pub total_rows: Option<u64>,
    pub has_more: bool,
    pub total_bytes_processed: Option<String>,
    pub cache_hit: Option<bool>,
    pub job_id: Option<String>,
    /// How the rows were read, for example `job` or `tabledata.list`.
    pub source: Option<String>,
}

impl QueryOutput {
    pub fn to_tabularis(&self, page: u32, page_size: u32) -> Value {
        json!({
            "columns": self.columns,
            "rows": self.rows,
            "affected_rows": self.affected_rows,
            "truncated": self.has_more,
            "pagination": {
                "page": page,
                "page_size": page_size,
                "total_rows": self.total_rows.and_then(|value| i64::try_from(value).ok()),
                "has_more": self.has_more
            },
            "metadata": {
                "total_bytes_processed": self.total_bytes_processed,
                "cache_hit": self.cache_hit,
                "job_id": self.job_id,
                "source": self.source
            }
        })
    }
}

pub fn fields_from_response(response: &Value) -> Result<Vec<FieldSchema>, String> {
    let Some(fields) = response.pointer("/schema/fields") else {
        return Ok(Vec::new());
    };
    serde_json::from_value(fields.clone())
        .map_err(|error| format!("Invalid BigQuery result schema: {error}"))
}

pub fn rows_from_response(response: &Value, fields: &[FieldSchema]) -> Vec<Vec<Value>> {
    response
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let cells = row.get("f").and_then(Value::as_array);
                    fields
                        .iter()
                        .enumerate()
                        .map(|(index, field)| {
                            cells
                                .and_then(|values| values.get(index))
                                .and_then(|cell| cell.get("v"))
                                .map(|value| convert_cell(value, field))
                                .unwrap_or(Value::Null)
                        })
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn convert_cell(value: &Value, field: &FieldSchema) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if field.mode.as_deref() == Some("REPEATED") {
        return Value::Array(
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.get("v")
                                .map(|inner| convert_scalar_or_record(inner, field))
                                .unwrap_or(Value::Null)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
    }
    convert_scalar_or_record(value, field)
}

fn convert_scalar_or_record(value: &Value, field: &FieldSchema) -> Value {
    match field.field_type.to_ascii_uppercase().as_str() {
        "RECORD" | "STRUCT" => {
            let cells = value.get("f").and_then(Value::as_array);
            let object = field
                .fields
                .iter()
                .enumerate()
                .map(|(index, nested)| {
                    let value = cells
                        .and_then(|values| values.get(index))
                        .and_then(|cell| cell.get("v"))
                        .map(|value| convert_cell(value, nested))
                        .unwrap_or(Value::Null);
                    (nested.name.clone(), value)
                })
                .collect::<Map<String, Value>>();
            Value::Object(object)
        }
        "BOOL" | "BOOLEAN" => value
            .as_str()
            .map(|value| Value::Bool(value.eq_ignore_ascii_case("true")))
            .unwrap_or_else(|| value.clone()),
        "INT64" | "INTEGER" => value
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|number| {
                if number.unsigned_abs() <= JS_MAX_SAFE_INTEGER as u64 {
                    Value::from(number)
                } else {
                    Value::String(number.to_string())
                }
            })
            .unwrap_or_else(|| value.clone()),
        "FLOAT" | "FLOAT64" => value
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| value.clone()),
        "JSON" => value
            .as_str()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_else(|| value.clone()),
        "TIMESTAMP" => value
            .as_str()
            .and_then(timestamp_to_rfc3339)
            .map(Value::String)
            .unwrap_or_else(|| value.clone()),
        "BYTES" => value
            .as_str()
            .map(|encoded| {
                let size = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map(|bytes| bytes.len())
                    .unwrap_or_default();
                Value::String(format!("BLOB:{size}:application/octet-stream:{encoded}"))
            })
            .unwrap_or_else(|| value.clone()),
        _ => value.clone(),
    }
}

/// Format a wire TIMESTAMP exactly. Results requested with
/// `formatOptions.useInt64Timestamp` carry integer microseconds; older
/// responses carry decimal or scientific epoch seconds.
pub fn timestamp_to_rfc3339(value: &str) -> Option<String> {
    let value = value.trim();
    let micros = if let Ok(micros) = value.parse::<i64>() {
        micros
    } else if value.contains(['e', 'E']) {
        (value.parse::<f64>().ok()? * 1_000_000.0).round() as i64
    } else {
        let (negative, digits) = match value.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let (seconds, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        let mut fraction = fraction.to_string();
        if fraction.len() > 6 || !fraction.chars().all(|digit| digit.is_ascii_digit()) {
            return None;
        }
        while fraction.len() < 6 {
            fraction.push('0');
        }
        let micros = seconds
            .parse::<i64>()
            .ok()?
            .checked_mul(1_000_000)?
            .checked_add(fraction.parse::<i64>().ok()?)?;
        if negative {
            -micros
        } else {
            micros
        }
    };
    DateTime::<Utc>::from_timestamp_micros(micros)
        .map(|date| date.to_rfc3339_opts(SecondsFormat::Micros, true))
}

pub fn api_error(response: &Value) -> Option<String> {
    response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            response
                .get("errors")
                .and_then(Value::as_array)
                .filter(|errors| !errors.is_empty())
                .map(|errors| {
                    errors
                        .iter()
                        .filter_map(|error| error.get("message").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
        })
        .filter(|message| !message.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{fields_from_response, rows_from_response, timestamp_to_rfc3339, FieldSchema};

    #[test]
    fn formats_timestamps_with_exact_microseconds() {
        assert_eq!(
            timestamp_to_rfc3339("1700000000123457").unwrap(),
            "2023-11-14T22:13:20.123457Z"
        );
        assert_eq!(
            timestamp_to_rfc3339("1700000000.123457").unwrap(),
            "2023-11-14T22:13:20.123457Z"
        );
        assert_eq!(
            timestamp_to_rfc3339("1.700000000123457E9").unwrap(),
            "2023-11-14T22:13:20.123457Z"
        );
        assert_eq!(
            timestamp_to_rfc3339("-1").unwrap(),
            "1969-12-31T23:59:59.999999Z"
        );
        assert_eq!(
            timestamp_to_rfc3339("-0.5").unwrap(),
            "1969-12-31T23:59:59.500000Z"
        );
        assert_eq!(
            timestamp_to_rfc3339("253402300799999999").unwrap(),
            "9999-12-31T23:59:59.999999Z"
        );
    }

    #[test]
    fn renders_nested_and_repeated_type_names() {
        let field = FieldSchema {
            name: "items".into(),
            field_type: "RECORD".into(),
            mode: Some("REPEATED".into()),
            description: None,
            fields: vec![FieldSchema {
                name: "amount".into(),
                field_type: "NUMERIC".into(),
                mode: None,
                description: None,
                fields: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(field.display_type(), "ARRAY<STRUCT<amount NUMERIC>>");
    }

    #[test]
    fn converts_bigquery_wire_rows_without_losing_large_integers() {
        let response = json!({
            "schema": {"fields": [
                {"name": "id", "type": "INT64"},
                {"name": "active", "type": "BOOL"},
                {"name": "payload", "type": "JSON"}
            ]},
            "rows": [{"f": [
                {"v": "9007199254740993"},
                {"v": "true"},
                {"v": "{\"ok\":1}"}
            ]}]
        });
        let fields = fields_from_response(&response).unwrap();
        let rows = rows_from_response(&response, &fields);
        assert_eq!(rows[0][0], "9007199254740993");
        assert_eq!(rows[0][1], true);
        assert_eq!(rows[0][2], json!({"ok": 1}));
    }
}
