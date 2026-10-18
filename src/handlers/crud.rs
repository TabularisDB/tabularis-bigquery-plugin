use serde_json::{json, Map, Value};

use crate::bigquery::FieldSchema;
use crate::catalog;
use crate::client::{self, Statement};
use crate::handlers::metadata::{respond, Target};
use crate::schema_ddl;
use crate::sql::{base_type, is_repeated, quote_identifier, QueryParams, USE_DEFAULT_SENTINEL};

/// A table resolved for row editing.
pub struct EditTarget {
    pub target: Target,
    pub dataset: String,
    pub table: String,
    pub fields: Vec<FieldSchema>,
    /// No informational primary key: rows are matched on comparable columns.
    pub keyless: bool,
}

impl EditTarget {
    pub async fn load(params: &Value) -> Result<Self, String> {
        let dataset = required_string(params, "schema")?.to_string();
        let table = params
            .get("table")
            .or_else(|| params.get("table_name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Invalid params: missing 'table'".to_string())?
            .to_string();
        let target = Target::from_params(params).await?;
        let resource = catalog::table(&target.client, &target.project, &dataset, &table).await?;
        schema_ddl::ensure_dml_allowed(&resource)?;
        Ok(Self {
            fields: client::table_fields(&resource)?,
            keyless: schema_ddl::primary_key_columns(&resource).is_empty(),
            target,
            dataset,
            table,
        })
    }

    pub fn path(&self) -> String {
        self.target.path(&self.dataset, &self.table)
    }

    pub fn field(&self, name: &str) -> Result<&FieldSchema, String> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| format!("Column '{name}' does not exist in {}", self.table))
    }

    async fn run(
        &self,
        sql: String,
        parameters: QueryParams,
    ) -> Result<crate::bigquery::QueryOutput, String> {
        let statement = Statement::new(sql)
            .with_parameters(parameters.into_vec())
            .in_dataset(&self.target.project, &self.dataset)
            .at_location(self.target.location(&self.dataset).await)
            .returning(1);
        self.target
            .client
            .run_statement(&statement)
            .await
            .map_err(|error| friendly_dml_error(&error))
    }
}

/// Columns that can identify a row with `=`. Floating point, JSON,
/// geography, arrays, structs, ranges and bytes are excluded.
pub fn is_comparable(field: &FieldSchema) -> bool {
    !is_repeated(field)
        && !matches!(
            base_type(field).as_str(),
            "FLOAT64" | "JSON" | "GEOGRAPHY" | "STRUCT" | "RANGE" | "BYTES"
        )
}

/// WHERE clause from the row identity sent by the host. For keyless tables
/// the host sends every column; only comparable ones are used.
pub fn build_predicate(
    identity: &Map<String, Value>,
    fields: &[FieldSchema],
    keyless: bool,
    parameters: &mut QueryParams,
) -> Result<String, String> {
    let mut columns = identity.keys().collect::<Vec<_>>();
    columns.sort();
    let mut predicates = Vec::new();
    for column in columns {
        let field = fields
            .iter()
            .find(|field| &field.name == column)
            .ok_or_else(|| format!("Column '{column}' does not exist"))?;
        if keyless && !is_comparable(field) {
            continue;
        }
        let value = &identity[column];
        if value.is_null() {
            predicates.push(format!("{} IS NULL", quote_identifier(column)));
        } else {
            predicates.push(format!(
                "{} = {}",
                quote_identifier(column),
                parameters.bind(value, Some(field))?
            ));
        }
    }
    if predicates.is_empty() {
        return Err(
            "This row cannot be identified: the table has no primary key and no comparable columns"
                .to_string(),
        );
    }
    Ok(predicates.join(" AND "))
}

/// Wraps a single-row DML statement for keyless tables in a transaction
/// that rolls back when more than one row would change.
pub fn guarded_script(dml: &str) -> String {
    format!(
        "DECLARE tabularis_affected INT64 DEFAULT 0;\n\
         BEGIN TRANSACTION;\n\
         {dml};\n\
         SET tabularis_affected = @@row_count;\n\
         IF tabularis_affected > 1 THEN\n  \
         ROLLBACK TRANSACTION;\n  \
         RAISE USING MESSAGE = FORMAT('The row is not unique: %d identical rows match it and the change was rolled back. Add a primary key or edit the rows with SQL.', tabularis_affected);\n\
         END IF;\n\
         COMMIT TRANSACTION;\n\
         SELECT tabularis_affected AS affected_rows;"
    )
}

