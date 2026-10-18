use serde_json::Value;

use crate::catalog;
use crate::handlers::metadata::{respond, Target};
use crate::models::{inner_params, ColumnDefinition, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::schema_ddl;
use crate::sql::{canonical_type, object_path, quote_identifier, string_literal};

/// Project for generated SQL. SQL builders only receive connection
/// parameters on hosts that attach them; without a project the statements
/// are dataset-qualified and resolve in the job project.
fn optional_project(params: &Value) -> Option<String> {
    params.get("params")?;
    ConnectionParams::from_value(inner_params(params))
        .project_id()
        .ok()
        .map(str::to_owned)
}

/// Rejects column changes on snapshots, external tables and views when the
/// host sends connection parameters; without them the table is not looked up.
async fn ensure_editable(params: &Value, schema: &str, table: &str) -> Result<(), String> {
    if params.get("params").is_none() {
        return Ok(());
    }
    // Best effort: a failed lookup leaves the decision to BigQuery.
    let Ok(target) = Target::from_params(params).await else {
        return Ok(());
    };
    match catalog::table(&target.client, &target.project, schema, table).await {
        Ok(resource) => schema_ddl::ensure_schema_editable(&resource),
        Err(_) => Ok(()),
    }
}

/// Optional table settings. Current hosts send only columns; these keys are
/// accepted for callers that provide them.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TableOptions {
    pub partition_by: Option<String>,
    pub cluster_by: Vec<String>,
    pub description: Option<String>,
}

impl TableOptions {
    fn from_params(params: &Value) -> Self {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let cluster_by = match params.get("cluster_by") {
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(Value::String(value)) => value
                .split(',')
                .map(str::trim)
                .filter(|column| !column.is_empty())
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        Self {
            partition_by: text("partition_by"),
            cluster_by,
            description: text("description").or_else(|| text("comment")),
        }
    }
}

pub async fn get_create_table_sql(id: Value, params: &Value) -> Value {
    catalog::invalidate();
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
    let path = object_path(optional_project(params).as_deref(), schema, table);
    match create_table_sql(&path, &columns, &TableOptions::from_params(params)) {
        Ok(sql) => ok_response(id, Value::from(vec![sql])),
        Err(error) => error_response(id, -32602, &error),
    }
}

pub async fn get_add_column_sql(id: Value, params: &Value) -> Value {
    catalog::invalidate();
    let Some((schema, table)) = target(params) else {
        return error_response(id, -32602, "Table and dataset are required");
    };
    let Some(column) = parse_column(params.get("column")) else {
        return error_response(id, -32602, "A valid column definition is required");
    };
    if let Err(error) = ensure_editable(params, schema, table).await {
        return error_response(id, -32602, &error);
    }
    let path = object_path(optional_project(params).as_deref(), schema, table);
    match add_column_sql(&path, &column) {
        Ok(statements) => ok_response(id, Value::from(statements)),
        Err(error) => error_response(id, -32602, &error),
    }
}

pub async fn get_alter_column_sql(id: Value, params: &Value) -> Value {
    catalog::invalidate();
    let Some((schema, table)) = target(params) else {
        return error_response(id, -32602, "Table and dataset are required");
    };
    let Some(old) = parse_column(params.get("old_column")) else {
        return error_response(id, -32602, "A valid old_column definition is required");
    };
    let Some(new) = parse_column(params.get("new_column")) else {
        return error_response(id, -32602, "A valid new_column definition is required");
    };
    if let Err(error) = ensure_editable(params, schema, table).await {
        return error_response(id, -32602, &error);
    }
    let path = object_path(optional_project(params).as_deref(), schema, table);
    match alter_column_sql(&path, &old, &new) {
        Ok(statements) => ok_response(id, Value::from(statements)),
        Err(error) => error_response(id, -32603, &error),
    }
}

pub async fn get_create_index_sql(id: Value, _params: &Value) -> Value {
    error_response(
        id,
        -32601,
        "BigQuery has no general-purpose indexes. Search and vector indexes use CREATE SEARCH INDEX / CREATE VECTOR INDEX in a query tab.",
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
            .map(str::trim)
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
    let actions = [
        params.get("on_delete").and_then(Value::as_str),
        params.get("on_update").and_then(Value::as_str),
    ];
    let project = optional_project(params);
    match foreign_key_sql(
        &object_path(project.as_deref(), schema, table),
        &object_path(project.as_deref(), schema, ref_table),
        name,
        column,
        ref_column,
        &actions,
    ) {
        Ok(sql) => ok_response(id, Value::from(vec![sql])),
        Err(error) => error_response(id, -32602, &error),
    }
}

pub async fn drop_index(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (schema, table) =
            target(params).ok_or_else(|| "Table and dataset are required".to_string())?;
        let index = params
            .get("index_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !index.eq_ignore_ascii_case("PRIMARY") {
            return Err(
                "BigQuery has no general-purpose indexes. Drop search or vector indexes with DROP SEARCH INDEX / DROP VECTOR INDEX in a query tab."
                    .to_string(),
            );
        }
        let target_handle = Target::from_params(params).await?;
        let sql = format!(
            "ALTER TABLE {} DROP PRIMARY KEY IF EXISTS",
            target_handle.path(schema, table)
        );
        target_handle.run_ddl(&sql, schema).await?;
        Ok(Value::Null)
    })
    .await
}

