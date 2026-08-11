use std::collections::HashMap;

use serde_json::{json, Value};

use crate::bigquery::FieldSchema;
use crate::client;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::sql::{fully_qualified, quote_identifier, value_literal};

pub async fn insert_record(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (connection, schema, table) = connection_target(params)?;
        let data = params
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| "Invalid params: missing or malformed 'data'".to_string())?;
        if data.is_empty() {
            return Err("Cannot insert an empty row".to_string());
        }
        let client = client::client_for(&connection).await?;
        let resource = client.get_table(schema, table).await?;
        let fields = client::table_fields(&resource)?;
        let by_name = field_map(&fields);
        let columns = data
            .keys()
            .map(|name| quote_identifier(name))
            .collect::<Vec<_>>();
        let values = data
            .iter()
            .map(|(name, value)| value_literal(value, by_name.get(name).copied()))
            .collect::<Result<Vec<_>, _>>()?;
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            fully_qualified(&client.project_id, schema, table),
            columns.join(", "),
            values.join(", ")
        );
        let result = client.execute_query(&sql, Some(schema), Some(0), 1).await?;
        Ok(json!(result.affected_rows.max(1)))
    })
    .await
}

pub async fn update_record(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (connection, schema, table) = connection_target(params)?;
        let column = required_string(params, "col_name")?;
        let value = params.get("new_val").cloned().unwrap_or(Value::Null);
        let predicates = primary_key_map(params)?;
        let client = client::client_for(&connection).await?;
        let resource = client.get_table(schema, table).await?;
        let fields = client::table_fields(&resource)?;
        let by_name = field_map(&fields);
        let assignment = value_literal(&value, by_name.get(column).copied())?;
        let where_clause = build_predicate(&predicates, &by_name)?;
        let sql = format!(
            "UPDATE {} SET {} = {} WHERE {}",
            fully_qualified(&client.project_id, schema, table),
            quote_identifier(column),
            assignment,
            where_clause
        );
        let result = client.execute_query(&sql, Some(schema), Some(0), 1).await?;
        Ok(json!(result.affected_rows))
    })
    .await
}

pub async fn delete_record(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (connection, schema, table) = connection_target(params)?;
        let predicates = primary_key_map(params)?;
        let client = client::client_for(&connection).await?;
        let resource = client.get_table(schema, table).await?;
        let fields = client::table_fields(&resource)?;
        let by_name = field_map(&fields);
        let where_clause = build_predicate(&predicates, &by_name)?;
        let sql = format!(
            "DELETE FROM {} WHERE {}",
            fully_qualified(&client.project_id, schema, table),
            where_clause
        );
        let result = client.execute_query(&sql, Some(schema), Some(0), 1).await?;
        Ok(json!(result.affected_rows))
    })
    .await
}

fn connection_target(params: &Value) -> Result<(ConnectionParams, &str, &str), String> {
    let connection = ConnectionParams::from_value(inner_params(params));
    let schema = required_string(params, "schema")?;
    let table = params
        .get("table")
        .or_else(|| params.get("table_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Invalid params: missing 'table'".to_string())?;
    Ok((connection, schema, table))
}

fn primary_key_map(params: &Value) -> Result<HashMap<String, Value>, String> {
    if let Some(map) = params.get("pk_map").and_then(Value::as_object) {
        if !map.is_empty() {
            return Ok(map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect());
        }
    }
    match (
        params.get("pk_col").and_then(Value::as_str),
        params.get("pk_val"),
    ) {
        (Some(column), Some(value)) => Ok(HashMap::from([(column.to_string(), value.clone())])),
        _ => Err("A non-empty primary-key map is required for update and delete".to_string()),
    }
}

fn field_map(fields: &[FieldSchema]) -> HashMap<String, &FieldSchema> {
    fields
        .iter()
        .map(|field| (field.name.clone(), field))
        .collect()
}

fn build_predicate(
    values: &HashMap<String, Value>,
    fields: &HashMap<String, &FieldSchema>,
) -> Result<String, String> {
    let mut keys = values.keys().collect::<Vec<_>>();
    keys.sort();
    keys.into_iter()
        .map(|column| {
            let value = &values[column];
            if value.is_null() {
                Ok(format!("{} IS NULL", quote_identifier(column)))
            } else {
                Ok(format!(
                    "{} = {}",
                    quote_identifier(column),
                    value_literal(value, fields.get(column).copied())?
                ))
            }
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|parts| parts.join(" AND "))
}

fn required_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Invalid params: missing '{key}'"))
}

async fn respond<F>(id: Value, future: F) -> Value
where
    F: std::future::Future<Output = Result<Value, String>>,
{
    match future.await {
        Ok(value) => ok_response(id, value),
        Err(error) => error_response(id, -32603, &error),
    }
}
