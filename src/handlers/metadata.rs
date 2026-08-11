use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::bigquery::QueryOutput;
use crate::client::{self, BigQueryClient};
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::sql::{qualified, value_literal};

pub async fn get_databases(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    match connection.project_id() {
        Ok(project) => ok_response(id, json!([project])),
        Err(error) => error_response(id, -32602, &error),
    }
}

pub async fn get_schemas(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let client = client::client_for(&connection).await?;
        Ok(json!(client.list_datasets().await?))
    })
    .await
}

pub async fn get_tables(id: Value, params: &Value) -> Value {
    list_objects(id, params, &["TABLE", "EXTERNAL", "SNAPSHOT", "CLONE"]).await
}

pub async fn get_views(id: Value, params: &Value) -> Value {
    list_objects(id, params, &["VIEW"]).await
}

pub async fn get_materialized_views(id: Value, params: &Value) -> Value {
    list_objects(id, params, &["MATERIALIZED_VIEW"]).await
}

async fn list_objects(id: Value, params: &Value, allowed_types: &[&str]) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let client = client::client_for(&connection).await?;
        let mut objects = client
            .list_tables(schema)
            .await?
            .into_iter()
            .filter(|table| {
                table
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| allowed_types.contains(&kind))
            })
            .filter_map(|table| {
                table
                    .pointer("/tableReference/tableId")
                    .and_then(Value::as_str)
                    .map(|name| json!({"name": name, "schema": schema, "comment": null}))
            })
            .collect::<Vec<_>>();
        objects.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        Ok(json!(objects))
    })
    .await
}

pub async fn get_columns(id: Value, params: &Value) -> Value {
    columns_for(id, params, &["table", "table_name"]).await
}

pub async fn get_view_columns(id: Value, params: &Value) -> Value {
    columns_for(id, params, &["view_name", "view"]).await
}

pub async fn get_materialized_view_columns(id: Value, params: &Value) -> Value {
    columns_for(id, params, &["view_name", "view"]).await
}

async fn columns_for(id: Value, params: &Value, keys: &[&str]) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let object = required_any(params, keys)?;
        let client = client::client_for(&connection).await?;
        Ok(json!(columns_value(&client, schema, object).await?))
    })
    .await
}

async fn columns_value(
    client: &BigQueryClient,
    schema: &str,
    table: &str,
) -> Result<Vec<Value>, String> {
    let resource = client.get_table(schema, table).await?;
    let fields = client::table_fields(&resource)?;
    let primary_keys = resource
        .pointer("/tableConstraints/primaryKey/columns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();

    Ok(fields
        .iter()
        .map(|field| {
            json!({
                "name": field.name,
                "data_type": field.display_type(),
                "is_nullable": field.mode.as_deref() != Some("REQUIRED"),
                "default_value": field.default_value_expression,
                "is_pk": primary_keys.contains(field.name.as_str()),
                "is_auto_increment": false,
                "comment": field.description
            })
        })
        .collect())
}

pub async fn get_foreign_keys(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let table = required_any(params, &["table", "table_name"])?;
        let client = client::client_for(&connection).await?;
        Ok(json!(foreign_keys_value(&client, schema, table).await?))
    })
    .await
}

async fn foreign_keys_value(
    client: &BigQueryClient,
    schema: &str,
    table: &str,
) -> Result<Vec<Value>, String> {
    let resource = client.get_table(schema, table).await?;
    let mut output = Vec::new();
    for foreign_key in resource
        .pointer("/tableConstraints/foreignKeys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = foreign_key
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("foreign_key");
        let referenced_table = foreign_key
            .pointer("/referencedTable/tableId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let referenced_schema = foreign_key
            .pointer("/referencedTable/datasetId")
            .and_then(Value::as_str)
            .unwrap_or(schema);
        for reference in foreign_key
            .get("columnReferences")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            output.push(json!({
                "name": name,
                "constraint_name": name,
                "column_name": reference.get("referencingColumn").and_then(Value::as_str).unwrap_or_default(),
                "ref_table": referenced_table,
                "referenced_table": referenced_table,
                "ref_column": reference.get("referencedColumn").and_then(Value::as_str).unwrap_or_default(),
                "referenced_column": reference.get("referencedColumn").and_then(Value::as_str).unwrap_or_default(),
                "foreign_schema_name": referenced_schema,
                "on_update": "NO ACTION",
                "on_delete": "NO ACTION"
            }));
        }
    }
    Ok(output)
}