pub fn friendly_dml_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("streaming buffer") {
        return format!(
            "BigQuery cannot UPDATE or DELETE rows that are still in the streaming buffer (rows streamed in the last few minutes, up to about 90). Retry later. Details: {error}"
        );
    }
    error.to_string()
}

fn affected_rows(output: &crate::bigquery::QueryOutput, keyless: bool) -> u64 {
    if keyless {
        if let Some(value) = output.rows.first().and_then(|row| row.first()) {
            return value
                .as_u64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                .unwrap_or_default();
        }
    }
    output.affected_rows
}

pub fn insert_sql(
    path: &str,
    data: &Map<String, Value>,
    fields: &[FieldSchema],
    parameters: &mut QueryParams,
) -> Result<String, String> {
    if data.is_empty() {
        return Err("Cannot insert an empty row".to_string());
    }
    let mut columns = Vec::new();
    let mut values = Vec::new();
    for (name, value) in data {
        let field = fields
            .iter()
            .find(|field| &field.name == name)
            .ok_or_else(|| format!("Column '{name}' does not exist"))?;
        columns.push(quote_identifier(name));
        if value.as_str() == Some(USE_DEFAULT_SENTINEL) {
            values.push("DEFAULT".to_string());
        } else {
            values.push(parameters.bind(value, Some(field))?);
        }
    }
    Ok(format!(
        "INSERT INTO {path} ({}) VALUES ({})",
        columns.join(", "),
        values.join(", ")
    ))
}

pub async fn insert_record(id: Value, params: &Value) -> Value {
    respond(id, async {
        let data = params
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| "Invalid params: missing or malformed 'data'".to_string())?;
        let edit = EditTarget::load(params).await?;
        let mut parameters = QueryParams::new();
        let sql = insert_sql(&edit.path(), data, &edit.fields, &mut parameters)?;
        let output = edit.run(sql, parameters).await?;
        Ok(json!(output.affected_rows))
    })
    .await
}

pub fn update_sql(
    path: &str,
    column: &str,
    value: &Value,
    identity: &Map<String, Value>,
    fields: &[FieldSchema],
    keyless: bool,
    parameters: &mut QueryParams,
) -> Result<String, String> {
    let field = fields
        .iter()
        .find(|field| field.name == column)
        .ok_or_else(|| format!("Column '{column}' does not exist"))?;
    let assignment = if value.as_str() == Some(USE_DEFAULT_SENTINEL) {
        "DEFAULT".to_string()
    } else {
        parameters.bind(value, Some(field))?
    };
    let predicate = build_predicate(identity, fields, keyless, parameters)?;
    Ok(format!(
        "UPDATE {path} SET {} = {assignment} WHERE {predicate}",
        quote_identifier(column)
    ))
}

pub async fn update_record(id: Value, params: &Value) -> Value {
    respond(id, async {
        let column = required_string(params, "col_name")?;
        let value = params.get("new_val").cloned().unwrap_or(Value::Null);
        let identity = row_identity(params)?;
        let edit = EditTarget::load(params).await?;
        let mut parameters = QueryParams::new();
        let sql = update_sql(
            &edit.path(),
            column,
            &value,
            &identity,
            &edit.fields,
            edit.keyless,
            &mut parameters,
        )?;
        let sql = if edit.keyless {
            guarded_script(&sql)
        } else {
            sql
        };
        let output = edit.run(sql, parameters).await?;
        Ok(json!(affected_rows(&output, edit.keyless)))
    })
    .await
}

pub async fn delete_record(id: Value, params: &Value) -> Value {
    respond(id, async {
        let identity = row_identity(params)?;
        let edit = EditTarget::load(params).await?;
        let mut parameters = QueryParams::new();
        let predicate = build_predicate(&identity, &edit.fields, edit.keyless, &mut parameters)?;
        let sql = format!("DELETE FROM {} WHERE {predicate}", edit.path());
        let sql = if edit.keyless {
            guarded_script(&sql)
        } else {
            sql
        };
        let output = edit.run(sql, parameters).await?;
        Ok(json!(affected_rows(&output, edit.keyless)))
    })
    .await
}

/// `pk_map`, or the legacy `pk_col` / `pk_val` pair.
pub fn row_identity(params: &Value) -> Result<Map<String, Value>, String> {
    if let Some(map) = params.get("pk_map").and_then(Value::as_object) {
        if !map.is_empty() {
            return Ok(map.clone());
        }
    }
    match (
        params.get("pk_col").and_then(Value::as_str),
        params.get("pk_val"),
    ) {
        (Some(column), Some(value)) => {
            let mut map = Map::new();
            map.insert(column.to_string(), value.clone());
            Ok(map)
        }
        _ => Err("A non-empty primary-key map is required for update and delete".to_string()),
    }
}

