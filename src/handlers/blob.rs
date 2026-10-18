use base64::Engine;
use serde_json::{json, Value};

use crate::catalog;
use crate::client::{self, Statement};
use crate::handlers::crud::{build_predicate, row_identity};
use crate::handlers::metadata::Target;
use crate::rpc::{error_response, ok_response};
use crate::schema_ddl;
use crate::sql::{quote_identifier, QueryParams};

pub async fn save_blob_to_file(id: Value, params: &Value) -> Value {
    let Some(path) = params.get("file_path").and_then(Value::as_str) else {
        return error_response(id, -32602, "Invalid params: missing 'file_path'");
    };
    match fetch_blob(params).await {
        Ok(bytes) => match tokio::fs::write(path, bytes).await {
            Ok(()) => ok_response(id, Value::Null),
            Err(error) => {
                error_response(id, -32603, &format!("Unable to write BLOB file: {error}"))
            }
        },
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

/// `SELECT TO_BASE64(column)` for one row, identified like row edits.
pub fn blob_sql(path: &str, column: &str, predicate: &str) -> String {
    format!(
        "SELECT TO_BASE64({}) AS data FROM {path} WHERE {predicate} LIMIT 1",
        quote_identifier(column)
    )
}

async fn fetch_blob(params: &Value) -> Result<Vec<u8>, String> {
    let schema = required(params, "schema")?;
    let table = required(params, "table")?;
    let column = required(params, "col_name")?;
    let identity = row_identity(params)?;
    let target = Target::from_params(params).await?;
    let resource = catalog::table(&target.client, &target.project, schema, table).await?;
    let fields = client::table_fields(&resource)?;
    if !fields.iter().any(|field| field.name == column) {
        return Err(format!("Column '{column}' does not exist in {table}"));
    }
    let keyless = schema_ddl::primary_key_columns(&resource).is_empty();
    let mut parameters = QueryParams::new();
    let predicate = build_predicate(&identity, &fields, keyless, &mut parameters)?;
    let statement = Statement::new(blob_sql(&target.path(schema, table), column, &predicate))
        .with_parameters(parameters.into_vec())
        .in_dataset(&target.project, schema)
        .at_location(target.location(schema).await)
        .returning(1);
    let output = target.client.run_statement(&statement).await?;
    let row = output
        .rows
        .first()
        .ok_or_else(|| "The row was not found; it may have been changed or deleted".to_string())?;
    let encoded = row
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "The BYTES value is NULL".to_string())?;
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

#[cfg(test)]
mod tests {
    use super::blob_sql;

    #[test]
    fn selects_one_base64_value() {
        assert_eq!(
            blob_sql("`p-1`.`d`.`t`", "data`x", "`id` = @p0"),
            "SELECT TO_BASE64(`data\\`x`) AS data FROM `p-1`.`d`.`t` WHERE `id` = @p0 LIMIT 1"
        );
    }
}