pub async fn get_indexes(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let table = required_any(params, &["table", "table_name"])?;
        let client = client::client_for(&connection).await?;
        let resource = client.get_table(schema, table).await?;
        let indexes = resource
            .pointer("/tableConstraints/primaryKey/columns")
            .and_then(Value::as_array)
            .map(|columns| {
                columns
                    .iter()
                    .enumerate()
                    .filter_map(|(index, column)| {
                        column.as_str().map(|column| {
                            json!({
                                "name": "PRIMARY",
                                "index_name": "PRIMARY",
                                "column_name": column,
                                "columns": [column],
                                "is_unique": true,
                                "is_primary": true,
                                "seq_in_index": index + 1,
                                "is_expression": false
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(json!(indexes))
    })
    .await
}

pub async fn get_view_definition(id: Value, params: &Value) -> Value {
    definition(id, params, false).await
}

pub async fn get_materialized_view_definition(id: Value, params: &Value) -> Value {
    definition(id, params, true).await
}

async fn definition(id: Value, params: &Value, materialized: bool) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "view"])?;
        let client = client::client_for(&connection).await?;
        let resource = client.get_table(schema, name).await?;
        let pointer = if materialized {
            "/materializedView/query"
        } else {
            "/view/query"
        };
        let query = resource
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Definition is unavailable for '{schema}.{name}'"))?;
        let kind = if materialized {
            "MATERIALIZED VIEW"
        } else {
            "VIEW"
        };
        Ok(json!(format!(
            "CREATE {kind} {} AS\n{query}",
            qualified(schema, name)
        )))
    })
    .await
}

pub async fn create_view(id: Value, params: &Value) -> Value {
    mutate_view(id, params, false).await
}

pub async fn alter_view(id: Value, params: &Value) -> Value {
    mutate_view(id, params, true).await
}

async fn mutate_view(id: Value, params: &Value, replace: bool) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "name"])?;
        let definition = required_any(params, &["definition"])?;
        let prefix = if replace {
            "CREATE OR REPLACE VIEW"
        } else {
            "CREATE VIEW"
        };
        let sql = format!("{prefix} {} AS\n{definition}", qualified(schema, name));
        let client = client::client_for(&connection).await?;
        client.execute_query(&sql, Some(schema), Some(0), 1).await?;
        Ok(Value::Null)
    })
    .await
}

pub async fn drop_view(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "name"])?;
        let client = client::client_for(&connection).await?;
        client
            .execute_query(
                &format!("DROP VIEW IF EXISTS {}", qualified(schema, name)),
                Some(schema),
                Some(0),
                1,
            )
            .await?;
        Ok(Value::Null)
    })
    .await
}

pub async fn refresh_materialized_view(id: Value, _params: &Value) -> Value {
    error_response(
        id,
        -32601,
        "BigQuery materialized views refresh automatically and do not support manual refresh",
    )
}

pub async fn get_routines(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (client, schema) = client_and_schema(params).await?;
        let project = &client.project_id;
        let sql = format!(
            "SELECT routine_name, routine_type FROM {} ORDER BY routine_name",
            crate::sql::quote_identifier(&format!(
                "{project}.{schema}.INFORMATION_SCHEMA.ROUTINES"
            ))
        );
        let output = client.execute_query(&sql, Some(schema), None, 1).await?;
        Ok(json!(rows_as_objects(&output)
            .into_iter()
            .map(|row| json!({
                "name": row.get("routine_name").cloned().unwrap_or(Value::Null),
                "routine_type": row.get("routine_type").cloned().unwrap_or_else(|| json!("FUNCTION")),
                "schema": schema
            }))
            .collect::<Vec<_>>()))
    })
    .await
}

pub async fn get_routine_parameters(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (client, schema) = client_and_schema(params).await?;
        let routine = required_any(params, &["routine_name", "routine"])?;
        let project = &client.project_id;
        let routine_literal = value_literal(&json!(routine), None)?;
        let sql = format!(
            "SELECT parameter_name, data_type, parameter_mode FROM {} WHERE specific_name = {routine_literal} ORDER BY ordinal_position",
            crate::sql::quote_identifier(&format!(
                "{project}.{schema}.INFORMATION_SCHEMA.PARAMETERS"
            ))
        );
        let output = client.execute_query(&sql, Some(schema), None, 1).await?;
        Ok(json!(rows_as_objects(&output)
            .into_iter()
            .map(|row| json!({
                "name": row.get("parameter_name").cloned().unwrap_or(Value::Null),
                "data_type": row.get("data_type").cloned().unwrap_or(Value::Null),
                "mode": row.get("parameter_mode").cloned().unwrap_or_else(|| json!("IN"))
            }))
            .collect::<Vec<_>>()))
    })
    .await
}

