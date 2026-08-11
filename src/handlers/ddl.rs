use serde_json::Value;

use crate::client;
use crate::models::{inner_params, ColumnDefinition, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::sql::{qualified, quote_identifier};

pub async fn get_create_table_sql(id: Value, params: &Value) -> Value {
    let table = params
        .get("table_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let schema = params
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let columns = parse_columns(params.get("columns"));
    if table.is_empty() || schema.is_empty() || columns.is_empty() {
        return error_response(
            id,
            -32602,
            "Table name, dataset and at least one column are required",
        );
    }
    ok_response(
        id,
        Value::from(vec![create_table_sql(schema, table, &columns)]),
    )
}

pub async fn get_add_column_sql(id: Value, params: &Value) -> Value {
    let Some((schema, table)) = target(params) else {
        return error_response(id, -32602, "Table and dataset are required");
    };
    let Some(column) = parse_column(params.get("column")) else {
        return error_response(id, -32602, "A valid column definition is required");
    };
    ok_response(
        id,
        Value::from(vec![format!(
            "ALTER TABLE {} ADD COLUMN {}",
            qualified(schema, table),
            column_sql(&column)
        )]),
    )
}

pub async fn get_alter_column_sql(id: Value, params: &Value) -> Value {
    let Some((schema, table)) = target(params) else {
        return error_response(id, -32602, "Table and dataset are required");
    };
    let Some(old) = parse_column(params.get("old_column")) else {
        return error_response(id, -32602, "A valid old_column definition is required");
    };
    let Some(new) = parse_column(params.get("new_column")) else {
        return error_response(id, -32602, "A valid new_column definition is required");
    };
    match alter_column_sql(schema, table, &old, &new) {
        Ok(statements) => ok_response(id, Value::from(statements)),
        Err(error) => error_response(id, -32603, &error),
    }
}

pub async fn get_create_index_sql(id: Value, _params: &Value) -> Value {
    error_response(
        id,
        -32601,
        "General-purpose CREATE INDEX is not supported by BigQuery",
    )
}

pub async fn get_create_foreign_key_sql(id: Value, params: &Value) -> Value {
    let Some((schema, table)) = target(params) else {
        return error_response(id, -32602, "Table and dataset are required");
    };
    let required = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let (Some(name), Some(column), Some(ref_table), Some(ref_column)) = (
        required("fk_name"),
        required("column"),
        required("ref_table"),
        required("ref_column"),
    ) else {
        return error_response(
            id,
            -32602,
            "Foreign-key name, columns and referenced table are required",
        );
    };
    let sql = format!(
        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}) NOT ENFORCED",
        qualified(schema, table),
        quote_identifier(name),
        quote_identifier(column),
        qualified(schema, ref_table),
        quote_identifier(ref_column)
    );
    ok_response(id, Value::from(vec![sql]))
}

pub async fn drop_index(id: Value, _params: &Value) -> Value {
    error_response(
        id,
        -32601,
        "General-purpose indexes are not supported by BigQuery",
    )
}

pub async fn drop_foreign_key(id: Value, params: &Value) -> Value {
    let connection = ConnectionParams::from_value(inner_params(params));
    let Some((schema, table)) = target(params) else {
        return error_response(id, -32602, "Table and dataset are required");
    };
    let Some(name) = params.get("fk_name").and_then(Value::as_str) else {
        return error_response(id, -32602, "Foreign-key name is required");
    };
    let sql = format!(
        "ALTER TABLE {} DROP CONSTRAINT {}",
        qualified(schema, table),
        quote_identifier(name)
    );
    let result = async {
        let client = client::client_for(&connection).await?;
        client
            .execute_query(&sql, Some(schema), Some(0), 1)
            .await
            .map(|_| ())
    }
    .await;
    match result {
        Ok(()) => ok_response(id, Value::Null),
        Err(error) => error_response(id, -32603, &error),
    }
}

