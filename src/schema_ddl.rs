//! GoogleSQL DDL rebuilt from BigQuery REST table resources.
//!
//! Used when `INFORMATION_SCHEMA.TABLES.ddl` is unavailable and to preserve
//! table options when a view is replaced.

use serde_json::Value;

use crate::bigquery::FieldSchema;
use crate::client::table_fields;
use crate::sql::{
    base_type, field_type_sql, fully_qualified, is_required, quote_identifier, string_literal,
};

/// BigQuery object kinds, as reported by `tables.list` / `tables.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Table,
    View,
    MaterializedView,
    External,
    Snapshot,
}

impl TableKind {
    pub fn from_resource(resource: &Value) -> Self {
        Self::from_type(
            resource
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("TABLE"),
        )
    }

    pub fn from_type(kind: &str) -> Self {
        match kind {
            "VIEW" => Self::View,
            "MATERIALIZED_VIEW" => Self::MaterializedView,
            "EXTERNAL" => Self::External,
            "SNAPSHOT" => Self::Snapshot,
            _ => Self::Table,
        }
    }

    /// Object keyword for `DROP <keyword>` and `CREATE <keyword>`.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Table => "TABLE",
            Self::View => "VIEW",
            Self::MaterializedView => "MATERIALIZED VIEW",
            Self::External => "EXTERNAL TABLE",
            Self::Snapshot => "SNAPSHOT TABLE",
        }
    }

    pub fn is_table_like(self) -> bool {
        matches!(self, Self::Table | Self::External | Self::Snapshot)
    }
}

pub fn drop_sql(kind: TableKind, path: &str) -> String {
    format!("DROP {} IF EXISTS {path}", kind.keyword())
}

/// Rejects DML on objects BigQuery cannot modify, with an actionable message.
pub fn ensure_dml_allowed(resource: &Value) -> Result<(), String> {
    match TableKind::from_resource(resource) {
        TableKind::Table => Ok(()),
        TableKind::Snapshot => Err(
            "Table snapshots are read-only in BigQuery. Restore the snapshot into a table with CREATE TABLE ... CLONE to edit rows."
                .to_string(),
        ),
        TableKind::External => Err(
            "External tables are read-only: BigQuery DML does not modify data stored outside BigQuery."
                .to_string(),
        ),
        TableKind::View | TableKind::MaterializedView => {
            Err("Views and materialized views cannot be edited with DML.".to_string())
        }
    }
}

/// Rejects schema changes on objects whose columns cannot be altered.
pub fn ensure_schema_editable(resource: &Value) -> Result<(), String> {
    match TableKind::from_resource(resource) {
        TableKind::Table => Ok(()),
        TableKind::Snapshot => {
            Err("Table snapshots are read-only: their schema cannot be altered.".to_string())
        }
        TableKind::External => Err(
            "External table columns cannot be altered with ALTER TABLE; recreate the table with CREATE OR REPLACE EXTERNAL TABLE."
                .to_string(),
        ),
        TableKind::View | TableKind::MaterializedView => {
            Err("Edit the view definition instead of altering its columns.".to_string())
        }
    }
}

