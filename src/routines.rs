//! Routine metadata and SQL built from BigQuery `routines.get` resources.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::bigquery::{FieldSchema, RangeElementType};
use crate::schema_ddl::options_clause;
use crate::sql::{canonical_type, quote_identifier, string_literal, value_literal};

/// Routine kinds as reported by the REST API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    ScalarFunction,
    AggregateFunction,
    TableFunction,
    Procedure,
}

impl RoutineKind {
    pub fn from_resource(resource: &Value) -> Self {
        Self::from_name(
            resource
                .get("routineType")
                .and_then(Value::as_str)
                .unwrap_or("SCALAR_FUNCTION"),
        )
    }

    /// Accepts REST names and the host's `FUNCTION` / `PROCEDURE`.
    pub fn from_name(name: &str) -> Self {
        match name
            .trim()
            .to_ascii_uppercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "PROCEDURE" => Self::Procedure,
            "TABLE_VALUED_FUNCTION" | "TABLE_FUNCTION" | "TVF" => Self::TableFunction,
            "AGGREGATE_FUNCTION" => Self::AggregateFunction,
            _ => Self::ScalarFunction,
        }
    }

    /// The host groups routines into `PROCEDURE` and `FUNCTION`.
    pub fn host_type(self) -> &'static str {
        match self {
            Self::Procedure => "PROCEDURE",
            _ => "FUNCTION",
        }
    }

    pub fn keyword(self) -> &'static str {
        match self {
            Self::ScalarFunction => "FUNCTION",
            Self::AggregateFunction => "AGGREGATE FUNCTION",
            Self::TableFunction => "TABLE FUNCTION",
            Self::Procedure => "PROCEDURE",
        }
    }

    pub fn drop_keyword(self) -> &'static str {
        match self {
            Self::AggregateFunction | Self::ScalarFunction => "FUNCTION",
            Self::TableFunction => "TABLE FUNCTION",
            Self::Procedure => "PROCEDURE",
        }
    }
}

/// `{name, routine_type}` entry for `get_routines`.
pub fn routine_info(resource: &Value) -> Option<Value> {
    let name = resource
        .pointer("/routineReference/routineId")
        .and_then(Value::as_str)?;
    Some(json!({
        "name": name,
        "routine_type": RoutineKind::from_resource(resource).host_type(),
        "definition": null
    }))
}

