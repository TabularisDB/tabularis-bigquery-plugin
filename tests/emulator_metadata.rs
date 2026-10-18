//! Live metadata, DDL and CRUD checks against the BigQuery emulator
//! (`ghcr.io/goccy/bigquery-emulator`).
//!
//! Skipped unless `BIGQUERY_EMULATOR_HOST` is set, for example:
//!
//! ```bash
//! docker run -d -p 9050:9050 ghcr.io/goccy/bigquery-emulator:latest --project=test --dataset=ds
//! BIGQUERY_EMULATOR_HOST=localhost:9050 cargo test --test emulator_metadata
//! ```
//!
//! The emulator implements a subset of BigQuery: `@@row_count`,
//! `INFORMATION_SCHEMA.TABLES.ddl`, informational keys and the routines API
//! are missing, so those paths fall back or are not exercised here.

mod support;

use serde_json::{json, Value};
use support::{call, unique_name, EMULATOR_DATASET, EMULATOR_PROJECT};

struct Emulator {
    params: Value,
}

impl Emulator {
    fn connect() -> Option<Self> {
        support::emulator_connection().map(|params| Self { params })
    }

    async fn call(&self, method: &str, mut arguments: Value) -> Result<Value, String> {
        arguments["params"] = self.params.clone();
        if arguments.get("schema").is_none() {
            arguments["schema"] = json!(EMULATOR_DATASET);
        }
        let response = call(method, arguments).await;
        match response.get("error") {
            Some(error) => Err(error["message"].as_str().unwrap_or_default().to_string()),
            None => Ok(response["result"].clone()),
        }
    }

    async fn query(&self, sql: &str) -> Result<Value, String> {
        self.call(
            "execute_query",
            json!({"query": sql, "limit": 100, "page": 1}),
        )
        .await
    }
}

fn path(table: &str) -> String {
    format!("`{EMULATOR_PROJECT}`.`{EMULATOR_DATASET}`.`{table}`")
}

/// The emulator drops informational primary keys, so row edits take the
/// keyless path, whose transaction guard uses `@@row_count`, which the
/// emulator does not implement.
fn assert_dml(result: Result<Value, String>, expected: u64) {
    match result {
        Ok(value) => assert_eq!(value.as_u64(), Some(expected)),
        Err(error) if error.contains("@@row_count") => {
            eprintln!("emulator lacks @@row_count; keyless edit guard not exercised")
        }
        Err(error) => panic!("DML failed: {error}"),
    }
}