fn create_table_sql(schema: &str, table: &str, columns: &[ColumnDefinition]) -> String {
    let mut definitions = columns.iter().map(column_sql).collect::<Vec<_>>();
    let primary_keys = columns
        .iter()
        .filter(|column| column.is_pk)
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>();
    if !primary_keys.is_empty() {
        definitions.push(format!(
            "PRIMARY KEY ({}) NOT ENFORCED",
            primary_keys.join(", ")
        ));
    }
    format!(
        "CREATE TABLE {} (\n  {}\n)",
        qualified(schema, table),
        definitions.join(",\n  ")
    )
}

fn column_sql(column: &ColumnDefinition) -> String {
    let mut output = format!(
        "{} {}",
        quote_identifier(&column.name),
        column.data_type.trim()
    );
    if !column.is_nullable {
        output.push_str(" NOT NULL");
    }
    if let Some(default) = column
        .default_value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        output.push_str(" DEFAULT ");
        output.push_str(default.trim());
    }
    output
}

fn alter_column_sql(
    schema: &str,
    table: &str,
    old: &ColumnDefinition,
    new: &ColumnDefinition,
) -> Result<Vec<String>, String> {
    let target = qualified(schema, table);
    let mut statements = Vec::new();
    if old.name != new.name {
        statements.push(format!(
            "ALTER TABLE {target} RENAME COLUMN {} TO {}",
            quote_identifier(&old.name),
            quote_identifier(&new.name)
        ));
    }
    let column = quote_identifier(&new.name);
    if !old.data_type.eq_ignore_ascii_case(&new.data_type) {
        statements.push(format!(
            "ALTER TABLE {target} ALTER COLUMN {column} SET DATA TYPE {}",
            new.data_type.trim()
        ));
    }
    if old.is_nullable != new.is_nullable {
        let action = if new.is_nullable {
            "DROP NOT NULL"
        } else {
            "SET NOT NULL"
        };
        statements.push(format!(
            "ALTER TABLE {target} ALTER COLUMN {column} {action}"
        ));
    }
    if old.default_value != new.default_value {
        let action = new
            .default_value
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("SET DEFAULT {}", value.trim()))
            .unwrap_or_else(|| "DROP DEFAULT".to_string());
        statements.push(format!(
            "ALTER TABLE {target} ALTER COLUMN {column} {action}"
        ));
    }
    if statements.is_empty() {
        Err("No column changes were detected".to_string())
    } else {
        Ok(statements)
    }
}

fn target(params: &Value) -> Option<(&str, &str)> {
    Some((
        params.get("schema")?.as_str()?.trim(),
        params.get("table")?.as_str()?.trim(),
    ))
    .filter(|(schema, table)| !schema.is_empty() && !table.is_empty())
}

fn parse_columns(value: Option<&Value>) -> Vec<ColumnDefinition> {
    value
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn parse_column(value: Option<&Value>) -> Option<ColumnDefinition> {
    value
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

#[cfg(test)]
mod tests {
    use super::{alter_column_sql, create_table_sql};
    use crate::models::ColumnDefinition;

    fn column(name: &str, data_type: &str, is_pk: bool) -> ColumnDefinition {
        ColumnDefinition {
            name: name.into(),
            data_type: data_type.into(),
            is_nullable: !is_pk,
            is_pk,
            is_auto_increment: false,
            default_value: None,
        }
    }

    #[test]
    fn creates_not_enforced_primary_key() {
        let sql = create_table_sql("analytics", "events", &[column("id", "INT64", true)]);
        assert!(sql.contains("PRIMARY KEY (`id`) NOT ENFORCED"));
    }

    #[test]
    fn builds_supported_column_alterations() {
        let old = column("value", "INT64", false);
        let mut new = column("amount", "NUMERIC", false);
        new.is_nullable = false;
        let sql = alter_column_sql("d", "t", &old, &new).unwrap();
        assert_eq!(sql.len(), 3);
    }
}