pub async fn drop_foreign_key(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (schema, table) =
            target(params).ok_or_else(|| "Table and dataset are required".to_string())?;
        let name = params
            .get("fk_name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "Foreign-key name is required".to_string())?;
        let target_handle = Target::from_params(params).await?;
        let sql = format!(
            "ALTER TABLE {} DROP CONSTRAINT IF EXISTS {}",
            target_handle.path(schema, table),
            quote_identifier(name)
        );
        target_handle.run_ddl(&sql, schema).await?;
        Ok(Value::Null)
    })
    .await
}

fn split_columns(columns: &str) -> Vec<String> {
    columns
        .split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(quote_identifier)
        .collect()
}

pub fn foreign_key_sql(
    table: &str,
    referenced: &str,
    name: &str,
    columns: &str,
    referenced_columns: &str,
    actions: &[Option<&str>],
) -> Result<String, String> {
    for action in actions.iter().flatten() {
        let action = action.trim().to_ascii_uppercase();
        if !action.is_empty() && action != "NO ACTION" && action != "RESTRICT" {
            return Err(format!(
                "BigQuery foreign keys are informational (NOT ENFORCED) and do not support ON DELETE/ON UPDATE {action}"
            ));
        }
    }
    let columns = split_columns(columns);
    let referenced_columns = split_columns(referenced_columns);
    if columns.len() != referenced_columns.len() {
        return Err("Foreign-key and referenced column counts must match".to_string());
    }
    Ok(format!(
        "ALTER TABLE {table} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {referenced}({}) NOT ENFORCED",
        quote_identifier(name),
        columns.join(", "),
        referenced_columns.join(", ")
    ))
}

