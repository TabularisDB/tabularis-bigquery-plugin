use base64::Engine;
use serde_json::{json, Value};

use crate::client;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::sql::{fully_qualified, quote_identifier, value_literal};

pub async fn save_blob_to_file(id: Value, params: &Value) -> Value {
    match fetch_blob(params).await {
        Ok(bytes) => {
            let Some(path) = params.get("file_path").and_then(Value::as_str) else {
                return error_response(id, -32602, "Invalid params: missing 'file_path'");
            };
            match tokio::fs::write(path, bytes).await {
                Ok(()) => ok_response(id, Value::Null),
                Err(error) => {
                    error_response(id, -32603, &format!("Unable to write BLOB file: {error}"))
                }
            }
        }
        Err(error) => error_response(id, -32603, &error),
    }
}

pub async fn fetch_blob_as_data_url(id: Value, params: &Value) -> Value {
    match fetch_blob(params).await {
        Ok(bytes) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            ok_response(
                id,
                json!(format!(
                    "BLOB:{}:application/octet-stream:{encoded}",
                    bytes.len()
                )),
            )
        }
        Err(error) => error_response(id, -32603, &error),
    }
}

async fn fetch_blob(params: &Value) -> Result<Vec<u8>, String> {
    let connection = ConnectionParams::from_value(inner_params(params));
    let schema = required(params, "schema")?;
    let table = required(params, "table")?;
    let column = required(params, "col_name")?;
    let pk_map = params
        .get("pk_map")
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
        .ok_or_else(|| "A non-empty pk_map is required".to_string())?;
    let client = client::client_for(&connection).await?;
    let resource = client.get_table(schema, table).await?;
    let fields = client::table_fields(&resource)?;
    let find_field = |name: &str| fields.iter().find(|field| field.name == name);
    let predicates = pk_map
        .iter()
        .map(|(name, value)| {
            let expression = if value.is_null() {
                "IS NULL".to_string()
            } else {
                format!("= {}", value_literal(value, find_field(name))?)
            };
            Ok(format!("{} {expression}", quote_identifier(name)))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let sql = format!(
        "SELECT TO_BASE64({}) AS data FROM {} WHERE {} LIMIT 1",
        quote_identifier(column),
        fully_qualified(&client.project_id, schema, table),
        predicates.join(" AND ")
    );
    let output = client.execute_query(&sql, Some(schema), Some(1), 1).await?;
    let encoded = output
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_str)
        .ok_or_else(|| "BLOB value was not found".to_string())?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("BigQuery returned invalid base64 data: {error}"))
}

fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Invalid params: missing '{key}'"))
}
