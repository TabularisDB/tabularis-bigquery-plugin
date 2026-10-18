use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::catalog::{self, FETCH_CONCURRENCY};
use crate::client::{self, BigQueryClient, Statement};
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::schema_ddl::{self, TableKind};
use crate::sql::{
    field_type_sql, fully_qualified, information_schema, string_literal, QueryParams,
};

/// Above this many objects `get_tables` skips the per-table description
/// lookup, so expanding very large datasets stays fast.
pub const COMMENT_LOOKUP_LIMIT: usize = 100;
const COMMENT_LOOKUP_CONCURRENCY: usize = 16;

const TABLE_TYPES: &[&str] = &["TABLE", "EXTERNAL", "SNAPSHOT", "CLONE"];

/// A connected client plus the project the host is browsing. The client
/// always runs jobs in the billing project; metadata calls use `project`.
pub struct Target {
    pub client: Arc<BigQueryClient>,
    pub project: String,
}

impl Target {
    pub async fn from_params(params: &Value) -> Result<Self, String> {
        let connection = ConnectionParams::from_value(inner_params(params));
        let project = connection.data_project_id()?.to_string();
        let client = client::client_for(&connection).await?;
        Ok(Self { client, project })
    }

    /// Location of a dataset, so jobs touching it run in its region even
    /// when the connection location differs.
    pub async fn location(&self, dataset: &str) -> Option<String> {
        catalog::dataset_location(&self.client, &self.project, dataset).await
    }

    pub fn path(&self, dataset: &str, object: &str) -> String {
        fully_qualified(&self.project, dataset, object)
    }

    /// Runs a DDL statement and clears the metadata cache.
    pub async fn run_ddl(&self, sql: &str, dataset: &str) -> Result<(), String> {
        let statement = Statement::new(sql)
            .in_dataset(&self.project, dataset)
            .at_location(self.location(dataset).await);
        let result = self.client.run_statement(&statement).await;
        catalog::invalidate();
        result.map(|_| ())
    }
}

pub async fn get_databases(id: Value, params: &Value) -> Value {
    respond(id, async {
        let connection = ConnectionParams::from_value(inner_params(params));
        let configured = connection.data_project_id()?.to_string();
        let client = client::client_for(&connection).await?;
        let listed = match client.list_projects().await {
            Ok(projects) => projects,
            Err(error) => {
                eprintln!(
                    "[bigquery] projects.list failed, using configured projects only: {error}"
                );
                Vec::new()
            }
        };
        Ok(json!(merge_projects(
            &configured,
            &client.project_id,
            listed,
            connection.additional_projects()
        )))
    })
    .await
}

/// Visible projects plus the configured, billing and additional projects,
/// de-duplicated and sorted.
pub fn merge_projects(
    configured: &str,
    billing: &str,
    listed: Vec<String>,
    additional: Vec<String>,
) -> Vec<String> {
    let mut projects = listed;
    projects.push(configured.to_string());
    projects.push(billing.to_string());
    projects.extend(additional);
    let mut seen = HashSet::new();
    projects.retain(|project| !project.is_empty() && seen.insert(project.clone()));
    projects.sort();
    projects
}

pub async fn get_schemas(id: Value, params: &Value) -> Value {
    respond(id, async {
        let target = Target::from_params(params).await?;
        Ok(json!(
            target.client.list_datasets_in(&target.project).await?
        ))
    })
    .await
}

pub async fn get_tables(id: Value, params: &Value) -> Value {
    list_objects(id, params, TABLE_TYPES).await
}

pub async fn get_views(id: Value, params: &Value) -> Value {
    list_objects(id, params, &["VIEW"]).await
}

pub async fn get_materialized_views(id: Value, params: &Value) -> Value {
    list_objects(id, params, &["MATERIALIZED_VIEW"]).await
}