pub fn create_table_sql(
    path: &str,
    columns: &[ColumnDefinition],
    options: &TableOptions,
) -> Result<String, String> {
    let mut definitions = columns
        .iter()
        .map(|column| column_sql(column, true))
        .collect::<Result<Vec<_>, _>>()?;
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
    let mut sql = format!("CREATE TABLE {path} (\n  {}\n)", definitions.join(",\n  "));
    if let Some(partition) = &options.partition_by {
        sql.push_str(&format!("\nPARTITION BY {partition}"));
    }
    if !options.cluster_by.is_empty() {
        sql.push_str(&format!(
            "\nCLUSTER BY {}",
            options
                .cluster_by
                .iter()
                .map(|column| quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(description) = &options.description {
        sql.push_str(&format!(
            "\nOPTIONS(description = {})",
            string_literal(description)
        ));
    }
    Ok(sql)
}

fn column_sql(column: &ColumnDefinition, allow_required: bool) -> Result<String, String> {
    if column.is_auto_increment {
        return Err(format!(
            "BigQuery has no auto-increment columns ('{}'). Use a DEFAULT such as GENERATE_UUID() instead.",
            column.name
        ));
    }
    let data_type = column.data_type.trim();
    if data_type.is_empty() {
        return Err(format!("Column '{}' needs a data type", column.name));
    }
    let mut output = format!("{} {data_type}", quote_identifier(&column.name));
    if let Some(default) = column
        .default_value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        output.push_str(" DEFAULT ");
        output.push_str(default);
    }
    if !column.is_nullable {
        if !allow_required {
            return Err(format!(
                "BigQuery cannot add a NOT NULL (REQUIRED) column to an existing table. Add '{}' as nullable instead.",
                column.name
            ));
        }
        output.push_str(" NOT NULL");
    }
    if let Some(description) = column.comment.as_deref().filter(|value| !value.is_empty()) {
        output.push_str(&format!(
            " OPTIONS(description = {})",
            string_literal(description)
        ));
    }
    Ok(output)
}

pub fn add_column_sql(path: &str, column: &ColumnDefinition) -> Result<Vec<String>, String> {
    let mut statements = vec![format!(
        "ALTER TABLE {path} ADD COLUMN {}",
        column_sql(column, false)?
    )];
    if column.is_pk {
        statements.push(format!(
            "ALTER TABLE {path} ADD PRIMARY KEY ({}) NOT ENFORCED",
            quote_identifier(&column.name)
        ));
    }
    Ok(statements)
}

/// A type name split into its canonical base and its parameters, e.g.
/// `numeric(10, 2)` → `("NUMERIC", [10, 2])`.
fn parse_type(data_type: &str) -> (String, Vec<u32>) {
    let trimmed = data_type.trim();
    match trimmed.find('(') {
        Some(open) if trimmed.ends_with(')') => {
            let parameters = trimmed[open + 1..trimmed.len() - 1]
                .split(',')
                .filter_map(|value| value.trim().parse::<u32>().ok())
                .collect();
            (canonical_type(&trimmed[..open]), parameters)
        }
        _ => (canonical_type(trimmed), Vec::new()),
    }
}

/// Precision and scale of a decimal type; unparameterized types use the
/// BigQuery maximums.
fn decimal_bounds(base: &str, parameters: &[u32]) -> (u32, u32) {
    let (default_precision, default_scale) = if base == "BIGNUMERIC" {
        (76, 38)
    } else {
        (38, 9)
    };
    match parameters {
        [precision, scale] => (*precision, *scale),
        [precision] => (*precision, 0),
        _ => (default_precision, default_scale),
    }
}

/// Column type changes BigQuery accepts in `ALTER COLUMN SET DATA TYPE`.
pub fn check_type_change(old: &str, new: &str) -> Result<(), String> {
    let (old_base, old_parameters) = parse_type(old);
    let (new_base, new_parameters) = parse_type(new);
    let allowed = match (old_base.as_str(), new_base.as_str()) {
        ("INT64", "NUMERIC" | "BIGNUMERIC") => {
            let (precision, scale) = decimal_bounds(&new_base, &new_parameters);
            precision.saturating_sub(scale) >= 19
        }
        ("INT64", "FLOAT64") | ("NUMERIC", "FLOAT64") | ("BIGNUMERIC", "FLOAT64") => true,
        ("NUMERIC", "NUMERIC" | "BIGNUMERIC") | ("BIGNUMERIC", "BIGNUMERIC") => {
            let (old_precision, old_scale) = decimal_bounds(&old_base, &old_parameters);
            let (new_precision, new_scale) = decimal_bounds(&new_base, &new_parameters);
            new_scale >= old_scale
                && new_precision.saturating_sub(new_scale)
                    >= old_precision.saturating_sub(old_scale)
        }
        ("STRING", "STRING") | ("BYTES", "BYTES") => {
            match (old_parameters.first(), new_parameters.first()) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(old_length), Some(new_length)) => new_length >= old_length,
            }
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "BigQuery cannot change a column from {} to {}. Supported changes: INT64 to NUMERIC, BIGNUMERIC or FLOAT64; NUMERIC to BIGNUMERIC or FLOAT64; and widening STRING, BYTES, NUMERIC or BIGNUMERIC parameters. Rewrite the table with CREATE OR REPLACE TABLE ... AS SELECT for other conversions.",
            old.trim(),
            new.trim()
        ))
    }
}

