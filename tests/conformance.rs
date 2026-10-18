//! RPC result conformance against the Tabularis host models.
//!
//! The model definitions below are copied from
//! `tabularis/src-tauri/src/models.rs` at host commit
//! `27a2489c656bc95817bf316fbaec1fda99087613` (doc comments trimmed). The
//! host deserializes plugin results with serde into these exact types, so a
//! result that fails here fails in Tabularis. Inputs are recorded BigQuery
//! REST resources in `tests/fixtures/rest/`.

#![allow(dead_code)]

use std::collections::HashMap;

use bigquery_plugin::handlers::metadata::{
    columns_from_resource, foreign_keys_from_resource, indexes_from_resource, merge_projects,
};
use bigquery_plugin::routines::{
    create_template, routine_call_sql, routine_ddl, routine_info, routine_parameters, CallArg,
};
use bigquery_plugin::rpc::handle_line;
use bigquery_plugin::schema_ddl::table_ddl;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

// BEGIN host model definitions.

#[derive(Debug, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TableColumn {
    pub name: String,
    pub data_type: String,
    pub is_pk: bool,
    pub is_nullable: bool,
    pub is_auto_increment: bool,
    #[serde(default)]
    pub is_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_maximum_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric_precision: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric_scale: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub column_name: String,
    pub ref_table: String,
    pub ref_column: String,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq_in_fk: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub column_name: String,
    pub is_unique: bool,
    pub is_primary: bool,
    pub seq_in_index: i32,
    #[serde(default)]
    pub is_expression: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<TableColumn>,
    pub foreign_keys: Vec<ForeignKey>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutineInfo {
    pub name: String,
    pub routine_type: String,
    pub definition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_args: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutineParameter {
    pub name: String,
    pub data_type: String,
    pub mode: String,
    pub ordinal_position: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoutineCallArg {
    pub name: String,
    pub mode: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub is_raw: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_pk: bool,
    pub is_auto_increment: bool,
    pub default_value: Option<String>,
}

// END host model definitions.

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/rest/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn host<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("host model rejected {value}: {error}"))
}

fn routine(name: &str) -> Value {
    fixture("routines.json")
        .as_array()
        .unwrap()
        .iter()
        .find(|routine| routine["routineReference"]["routineId"] == name)
        .cloned()
        .unwrap()
}

async fn rpc(method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "method": method, "params": params, "id": 1});
    handle_line(&request.to_string()).await
}

#[test]
fn columns_match_host_table_column() {
    let columns: Vec<TableColumn> = host(json!(columns_from_resource(&fixture(
        "table_orders.json"
    ))
    .unwrap()));
    let by_name = columns
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect::<HashMap<_, _>>();
    assert_eq!(columns.len(), 10);
    assert!(by_name["order_id"].is_pk && by_name["line_id"].is_pk);
    assert!(!by_name["order_id"].is_nullable);
    assert_eq!(by_name["order_id"].comment.as_deref(), Some("Order key"));
    assert_eq!(by_name["status"].data_type, "STRING(16)");
    assert_eq!(by_name["status"].character_maximum_length, Some(16));
    assert_eq!(by_name["status"].default_value.as_deref(), Some("'new'"));
    assert_eq!(by_name["amount"].numeric_precision, Some(12));
    assert_eq!(by_name["amount"].numeric_scale, Some(2));
    assert_eq!(by_name["validity"].data_type, "RANGE<DATE>");
    assert_eq!(
        by_name["items"].data_type,
        "ARRAY<STRUCT<sku STRING, qty INT64>>"
    );
}

#[test]
fn foreign_keys_and_indexes_match_host_models() {
    let resource = fixture("table_orders.json");
    let keys: Vec<ForeignKey> = host(json!(foreign_keys_from_resource(&resource, "sales")));
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0].name, "fk_customer");
    assert_eq!(keys[0].ref_table, "customers");
    assert_eq!(keys[1].name, "fk_order_lines");
    assert_eq!(
        keys[1..]
            .iter()
            .map(|key| key.seq_in_fk)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
    let indexes: Vec<Index> = host(json!(indexes_from_resource(&resource)));
    assert_eq!(indexes.len(), 2);
    assert!(indexes
        .iter()
        .all(|index| index.is_primary && index.name == "PRIMARY"));
    assert_eq!(indexes[1].seq_in_index, 2);
}

