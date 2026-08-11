use serde_json::Value;

use crate::bigquery::FieldSchema;

pub fn quote_identifier(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "\\`"))
}

pub fn qualified(dataset: &str, object: &str) -> String {
    quote_identifier(&format!("{dataset}.{object}"))
}

pub fn fully_qualified(project: &str, dataset: &str, object: &str) -> String {
    quote_identifier(&format!("{project}.{dataset}.{object}"))
}

pub fn value_literal(value: &Value, field: Option<&FieldSchema>) -> Result<String, String> {
    if value.is_null() {
        return Ok("NULL".to_string());
    }
    let field_type = field
        .map(|field| field.field_type.to_ascii_uppercase())
        .unwrap_or_else(|| infer_type(value).to_string());

    if field.and_then(|field| field.mode.as_deref()) == Some("REPEATED") {
        let values = normalize_array(value)?;
        let mut scalar_field = field.cloned().unwrap_or_else(|| FieldSchema {
            name: String::new(),
            field_type: "STRING".to_string(),
            mode: None,
            description: None,
            fields: vec![],
            default_value_expression: None,
        });
        scalar_field.mode = None;
        let literals = values
            .iter()
            .map(|value| value_literal(value, Some(&scalar_field)))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(format!("[{}]", literals.join(", ")));
    }

    match field_type.as_str() {
        "RECORD" | "STRUCT" => struct_literal(value, field),
        "BOOL" | "BOOLEAN" => value
            .as_bool()
            .or_else(|| value.as_str().and_then(parse_bool))
            .map(|value| if value { "TRUE" } else { "FALSE" }.to_string())
            .ok_or_else(|| "Expected a boolean value".to_string()),
        "INT64" | "INTEGER" => validated_number(value, true),
        "FLOAT" | "FLOAT64" | "NUMERIC" | "BIGNUMERIC" => validated_number(value, false),
        "JSON" => {
            let serialized = if let Some(value) = value.as_str() {
                serde_json::from_str::<Value>(value)
                    .map_err(|error| format!("Invalid JSON value: {error}"))?
                    .to_string()
            } else {
                value.to_string()
            };
            Ok(format!("JSON '{}'", escape_string(&serialized)))
        }
        "BYTES" => {
            let value = value
                .as_str()
                .ok_or_else(|| "Expected base64 text for BYTES".to_string())?;
            let encoded = value
                .strip_prefix("BLOB:")
                .and_then(|rest| rest.splitn(4, ':').nth(3))
                .unwrap_or(value);
            Ok(format!("FROM_BASE64('{}')", escape_string(encoded)))
        }
        "DATE" | "TIME" | "DATETIME" | "TIMESTAMP" | "GEOGRAPHY" => {
            let value = scalar_text(value)?;
            Ok(format!("{field_type} '{}'", escape_string(&value)))
        }
        _ => Ok(format!("'{}'", escape_string(&scalar_text(value)?))),
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
    let values = if fields.is_empty() {
        object
            .iter()
            .map(|(name, value)| {
                Ok(format!(
                    "{} AS {}",
                    value_literal(value, None)?,
                    quote_identifier(name)
                ))
            })
            .collect::<Result<Vec<_>, String>>()?
    } else {
        fields
            .iter()
            .map(|nested| {
                let value = object.get(&nested.name).unwrap_or(&Value::Null);
                Ok(format!(
                    "{} AS {}",
                    value_literal(value, Some(nested))?,
                    quote_identifier(&nested.name)
                ))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    Ok(format!("STRUCT({})", values.join(", ")))
}

fn normalize_array(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(values) => Ok(values.clone()),
        Value::String(value) => serde_json::from_str::<Vec<Value>>(value)
            .map_err(|error| format!("Invalid ARRAY JSON: {error}")),
        _ => Err("ARRAY value must be a JSON array".to_string()),
    }
}

fn validated_number(value: &Value, integer: bool) -> Result<String, String> {
    let text = scalar_text(value)?;
    let valid = if integer {
        text.parse::<i128>().is_ok()
    } else {
        text.parse::<f64>().is_ok() && !matches!(text.as_str(), "NaN" | "inf" | "-inf")
    };
    valid
        .then_some(text.clone())
        .ok_or_else(|| format!("Invalid numeric value '{text}'"))
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
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn escape_string(value: &str) -> String {
    value
        .replace('\0', "")
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{qualified, value_literal};

    #[test]
    fn quotes_qualified_identifiers() {
        assert_eq!(
            qualified("sales-eu", "order`items"),
            "`sales-eu.order\\`items`"
        );
    }

    #[test]
    fn escapes_strings_and_preserves_numbers() {
        assert_eq!(value_literal(&json!(42), None).unwrap(), "42");
        assert_eq!(
            value_literal(&json!("O'Reilly"), None).unwrap(),
            "'O\\'Reilly'"
        );
    }
}