pub async fn get_routine_definition(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (client, schema) = client_and_schema(params).await?;
        let routine = required_any(params, &["routine_name", "routine"])?;
        let project = &client.project_id;
        let routine_literal = value_literal(&json!(routine), None)?;
        let sql = format!(
            "SELECT ddl FROM {} WHERE routine_name = {routine_literal} LIMIT 1",
            crate::sql::quote_identifier(&format!(
                "{project}.{schema}.INFORMATION_SCHEMA.ROUTINES"
            ))
        );
        let output = client.execute_query(&sql, Some(schema), Some(1), 1).await?;
        output
            .rows
            .first()
            .and_then(|row| row.first())
            .cloned()
            .ok_or_else(|| format!("Routine '{routine}' was not found"))
    })
    .await
}

pub async fn get_schema_snapshot(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let client = client::client_for(&connection).await?;
        let tables = client.list_tables(schema).await?;
        let mut snapshot = Vec::new();
        for table in tables {
            let Some(name) = table
                .pointer("/tableReference/tableId")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let kind = table
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(kind, "TABLE" | "EXTERNAL" | "SNAPSHOT" | "CLONE") {
                continue;
            }
            snapshot.push(json!({
                "name": name,
                "columns": columns_value(&client, schema, name).await?,
                "foreign_keys": foreign_keys_value(&client, schema, name).await?
            }));
        }
        Ok(json!(snapshot))
    })
    .await
}

pub async fn get_all_columns_batch(id: Value, params: &Value) -> Value {
    batch_metadata(id, params, true).await
}

pub async fn get_all_foreign_keys_batch(id: Value, params: &Value) -> Value {
    batch_metadata(id, params, false).await
}

async fn batch_metadata(id: Value, params: &Value, columns: bool) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let tables = params
            .get("tables")
            .and_then(Value::as_array)
            .map(|tables| tables.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let client = client::client_for(&connection).await?;
        let mut result = serde_json::Map::new();
        for table in tables {
            let value = if columns {
                json!(columns_value(&client, schema, table).await?)
            } else {
                json!(foreign_keys_value(&client, schema, table).await?)
            };
            result.insert(table.to_string(), value);
        }
        Ok(Value::Object(result))
    })
    .await
}

pub async fn get_ai_schema_context(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let schema = required_schema(params)?;
        let max_tables = params
            .get("max_tables")
            .and_then(Value::as_u64)
            .unwrap_or(20) as usize;
        let client = client::client_for(&connection).await?;
        let tables = client
            .list_tables(schema)
            .await?
            .into_iter()
            .filter(|table| table.get("type").and_then(Value::as_str) == Some("TABLE"))
            .filter_map(|table| {
                table
                    .pointer("/tableReference/tableId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        let total = tables.len();
        let mut output = Vec::new();
        for table in tables.into_iter().take(max_tables) {
            output.push(json!({
                "name": table,
                "columns": columns_value(&client, schema, &table).await?,
                "foreign_keys": foreign_keys_value(&client, schema, &table).await?
            }));
        }
        Ok(json!({"tables": output, "total_table_count": total}))
    })
    .await
}

fn rows_as_objects(output: &QueryOutput) -> Vec<HashMap<String, Value>> {
    output
        .rows
        .iter()
        .map(|row| {
            output
                .columns
                .iter()
                .cloned()
                .zip(row.iter().cloned())
                .collect()
        })
        .collect()
}

async fn client_and_schema(
    params: &Value,
) -> Result<(std::sync::Arc<BigQueryClient>, &str), String> {
    let connection = ConnectionParams::from_value(inner_params(params));
    let schema = required_schema(params)?;
    let client = client::client_for(&connection).await?;
    Ok((client, schema))
}

fn required_schema(params: &Value) -> Result<&str, String> {
    params
        .get("schema")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A BigQuery dataset is required as 'schema'".to_string())
}

fn required_any<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str, String> {
    keys.iter()
        .find_map(|key| params.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "Missing required parameter '{}': expected a non-empty string",
                keys[0]
            )
        })
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