#[tokio::test]
async fn metadata_and_crud_round_trip() {
    let Some(emulator) = Emulator::connect() else {
        eprintln!("BIGQUERY_EMULATOR_HOST is not set; skipping emulator test");
        return;
    };
    let table = unique_name("metadata_items");
    let table = table.as_str();
    let path = path(table);
    let _ = emulator
        .query(&format!("DROP TABLE IF EXISTS {path}"))
        .await;
    emulator
        .query(&format!(
            "CREATE TABLE {path} (id INT64 NOT NULL, name STRING, price NUMERIC, note STRING, PRIMARY KEY (id) NOT ENFORCED)"
        ))
        .await
        .expect("create table");

    let databases = emulator.call("get_databases", json!({})).await.unwrap();
    assert!(databases
        .as_array()
        .unwrap()
        .iter()
        .any(|project| project == EMULATOR_PROJECT));

    let tables = emulator.call("get_tables", json!({})).await.unwrap();
    assert!(tables
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == table));

    let columns = emulator
        .call("get_columns", json!({"table": table}))
        .await
        .unwrap();
    assert_eq!(columns.as_array().unwrap().len(), 4);
    assert_eq!(columns[0]["name"], "id");
    assert_eq!(columns[2]["data_type"], "NUMERIC");

    let inserted = emulator
        .call(
            "insert_record",
            json!({
                "table": table,
                "data": {"id": 1, "name": "O'Reilly\nline", "price": "12.50", "note": "a\\b"}
            }),
        )
        .await
        .expect("insert");
    assert!(inserted.as_u64().is_some());

    let rows = emulator
        .query(&format!(
            "SELECT name, CAST(price AS STRING), note FROM {path} WHERE id = 1"
        ))
        .await
        .unwrap();
    assert_eq!(rows["rows"][0][0], "O'Reilly\nline");
    assert_eq!(rows["rows"][0][2], "a\\b");

    let identity = json!({"id": 1});
    let updated = emulator
        .call(
            "update_record",
            json!({"table": table, "pk_map": identity, "col_name": "note", "new_val": "tab\there"}),
        )
        .await;
    assert_dml(updated, 1);

    let snapshot = emulator
        .call("get_schema_snapshot", json!({}))
        .await
        .unwrap();
    assert!(snapshot
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == table));
    let batch = emulator
        .call("get_all_columns_batch", json!({}))
        .await
        .unwrap();
    assert_eq!(batch[table].as_array().unwrap().len(), 4);

    let view = unique_name("metadata_view");
    let view = view.as_str();
    let created = emulator
        .call(
            "create_view",
            json!({"view_name": view, "definition": format!("SELECT id, name FROM {path}")}),
        )
        .await;
    created.expect("create view");
    let definition = emulator
        .call("get_view_definition", json!({"view_name": view}))
        .await;
    assert!(definition
        .expect("view definition")
        .as_str()
        .unwrap()
        .contains(view));
    let _ = emulator.call("drop_view", json!({"view_name": view})).await;

    let deleted = emulator
        .call(
            "delete_record",
            json!({"table": table, "pk_map": {"id": 1}}),
        )
        .await;
    assert_dml(deleted, 1);

    let ddl = emulator
        .call("get_table_ddl", json!({"table": table}))
        .await
        .expect("table ddl");
    assert!(ddl.as_str().unwrap().contains(table));

    let drop = emulator
        .call(
            "get_drop_table_sql",
            json!({"table": table, "database": EMULATOR_PROJECT}),
        )
        .await
        .expect("drop table sql");
    assert_eq!(drop, json!(format!("DROP TABLE IF EXISTS {path}")));
    emulator
        .query(drop.as_str().unwrap())
        .await
        .expect("drop table");
}

#[tokio::test]
async fn typed_parameters_round_trip() {
    let Some(emulator) = Emulator::connect() else {
        eprintln!("BIGQUERY_EMULATOR_HOST is not set; skipping emulator test");
        return;
    };
    let table = unique_name("metadata_types");
    let table = table.as_str();
    let path = path(table);
    let _ = emulator
        .query(&format!("DROP TABLE IF EXISTS {path}"))
        .await;
    emulator
        .query(&format!(
            "CREATE TABLE {path} (id INT64, flag BOOL, day DATE, ts TIMESTAMP, ratio FLOAT64, \
             blob BYTES, doc JSON, place GEOGRAPHY, ids ARRAY<INT64>, \
             item STRUCT<sku STRING, qty INT64>, big BIGNUMERIC)"
        ))
        .await
        .expect("create table");
    emulator
        .call(
            "insert_record",
            json!({
                "table": table,
                "data": {
                    "id": 7,
                    "flag": true,
                    "day": "2024-02-29",
                    "ts": "2024-01-02T03:04:05.123456Z",
                    "ratio": 0.5,
                    "blob": "BLOB:3:application/octet-stream:AQID",
                    "doc": {"k": "v'\n"},
                    "place": "POINT(1 2)",
                    "ids": [1, 2, 3],
                    "item": {"sku": "a'b", "qty": 2},
                    "big": "123456789012345678901234567890.123456789"
                }
            }),
        )
        .await
        .expect("insert typed row");
    let rows = emulator
        .query(&format!(
            "SELECT flag, CAST(day AS STRING), TO_BASE64(blob), JSON_VALUE(doc, '$.k'), \
             ARRAY_LENGTH(ids), item.sku, CAST(big AS STRING) FROM {path} WHERE id = 7"
        ))
        .await
        .unwrap();
    let row = &rows["rows"][0];
    assert_eq!(row[0], true);
    assert_eq!(row[1], "2024-02-29");
    assert_eq!(row[2], "AQID");
    assert_eq!(row[3], "v'\n");
    assert_eq!(row[4], 3);
    assert_eq!(row[5], "a'b");
    assert_eq!(row[6], "123456789012345678901234567890.123456789");
    let _ = emulator
        .query(&format!("DROP TABLE IF EXISTS {path}"))
        .await;
}