/// Renders a REST `StandardSqlDataType`.
pub fn standard_type_sql(data_type: &Value) -> String {
    let kind = data_type
        .get("typeKind")
        .and_then(Value::as_str)
        .unwrap_or("TYPE_KIND_UNSPECIFIED");
    match kind {
        "ARRAY" => format!(
            "ARRAY<{}>",
            data_type
                .get("arrayElementType")
                .map(standard_type_sql)
                .unwrap_or_else(|| "STRING".to_string())
        ),
        "STRUCT" => {
            let fields = data_type
                .pointer("/structType/fields")
                .and_then(Value::as_array)
                .map(|fields| {
                    fields
                        .iter()
                        .map(|field| {
                            let field_type = field
                                .get("type")
                                .map(standard_type_sql)
                                .unwrap_or_else(|| "STRING".to_string());
                            match field.get("name").and_then(Value::as_str) {
                                Some(name) if !name.is_empty() => {
                                    format!("{} {field_type}", quote_identifier(name))
                                }
                                _ => field_type,
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!("STRUCT<{fields}>")
        }
        "RANGE" => format!(
            "RANGE<{}>",
            data_type
                .get("rangeElementType")
                .map(standard_type_sql)
                .unwrap_or_else(|| "DATE".to_string())
        ),
        "TYPE_KIND_UNSPECIFIED" => "ANY TYPE".to_string(),
        other => canonical_type(other),
    }
}

/// Converts a `StandardSqlDataType` into the table field shape used by the
/// literal renderer.
pub fn standard_type_field(name: &str, data_type: &Value) -> FieldSchema {
    let kind = data_type
        .get("typeKind")
        .and_then(Value::as_str)
        .unwrap_or("STRING");
    match kind {
        "ARRAY" => {
            let mut element = data_type
                .get("arrayElementType")
                .map(|element| standard_type_field(name, element))
                .unwrap_or_else(|| FieldSchema {
                    name: name.to_string(),
                    field_type: "STRING".to_string(),
                    ..Default::default()
                });
            element.mode = Some("REPEATED".to_string());
            element
        }
        "STRUCT" => FieldSchema {
            name: name.to_string(),
            field_type: "RECORD".to_string(),
            fields: data_type
                .pointer("/structType/fields")
                .and_then(Value::as_array)
                .map(|fields| {
                    fields
                        .iter()
                        .map(|field| {
                            standard_type_field(
                                field
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                                field.get("type").unwrap_or(&Value::Null),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        },
        "RANGE" => FieldSchema {
            name: name.to_string(),
            field_type: "RANGE".to_string(),
            range_element_type: Some(RangeElementType {
                element_type: data_type
                    .pointer("/rangeElementType/typeKind")
                    .and_then(Value::as_str)
                    .unwrap_or("DATE")
                    .to_string(),
            }),
            ..Default::default()
        },
        other => FieldSchema {
            name: name.to_string(),
            field_type: other.to_string(),
            ..Default::default()
        },
    }
}

fn arguments(resource: &Value) -> Vec<Value> {
    resource
        .get("arguments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn argument_type_sql(argument: &Value) -> String {
    if argument.get("argumentKind").and_then(Value::as_str) == Some("ANY_TYPE") {
        return "ANY TYPE".to_string();
    }
    argument
        .get("dataType")
        .map(standard_type_sql)
        .unwrap_or_else(|| "ANY TYPE".to_string())
}

fn return_table_sql(resource: &Value) -> Option<String> {
    let columns = resource
        .pointer("/returnTableType/columns")
        .and_then(Value::as_array)?;
    Some(format!(
        "TABLE<{}>",
        columns
            .iter()
            .map(|column| {
                format!(
                    "{} {}",
                    quote_identifier(
                        column
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ),
                    column
                        .get("type")
                        .map(standard_type_sql)
                        .unwrap_or_else(|| "STRING".to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn return_type_sql(resource: &Value) -> Option<String> {
    resource.get("returnType").map(standard_type_sql)
}

/// Host `RoutineParameter` rows. Arguments are numbered from 1; a function's
/// return type is reported as the unnamed `OUT` row at position 0.
pub fn routine_parameters(resource: &Value) -> Vec<Value> {
    let kind = RoutineKind::from_resource(resource);
    let mut parameters = Vec::new();
    if kind != RoutineKind::Procedure {
        let return_type = match kind {
            RoutineKind::TableFunction => return_table_sql(resource),
            _ => return_type_sql(resource),
        };
        if let Some(data_type) = return_type {
            parameters.push(json!({
                "name": "",
                "data_type": data_type,
                "mode": "OUT",
                "ordinal_position": 0
            }));
        }
    }
    for (index, argument) in arguments(resource).iter().enumerate() {
        let position = index + 1;
        let name = argument
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("arg{position}"));
        let mode = argument
            .get("mode")
            .and_then(Value::as_str)
            .filter(|mode| matches!(*mode, "IN" | "OUT" | "INOUT"))
            .unwrap_or("IN");
        parameters.push(json!({
            "name": name,
            "data_type": argument_type_sql(argument),
            "mode": mode,
            "ordinal_position": position
        }));
    }
    parameters
}

/// A string literal for a routine body, preferring a readable raw string.
pub fn body_literal(body: &str) -> String {
    if !body.contains("\"\"\"") && !body.ends_with('\\') && !body.ends_with('"') {
        return format!("r\"\"\"{body}\"\"\"");
    }
    let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"\"\"{escaped}\"\"\"")
}

fn language(resource: &Value) -> String {
    resource
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("SQL")
        .to_ascii_uppercase()
}

fn routine_options(resource: &Value, kind: RoutineKind) -> Vec<(String, String)> {
    let mut options = Vec::new();
    if let Some(description) = resource.get("description").and_then(Value::as_str) {
        options.push(("description".to_string(), string_literal(description)));
    }
    if let Some(libraries) = resource.get("importedLibraries").and_then(Value::as_array) {
        let libraries = libraries
            .iter()
            .filter_map(Value::as_str)
            .map(string_literal)
            .collect::<Vec<_>>();
        if !libraries.is_empty() {
            options.push(("library".to_string(), format!("[{}]", libraries.join(", "))));
        }
    }
    if kind == RoutineKind::Procedure
        && resource.get("strictMode").and_then(Value::as_bool) == Some(false)
    {
        options.push(("strict_mode".to_string(), "FALSE".to_string()));
    }
    if let Some(governance) = resource.get("dataGovernanceType").and_then(Value::as_str) {
        options.push((
            "data_governance_type".to_string(),
            string_literal(governance),
        ));
    }
    if let Some(python) = resource.get("pythonOptions") {
        if let Some(entry_point) = python.get("entryPoint").and_then(Value::as_str) {
            options.push(("entry_point".to_string(), string_literal(entry_point)));
        }
        if let Some(packages) = python.get("packages").and_then(Value::as_array) {
            let packages = packages
                .iter()
                .filter_map(Value::as_str)
                .map(string_literal)
                .collect::<Vec<_>>();
            options.push(("packages".to_string(), format!("[{}]", packages.join(", "))));
        }
    }
    if let Some(runtime) = resource
        .pointer("/externalRuntimeOptions/runtimeVersion")
        .and_then(Value::as_str)
    {
        options.push(("runtime_version".to_string(), string_literal(runtime)));
    }
    if let Some(remote) = resource.get("remoteFunctionOptions") {
        if let Some(endpoint) = remote.get("endpoint").and_then(Value::as_str) {
            options.push(("endpoint".to_string(), string_literal(endpoint)));
        }
        if let Some(rows) = remote.get("maxBatchingRows").and_then(Value::as_str) {
            options.push(("max_batching_rows".to_string(), rows.to_string()));
        }
        if let Some(context) = remote.get("userDefinedContext").and_then(Value::as_object) {
            let mut pairs = context
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
            options.push((
                "user_defined_context".to_string(),
                format!("[{}]", pairs.join(", ")),
            ));
        }
    }
    if let Some(spark) = resource.get("sparkOptions") {
        options.push(("engine".to_string(), "'SPARK'".to_string()));
        for (key, option) in [
            ("runtimeVersion", "runtime_version"),
            ("mainFileUri", "main_file_uri"),
            ("mainClass", "main_class"),
            ("containerImage", "container_image"),
        ] {
            if let Some(value) = spark.get(key).and_then(Value::as_str) {
                options.push((option.to_string(), string_literal(value)));
            }
        }
    }
    options
}

fn connection_clause(connection: &str) -> String {
    let parts = connection.split('/').collect::<Vec<_>>();
    let path = if parts.len() == 6 && parts[0] == "projects" {
        format!("{}.{}.{}", parts[1], parts[3], parts[5])
    } else {
        connection.to_string()
    };
    format!("WITH CONNECTION {}", quote_identifier(&path))
}

/// Rebuilds `CREATE [OR REPLACE] ...` for a routine from its REST resource,
/// including the language, non-SQL bodies and OPTIONS.
pub fn routine_ddl(resource: &Value, path: &str, replace: bool) -> Result<String, String> {
    let kind = RoutineKind::from_resource(resource);
    let body = resource
        .get("definitionBody")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let language = language(resource);
    let options = routine_options(resource, kind);
    let create = if replace {
        "CREATE OR REPLACE"
    } else {
        "CREATE"
    };
    let argument_list = arguments(resource)
        .iter()
        .map(|argument| {
            let name = quote_identifier(
                argument
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            let data_type = argument_type_sql(argument);
            match kind {
                RoutineKind::Procedure => {
                    let mode = argument.get("mode").and_then(Value::as_str).unwrap_or("IN");
                    format!("{mode} {name} {data_type}")
                }
                RoutineKind::AggregateFunction
                    if argument.get("isAggregate").and_then(Value::as_bool) == Some(false) =>
                {
                    format!("{name} {data_type} NOT AGGREGATE")
                }
                _ => format!("{name} {data_type}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("{create} {} {path}({argument_list})", kind.keyword());

    match kind {
        RoutineKind::Procedure => {
            if language != "SQL" {
                if let Some(connection) = resource
                    .pointer("/sparkOptions/connection")
                    .and_then(Value::as_str)
                {
                    sql.push('\n');
                    sql.push_str(&connection_clause(connection));
                }
                sql.push_str(&options_clause(&options));
                sql.push_str(&format!("\nLANGUAGE {language}"));
                if !body.is_empty() {
                    sql.push_str(&format!("\nAS {}", body_literal(body)));
                }
                return Ok(sql);
            }
            sql.push_str(&options_clause(&options));
            let trimmed = body.trim();
            if trimmed.to_ascii_uppercase().starts_with("BEGIN") {
                sql.push_str(&format!("\n{trimmed}"));
            } else {
                sql.push_str(&format!("\nBEGIN\n{trimmed}\nEND"));
            }
            Ok(sql)
        }
        RoutineKind::TableFunction => {
            if let Some(table) = return_table_sql(resource) {
                sql.push_str(&format!("\nRETURNS {table}"));
            }
            sql.push_str(&options_clause(&options));
            sql.push_str(&format!("\nAS\n{}", body.trim()));
            Ok(sql)
        }
        RoutineKind::ScalarFunction | RoutineKind::AggregateFunction => {
            if let Some(return_type) = return_type_sql(resource) {
                sql.push_str(&format!("\nRETURNS {return_type}"));
            }
            if let Some(remote) = resource.get("remoteFunctionOptions") {
                let connection = remote
                    .get("connection")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                sql.push_str(&format!("\nREMOTE {}", connection_clause(connection)));
                sql.push_str(&options_clause(&options));
                return Ok(sql);
            }
            if language == "SQL" {
                sql.push_str(&format!("\nAS (\n  {}\n)", body.trim()));
                sql.push_str(&options_clause(&options));
                return Ok(sql);
            }
            match resource.get("determinismLevel").and_then(Value::as_str) {
                Some("DETERMINISTIC") => sql.push_str("\nDETERMINISTIC"),
                Some("NOT_DETERMINISTIC") => sql.push_str("\nNOT DETERMINISTIC"),
                _ => {}
            }
            let language_name = match language.as_str() {
                "JAVASCRIPT" => "js".to_string(),
                "PYTHON" => "python".to_string(),
                other => other.to_ascii_lowercase(),
            };
            sql.push_str(&format!("\nLANGUAGE {language_name}"));
            if let Some(connection) = resource
                .pointer("/externalRuntimeOptions/connection")
                .and_then(Value::as_str)
            {
                sql.push('\n');
                sql.push_str(&connection_clause(connection));
            }
            sql.push_str(&options_clause(&options));
            sql.push_str(&format!("\nAS {}", body_literal(body)));
            Ok(sql)
        }
    }
}

pub fn drop_routine_sql(kind: RoutineKind, path: &str) -> String {
    format!("DROP {} IF EXISTS {path}", kind.drop_keyword())
}

/// Mirror of the host `RoutineCallArg`.
#[derive(Debug, Clone, Deserialize)]
pub struct CallArg {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub is_raw: bool,
}

fn default_mode() -> String {
    "IN".to_string()
}

fn argument_value(arg: &CallArg, data_type: Option<&Value>) -> Result<String, String> {
    let Some(value) = arg.value.as_deref() else {
        return Ok("NULL".to_string());
    };
    if arg.is_raw {
        return Ok(value.to_string());
    }
    match data_type {
        Some(data_type) if data_type.get("typeKind").is_some() => value_literal(
            &Value::String(value.to_string()),
            Some(&standard_type_field(&arg.name, data_type)),
        )
        .map_err(|error| format!("Argument '{}': {error}", arg.name)),
        _ => Ok(string_literal(value)),
    }
}

/// Builds the invocation script for the Run Routine dialog:
/// `CALL` for procedures (OUT/INOUT through script variables),
/// `SELECT fn(...)` for scalar and aggregate functions and
/// `SELECT * FROM tvf(...)` for table functions.
pub fn routine_call_sql(resource: &Value, path: &str, args: &[CallArg]) -> Result<String, String> {
    let kind = RoutineKind::from_resource(resource);
    let declared = arguments(resource);
    let data_type = |index: usize, arg: &CallArg| {
        declared
            .iter()
            .find(|argument| {
                argument.get("name").and_then(Value::as_str) == Some(arg.name.as_str())
            })
            .or_else(|| declared.get(index))
            .and_then(|argument| argument.get("dataType"))
            .cloned()
    };

    if kind != RoutineKind::Procedure {
        let values = args
            .iter()
            .enumerate()
            .map(|(index, arg)| argument_value(arg, data_type(index, arg).as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        let call = format!("{path}({})", values.join(", "));
        return Ok(match kind {
            RoutineKind::TableFunction => format!("SELECT *\nFROM {call};"),
            _ => format!("SELECT {call} AS result;"),
        });
    }

    let mut declarations = Vec::new();
    let mut call_args = Vec::new();
    let mut outputs = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        let mode = arg.mode.to_ascii_uppercase();
        if mode == "IN" {
            call_args.push(argument_value(arg, data_type(index, arg).as_ref())?);
            continue;
        }
        let variable = format!("tabularis_arg_{}", index + 1);
        let declared_type = data_type(index, arg)
            .map(|data_type| standard_type_sql(&data_type))
            .filter(|data_type| data_type != "ANY TYPE")
            .unwrap_or_else(|| "STRING".to_string());
        let default = if mode == "INOUT" && arg.value.is_some() {
            format!(
                " DEFAULT {}",
                argument_value(arg, data_type(index, arg).as_ref())?
            )
        } else {
            String::new()
        };
        declarations.push(format!("DECLARE {variable} {declared_type}{default};"));
        let label = if arg.name.is_empty() {
            variable.clone()
        } else {
            arg.name.clone()
        };
        outputs.push(format!("{variable} AS {}", quote_identifier(&label)));
        call_args.push(variable);
    }
    let mut script = declarations;
    script.push(format!("CALL {path}({});", call_args.join(", ")));
    if !outputs.is_empty() {
        script.push(format!("SELECT {};", outputs.join(", ")));
    }
    Ok(script.join("\n"))
}

/// Starter script for a new routine.
pub fn create_template(routine_type: &str, project: Option<&str>, dataset: Option<&str>) -> String {
    let path = |name: &str| match (
        project.filter(|value| !value.is_empty()),
        dataset.filter(|value| !value.is_empty()),
    ) {
        (Some(project), Some(dataset)) => crate::sql::fully_qualified(project, dataset, name),
        (None, Some(dataset)) => crate::sql::qualified(dataset, name),
        _ => quote_identifier(name),
    };
    match RoutineKind::from_name(routine_type) {
        RoutineKind::Procedure => format!(
            "CREATE OR REPLACE PROCEDURE {}(IN input_id INT64, OUT row_count INT64)\n\
             OPTIONS(description = 'Describe the procedure')\n\
             BEGIN\n  \
             -- Multi-statement GoogleSQL body\n  \
             SET row_count = (\n    SELECT COUNT(*)\n    FROM {}\n    WHERE id = input_id\n  );\n\
             END;",
            path("my_procedure"),
            path("my_table")
        ),
        RoutineKind::TableFunction => format!(
            "CREATE OR REPLACE TABLE FUNCTION {}(min_id INT64)\n\
             OPTIONS(description = 'Describe the table function')\n\
             AS\n\
             SELECT *\nFROM {}\nWHERE id >= min_id;",
            path("my_table_function"),
            path("my_table")
        ),
        RoutineKind::ScalarFunction | RoutineKind::AggregateFunction => format!(
            "CREATE OR REPLACE FUNCTION {}(x INT64, y INT64)\n\
             RETURNS INT64\n\
             AS (\n  x + y\n)\n\
             OPTIONS(description = 'Describe the function');\n\n\
             -- JavaScript UDF:\n\
             -- CREATE OR REPLACE FUNCTION {}(s STRING)\n\
             -- RETURNS STRING\n\
             -- LANGUAGE js\n\
             -- AS r\"\"\"return s.toUpperCase();\"\"\";\n\n\
             -- Table function:\n\
             -- CREATE OR REPLACE TABLE FUNCTION {}(min_id INT64)\n\
             -- AS SELECT * FROM {} WHERE id >= min_id;",
            path("my_function"),
            path("my_js_function"),
            path("my_table_function"),
            path("my_table")
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn scalar() -> Value {
        json!({
            "routineReference": {"projectId": "my-proj", "datasetId": "d", "routineId": "add_tax"},
            "routineType": "SCALAR_FUNCTION",
            "language": "SQL",
            "arguments": [
                {"name": "amount", "dataType": {"typeKind": "NUMERIC"}},
                {"name": "rate", "dataType": {"typeKind": "FLOAT64"}}
            ],
            "returnType": {"typeKind": "NUMERIC"},
            "definitionBody": "amount * (1 + rate)",
            "description": "Adds 'tax'"
        })
    }

    #[test]
    fn maps_routine_kinds_to_host_types() {
        assert_eq!(routine_info(&scalar()).unwrap()["routine_type"], "FUNCTION");
        let procedure = json!({"routineReference": {"routineId": "p"}, "routineType": "PROCEDURE"});
        assert_eq!(
            routine_info(&procedure).unwrap()["routine_type"],
            "PROCEDURE"
        );
        let tvf =
            json!({"routineReference": {"routineId": "t"}, "routineType": "TABLE_VALUED_FUNCTION"});
        assert_eq!(routine_info(&tvf).unwrap()["routine_type"], "FUNCTION");
        assert_eq!(
            RoutineKind::from_name("TABLE FUNCTION"),
            RoutineKind::TableFunction
        );
        assert_eq!(
            drop_routine_sql(RoutineKind::TableFunction, "`p-1`.`d`.`t`"),
            "DROP TABLE FUNCTION IF EXISTS `p-1`.`d`.`t`"
        );
        assert_eq!(
            drop_routine_sql(RoutineKind::AggregateFunction, "`p`.`d`.`a`"),
            "DROP FUNCTION IF EXISTS `p`.`d`.`a`"
        );
    }

    #[test]
    fn parameters_have_positions_and_a_named_return_row() {
        let parameters = routine_parameters(&scalar());
        assert_eq!(
            parameters,
            vec![
                json!({"name": "", "data_type": "NUMERIC", "mode": "OUT", "ordinal_position": 0}),
                json!({"name": "amount", "data_type": "NUMERIC", "mode": "IN", "ordinal_position": 1}),
                json!({"name": "rate", "data_type": "FLOAT64", "mode": "IN", "ordinal_position": 2}),
            ]
        );
        let procedure = json!({
            "routineType": "PROCEDURE",
            "arguments": [
                {"name": "x", "mode": "INOUT", "dataType": {"typeKind": "ARRAY", "arrayElementType": {"typeKind": "STRING"}}},
                {"argumentKind": "ANY_TYPE", "mode": "OUT"}
            ]
        });
        assert_eq!(
            routine_parameters(&procedure),
            vec![
                json!({"name": "x", "data_type": "ARRAY<STRING>", "mode": "INOUT", "ordinal_position": 1}),
                json!({"name": "arg2", "data_type": "ANY TYPE", "mode": "OUT", "ordinal_position": 2}),
            ]
        );
        let inferred = json!({"routineType": "SCALAR_FUNCTION", "arguments": []});
        assert!(routine_parameters(&inferred).is_empty());
    }

    #[test]
    fn rebuilds_sql_and_javascript_functions() {
        assert_eq!(
            routine_ddl(&scalar(), "`my-proj`.`d`.`add_tax`", true).unwrap(),
            "CREATE OR REPLACE FUNCTION `my-proj`.`d`.`add_tax`(`amount` NUMERIC, `rate` FLOAT64)\nRETURNS NUMERIC\nAS (\n  amount * (1 + rate)\n) OPTIONS(description = 'Adds \\'tax\\'')"
        );
        let js = json!({
            "routineType": "SCALAR_FUNCTION",
            "language": "JAVASCRIPT",
            "arguments": [{"name": "s", "dataType": {"typeKind": "STRING"}}],
            "returnType": {"typeKind": "STRING"},
            "determinismLevel": "DETERMINISTIC",
            "importedLibraries": ["gs://bucket/lib.js"],
            "definitionBody": "return s + \"\\n\";"
        });
        assert_eq!(
            routine_ddl(&js, "`p`.`d`.`f`", true).unwrap(),
            "CREATE OR REPLACE FUNCTION `p`.`d`.`f`(`s` STRING)\nRETURNS STRING\nDETERMINISTIC\nLANGUAGE js OPTIONS(library = ['gs://bucket/lib.js'])\nAS r\"\"\"return s + \"\\n\";\"\"\""
        );
    }

    #[test]
    fn rebuilds_table_functions_procedures_and_aggregates() {
        let tvf = json!({
            "routineType": "TABLE_VALUED_FUNCTION",
            "arguments": [{"name": "min_id", "dataType": {"typeKind": "INT64"}}],
            "returnTableType": {"columns": [{"name": "id", "type": {"typeKind": "INT64"}}]},
            "definitionBody": "SELECT id FROM `p`.`d`.`t` WHERE id >= min_id"
        });
        assert_eq!(
            routine_ddl(&tvf, "`p`.`d`.`tvf`", true).unwrap(),
            "CREATE OR REPLACE TABLE FUNCTION `p`.`d`.`tvf`(`min_id` INT64)\nRETURNS TABLE<`id` INT64>\nAS\nSELECT id FROM `p`.`d`.`t` WHERE id >= min_id"
        );
        let procedure = json!({
            "routineType": "PROCEDURE",
            "strictMode": false,
            "arguments": [
                {"name": "a", "mode": "IN", "dataType": {"typeKind": "INT64"}},
                {"name": "b", "mode": "OUT", "dataType": {"typeKind": "STRUCT", "structType": {"fields": [{"name": "x", "type": {"typeKind": "DATE"}}]}}}
            ],
            "definitionBody": "BEGIN\n  SET b = STRUCT(CURRENT_DATE());\nEND"
        });
        assert_eq!(
            routine_ddl(&procedure, "`p`.`d`.`proc`", true).unwrap(),
            "CREATE OR REPLACE PROCEDURE `p`.`d`.`proc`(IN `a` INT64, OUT `b` STRUCT<`x` DATE>) OPTIONS(strict_mode = FALSE)\nBEGIN\n  SET b = STRUCT(CURRENT_DATE());\nEND"
        );
        let aggregate = json!({
            "routineType": "AGGREGATE_FUNCTION",
            "arguments": [
                {"name": "v", "dataType": {"typeKind": "FLOAT64"}},
                {"name": "w", "isAggregate": false, "dataType": {"typeKind": "FLOAT64"}}
            ],
            "returnType": {"typeKind": "FLOAT64"},
            "definitionBody": "SUM(v) * w"
        });
        assert_eq!(
            routine_ddl(&aggregate, "`p`.`d`.`agg`", false).unwrap(),
            "CREATE AGGREGATE FUNCTION `p`.`d`.`agg`(`v` FLOAT64, `w` FLOAT64 NOT AGGREGATE)\nRETURNS FLOAT64\nAS (\n  SUM(v) * w\n)"
        );
    }

    #[test]
    fn body_literals_stay_valid() {
        assert_eq!(body_literal("return 1;"), "r\"\"\"return 1;\"\"\"");
        assert_eq!(
            body_literal("x = \"\"\"; y\\"),
            "\"\"\"x = \\\"\\\"\\\"; y\\\\\"\"\""
        );
        assert_eq!(body_literal("say \"hi\""), "\"\"\"say \\\"hi\\\"\"\"\"");
    }

    #[test]
    fn builds_calls_with_typed_arguments() {
        let args = vec![
            CallArg {
                name: "amount".into(),
                mode: "IN".into(),
                value: Some("10.5".into()),
                is_raw: false,
            },
            CallArg {
                name: "rate".into(),
                mode: "IN".into(),
                value: None,
                is_raw: false,
            },
        ];
        assert_eq!(
            routine_call_sql(&scalar(), "`my-proj`.`d`.`add_tax`", &args).unwrap(),
            "SELECT `my-proj`.`d`.`add_tax`(NUMERIC '10.5', NULL) AS result;"
        );
        let bad = vec![CallArg {
            name: "amount".into(),
            mode: "IN".into(),
            value: Some("ten".into()),
            is_raw: false,
        }];
        assert!(routine_call_sql(&scalar(), "`p`.`d`.`f`", &bad).is_err());

        let tvf = json!({
            "routineType": "TABLE_VALUED_FUNCTION",
            "arguments": [{"name": "label", "dataType": {"typeKind": "STRING"}}]
        });
        let args = vec![CallArg {
            name: "label".into(),
            mode: "IN".into(),
            value: Some("it's\nme".into()),
            is_raw: false,
        }];
        assert_eq!(
            routine_call_sql(&tvf, "`p`.`d`.`tvf`", &args).unwrap(),
            "SELECT *\nFROM `p`.`d`.`tvf`('it\\'s\\nme');"
        );

        let procedure = json!({
            "routineType": "PROCEDURE",
            "arguments": [
                {"name": "a", "mode": "IN", "dataType": {"typeKind": "INT64"}},
                {"name": "b", "mode": "INOUT", "dataType": {"typeKind": "STRING"}},
                {"name": "c", "mode": "OUT", "dataType": {"typeKind": "INT64"}}
            ]
        });
        let args = vec![
            CallArg {
                name: "a".into(),
                mode: "IN".into(),
                value: Some("1".into()),
                is_raw: false,
            },
            CallArg {
                name: "b".into(),
                mode: "INOUT".into(),
                value: Some("x".into()),
                is_raw: false,
            },
            CallArg {
                name: "c".into(),
                mode: "OUT".into(),
                value: None,
                is_raw: false,
            },
        ];
        assert_eq!(
            routine_call_sql(&procedure, "`p`.`d`.`proc`", &args).unwrap(),
            "DECLARE tabularis_arg_2 STRING DEFAULT 'x';\nDECLARE tabularis_arg_3 INT64;\nCALL `p`.`d`.`proc`(1, tabularis_arg_2, tabularis_arg_3);\nSELECT tabularis_arg_2 AS `b`, tabularis_arg_3 AS `c`;"
        );
        let raw = vec![CallArg {
            name: "a".into(),
            mode: "IN".into(),
            value: Some("1 + 1".into()),
            is_raw: true,
        }];
        assert_eq!(
            routine_call_sql(&procedure, "`p`.`d`.`proc`", &raw).unwrap(),
            "CALL `p`.`d`.`proc`(1 + 1);"
        );
    }

    #[test]
    fn templates_are_fully_qualified() {
        let procedure = create_template("PROCEDURE", Some("my-proj"), Some("d"));
        assert!(procedure.starts_with("CREATE OR REPLACE PROCEDURE `my-proj`.`d`.`my_procedure`("));
        assert!(procedure.contains("BEGIN") && procedure.trim_end().ends_with("END;"));
        let function = create_template("FUNCTION", None, Some("d"));
        assert!(function.starts_with("CREATE OR REPLACE FUNCTION `d`.`my_function`("));
        let tvf = create_template("TABLE_VALUED_FUNCTION", Some("p"), Some("d"));
        assert!(tvf.starts_with("CREATE OR REPLACE TABLE FUNCTION `p`.`d`.`my_table_function`"));
    }
}