pub fn alter_column_sql(
    path: &str,
    old: &ColumnDefinition,
    new: &ColumnDefinition,
) -> Result<Vec<String>, String> {
    if new.is_auto_increment && !old.is_auto_increment {
        return Err("BigQuery has no auto-increment columns".to_string());
    }
    let mut statements = Vec::new();
    if old.name != new.name {
        statements.push(format!(
            "ALTER TABLE {path} RENAME COLUMN {} TO {}",
            quote_identifier(&old.name),
            quote_identifier(&new.name)
        ));
    }
    let column = quote_identifier(&new.name);
    let same_type = old
        .data_type
        .split_whitespace()
        .collect::<String>()
        .eq_ignore_ascii_case(&new.data_type.split_whitespace().collect::<String>())
        || parse_type(&old.data_type) == parse_type(&new.data_type);
    if !same_type {
        check_type_change(&old.data_type, &new.data_type)?;
        statements.push(format!(
            "ALTER TABLE {path} ALTER COLUMN {column} SET DATA TYPE {}",
            new.data_type.trim()
        ));
    }
    if old.is_nullable != new.is_nullable {
        if !new.is_nullable {
            return Err(format!(
                "BigQuery cannot make an existing column NOT NULL ('{}'). Only relaxing REQUIRED to NULLABLE is supported.",
                new.name
            ));
        }
        statements.push(format!(
            "ALTER TABLE {path} ALTER COLUMN {column} DROP NOT NULL"
        ));
    }
    let normalize = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    if normalize(&old.default_value) != normalize(&new.default_value) {
        let action = normalize(&new.default_value)
            .map(|value| format!("SET DEFAULT {value}"))
            .unwrap_or_else(|| "DROP DEFAULT".to_string());
        statements.push(format!("ALTER TABLE {path} ALTER COLUMN {column} {action}"));
    }
    if let Some(comment) = &new.comment {
        if old.comment.as_ref() != Some(comment) {
            let value = if comment.is_empty() {
                "NULL".to_string()
            } else {
                string_literal(comment)
            };
            statements.push(format!(
                "ALTER TABLE {path} ALTER COLUMN {column} SET OPTIONS(description = {value})"
            ));
        }
    }
    if old.is_pk != new.is_pk {
        if new.is_pk {
            statements.push(format!(
                "ALTER TABLE {path} ADD PRIMARY KEY ({column}) NOT ENFORCED"
            ));
        } else {
            statements.push(format!("ALTER TABLE {path} DROP PRIMARY KEY IF EXISTS"));
        }
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
        params
            .get("table")
            .or_else(|| params.get("table_name"))?
            .as_str()?
            .trim(),
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
    use serde_json::json;

    use super::*;

    fn column(name: &str, data_type: &str, is_pk: bool) -> ColumnDefinition {
        ColumnDefinition {
            name: name.into(),
            data_type: data_type.into(),
            is_nullable: !is_pk,
            is_pk,
            is_auto_increment: false,
            default_value: None,
            comment: None,
        }
    }

    const PATH: &str = "`my-proj`.`d`.`t`";

    #[test]
    fn creates_tables_with_keys_partitioning_and_options() {
        let mut payload = column("payload", "JSON", false);
        payload.comment = Some("Raw 'event'\nbody".into());
        let mut created = column("created_at", "TIMESTAMP", false);
        created.default_value = Some("CURRENT_TIMESTAMP()".into());
        let options = TableOptions::from_params(&json!({
            "partition_by": "DATE(created_at)",
            "cluster_by": "id, payload",
            "description": "Events"
        }));
        let sql = create_table_sql(
            PATH,
            &[column("id", "INT64", true), created, payload],
            &options,
        )
        .unwrap();
        assert_eq!(
            sql,
            "CREATE TABLE `my-proj`.`d`.`t` (\n  \
             `id` INT64 NOT NULL,\n  \
             `created_at` TIMESTAMP DEFAULT CURRENT_TIMESTAMP(),\n  \
             `payload` JSON OPTIONS(description = 'Raw \\'event\\'\\nbody'),\n  \
             PRIMARY KEY (`id`) NOT ENFORCED\n)\n\
             PARTITION BY DATE(created_at)\n\
             CLUSTER BY `id`, `payload`\n\
             OPTIONS(description = 'Events')"
        );
        let mut serial = column("id", "INT64", true);
        serial.is_auto_increment = true;
        assert!(create_table_sql(PATH, &[serial], &TableOptions::default()).is_err());
    }

    #[test]
    fn add_column_refuses_required_and_adds_primary_key() {
        let required = column("id", "INT64", true);
        assert!(add_column_sql(PATH, &required)
            .unwrap_err()
            .contains("NOT NULL"));
        let mut key = column("id", "INT64", false);
        key.is_pk = true;
        assert_eq!(
            add_column_sql(PATH, &key).unwrap(),
            vec![
                "ALTER TABLE `my-proj`.`d`.`t` ADD COLUMN `id` INT64".to_string(),
                "ALTER TABLE `my-proj`.`d`.`t` ADD PRIMARY KEY (`id`) NOT ENFORCED".to_string(),
            ]
        );
    }

    #[test]
    fn alters_only_supported_column_changes() {
        let old = column("value", "INT64", false);
        let mut new = column("amount", "NUMERIC", false);
        new.comment = Some("Net".into());
        assert_eq!(
            alter_column_sql(PATH, &old, &new).unwrap(),
            vec![
                "ALTER TABLE `my-proj`.`d`.`t` RENAME COLUMN `value` TO `amount`",
                "ALTER TABLE `my-proj`.`d`.`t` ALTER COLUMN `amount` SET DATA TYPE NUMERIC",
                "ALTER TABLE `my-proj`.`d`.`t` ALTER COLUMN `amount` SET OPTIONS(description = 'Net')",
            ]
        );
        let mut required = column("value", "INT64", false);
        required.is_nullable = false;
        assert_eq!(
            alter_column_sql(PATH, &required, &column("value", "INT64", false)).unwrap(),
            vec!["ALTER TABLE `my-proj`.`d`.`t` ALTER COLUMN `value` DROP NOT NULL"]
        );
        assert!(
            alter_column_sql(PATH, &column("value", "INT64", false), &required)
                .unwrap_err()
                .contains("NOT NULL")
        );
        assert!(
            alter_column_sql(PATH, &old, &column("value", "STRING", false))
                .unwrap_err()
                .contains("cannot change")
        );
        assert!(alter_column_sql(PATH, &old, &old.clone()).is_err());
        let mut keyed = old.clone();
        keyed.is_pk = true;
        assert_eq!(
            alter_column_sql(PATH, &old, &keyed).unwrap(),
            vec!["ALTER TABLE `my-proj`.`d`.`t` ADD PRIMARY KEY (`value`) NOT ENFORCED"]
        );
        assert_eq!(
            alter_column_sql(PATH, &keyed, &old).unwrap(),
            vec!["ALTER TABLE `my-proj`.`d`.`t` DROP PRIMARY KEY IF EXISTS"]
        );
        let mut defaulted = old.clone();
        defaulted.default_value = Some("0".into());
        assert_eq!(
            alter_column_sql(PATH, &old, &defaulted).unwrap(),
            vec!["ALTER TABLE `my-proj`.`d`.`t` ALTER COLUMN `value` SET DEFAULT 0"]
        );
    }

    #[test]
    fn validates_type_widenings() {
        assert!(check_type_change("INT64", "FLOAT64").is_ok());
        assert!(check_type_change("INTEGER", "BIGNUMERIC").is_ok());
        assert!(check_type_change("NUMERIC", "BIGNUMERIC").is_ok());
        assert!(check_type_change("NUMERIC(10, 2)", "NUMERIC(12, 4)").is_ok());
        assert!(check_type_change("NUMERIC(10, 2)", "NUMERIC(10, 4)").is_err());
        assert!(check_type_change("STRING(10)", "STRING(20)").is_ok());
        assert!(check_type_change("STRING(10)", "STRING").is_ok());
        assert!(check_type_change("STRING", "STRING(10)").is_err());
        assert!(check_type_change("FLOAT64", "INT64").is_err());
        assert!(check_type_change("BIGNUMERIC", "NUMERIC").is_err());
        assert!(check_type_change("INT64", "NUMERIC(5, 0)").is_err());
    }

    #[test]
    fn foreign_keys_are_not_enforced_and_reject_actions() {
        assert_eq!(
            foreign_key_sql(PATH, "`my-proj`.`d`.`r`", "fk", "a, b", "x, y", &[Some("NO ACTION"), None])
                .unwrap(),
            "ALTER TABLE `my-proj`.`d`.`t` ADD CONSTRAINT `fk` FOREIGN KEY (`a`, `b`) REFERENCES `my-proj`.`d`.`r`(`x`, `y`) NOT ENFORCED"
        );
        assert!(foreign_key_sql(PATH, "r", "fk", "a", "x", &[Some("CASCADE")]).is_err());
        assert!(foreign_key_sql(PATH, "r", "fk", "a, b", "x", &[]).is_err());
    }

    #[test]
    fn uses_project_only_when_connection_params_are_sent() {
        assert_eq!(optional_project(&json!({"schema": "d"})), None);
        assert_eq!(
            optional_project(&json!({"params": {"database": "my-proj"}})),
            Some("my-proj".to_string())
        );
    }
}