fn object_names(tables: &[Value], allowed_types: &[&str]) -> Vec<String> {
    let mut names = tables
        .iter()
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
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

async fn list_objects(id: Value, params: &Value, allowed_types: &[&str]) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let target = Target::from_params(params).await?;
        let tables = catalog::tables(&target.client, &target.project, schema).await?;
        let names = object_names(&tables, allowed_types);
        // tables.list does not return descriptions; fetch them for
        // reasonably sized datasets (resources are cached for later calls).
        let resources = if names.len() <= COMMENT_LOOKUP_LIMIT {
            catalog::tables_batch(
                target.client.clone(),
                &target.project,
                schema,
                names.clone(),
                COMMENT_LOOKUP_CONCURRENCY,
            )
            .await
        } else {
            Vec::new()
        };
        let objects = names
            .iter()
            .map(|name| {
                let comment = resources
                    .iter()
                    .find(|(resource_name, _)| resource_name == name)
                    .and_then(|(_, resource)| resource.as_ref().ok())
                    .and_then(|resource| resource.get("description"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"name": name, "schema": schema, "comment": comment})
            })
            .collect::<Vec<_>>();
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
        let schema = required_schema(params)?;
        let object = required_any(params, keys)?;
        let target = Target::from_params(params).await?;
        let resource = catalog::table(&target.client, &target.project, schema, object).await?;
        Ok(json!(columns_from_resource(&resource)?))
    })
    .await
}

/// Host `TableColumn` rows from a `tables.get` resource.
pub fn columns_from_resource(resource: &Value) -> Result<Vec<Value>, String> {
    let fields = client::table_fields(resource)?;
    let primary_keys = schema_ddl::primary_key_columns(resource);
    let parse =
        |value: &Option<String>| value.as_deref().and_then(|value| value.parse::<i64>().ok());
    Ok(fields
        .iter()
        .map(|field| {
            json!({
                "name": field.name,
                "data_type": field_type_sql(field, false),
                "is_nullable": field.mode.as_deref() != Some("REQUIRED"),
                "default_value": field.default_value_expression,
                "is_pk": primary_keys.iter().any(|column| column == &field.name),
                "is_auto_increment": false,
                "is_generated": false,
                "character_maximum_length": parse(&field.max_length),
                "numeric_precision": parse(&field.precision),
                "numeric_scale": parse(&field.scale),
                "comment": field.description
            })
        })
        .collect())
}

pub async fn get_foreign_keys(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let table = required_any(params, &["table", "table_name"])?;
        let target = Target::from_params(params).await?;
        let resource = catalog::table(&target.client, &target.project, schema, table).await?;
        Ok(json!(foreign_keys_from_resource(&resource, schema)))
    })
    .await
}

/// Host `ForeignKey` rows (one per column, ordered by `seq_in_fk`).
pub fn foreign_keys_from_resource(resource: &Value, schema: &str) -> Vec<Value> {
    let mut output = Vec::new();
    for foreign_key in resource
        .pointer("/tableConstraints/foreignKeys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let referenced_table = foreign_key
            .pointer("/referencedTable/tableId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = foreign_key
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("fk_{referenced_table}"));
        let referenced_schema = foreign_key
            .pointer("/referencedTable/datasetId")
            .and_then(Value::as_str)
            .unwrap_or(schema);
        let referenced_project = foreign_key
            .pointer("/referencedTable/projectId")
            .and_then(Value::as_str);
        for (index, reference) in foreign_key
            .get("columnReferences")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let column = reference
                .get("referencingColumn")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let referenced_column = reference
                .get("referencedColumn")
                .and_then(Value::as_str)
                .unwrap_or_default();
            output.push(json!({
                "name": name,
                "constraint_name": name,
                "column_name": column,
                "ref_table": referenced_table,
                "referenced_table": referenced_table,
                "ref_column": referenced_column,
                "referenced_column": referenced_column,
                "foreign_schema_name": referenced_schema,
                "foreign_project": referenced_project,
                "on_update": "NO ACTION",
                "on_delete": "NO ACTION",
                "seq_in_fk": index + 1
            }));
        }
    }
    output
}

pub async fn get_indexes(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let table = required_any(params, &["table", "table_name"])?;
        let target = Target::from_params(params).await?;
        let resource = catalog::table(&target.client, &target.project, schema, table).await?;
        Ok(json!(indexes_from_resource(&resource)))
    })
    .await
}