pub fn primary_key_columns(resource: &Value) -> Vec<String> {
    resource
        .pointer("/tableConstraints/primaryKey/columns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn is_legacy_sql_view(resource: &Value) -> bool {
    resource
        .pointer("/view/useLegacySql")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `OPTIONS(...)` with a leading space, or an empty string.
pub fn options_clause(options: &[(String, String)]) -> String {
    if options.is_empty() {
        return String::new();
    }
    let body = options
        .iter()
        .map(|(key, value)| format!("{key} = {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" OPTIONS({body})")
}

/// Description, friendly name and labels shared by tables and views.
pub fn common_options(resource: &Value) -> Vec<(String, String)> {
    let mut options = Vec::new();
    if let Some(name) = resource.get("friendlyName").and_then(Value::as_str) {
        options.push(("friendly_name".to_string(), string_literal(name)));
    }
    if let Some(description) = resource.get("description").and_then(Value::as_str) {
        options.push(("description".to_string(), string_literal(description)));
    }
    if let Some(labels) = resource.get("labels").and_then(Value::as_object) {
        if !labels.is_empty() {
            let mut pairs = labels
                .iter()
                .map(|(key, value)| {
                    format!(
                        "({}, {})",
                        string_literal(key),
                        string_literal(value.as_str().unwrap_or_default())
                    )
                })
                .collect::<Vec<_>>();
            pairs.sort();
            options.push(("labels".to_string(), format!("[{}]", pairs.join(", "))));
        }
    }
    options
}

fn partition_options(resource: &Value) -> Vec<(String, String)> {
    let mut options = Vec::new();
    if let Some(ms) = resource
        .pointer("/timePartitioning/expirationMs")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
    {
        let days = ms / 86_400_000.0;
        let rendered = if days.fract() == 0.0 {
            format!("{days:.0}")
        } else {
            format!("{days}")
        };
        options.push(("partition_expiration_days".to_string(), rendered));
    }
    let require = resource
        .get("requirePartitionFilter")
        .or_else(|| resource.pointer("/timePartitioning/requirePartitionFilter"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if require {
        options.push(("require_partition_filter".to_string(), "TRUE".to_string()));
    }
    options
}

/// ` PARTITION BY ...` with a leading newline, or an empty string.
pub fn partition_clause(resource: &Value, fields: &[FieldSchema]) -> String {
    if let Some(partitioning) = resource.get("timePartitioning") {
        let granularity = partitioning
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("DAY");
        let expression = match partitioning.get("field").and_then(Value::as_str) {
            Some(column) => {
                let column_type = fields
                    .iter()
                    .find(|field| field.name == column)
                    .map(base_type)
                    .unwrap_or_else(|| "TIMESTAMP".to_string());
                let quoted = quote_identifier(column);
                match (column_type.as_str(), granularity) {
                    ("DATE", "DAY") => quoted,
                    ("DATE", _) => format!("DATE_TRUNC({quoted}, {granularity})"),
                    ("DATETIME", _) => format!("DATETIME_TRUNC({quoted}, {granularity})"),
                    (_, "DAY") => format!("DATE({quoted})"),
                    _ => format!("TIMESTAMP_TRUNC({quoted}, {granularity})"),
                }
            }
            None if granularity == "DAY" => "_PARTITIONDATE".to_string(),
            None => format!("TIMESTAMP_TRUNC(_PARTITIONTIME, {granularity})"),
        };
        return format!("\nPARTITION BY {expression}");
    }
    if let Some(range) = resource.get("rangePartitioning") {
        let column = range
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let bound = |key: &str| {
            range
                .pointer(&format!("/range/{key}"))
                .and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| value.as_i64().map(|v| v.to_string()))
                })
                .unwrap_or_else(|| "0".to_string())
        };
        return format!(
            "\nPARTITION BY RANGE_BUCKET({}, GENERATE_ARRAY({}, {}, {}))",
            quote_identifier(column),
            bound("start"),
            bound("end"),
            bound("interval")
        );
    }
    String::new()
}

pub fn cluster_clause(resource: &Value) -> String {
    let columns = resource
        .pointer("/clustering/fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(Value::as_str)
                .map(quote_identifier)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if columns.is_empty() {
        String::new()
    } else {
        format!("\nCLUSTER BY {}", columns.join(", "))
    }
}

/// One column definition for CREATE TABLE / ADD COLUMN.
pub fn column_definition(field: &FieldSchema) -> String {
    let mut column = format!(
        "{} {}",
        quote_identifier(&field.name),
        field_type_sql(field, true)
    );
    // GoogleSQL column_schema: name type [NOT NULL] [DEFAULT expr] [OPTIONS(...)]
    if is_required(field) {
        column.push_str(" NOT NULL");
    }
    if let Some(default) = field
        .default_value_expression
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        column.push_str(" DEFAULT ");
        column.push_str(default.trim());
    }
    if let Some(description) = field.description.as_deref() {
        column.push_str(&format!(
            " OPTIONS(description = {})",
            string_literal(description)
        ));
    }
    column
}

fn constraint_lines(resource: &Value, project: &str, dataset: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let primary_key = primary_key_columns(resource);
    if !primary_key.is_empty() {
        lines.push(format!(
            "PRIMARY KEY ({}) NOT ENFORCED",
            primary_key
                .iter()
                .map(|column| quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for foreign_key in resource
        .pointer("/tableConstraints/foreignKeys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let references = foreign_key
            .get("columnReferences")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let column = |key: &str| {
            references
                .iter()
                .filter_map(|reference| reference.get(key).and_then(Value::as_str))
                .map(quote_identifier)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let referenced = |key: &str, fallback: &str| {
            foreign_key
                .pointer(&format!("/referencedTable/{key}"))
                .and_then(Value::as_str)
                .unwrap_or(fallback)
                .to_string()
        };
        let target = fully_qualified(
            &referenced("projectId", project),
            &referenced("datasetId", dataset),
            &referenced("tableId", ""),
        );
        let prefix = foreign_key
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| format!("CONSTRAINT {} ", quote_identifier(name)))
            .unwrap_or_default();
        lines.push(format!(
            "{prefix}FOREIGN KEY ({}) REFERENCES {target}({}) NOT ENFORCED",
            column("referencingColumn"),
            column("referencedColumn")
        ));
    }
    lines
}

fn column_block(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!(" (\n  {}\n)", lines.join(",\n  "))
    }
}

/// Rebuilds the CREATE statement of any table-like object from its resource.
pub fn table_ddl(
    resource: &Value,
    project: &str,
    dataset: &str,
    name: &str,
) -> Result<String, String> {
    let path = fully_qualified(project, dataset, name);
    let kind = TableKind::from_resource(resource);
    match kind {
        TableKind::View => view_ddl(resource, &path, None, false),
        TableKind::MaterializedView => materialized_view_ddl(resource, &path),
        TableKind::Snapshot => {
            let base = |key: &str| {
                resource
                    .pointer(&format!("/snapshotDefinition/baseTableReference/{key}"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let mut sql = format!(
                "CREATE SNAPSHOT TABLE {path}\nCLONE {}",
                fully_qualified(&base("projectId"), &base("datasetId"), &base("tableId"))
            );
            if let Some(time) = resource
                .pointer("/snapshotDefinition/snapshotTime")
                .and_then(Value::as_str)
            {
                sql.push_str(&format!(
                    "\nFOR SYSTEM_TIME AS OF TIMESTAMP {}",
                    string_literal(time)
                ));
            }
            sql.push_str(&options_clause(&common_options(resource)));
            Ok(sql)
        }
        TableKind::External => {
            let fields = table_fields(resource)?;
            let columns = fields.iter().map(column_definition).collect::<Vec<_>>();
            let mut options = Vec::new();
            if let Some(format) = resource
                .pointer("/externalDataConfiguration/sourceFormat")
                .and_then(Value::as_str)
            {
                let format = match format {
                    "NEWLINE_DELIMITED_JSON" => "JSON",
                    "GOOGLE_SHEETS" => "GOOGLE_SHEETS",
                    other => other,
                };
                options.push(("format".to_string(), string_literal(format)));
            }
            if let Some(uris) = resource
                .pointer("/externalDataConfiguration/sourceUris")
                .and_then(Value::as_array)
            {
                let uris = uris
                    .iter()
                    .filter_map(Value::as_str)
                    .map(string_literal)
                    .collect::<Vec<_>>();
                options.push(("uris".to_string(), format!("[{}]", uris.join(", "))));
            }
            options.extend(common_options(resource));
            let connection = resource
                .pointer("/externalDataConfiguration/connectionId")
                .and_then(Value::as_str)
                .map(|connection| format!("\nWITH CONNECTION {}", connection_path(connection)))
                .unwrap_or_default();
            Ok(format!(
                "CREATE EXTERNAL TABLE {path}{}{connection}{}",
                column_block(&columns),
                options_clause(&options)
            ))
        }
        TableKind::Table => {
            let fields = table_fields(resource)?;
            let mut lines = fields.iter().map(column_definition).collect::<Vec<_>>();
            lines.extend(constraint_lines(resource, project, dataset));
            let mut options = partition_options(resource);
            options.extend(common_options(resource));
            let collation = resource
                .get("defaultCollation")
                .and_then(Value::as_str)
                .map(|collation| format!("\nDEFAULT COLLATE {}", string_literal(collation)))
                .unwrap_or_default();
            let clone = resource
                .pointer("/cloneDefinition/baseTableReference")
                .map(|base| {
                    let part =
                        |key: &str| base.get(key).and_then(Value::as_str).unwrap_or_default();
                    format!(
                        "\n-- Cloned from {}",
                        fully_qualified(part("projectId"), part("datasetId"), part("tableId"))
                    )
                })
                .unwrap_or_default();
            Ok(format!(
                "CREATE TABLE {path}{}{collation}{}{}{}{clone}",
                column_block(&lines),
                partition_clause(resource, &fields),
                cluster_clause(resource),
                options_clause(&options)
            ))
        }
    }
}

/// `projects/p/locations/l/connections/c` or `p.l.c` → `` `p.l.c` ``.
fn connection_path(connection: &str) -> String {
    let parts = connection.split('/').collect::<Vec<_>>();
    if parts.len() == 6 && parts[0] == "projects" {
        quote_identifier(&format!("{}.{}.{}", parts[1], parts[3], parts[5]))
    } else {
        quote_identifier(connection)
    }
}

/// CREATE [OR REPLACE] VIEW with the resource options. `query` replaces the
/// stored query when an edited definition is saved.
pub fn view_ddl(
    resource: &Value,
    path: &str,
    query: Option<&str>,
    replace: bool,
) -> Result<String, String> {
    let stored = resource.pointer("/view/query").and_then(Value::as_str);
    let query = query
        .or(stored)
        .ok_or_else(|| "The view definition is unavailable".to_string())?;
    let prefix = if replace {
        "CREATE OR REPLACE VIEW"
    } else {
        "CREATE VIEW"
    };
    let mut sql = format!(
        "{prefix} {path}{}\nAS\n{}",
        options_clause(&common_options(resource)),
        query.trim()
    );
    if is_legacy_sql_view(resource) && query == stored.unwrap_or_default() {
        sql = format!(
            "-- Legacy SQL view: BigQuery stores this query as legacy SQL.\n-- Tabularis can display it but cannot replace it as a GoogleSQL view.\n{sql}"
        );
    }
    Ok(sql)
}

pub fn materialized_view_ddl(resource: &Value, path: &str) -> Result<String, String> {
    let query = resource
        .pointer("/materializedView/query")
        .and_then(Value::as_str)
        .ok_or_else(|| "The materialized view definition is unavailable".to_string())?;
    let fields = table_fields(resource).unwrap_or_default();
    let mut options = Vec::new();
    if let Some(enabled) = resource
        .pointer("/materializedView/enableRefresh")
        .and_then(Value::as_bool)
    {
        options.push((
            "enable_refresh".to_string(),
            if enabled { "TRUE" } else { "FALSE" }.to_string(),
        ));
    }
    if let Some(ms) = resource
        .pointer("/materializedView/refreshIntervalMs")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok())
    {
        options.push((
            "refresh_interval_minutes".to_string(),
            (ms / 60_000).to_string(),
        ));
    }
    if let Some(staleness) = resource
        .pointer("/materializedView/maxStaleness")
        .and_then(Value::as_str)
    {
        options.push((
            "max_staleness".to_string(),
            format!("INTERVAL {}", string_literal(staleness)),
        ));
    }
    if resource
        .pointer("/materializedView/allowNonIncrementalDefinition")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        options.push((
            "allow_non_incremental_definition".to_string(),
            "TRUE".to_string(),
        ));
    }
    options.extend(partition_options(resource));
    options.extend(common_options(resource));
    Ok(format!(
        "CREATE MATERIALIZED VIEW {path}{}{}{}\nAS\n{}",
        partition_clause(resource, &fields),
        cluster_clause(resource),
        options_clause(&options),
        query.trim()
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn table_resource() -> Value {
        json!({
            "type": "TABLE",
            "description": "Daily \"orders\"\nwith notes",
            "labels": {"team": "data", "env": "prod"},
            "schema": {"fields": [
                {"name": "id", "type": "INTEGER", "mode": "REQUIRED"},
                {"name": "created_at", "type": "TIMESTAMP"},
                {"name": "amount", "type": "NUMERIC", "precision": "12", "scale": "2", "description": "O'Brien amount"},
                {"name": "status", "type": "STRING", "maxLength": "10", "defaultValueExpression": "'new'"},
                {"name": "items", "type": "RECORD", "mode": "REPEATED", "fields": [
                    {"name": "sku", "type": "STRING", "mode": "REQUIRED"}
                ]},
                {"name": "valid", "type": "RANGE", "rangeElementType": {"type": "DATE"}},
                {"name": "customer_id", "type": "INT64"}
            ]},
            "timePartitioning": {"type": "DAY", "field": "created_at", "expirationMs": "7776000000"},
            "requirePartitionFilter": true,
            "clustering": {"fields": ["status", "customer_id"]},
            "tableConstraints": {
                "primaryKey": {"columns": ["id"]},
                "foreignKeys": [{
                    "name": "fk_customer",
                    "referencedTable": {"projectId": "my-proj", "datasetId": "crm", "tableId": "customers"},
                    "columnReferences": [{"referencingColumn": "customer_id", "referencedColumn": "id"}]
                }]
            }
        })
    }

    #[test]
    fn column_definition_puts_not_null_before_default() {
        let field: FieldSchema = serde_json::from_value(json!({
            "name": "status",
            "type": "STRING",
            "mode": "REQUIRED",
            "defaultValueExpression": "'new'",
            "description": "State"
        }))
        .unwrap();
        assert_eq!(
            column_definition(&field),
            "`status` STRING NOT NULL DEFAULT 'new' OPTIONS(description = 'State')"
        );
    }

    #[test]
    fn rebuilds_partitioned_clustered_table_ddl() {
        let ddl = table_ddl(&table_resource(), "my-proj", "sales", "orders").unwrap();
        assert_eq!(
            ddl,
            "CREATE TABLE `my-proj`.`sales`.`orders` (\n  \
             `id` INT64 NOT NULL,\n  \
             `created_at` TIMESTAMP,\n  \
             `amount` NUMERIC(12, 2) OPTIONS(description = 'O\\'Brien amount'),\n  \
             `status` STRING(10) DEFAULT 'new',\n  \
             `items` ARRAY<STRUCT<`sku` STRING NOT NULL>>,\n  \
             `valid` RANGE<DATE>,\n  \
             `customer_id` INT64,\n  \
             PRIMARY KEY (`id`) NOT ENFORCED,\n  \
             CONSTRAINT `fk_customer` FOREIGN KEY (`customer_id`) REFERENCES `my-proj`.`crm`.`customers`(`id`) NOT ENFORCED\n)\n\
             PARTITION BY DATE(`created_at`)\n\
             CLUSTER BY `status`, `customer_id` \
             OPTIONS(partition_expiration_days = 90, require_partition_filter = TRUE, description = 'Daily \"orders\"\\nwith notes', labels = [('env', 'prod'), ('team', 'data')])"
        );
    }

    #[test]
    fn rebuilds_ingestion_and_range_partitioning() {
        let ingestion = json!({"timePartitioning": {"type": "HOUR"}});
        assert_eq!(
            partition_clause(&ingestion, &[]),
            "\nPARTITION BY TIMESTAMP_TRUNC(_PARTITIONTIME, HOUR)"
        );
        let daily = json!({"timePartitioning": {"type": "DAY"}});
        assert_eq!(
            partition_clause(&daily, &[]),
            "\nPARTITION BY _PARTITIONDATE"
        );
        let range = json!({"rangePartitioning": {"field": "bucket", "range": {"start": "0", "end": "100", "interval": "10"}}});
        assert_eq!(
            partition_clause(&range, &[]),
            "\nPARTITION BY RANGE_BUCKET(`bucket`, GENERATE_ARRAY(0, 100, 10))"
        );
    }

    #[test]
    fn rebuilds_views_snapshots_and_external_tables() {
        let view = json!({"type": "VIEW", "description": "v", "view": {"query": "SELECT 1", "useLegacySql": false}});
        assert_eq!(
            table_ddl(&view, "p-1", "d", "v").unwrap(),
            "CREATE VIEW `p-1`.`d`.`v` OPTIONS(description = 'v')\nAS\nSELECT 1"
        );
        let legacy = json!({"type": "VIEW", "view": {"query": "SELECT 1 FROM [p:d.t]", "useLegacySql": true}});
        assert!(table_ddl(&legacy, "p", "d", "v")
            .unwrap()
            .starts_with("-- Legacy SQL view"));
        let snapshot = json!({
            "type": "SNAPSHOT",
            "snapshotDefinition": {
                "baseTableReference": {"projectId": "p-1", "datasetId": "d", "tableId": "t"},
                "snapshotTime": "2024-01-01T00:00:00Z"
            }
        });
        assert_eq!(
            table_ddl(&snapshot, "p-1", "d", "s").unwrap(),
            "CREATE SNAPSHOT TABLE `p-1`.`d`.`s`\nCLONE `p-1`.`d`.`t`\nFOR SYSTEM_TIME AS OF TIMESTAMP '2024-01-01T00:00:00Z'"
        );
        let external = json!({
            "type": "EXTERNAL",
            "schema": {"fields": [{"name": "a", "type": "STRING"}]},
            "externalDataConfiguration": {"sourceFormat": "CSV", "sourceUris": ["gs://b/x.csv"]}
        });
        assert_eq!(
            table_ddl(&external, "p", "d", "e").unwrap(),
            "CREATE EXTERNAL TABLE `p`.`d`.`e` (\n  `a` STRING\n) OPTIONS(format = 'CSV', uris = ['gs://b/x.csv'])"
        );
    }

    #[test]
    fn rebuilds_materialized_view_with_refresh_options() {
        let mv = json!({
            "type": "MATERIALIZED_VIEW",
            "schema": {"fields": [{"name": "day", "type": "DATE"}]},
            "timePartitioning": {"type": "DAY", "field": "day"},
            "clustering": {"fields": ["day"]},
            "materializedView": {"query": "SELECT day FROM t", "enableRefresh": true, "refreshIntervalMs": "1800000"}
        });
        assert_eq!(
            table_ddl(&mv, "p", "d", "mv").unwrap(),
            "CREATE MATERIALIZED VIEW `p`.`d`.`mv`\nPARTITION BY `day`\nCLUSTER BY `day` OPTIONS(enable_refresh = TRUE, refresh_interval_minutes = 30)\nAS\nSELECT day FROM t"
        );
    }

    #[test]
    fn maps_kinds_to_drop_keywords_and_dml_rules() {
        assert_eq!(
            drop_sql(TableKind::from_type("SNAPSHOT"), "`p`.`d`.`s`"),
            "DROP SNAPSHOT TABLE IF EXISTS `p`.`d`.`s`"
        );
        assert_eq!(TableKind::from_type("EXTERNAL").keyword(), "EXTERNAL TABLE");
        assert_eq!(
            TableKind::from_type("MATERIALIZED_VIEW").keyword(),
            "MATERIALIZED VIEW"
        );
        assert!(ensure_dml_allowed(&json!({"type": "TABLE"})).is_ok());
        assert!(ensure_dml_allowed(&json!({"type": "SNAPSHOT"}))
            .unwrap_err()
            .contains("read-only"));
        assert!(ensure_dml_allowed(&json!({"type": "EXTERNAL"})).is_err());
        assert!(ensure_schema_editable(&json!({"type": "SNAPSHOT"})).is_err());
    }
}
