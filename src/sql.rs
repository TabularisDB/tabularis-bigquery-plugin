//! GoogleSQL identifier quoting, literal rendering and typed query parameters.

use base64::Engine;
use serde_json::{json, Value};

use crate::bigquery::FieldSchema;

/// The value Tabularis sends when a cell is reset to its column default.
pub const USE_DEFAULT_SENTINEL: &str = "__USE_DEFAULT__";

/// Quotes one identifier part with backticks. Every part of a path is quoted
/// separately, so project IDs with dashes and names with dots stay intact.
pub fn quote_identifier(identifier: &str) -> String {
    let mut quoted = String::with_capacity(identifier.len() + 2);
    quoted.push('`');
    for character in identifier.chars() {
        match character {
            '`' => quoted.push_str("\\`"),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('`');
    quoted
}

/// `` `dataset`.`object` `` (resolved against the job project).
pub fn qualified(dataset: &str, object: &str) -> String {
    format!("{}.{}", quote_identifier(dataset), quote_identifier(object))
}

/// `` `project`.`dataset`.`object` ``.
pub fn fully_qualified(project: &str, dataset: &str, object: &str) -> String {
    format!(
        "{}.{}.{}",
        quote_identifier(project),
        quote_identifier(dataset),
        quote_identifier(object)
    )
}

/// Fully qualified when the project is known, dataset-qualified otherwise.
pub fn object_path(project: Option<&str>, dataset: &str, object: &str) -> String {
    match project.filter(|project| !project.is_empty()) {
        Some(project) => fully_qualified(project, dataset, object),
        None => qualified(dataset, object),
    }
}

/// `` `project`.`dataset`.INFORMATION_SCHEMA.<view> ``.
pub fn information_schema(project: &str, dataset: &str, view: &str) -> String {
    format!(
        "{}.{}.INFORMATION_SCHEMA.{view}",
        quote_identifier(project),
        quote_identifier(dataset)
    )
}

/// A single-quoted GoogleSQL string literal.
pub fn string_literal(value: &str) -> String {
    format!("'{}'", escape_string(value))
}

/// Escapes a string for a quoted GoogleSQL literal. Newlines, tabs and other
/// control characters are written as escapes so the literal stays valid.
pub fn escape_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if (control as u32) < 0x20 || control as u32 == 0x7f => {
                escaped.push_str(&format!("\\x{:02x}", control as u32));
            }
            control if control.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// Canonical upper-case type name for a field (legacy names normalized).
pub fn base_type(field: &FieldSchema) -> String {
    canonical_type(&field.field_type)
}

pub fn canonical_type(name: &str) -> String {
    match name.trim().to_ascii_uppercase().as_str() {
        "INTEGER" => "INT64".to_string(),
        "FLOAT" => "FLOAT64".to_string(),
        "BOOLEAN" => "BOOL".to_string(),
        "RECORD" => "STRUCT".to_string(),
        "BIGDECIMAL" => "BIGNUMERIC".to_string(),
        "DECIMAL" => "NUMERIC".to_string(),
        other => other.to_string(),
    }
}

pub fn is_repeated(field: &FieldSchema) -> bool {
    field.mode.as_deref() == Some("REPEATED")
}

pub fn is_required(field: &FieldSchema) -> bool {
    field.mode.as_deref() == Some("REQUIRED")
}

/// Type of one element of the field, ignoring `REPEATED`.
pub fn element_type_sql(field: &FieldSchema, ddl: bool) -> String {
    let base = base_type(field);
    match base.as_str() {
        "STRUCT" => {
            let fields = field
                .fields
                .iter()
                .map(|nested| {
                    let name = if ddl {
                        quote_identifier(&nested.name)
                    } else {
                        nested.name.clone()
                    };
                    let mut column = format!("{name} {}", field_type_sql(nested, ddl));
                    if ddl && is_required(nested) {
                        column.push_str(" NOT NULL");
                    }
                    column
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("STRUCT<{fields}>")
        }
        "RANGE" => {
            let element = field
                .range_element_type
                .as_ref()
                .map(|range| canonical_type(&range.element_type))
                .unwrap_or_else(|| "DATE".to_string());
            format!("RANGE<{element}>")
        }
        "STRING" | "BYTES" => match field.max_length.as_deref() {
            Some(length) => format!("{base}({length})"),
            None => base,
        },
        "NUMERIC" | "BIGNUMERIC" => match (field.precision.as_deref(), field.scale.as_deref()) {
            (Some(precision), Some(scale)) => format!("{base}({precision}, {scale})"),
            (Some(precision), None) => format!("{base}({precision})"),
            _ => base,
        },
        _ => base,
    }
}

/// Full column type including `ARRAY<...>` for repeated fields. With `ddl`
/// set, nested names are quoted and nested `NOT NULL` is preserved.
pub fn field_type_sql(field: &FieldSchema, ddl: bool) -> String {
    let element = element_type_sql(field, ddl);
    if is_repeated(field) {
        format!("ARRAY<{element}>")
    } else {
        element
    }
}

/// Renders a JSON cell value as a GoogleSQL literal of the column type.
pub fn value_literal(value: &Value, field: Option<&FieldSchema>) -> Result<String, String> {
    if value.is_null() {
        return Ok("NULL".to_string());
    }
    if let Some(field) = field.filter(|field| is_repeated(field)) {
        let values = normalize_array(value)?;
        let mut element = field.clone();
        element.mode = None;
        let literals = values
            .iter()
            .map(|value| {
                if value.is_null() {
                    Err("BigQuery arrays cannot contain NULL elements".to_string())
                } else {
                    value_literal(value, Some(&element))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(format!(
            "ARRAY<{}>[{}]",
            element_type_sql(&element, true),
            literals.join(", ")
        ));
    }
    let field_type = field
        .map(base_type)
        .unwrap_or_else(|| infer_type(value).to_string());

    match field_type.as_str() {
        "STRUCT" => struct_literal(value, field),
        "ARRAY" => {
            let literals = normalize_array(value)?
                .iter()
                .map(|value| value_literal(value, None))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[{}]", literals.join(", ")))
        }
        "BOOL" => bool_text(value).map(|value| value.to_ascii_uppercase()),
        "INT64" => integer_text(value),
        "FLOAT64" => {
            let text = float_text(value)?;
            if matches!(text.as_str(), "NaN" | "Infinity" | "-Infinity") {
                Ok(format!("CAST('{text}' AS FLOAT64)"))
            } else {
                Ok(text)
            }
        }
        "NUMERIC" | "BIGNUMERIC" => Ok(format!("{field_type} '{}'", decimal_text(value)?)),
        "JSON" => Ok(format!("JSON {}", string_literal(&json_text(value)?))),
        "BYTES" => Ok(format!(
            "FROM_BASE64({})",
            string_literal(&bytes_text(value)?)
        )),
        "DATE" | "TIME" | "DATETIME" | "TIMESTAMP" => Ok(format!(
            "{field_type} {}",
            string_literal(&temporal_text(&field_type, value)?)
        )),
        "GEOGRAPHY" => {
            let text = scalar_text(value)?;
            if text.trim_start().starts_with('{') {
                Ok(format!("ST_GEOGFROMGEOJSON({})", string_literal(&text)))
            } else {
                Ok(format!("ST_GEOGFROMTEXT({})", string_literal(&text)))
            }
        }
        "INTERVAL" => Ok(format!(
            "CAST({} AS INTERVAL)",
            string_literal(&scalar_text(value)?)
        )),
        "RANGE" => {
            let element = range_element(field);
            Ok(format!(
                "RANGE<{element}> {}",
                string_literal(&range_text(value)?)
            ))
        }
        _ => Ok(string_literal(&scalar_text(value)?)),
    }
}

fn struct_literal(value: &Value, field: Option<&FieldSchema>) -> Result<String, String> {
    let object = match value {
        Value::Object(object) => object.clone(),
        Value::String(value) => serde_json::from_str::<Value>(value)
            .map_err(|error| format!("Invalid STRUCT JSON: {error}"))?
            .as_object()
            .cloned()
            .ok_or_else(|| "STRUCT value must be a JSON object".to_string())?,
        _ => return Err("STRUCT value must be a JSON object".to_string()),
    };
    let fields = field.map(|field| field.fields.as_slice()).unwrap_or(&[]);
    if fields.is_empty() {
        let values = object
            .iter()
            .map(|(name, value)| {
                Ok(format!(
                    "{} AS {}",
                    value_literal(value, None)?,
                    quote_identifier(name)
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        return Ok(format!("STRUCT({})", values.join(", ")));
    }
    if let Some(unknown) = object
        .keys()
        .find(|key| !fields.iter().any(|field| &field.name == *key))
    {
        return Err(format!("STRUCT field '{unknown}' does not exist"));
    }
    let values = fields
        .iter()
        .map(|nested| {
            value_literal(
                object.get(&nested.name).unwrap_or(&Value::Null),
                Some(nested),
            )
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut element = field.cloned().unwrap_or_default();
    element.mode = None;
    Ok(format!(
        "{}({})",
        element_type_sql(&element, true),
        values.join(", ")
    ))
}

/// Collects named `@pN` query parameters for one statement.
#[derive(Debug, Default)]
pub struct QueryParams {
    parameters: Vec<Value>,
}

impl QueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_vec(self) -> Vec<Value> {
        self.parameters
    }

    pub fn len(&self) -> usize {
        self.parameters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    fn push(&mut self, parameter_type: Value, parameter_value: Value) -> String {
        let name = format!("p{}", self.parameters.len());
        self.parameters.push(json!({
            "name": name,
            "parameterType": parameter_type,
            "parameterValue": parameter_value
        }));
        format!("@{name}")
    }

    /// Binds a STRING parameter, e.g. an object name in a catalog query.
    pub fn string(&mut self, value: &str) -> String {
        self.scalar("STRING", value.to_string())
    }

    fn scalar(&mut self, type_name: &str, value: String) -> String {
        self.push(json!({"type": type_name}), json!({"value": value}))
    }

    /// Returns the SQL expression that yields `value` as the column type.
    /// Scalars become typed parameters; BYTES, NUMERIC, JSON, GEOGRAPHY,
    /// INTERVAL and RANGE travel as STRING parameters converted in SQL, and
    /// ARRAY / STRUCT values become validated typed literals.
    pub fn bind(&mut self, value: &Value, field: Option<&FieldSchema>) -> Result<String, String> {
        if value.is_null() {
            return Ok("NULL".to_string());
        }
        // Arrays and structs are rendered as validated typed literals, which
        // keeps nested element types exact.
        if let Some(field) = field.filter(|field| is_repeated(field)) {
            return value_literal(value, Some(field));
        }
        let field_type = field
            .map(base_type)
            .unwrap_or_else(|| infer_type(value).to_string());
        match field_type.as_str() {
            "STRUCT" | "ARRAY" => value_literal(value, field),
            "JSON" => {
                let parameter = self.scalar("STRING", json_text(value)?);
                Ok(format!("PARSE_JSON({parameter})"))
            }
            "GEOGRAPHY" => {
                let text = scalar_text(value)?;
                let geojson = text.trim_start().starts_with('{');
                let parameter = self.scalar("STRING", text);
                Ok(if geojson {
                    format!("ST_GEOGFROMGEOJSON({parameter})")
                } else {
                    format!("ST_GEOGFROMTEXT({parameter})")
                })
            }
            "INTERVAL" => {
                let parameter = self.scalar("STRING", scalar_text(value)?);
                Ok(format!("CAST({parameter} AS INTERVAL)"))
            }
            "RANGE" => {
                let element = range_element(field);
                let parameter = self.scalar("STRING", range_text(value)?);
                Ok(format!("CAST({parameter} AS RANGE<{element}>)"))
            }
            "BYTES" => {
                let parameter = self.scalar("STRING", bytes_text(value)?);
                Ok(format!("FROM_BASE64({parameter})"))
            }
            // Decimal text is cast in SQL so no precision is lost on the way.
            "NUMERIC" | "BIGNUMERIC" => {
                let parameter = self.scalar("STRING", decimal_text(value)?);
                Ok(format!("CAST({parameter} AS {field_type})"))
            }
            other if is_simple_parameter_type(other) => {
                let text = scalar_parameter_text(other, value)?;
                Ok(self.scalar(other, text))
            }
            _ => {
                let text = scalar_text(value)?;
                Ok(self.scalar("STRING", text))
            }
        }
    }
}

fn is_simple_parameter_type(type_name: &str) -> bool {
    matches!(
        type_name,
        "STRING"
            | "BYTES"
            | "INT64"
            | "FLOAT64"
            | "NUMERIC"
            | "BIGNUMERIC"
            | "BOOL"
            | "DATE"
            | "TIME"
            | "DATETIME"
            | "TIMESTAMP"
    )
}

fn scalar_parameter_text(type_name: &str, value: &Value) -> Result<String, String> {
    match type_name {
        "BOOL" => bool_text(value),
        "INT64" => integer_text(value),
        "FLOAT64" => float_text(value),
        "NUMERIC" | "BIGNUMERIC" => decimal_text(value),
        "BYTES" => bytes_text(value),
        "DATE" | "TIME" | "DATETIME" | "TIMESTAMP" => temporal_text(type_name, value),
        _ => scalar_text(value),
    }
}

fn range_element(field: Option<&FieldSchema>) -> String {
    field
        .and_then(|field| field.range_element_type.as_ref())
        .map(|range| canonical_type(&range.element_type))
        .unwrap_or_else(|| "DATE".to_string())
}

fn range_text(value: &Value) -> Result<String, String> {
    match value {
        Value::Object(object) => {
            let bound = |key: &str| match object.get(key) {
                None | Some(Value::Null) => Ok("UNBOUNDED".to_string()),
                Some(value) => scalar_text(value),
            };
            Ok(format!("[{}, {})", bound("start")?, bound("end")?))
        }
        _ => {
            let text = scalar_text(value)?;
            let trimmed = text.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(')') && trimmed.contains(',') {
                Ok(trimmed.to_string())
            } else {
                Err(format!(
                    "Invalid RANGE value '{text}': expected '[start, end)' with UNBOUNDED for open bounds"
                ))
            }
        }
    }
}

fn bool_text(value: &Value) -> Result<String, String> {
    value
        .as_bool()
        .or_else(|| value.as_str().and_then(parse_bool))
        .map(|value| value.to_string())
        .ok_or_else(|| "Expected a boolean value".to_string())
}

fn integer_text(value: &Value) -> Result<String, String> {
    let text = scalar_text(value)?;
    text.trim()
        .parse::<i64>()
        .map(|number| number.to_string())
        .map_err(|_| format!("Invalid INT64 value '{text}'"))
}

fn float_text(value: &Value) -> Result<String, String> {
    let text = scalar_text(value)?;
    let trimmed = text.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "nan" => return Ok("NaN".to_string()),
        "inf" | "+inf" | "infinity" | "+infinity" => return Ok("Infinity".to_string()),
        "-inf" | "-infinity" => return Ok("-Infinity".to_string()),
        _ => {}
    }
    if trimmed.parse::<f64>().is_ok_and(f64::is_finite) {
        Ok(trimmed.to_string())
    } else {
        Err(format!("Invalid FLOAT64 value '{text}'"))
    }
}

/// Validates a NUMERIC/BIGNUMERIC decimal without going through f64.
fn decimal_text(value: &Value) -> Result<String, String> {
    let text = scalar_text(value)?;
    let trimmed = text.trim();
    let (mantissa, exponent) = match trimmed.find(['e', 'E']) {
        Some(index) => (&trimmed[..index], Some(&trimmed[index + 1..])),
        None => (trimmed, None),
    };
    let unsigned = mantissa.strip_prefix(['+', '-']).unwrap_or(mantissa);
    let mut parts = unsigned.splitn(2, '.');
    let integer = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    let digits_ok = |part: &str| part.chars().all(|character| character.is_ascii_digit());
    let exponent_ok = exponent.is_none_or(|exponent| {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !digits.is_empty() && digits_ok(digits)
    });
    if (integer.is_empty() && fraction.is_empty())
        || !digits_ok(integer)
        || !digits_ok(fraction)
        || !exponent_ok
    {
        return Err(format!("Invalid NUMERIC value '{text}'"));
    }
    Ok(trimmed.to_string())
}

fn json_text(value: &Value) -> Result<String, String> {
    match value.as_str() {
        Some(text) => serde_json::from_str::<Value>(text)
            .map(|parsed| parsed.to_string())
            .map_err(|error| format!("Invalid JSON value: {error}")),
        None => Ok(value.to_string()),
    }
}

/// Accepts plain base64 or the Tabularis `BLOB:<size>:<mime>:<base64>` wire
/// format and returns validated base64.
fn bytes_text(value: &Value) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "Expected base64 text for BYTES".to_string())?;
    let encoded = text
        .strip_prefix("BLOB:")
        .and_then(|rest| rest.splitn(3, ':').nth(2))
        .unwrap_or(text)
        .trim();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("Invalid base64 BYTES value: {error}"))?;
    Ok(encoded.to_string())
}

/// Normalizes the RFC 3339 text Tabularis shows for temporal cells into the
/// canonical GoogleSQL format.
fn temporal_text(type_name: &str, value: &Value) -> Result<String, String> {
    let text = scalar_text(value)?;
    let mut normalized = text.trim().to_string();
    if matches!(type_name, "DATETIME" | "TIMESTAMP")
        && normalized.len() > 10
        && normalized.as_bytes()[10] == b'T'
    {
        normalized.replace_range(10..11, " ");
    }
    if type_name == "TIMESTAMP" {
        if let Some(stripped) = normalized.strip_suffix('Z') {
            normalized = format!("{stripped}+00:00");
        }
    }
    if normalized.is_empty() {
        return Err(format!("Invalid {type_name} value ''"));
    }
    Ok(normalized)
}

fn normalize_array(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(values) => Ok(values.clone()),
        Value::String(value) => serde_json::from_str::<Vec<Value>>(value)
            .map_err(|error| format!("Invalid ARRAY JSON: {error}")),
        _ => Err("ARRAY value must be a JSON array".to_string()),
    }
}

fn scalar_text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(true) => Ok("true".to_string()),
        Value::Bool(false) => Ok("false".to_string()),
        _ => Err("Expected a scalar value".to_string()),
    }
}

fn infer_type(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "BOOL",
        Value::Number(number) if number.is_i64() || number.is_u64() => "INT64",
        Value::Number(_) => "FLOAT64",
        Value::Array(_) => "ARRAY",
        Value::Object(_) => "JSON",
        _ => "STRING",
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::bigquery::RangeElementType;

    fn field(name: &str, field_type: &str) -> FieldSchema {
        FieldSchema {
            name: name.into(),
            field_type: field_type.into(),
            ..Default::default()
        }
    }

    #[test]
    fn quotes_each_path_part() {
        assert_eq!(
            fully_qualified("my-project-123", "sales.eu", "order`items"),
            "`my-project-123`.`sales.eu`.`order\\`items`"
        );
        assert_eq!(qualified("ds", "t"), "`ds`.`t`");
        assert_eq!(object_path(None, "ds", "t"), "`ds`.`t`");
        assert_eq!(object_path(Some("p-1"), "ds", "t"), "`p-1`.`ds`.`t`");
        assert_eq!(
            information_schema("bigquery-public-data", "samples", "TABLES"),
            "`bigquery-public-data`.`samples`.INFORMATION_SCHEMA.TABLES"
        );
        assert_eq!(quote_identifier("a\\b"), "`a\\\\b`");
    }

    #[test]
    fn escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(string_literal("O'Reilly"), "'O\\'Reilly'");
        assert_eq!(string_literal("a\\b"), "'a\\\\b'");
        assert_eq!(
            string_literal("line1\nline2\r\tend"),
            "'line1\\nline2\\r\\tend'"
        );
        assert_eq!(string_literal("bell\u{7}nul\u{0}"), "'bell\\x07nul\\x00'");
        assert_eq!(string_literal("c1\u{85}"), "'c1\\u0085'");
        assert_eq!(
            string_literal("caffè ☕ 東京 \u{1F600}"),
            "'caffè ☕ 東京 \u{1F600}'"
        );
        assert_eq!(string_literal("say \"hi\""), "'say \"hi\"'");
    }

    #[test]
    fn renders_scalar_literals() {
        assert_eq!(value_literal(&json!(42), None).unwrap(), "42");
        assert_eq!(
            value_literal(&json!("42"), Some(&field("n", "INTEGER"))).unwrap(),
            "42"
        );
        assert!(value_literal(&json!("4x"), Some(&field("n", "INT64"))).is_err());
        assert_eq!(
            value_literal(&json!("1.50"), Some(&field("n", "NUMERIC"))).unwrap(),
            "NUMERIC '1.50'"
        );
        assert_eq!(
            value_literal(
                &json!("-12345678901234567890.123456789"),
                Some(&field("n", "BIGNUMERIC"))
            )
            .unwrap(),
            "BIGNUMERIC '-12345678901234567890.123456789'"
        );
        assert!(value_literal(&json!("1.2.3"), Some(&field("n", "NUMERIC"))).is_err());
        assert!(value_literal(&json!("1; DROP"), Some(&field("n", "NUMERIC"))).is_err());
        assert_eq!(
            value_literal(&json!("NaN"), Some(&field("f", "FLOAT64"))).unwrap(),
            "CAST('NaN' AS FLOAT64)"
        );
        assert_eq!(
            value_literal(&json!(1.5), Some(&field("f", "FLOAT"))).unwrap(),
            "1.5"
        );
        assert_eq!(
            value_literal(&json!("true"), Some(&field("b", "BOOLEAN"))).unwrap(),
            "TRUE"
        );
        assert_eq!(
            value_literal(&json!("a\nb"), Some(&field("s", "STRING"))).unwrap(),
            "'a\\nb'"
        );
    }

    #[test]
    fn renders_special_type_literals() {
        assert_eq!(
            value_literal(&json!("POINT(1 2)"), Some(&field("g", "GEOGRAPHY"))).unwrap(),
            "ST_GEOGFROMTEXT('POINT(1 2)')"
        );
        assert_eq!(
            value_literal(&json!("1-2 3 4:5:6"), Some(&field("i", "INTERVAL"))).unwrap(),
            "CAST('1-2 3 4:5:6' AS INTERVAL)"
        );
        let mut range = field("r", "RANGE");
        range.range_element_type = Some(RangeElementType {
            element_type: "DATE".into(),
        });
        assert_eq!(
            value_literal(&json!("[2024-01-01, UNBOUNDED)"), Some(&range)).unwrap(),
            "RANGE<DATE> '[2024-01-01, UNBOUNDED)'"
        );
        assert_eq!(
            value_literal(&json!({"start": "2024-01-01", "end": null}), Some(&range)).unwrap(),
            "RANGE<DATE> '[2024-01-01, UNBOUNDED)'"
        );
        assert!(value_literal(&json!("2024-01-01"), Some(&range)).is_err());
        assert_eq!(
            value_literal(
                &json!("BLOB:3:application/octet-stream:AQID"),
                Some(&field("b", "BYTES"))
            )
            .unwrap(),
            "FROM_BASE64('AQID')"
        );
        assert!(value_literal(&json!("not base64!"), Some(&field("b", "BYTES"))).is_err());
        assert_eq!(
            value_literal(&json!("{\"a\": \"it's\\n\"}"), Some(&field("j", "JSON"))).unwrap(),
            "JSON '{\"a\":\"it\\'s\\\\n\"}'"
        );
        assert_eq!(
            value_literal(
                &json!("2024-01-02T03:04:05.123456Z"),
                Some(&field("t", "TIMESTAMP"))
            )
            .unwrap(),
            "TIMESTAMP '2024-01-02 03:04:05.123456+00:00'"
        );
        assert_eq!(
            value_literal(&json!("2024-01-02T03:04:05"), Some(&field("t", "DATETIME"))).unwrap(),
            "DATETIME '2024-01-02 03:04:05'"
        );
    }

    #[test]
    fn renders_nested_struct_and_array_literals() {
        let mut tags = field("tags", "STRING");
        tags.mode = Some("REPEATED".into());
        let mut record = field("item", "RECORD");
        record.fields = vec![
            field("sku", "STRING"),
            field("price", "NUMERIC"),
            tags.clone(),
        ];
        assert_eq!(
            value_literal(
                &json!({"sku": "a'b", "price": "9.99", "tags": ["x", "y\n"]}),
                Some(&record)
            )
            .unwrap(),
            "STRUCT<`sku` STRING, `price` NUMERIC, `tags` ARRAY<STRING>>('a\\'b', NUMERIC '9.99', ARRAY<STRING>['x', 'y\\n'])"
        );
        assert!(value_literal(&json!({"nope": 1}), Some(&record)).is_err());
        let mut items = record.clone();
        items.mode = Some("REPEATED".into());
        assert_eq!(
            value_literal(&json!("[]"), Some(&items)).unwrap(),
            "ARRAY<STRUCT<`sku` STRING, `price` NUMERIC, `tags` ARRAY<STRING>>>[]"
        );
        assert!(value_literal(&json!([null]), Some(&tags)).is_err());
    }

    #[test]
    fn renders_column_types() {
        let mut name = field("name", "STRING");
        name.max_length = Some("50".into());
        assert_eq!(field_type_sql(&name, true), "STRING(50)");
        let mut amount = field("amount", "NUMERIC");
        amount.precision = Some("10".into());
        amount.scale = Some("2".into());
        assert_eq!(field_type_sql(&amount, true), "NUMERIC(10, 2)");
        let mut required = field("id", "INTEGER");
        required.mode = Some("REQUIRED".into());
        let mut record = field("s", "RECORD");
        record.mode = Some("REPEATED".into());
        record.fields = vec![required, field("my field", "FLOAT")];
        assert_eq!(
            field_type_sql(&record, true),
            "ARRAY<STRUCT<`id` INT64 NOT NULL, `my field` FLOAT64>>"
        );
        assert_eq!(
            field_type_sql(&record, false),
            "ARRAY<STRUCT<id INT64, my field FLOAT64>>"
        );
    }

    #[test]
    fn binds_typed_parameters() {
        let mut params = QueryParams::new();
        assert_eq!(
            params
                .bind(&json!("x\n'y"), Some(&field("s", "STRING")))
                .unwrap(),
            "@p0"
        );
        assert_eq!(
            params
                .bind(&json!(7), Some(&field("n", "INTEGER")))
                .unwrap(),
            "@p1"
        );
        assert_eq!(
            params
                .bind(&json!("POINT(0 0)"), Some(&field("g", "GEOGRAPHY")))
                .unwrap(),
            "ST_GEOGFROMTEXT(@p2)"
        );
        assert_eq!(
            params
                .bind(&json!({"a": 1}), Some(&field("j", "JSON")))
                .unwrap(),
            "PARSE_JSON(@p3)"
        );
        assert_eq!(
            params
                .bind(&json!("1-0"), Some(&field("i", "INTERVAL")))
                .unwrap(),
            "CAST(@p4 AS INTERVAL)"
        );
        let mut range = field("r", "RANGE");
        range.range_element_type = Some(RangeElementType {
            element_type: "TIMESTAMP".into(),
        });
        assert_eq!(
            params
                .bind(&json!("[UNBOUNDED, 2024-01-01 00:00:00+00)"), Some(&range))
                .unwrap(),
            "CAST(@p5 AS RANGE<TIMESTAMP>)"
        );
        let mut ints = field("ids", "INT64");
        ints.mode = Some("REPEATED".into());
        assert_eq!(
            params.bind(&json!([1, "2"]), Some(&ints)).unwrap(),
            "ARRAY<INT64>[1, 2]"
        );
        assert_eq!(params.bind(&Value::Null, Some(&ints)).unwrap(), "NULL");
        assert!(params
            .bind(&json!("abc"), Some(&field("n", "INT64")))
            .is_err());
        assert_eq!(
            params
                .bind(&json!("AQID"), Some(&field("b", "BYTES")))
                .unwrap(),
            "FROM_BASE64(@p6)"
        );
        assert_eq!(
            params
                .bind(&json!("1.5"), Some(&field("n", "BIGNUMERIC")))
                .unwrap(),
            "CAST(@p7 AS BIGNUMERIC)"
        );
        let values = params.into_vec();
        assert_eq!(values.len(), 8);
        assert_eq!(
            values[0],
            json!({"name": "p0", "parameterType": {"type": "STRING"}, "parameterValue": {"value": "x\n'y"}})
        );
        assert_eq!(values[1]["parameterType"]["type"], "INT64");
        assert_eq!(values[1]["parameterValue"]["value"], "7");
        assert_eq!(values[3]["parameterValue"]["value"], "{\"a\":1}");
        assert_eq!(values[6]["parameterValue"]["value"], "AQID");
        assert_eq!(values[7]["parameterType"]["type"], "STRING");
    }

    #[test]
    fn struct_parameters_fall_back_to_literals() {
        let mut record = field("s", "RECORD");
        record.fields = vec![field("a", "INT64")];
        let mut params = QueryParams::new();
        assert_eq!(
            params.bind(&json!({"a": 1}), Some(&record)).unwrap(),
            "STRUCT<`a` INT64>(1)"
        );
        assert!(params.is_empty());
    }
}