/// The informational primary key, reported as the `PRIMARY` index.
pub fn indexes_from_resource(resource: &Value) -> Vec<Value> {
    schema_ddl::primary_key_columns(resource)
        .iter()
        .enumerate()
        .map(|(index, column)| {
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
        .collect()
}

/// `INFORMATION_SCHEMA.TABLES.ddl` for one object, run in the dataset
/// location with the object name as a query parameter.
async fn information_schema_ddl(
    target: &Target,
    schema: &str,
    name: &str,
) -> Result<String, String> {
    let mut parameters = QueryParams::new();
    let name_parameter = parameters.string(name);
    let sql = format!(
        "SELECT ddl FROM {} WHERE table_name = {name_parameter}",
        information_schema(&target.project, schema, "TABLES")
    );
    let location = target.location(schema).await;
    let statement = Statement::new(sql)
        .with_parameters(parameters.into_vec())
        .at_location(location)
        .returning(1);
    let output = target.client.run_statement(&statement).await?;
    output
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_str)
        .filter(|ddl| !ddl.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("INFORMATION_SCHEMA returned no DDL for '{schema}.{name}'"))
}

/// DDL of a table-like object: `INFORMATION_SCHEMA.TABLES.ddl` when it can be
/// read, otherwise rebuilt from the REST resource.
async fn object_ddl(target: &Target, schema: &str, name: &str) -> Result<String, String> {
    let resource = catalog::table(&target.client, &target.project, schema, name).await?;
    if schema_ddl::is_legacy_sql_view(&resource) {
        return schema_ddl::table_ddl(&resource, &target.project, schema, name);
    }
    match information_schema_ddl(target, schema, name).await {
        Ok(ddl) => Ok(ddl),
        Err(error) => {
            eprintln!(
                "[bigquery] INFORMATION_SCHEMA DDL unavailable, rebuilding from metadata: {error}"
            );
            schema_ddl::table_ddl(&resource, &target.project, schema, name)
        }
    }
}

pub async fn get_table_ddl(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let table = required_any(params, &["table", "table_name"])?;
        let target = Target::from_params(params).await?;
        Ok(json!(object_ddl(&target, schema, table).await?))
    })
    .await
}

pub async fn get_view_definition(id: Value, params: &Value) -> Value {
    definition(id, params).await
}

pub async fn get_materialized_view_definition(id: Value, params: &Value) -> Value {
    definition(id, params).await
}

async fn definition(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "view"])?;
        let target = Target::from_params(params).await?;
        Ok(json!(object_ddl(&target, schema, name).await?))
    })
    .await
}

pub async fn create_view(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "name"])?;
        let definition = required_any(params, &["definition"])?;
        let target = Target::from_params(params).await?;
        let sql = if starts_with_create(definition) {
            definition.to_string()
        } else {
            format!("CREATE VIEW {} AS\n{definition}", target.path(schema, name))
        };
        target.run_ddl(&sql, schema).await?;
        Ok(Value::Null)
    })
    .await
}

pub async fn alter_view(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "name"])?;
        let definition = required_any(params, &["definition"])?;
        let target = Target::from_params(params).await?;
        let resource = catalog::table(&target.client, &target.project, schema, name).await?;
        let sql = replace_view_sql(&resource, &target.path(schema, name), definition)?;
        target.run_ddl(&sql, schema).await?;
        Ok(Value::Null)
    })
    .await
}

/// `CREATE OR REPLACE VIEW` that keeps the view description, friendly name
/// and labels. Legacy SQL views are refused.
pub fn replace_view_sql(resource: &Value, path: &str, definition: &str) -> Result<String, String> {
    match TableKind::from_resource(resource) {
        TableKind::View => {}
        TableKind::MaterializedView => {
            return Err(
                "Materialized views cannot be replaced from the view editor; run CREATE OR REPLACE MATERIALIZED VIEW in a query tab."
                    .to_string(),
            )
        }
        _ => return Err("Only views can be altered with the view editor".to_string()),
    }
    if schema_ddl::is_legacy_sql_view(resource) {
        return Err(
            "This view uses legacy SQL. Tabularis edits GoogleSQL views only; recreate it in GoogleSQL with CREATE OR REPLACE VIEW in a query tab."
                .to_string(),
        );
    }
    if starts_with_create(definition) {
        return Ok(definition.to_string());
    }
    schema_ddl::view_ddl(resource, path, Some(definition), true)
}

fn starts_with_create(sql: &str) -> bool {
    sql.trim_start()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("CREATE"))
}