fn required_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Invalid params: missing '{key}'"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fields() -> Vec<FieldSchema> {
        serde_json::from_value(json!([
            {"name": "id", "type": "INTEGER"},
            {"name": "name", "type": "STRING"},
            {"name": "score", "type": "FLOAT"},
            {"name": "payload", "type": "JSON"},
            {"name": "location", "type": "GEOGRAPHY"},
            {"name": "tags", "type": "STRING", "mode": "REPEATED"},
            {"name": "price", "type": "NUMERIC"},
            {"name": "data", "type": "BYTES"}
        ]))
        .unwrap()
    }

    #[test]
    fn inserts_with_typed_parameters_and_defaults() {
        let data = json!({"name": "O'Reilly\n", "price": "1.25", "location": "POINT(1 2)", "id": "__USE_DEFAULT__"});
        let mut parameters = QueryParams::new();
        let sql = insert_sql(
            "`p-1`.`d`.`t`",
            data.as_object().unwrap(),
            &fields(),
            &mut parameters,
        )
        .unwrap();
        assert_eq!(
            sql,
            "INSERT INTO `p-1`.`d`.`t` (`id`, `location`, `name`, `price`) VALUES (DEFAULT, ST_GEOGFROMTEXT(@p0), @p1, CAST(@p2 AS NUMERIC))"
        );
        let parameters = parameters.into_vec();
        assert_eq!(parameters[1]["parameterValue"]["value"], "O'Reilly\n");
        assert_eq!(parameters[2]["parameterValue"]["value"], "1.25");
        let unknown = json!({"nope": 1});
        assert!(insert_sql(
            "t",
            unknown.as_object().unwrap(),
            &fields(),
            &mut QueryParams::new()
        )
        .is_err());
    }

    #[test]
    fn updates_by_primary_key() {
        let identity = json!({"id": 7}).as_object().unwrap().clone();
        let mut parameters = QueryParams::new();
        let sql = update_sql(
            "`p`.`d`.`t`",
            "payload",
            &json!({"a": [1, 2]}),
            &identity,
            &fields(),
            false,
            &mut parameters,
        )
        .unwrap();
        assert_eq!(
            sql,
            "UPDATE `p`.`d`.`t` SET `payload` = PARSE_JSON(@p0) WHERE `id` = @p1"
        );
        let parameters = parameters.into_vec();
        assert_eq!(parameters[0]["parameterValue"]["value"], "{\"a\":[1,2]}");
        assert_eq!(parameters[1]["parameterType"]["type"], "INT64");
    }

    #[test]
    fn keyless_predicates_skip_non_comparable_columns() {
        let identity = json!({
            "id": 1, "name": null, "score": 1.5, "payload": {"x": 1},
            "location": "POINT(0 0)", "tags": ["a"], "price": "2.50", "data": "AQID"
        });
        let mut parameters = QueryParams::new();
        let predicate = build_predicate(
            identity.as_object().unwrap(),
            &fields(),
            true,
            &mut parameters,
        )
        .unwrap();
        assert_eq!(
            predicate,
            "`id` = @p0 AND `name` IS NULL AND `price` = CAST(@p1 AS NUMERIC)"
        );
        let only_floats = json!({"score": 1.0});
        assert!(build_predicate(
            only_floats.as_object().unwrap(),
            &fields(),
            true,
            &mut QueryParams::new()
        )
        .is_err());
        let guarded = guarded_script("DELETE FROM `p`.`d`.`t` WHERE `id` = @p0");
        assert!(guarded.contains("BEGIN TRANSACTION;\nDELETE FROM `p`.`d`.`t` WHERE `id` = @p0;"));
        assert!(guarded.contains("ROLLBACK TRANSACTION"));
        assert!(guarded
            .trim_end()
            .ends_with("SELECT tabularis_affected AS affected_rows;"));
    }

    #[test]
    fn maps_streaming_buffer_errors() {
        let error = "UPDATE or DELETE statement over table p.d.t would affect rows in the streaming buffer, which is not supported";
        assert!(friendly_dml_error(error).starts_with(
            "BigQuery cannot UPDATE or DELETE rows that are still in the streaming buffer"
        ));
        assert_eq!(friendly_dml_error("other"), "other");
    }

    #[test]
    fn reads_legacy_identity_shape() {
        let identity = row_identity(&json!({"pk_col": "id", "pk_val": 3})).unwrap();
        assert_eq!(identity["id"], 3);
        assert!(row_identity(&json!({"pk_map": {}})).is_err());
    }
}