#[test]
fn schema_snapshot_rows_match_host_table_schema() {
    let resource = fixture("table_orders.json");
    let snapshot: TableSchema = host(json!({
        "name": "orders",
        "columns": columns_from_resource(&resource).unwrap(),
        "foreign_keys": foreign_keys_from_resource(&resource, "sales")
    }));
    assert_eq!(snapshot.columns.len(), 10);
}

#[test]
fn routines_match_host_routine_info() {
    let routines = fixture("routines.json")
        .as_array()
        .unwrap()
        .iter()
        .filter_map(routine_info)
        .collect::<Vec<_>>();
    let routines: Vec<RoutineInfo> = host(json!(routines));
    let types = routines
        .iter()
        .map(|routine| (routine.name.as_str(), routine.routine_type.as_str()))
        .collect::<HashMap<_, _>>();
    assert_eq!(types["close_orders"], "PROCEDURE");
    assert_eq!(types["orders_since"], "FUNCTION");
    assert_eq!(types["weighted_avg"], "FUNCTION");
    assert!(routines
        .iter()
        .all(|routine| routine.identity_args.is_none()));
}

#[test]
fn routine_parameters_match_host_shape() {
    for resource in fixture("routines.json").as_array().unwrap() {
        let parameters: Vec<RoutineParameter> = host(json!(routine_parameters(resource)));
        let arguments = resource["arguments"].as_array().map_or(0, Vec::len);
        let calls = parameters
            .iter()
            .filter(|parameter| parameter.ordinal_position >= 1)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), arguments);
        for (index, parameter) in calls.iter().enumerate() {
            assert_eq!(parameter.ordinal_position, index as i32 + 1);
            assert!(!parameter.name.is_empty());
        }
        for parameter in parameters
            .iter()
            .filter(|parameter| parameter.ordinal_position == 0)
        {
            assert_eq!(parameter.name, "");
            assert_eq!(parameter.mode, "OUT");
        }
    }
    let tvf: Vec<RoutineParameter> = host(json!(routine_parameters(&routine("orders_since"))));
    assert_eq!(
        tvf[0].data_type,
        "TABLE<`order_id` INT64, `items` ARRAY<STRUCT<`sku` STRING>>>"
    );
    let procedure: Vec<RoutineParameter> =
        host(json!(routine_parameters(&routine("close_orders"))));
    assert_eq!(
        procedure
            .iter()
            .map(|parameter| parameter.mode.as_str())
            .collect::<Vec<_>>(),
        vec!["IN", "OUT", "INOUT"]
    );
}

#[test]
fn routine_scripts_are_fully_qualified() {
    let path = "`my-project-1`.`sales`.`close_orders`";
    let ddl = routine_ddl(&routine("close_orders"), path, true).unwrap();
    assert!(ddl.starts_with(
        "CREATE OR REPLACE PROCEDURE `my-project-1`.`sales`.`close_orders`(IN `before` DATE, OUT `closed` INT64, INOUT `note` STRING)\nBEGIN"
    ));
    let host_args: Vec<RoutineCallArg> = host(json!([
        {"name": "before", "mode": "IN", "value": "2024-01-01", "is_raw": false},
        {"name": "closed", "mode": "OUT", "value": null, "is_raw": false},
        {"name": "note", "mode": "INOUT", "value": "it's done", "is_raw": false}
    ]));
    let args: Vec<CallArg> = serde_json::from_value(json!(host_args)).unwrap();
    assert_eq!(
        routine_call_sql(&routine("close_orders"), path, &args).unwrap(),
        "DECLARE tabularis_arg_2 INT64;\nDECLARE tabularis_arg_3 STRING DEFAULT 'it\\'s done';\nCALL `my-project-1`.`sales`.`close_orders`(DATE '2024-01-01', tabularis_arg_2, tabularis_arg_3);\nSELECT tabularis_arg_2 AS `closed`, tabularis_arg_3 AS `note`;"
    );
    let js = routine_ddl(
        &routine("upper_js"),
        "`my-project-1`.`sales`.`upper_js`",
        true,
    )
    .unwrap();
    assert!(js.contains("LANGUAGE js") && js.ends_with("AS r\"\"\"return s.toUpperCase();\"\"\""));
    let inferred = routine_ddl(&routine("inferred"), "`p`.`d`.`inferred`", true).unwrap();
    assert!(
        inferred.starts_with("CREATE OR REPLACE FUNCTION `p`.`d`.`inferred`(`x` ANY TYPE)\nAS (")
    );
    assert!(
        create_template("FUNCTION", Some("my-project-1"), Some("sales"))
            .starts_with("CREATE OR REPLACE FUNCTION `my-project-1`.`sales`.")
    );
}