pub async fn drop_view(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "name"])?;
        let target = Target::from_params(params).await?;
        let kind = match catalog::table(&target.client, &target.project, schema, name).await {
            Ok(resource) => TableKind::from_resource(&resource),
            Err(_) => TableKind::View,
        };
        let sql = schema_ddl::drop_sql(kind, &target.path(schema, name));
        target.run_ddl(&sql, schema).await?;
        Ok(Value::Null)
    })
    .await
}

/// `CALL BQ.REFRESH_MATERIALIZED_VIEW('project.dataset.view')`.
pub fn refresh_materialized_view_sql(project: &str, dataset: &str, view: &str) -> String {
    format!(
        "CALL BQ.REFRESH_MATERIALIZED_VIEW({})",
        string_literal(&format!("{project}.{dataset}.{view}"))
    )
}

pub async fn refresh_materialized_view(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let name = required_any(params, &["view_name", "view", "name"])?;
        let target = Target::from_params(params).await?;
        let sql = refresh_materialized_view_sql(&target.project, schema, name);
        let location = target.location(schema).await;
        target
            .client
            .run_statement(&Statement::new(sql).at_location(location))
            .await?;
        catalog::invalidate();
        Ok(Value::Null)
    })
    .await
}

/// Lists table-like objects and fetches each resource once.
async fn table_resources(
    target: &Target,
    schema: &str,
    allowed_types: &[&str],
    limit: Option<usize>,
) -> Result<(Vec<(String, Value)>, usize), String> {
    let tables = catalog::tables(&target.client, &target.project, schema).await?;
    let names = object_names(&tables, allowed_types);
    let total = names.len();
    let names = match limit {
        Some(limit) => names.into_iter().take(limit).collect(),
        None => names,
    };
    let fetched = catalog::tables_batch(
        target.client.clone(),
        &target.project,
        schema,
        names,
        FETCH_CONCURRENCY,
    )
    .await;
    let mut resources = Vec::with_capacity(fetched.len());
    for (name, resource) in fetched {
        resources.push((name, resource?));
    }
    Ok((resources, total))
}

fn table_schema(name: &str, resource: &Value, schema: &str) -> Result<Value, String> {
    Ok(json!({
        "name": name,
        "columns": columns_from_resource(resource)?,
        "foreign_keys": foreign_keys_from_resource(resource, schema)
    }))
}

pub async fn get_schema_snapshot(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let target = Target::from_params(params).await?;
        let (resources, _) = table_resources(&target, schema, TABLE_TYPES, None).await?;
        let snapshot = resources
            .iter()
            .map(|(name, resource)| table_schema(name, resource, schema))
            .collect::<Result<Vec<_>, _>>()?;
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
        let schema = required_schema(params)?;
        let target = Target::from_params(params).await?;
        let requested = params
            .get("tables")
            .and_then(Value::as_array)
            .map(|tables| {
                tables
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<HashSet<_>>()
            });
        // Only fetch the tables the host asked for.
        let tables = catalog::tables(&target.client, &target.project, schema).await?;
        let names: Vec<String> = object_names(&tables, TABLE_TYPES)
            .into_iter()
            .filter(|name| {
                requested
                    .as_ref()
                    .is_none_or(|requested| requested.contains(name.as_str()))
            })
            .collect();
        let fetched = catalog::tables_batch(
            target.client.clone(),
            &target.project,
            schema,
            names,
            FETCH_CONCURRENCY,
        )
        .await;
        // One unreadable or concurrently deleted table must not fail the
        // whole batch: skip it and keep the others.
        let mut result = Map::new();
        for (name, resource) in fetched {
            let value = resource.and_then(|resource| {
                if columns {
                    columns_from_resource(&resource).map(|columns| json!(columns))
                } else {
                    Ok(json!(foreign_keys_from_resource(&resource, schema)))
                }
            });
            match value {
                Ok(value) => {
                    result.insert(name, value);
                }
                Err(error) => {
                    eprintln!("bigquery-plugin: skipping metadata of {schema}.{name}: {error}")
                }
            }
        }
        Ok(Value::Object(result))
    })
    .await
}

pub async fn get_ai_schema_context(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let max_tables = params
            .get("max_tables")
            .and_then(Value::as_u64)
            .unwrap_or(20) as usize;
        let target = Target::from_params(params).await?;
        let (resources, total) =
            table_resources(&target, schema, TABLE_TYPES, Some(max_tables)).await?;
        let tables = resources
            .iter()
            .map(|(name, resource)| table_schema(name, resource, schema))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"tables": tables, "total_table_count": total}))
    })
    .await
}

pub fn required_schema(params: &Value) -> Result<&str, String> {
    params
        .get("schema")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A BigQuery dataset is required as 'schema'".to_string())
}

pub fn required_any<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str, String> {
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

pub async fn respond<F>(id: Value, future: F) -> Value
where
    F: Future<Output = Result<Value, String>>,
{
    match future.await {
        Ok(value) => ok_response(id, value),
        Err(error) => error_response(id, -32603, &error),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merges_projects_without_duplicates() {
        assert_eq!(
            merge_projects(
                "my-proj",
                "billing-proj",
                vec!["z-proj".into(), "my-proj".into()],
                vec!["bigquery-public-data".into()]
            ),
            vec!["bigquery-public-data", "billing-proj", "my-proj", "z-proj"]
        );
        assert_eq!(merge_projects("p", "p", vec![], vec![]), vec!["p"]);
    }

    #[test]
    fn object_names_filter_and_sort_types() {
        let tables = vec![
            json!({"type": "VIEW", "tableReference": {"tableId": "v"}}),
            json!({"type": "TABLE", "tableReference": {"tableId": "b"}}),
            json!({"type": "SNAPSHOT", "tableReference": {"tableId": "a"}}),
            json!({"type": "EXTERNAL", "tableReference": {"tableId": "c"}}),
        ];
        assert_eq!(object_names(&tables, TABLE_TYPES), vec!["a", "b", "c"]);
        assert_eq!(object_names(&tables, &["VIEW"]), vec!["v"]);
    }

    #[test]
    fn refresh_uses_a_string_path() {
        assert_eq!(
            refresh_materialized_view_sql("my-proj", "d", "mv'1"),
            "CALL BQ.REFRESH_MATERIALIZED_VIEW('my-proj.d.mv\\'1')"
        );
    }

    #[test]
    fn replacing_views_keeps_options_and_refuses_legacy_sql() {
        let view = json!({"type": "VIEW", "description": "Daily", "labels": {"a": "b"}, "view": {"query": "SELECT 1"}});
        assert_eq!(
            replace_view_sql(&view, "`p-1`.`d`.`v`", "SELECT 2").unwrap(),
            "CREATE OR REPLACE VIEW `p-1`.`d`.`v` OPTIONS(description = 'Daily', labels = [('a', 'b')])\nAS\nSELECT 2"
        );
        let legacy = json!({"type": "VIEW", "view": {"query": "SELECT 1", "useLegacySql": true}});
        assert!(replace_view_sql(&legacy, "`p`.`d`.`v`", "SELECT 2")
            .unwrap_err()
            .contains("legacy SQL"));
        let mv = json!({"type": "MATERIALIZED_VIEW"});
        assert!(replace_view_sql(&mv, "`p`.`d`.`v`", "SELECT 2").is_err());
    }

    #[test]
    fn columns_include_precision_and_descriptions() {
        let resource = json!({
            "schema": {"fields": [
                {"name": "id", "type": "INTEGER", "mode": "REQUIRED"},
                {"name": "price", "type": "NUMERIC", "precision": "10", "scale": "2", "description": "Net"},
                {"name": "code", "type": "STRING", "maxLength": "8"}
            ]},
            "tableConstraints": {"primaryKey": {"columns": ["id"]}}
        });
        let columns = columns_from_resource(&resource).unwrap();
        assert_eq!(columns[0]["data_type"], "INT64");
        assert_eq!(columns[0]["is_pk"], true);
        assert_eq!(columns[0]["is_nullable"], false);
        assert_eq!(columns[1]["data_type"], "NUMERIC(10, 2)");
        assert_eq!(columns[1]["numeric_precision"], 10);
        assert_eq!(columns[1]["numeric_scale"], 2);
        assert_eq!(columns[1]["comment"], "Net");
        assert_eq!(columns[2]["character_maximum_length"], 8);
    }
}