#[test]
fn generated_table_ddl_preserves_layout() {
    let ddl = table_ddl(
        &fixture("table_orders.json"),
        "my-project-1",
        "sales",
        "orders",
    )
    .unwrap();
    assert!(ddl.starts_with("CREATE TABLE `my-project-1`.`sales`.`orders` ("));
    assert!(ddl.contains("PRIMARY KEY (`order_id`, `line_id`) NOT ENFORCED"));
    assert!(ddl.contains(
        "FOREIGN KEY (`order_id`, `line_id`) REFERENCES `my-project-1`.`sales`.`order_lines`(`order_id`, `line_id`) NOT ENFORCED"
    ));
    assert!(ddl.contains("\nPARTITION BY DATE(`placed_at`)\nCLUSTER BY `customer_id`"));
    assert!(ddl.contains("OPTIONS(description = 'Customer orders')"));
}

#[test]
fn databases_are_a_string_list() {
    let databases: Vec<String> = host(json!(merge_projects(
        "my-project-1",
        "my-project-1",
        vec!["other-project".into()],
        vec!["bigquery-public-data".into()]
    )));
    assert_eq!(
        databases,
        vec!["bigquery-public-data", "my-project-1", "other-project"]
    );
}

#[tokio::test]
async fn sql_builders_return_host_shapes() {
    let column = ColumnDefinition {
        name: "note".into(),
        data_type: "STRING".into(),
        is_nullable: true,
        is_pk: false,
        is_auto_increment: false,
        default_value: None,
    };
    let response = rpc(
        "get_create_table_sql",
        json!({"table_name": "events", "columns": [column], "schema": "analytics"}),
    )
    .await;
    let statements: Vec<String> = host(response["result"].clone());
    assert_eq!(
        statements,
        vec!["CREATE TABLE `analytics`.`events` (\n  `note` STRING\n)"]
    );

    let response = rpc(
        "get_create_table_sql",
        json!({
            "table_name": "events",
            "schema": "analytics",
            "columns": [column],
            "params": {"driver": "bigquery", "database": "my-project-1"}
        }),
    )
    .await;
    let statements: Vec<String> = host(response["result"].clone());
    assert_eq!(
        statements,
        vec!["CREATE TABLE `my-project-1`.`analytics`.`events` (\n  `note` STRING\n)"]
    );

    let response = rpc(
        "get_add_column_sql",
        json!({"table": "events", "schema": "analytics", "column": column}),
    )
    .await;
    let statements: Vec<String> = host(response["result"].clone());
    assert_eq!(
        statements,
        vec!["ALTER TABLE `analytics`.`events` ADD COLUMN `note` STRING"]
    );

    let mut required = column.clone();
    required.is_nullable = false;
    let response = rpc(
        "get_add_column_sql",
        json!({"table": "events", "schema": "analytics", "column": required}),
    )
    .await;
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("NOT NULL"));

    let mut widened = column.clone();
    widened.data_type = "INT64".into();
    let response = rpc(
        "get_alter_column_sql",
        json!({"table": "events", "schema": "analytics", "old_column": column, "new_column": widened}),
    )
    .await;
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cannot change a column from STRING to INT64"));

    let response = rpc(
        "routine_create_template",
        json!({"routine_type": "PROCEDURE", "schema": "sales"}),
    )
    .await;
    let template: String = host(response["result"].clone());
    assert!(template.starts_with("CREATE OR REPLACE PROCEDURE `sales`.`my_procedure`("));

    let response = rpc("get_create_index_sql", json!({"table": "t", "index_name": "i", "columns": ["a"], "is_unique": false, "schema": "d"})).await;
    assert_eq!(response["error"]["code"], -32601);
}

#[tokio::test]
async fn routine_management_methods_are_routed() {
    for method in [
        "get_routine_edit_script",
        "build_routine_call_sql",
        "drop_routine",
        "get_table_ddl",
    ] {
        let response = rpc(method, json!({"params": {"driver": "bigquery"}})).await;
        assert_ne!(
            response["error"]["code"], -32601,
            "{method} must be implemented"
        );
    }
}
