//! DDL builder requests in the exact shape the Tabularis host sends since the
//! DDL connection context (host commit
//! `03bee207fe62a64d482dbbc18e4b688203c7fec3`, `plugins/driver.rs`): every
//! builder carries `params` with the browsed project applied, columns may
//! carry `comment`, and the explorer asks `get_drop_table_sql` for the drop
//! statement.

mod support;

use serde_json::{json, Value};
use support::{call, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `ConnectionParams` as the host serializes it: browsed project in
/// `database`, the billing project and plugin settings in `extra`.
fn host_params(endpoint: &str) -> Value {
    json!({
        "driver": "bigquery",
        "host": null,
        "port": null,
        "username": null,
        "password": null,
        "database": "analytics-project",
        "ssl_mode": null,
        "ssl_ca": null,
        "ssl_cert": null,
        "ssl_key": null,
        "ssh_enabled": null,
        "ssh_connection_id": null,
        "save_in_keychain": null,
        "k8s_enabled": null,
        "k8s_connection_id": null,
        "ssm_enabled": null,
        "connection_id": "conn-1",
        "extra": {
            "auth_method": "none",
            "api_endpoint": endpoint,
            "billing_project": "billing-project"
        }
    })
}

/// Host `ColumnDefinition`: `comment` is omitted when unset.
fn column(name: &str, data_type: &str, comment: Option<&str>) -> Value {
    let mut column = json!({
        "name": name,
        "data_type": data_type,
        "is_nullable": true,
        "is_pk": false,
        "is_auto_increment": false,
        "default_value": null
    });
    if let Some(comment) = comment {
        column["comment"] = json!(comment);
    }
    column
}

fn statements(response: &Value) -> Vec<String> {
    serde_json::from_value(result(response).clone()).unwrap()
}

/// Endpoint for builders that never call BigQuery.
const UNUSED: &str = "http://127.0.0.1:9";

#[tokio::test]
async fn create_table_is_project_qualified_with_column_descriptions() {
    let response = call(
        "get_create_table_sql",
        json!({
            "params": host_params(UNUSED),
            "table_name": "events",
            "columns": [column("id", "INT64", None), column("note", "STRING", Some("Free 'text'"))],
            "schema": "sales"
        }),
    )
    .await;
    assert_eq!(
        statements(&response),
        vec![
            "CREATE TABLE `analytics-project`.`sales`.`events` (\n  \
             `id` INT64,\n  \
             `note` STRING OPTIONS(description = 'Free \\'text\\'')\n)"
        ]
    );
}

#[tokio::test]
async fn add_column_is_project_qualified_with_description() {
    // Column changes look the table up first; an unknown table is left to
    // BigQuery.
    let server = MockServer::start().await;
    let response = call(
        "get_add_column_sql",
        json!({
            "params": host_params(&server.uri()),
            "table": "events",
            "column": column("note", "STRING", Some("Free text")),
            "schema": "sales"
        }),
    )
    .await;
    assert_eq!(
        statements(&response),
        vec!["ALTER TABLE `analytics-project`.`sales`.`events` ADD COLUMN `note` STRING OPTIONS(description = 'Free text')"]
    );
}

#[tokio::test]
async fn alter_column_sets_and_clears_the_description() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/bigquery/v2/projects/analytics-project/datasets/sales/tables/events",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "TABLE",
            "schema": {"fields": [{"name": "note", "type": "STRING", "mode": "NULLABLE"}]}
        })))
        .mount(&server)
        .await;
    let params = host_params(&server.uri());

    let response = call(
        "get_alter_column_sql",
        json!({
            "params": params,
            "table": "events",
            "old_column": column("note", "STRING", None),
            "new_column": column("note", "STRING", Some("Primary contact")),
            "schema": "sales"
        }),
    )
    .await;
    assert_eq!(
        statements(&response),
        vec!["ALTER TABLE `analytics-project`.`sales`.`events` ALTER COLUMN `note` SET OPTIONS(description = 'Primary contact')"]
    );

    let response = call(
        "get_alter_column_sql",
        json!({
            "params": params,
            "table": "events",
            "old_column": column("note", "STRING", Some("Primary contact")),
            "new_column": column("note", "STRING", None),
            "schema": "sales"
        }),
    )
    .await;
    assert_eq!(
        statements(&response),
        vec!["ALTER TABLE `analytics-project`.`sales`.`events` ALTER COLUMN `note` SET OPTIONS(description = NULL)"]
    );
}

#[tokio::test]
async fn foreign_keys_reference_the_browsed_project() {
    let response = call(
        "get_create_foreign_key_sql",
        json!({
            "params": host_params(UNUSED),
            "table": "orders",
            "fk_name": "fk_user",
            "column": "user_id",
            "ref_table": "users",
            "ref_column": "id",
            "on_delete": null,
            "on_update": null,
            "schema": "sales"
        }),
    )
    .await;
    assert_eq!(
        statements(&response),
        vec!["ALTER TABLE `analytics-project`.`sales`.`orders` ADD CONSTRAINT `fk_user` FOREIGN KEY (`user_id`) REFERENCES `analytics-project`.`sales`.`users`(`id`) NOT ENFORCED"]
    );
}

#[tokio::test]
async fn routine_template_and_index_requests_carry_the_context() {
    let response = call(
        "routine_create_template",
        json!({"params": host_params(UNUSED), "routine_type": "PROCEDURE", "schema": "sales"}),
    )
    .await;
    let template = result(&response).as_str().unwrap();
    assert!(
        template
            .starts_with("CREATE OR REPLACE PROCEDURE `analytics-project`.`sales`.`my_procedure`("),
        "{template}"
    );
    assert!(template.contains("FROM `analytics-project`.`sales`.`my_table`"));

    let response = call(
        "get_create_index_sql",
        json!({
            "params": host_params(UNUSED),
            "table": "events",
            "index_name": "idx",
            "columns": ["note"],
            "is_unique": false,
            "schema": "sales"
        }),
    )
    .await;
    assert_eq!(response["error"]["code"], -32601);
}

#[tokio::test]
async fn all_databases_mode_falls_back_to_the_billing_project() {
    let server = MockServer::start().await;
    let mut params = host_params(&server.uri());
    params["database"] = json!("");
    let response = call(
        "get_add_column_sql",
        json!({"params": params, "table": "events", "column": column("n", "INT64", None), "schema": "sales"}),
    )
    .await;
    assert_eq!(
        statements(&response),
        vec!["ALTER TABLE `billing-project`.`sales`.`events` ADD COLUMN `n` INT64"]
    );
}

async fn mount_kind(server: &MockServer, table: &str, kind: Option<&str>) {
    let route = format!("/bigquery/v2/projects/analytics-project/datasets/sales/tables/{table}");
    let response = match kind {
        Some(kind) => ResponseTemplate::new(200).set_body_json(json!({
            "type": kind,
            "tableReference": {"projectId": "analytics-project", "datasetId": "sales", "tableId": table}
        })),
        None => ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": 404, "message": "Not found", "errors": [{"reason": "notFound"}]}
        })),
    };
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

#[tokio::test]
async fn drop_table_uses_the_object_kind_keyword() {
    let server = MockServer::start().await;
    let cases = [
        ("plain", Some("TABLE"), "DROP TABLE IF EXISTS"),
        ("ext", Some("EXTERNAL"), "DROP EXTERNAL TABLE IF EXISTS"),
        (
            "daily_snapshot",
            Some("SNAPSHOT"),
            "DROP SNAPSHOT TABLE IF EXISTS",
        ),
        ("v", Some("VIEW"), "DROP VIEW IF EXISTS"),
        (
            "mv",
            Some("MATERIALIZED_VIEW"),
            "DROP MATERIALIZED VIEW IF EXISTS",
        ),
        ("missing", None, "DROP TABLE IF EXISTS"),
    ];
    for (table, kind, _) in cases {
        mount_kind(&server, table, kind).await;
    }
    for (table, _, keyword) in cases {
        let response = call(
            "get_drop_table_sql",
            json!({
                "params": host_params(&server.uri()),
                "table": table,
                "schema": "sales",
                "database": "analytics-project"
            }),
        )
        .await;
        assert_eq!(
            result(&response),
            &json!(format!("{keyword} `analytics-project`.`sales`.`{table}`")),
            "{table}"
        );
    }
}

#[tokio::test]
async fn drop_table_without_context_falls_back_to_the_host() {
    for request in [
        json!({"table": "t", "schema": "sales", "database": null}),
        json!({"params": host_params(UNUSED), "table": "t", "schema": null, "database": null}),
    ] {
        let response = call("get_drop_table_sql", request).await;
        assert_eq!(result(&response), &Value::Null);
    }
}

#[tokio::test]
async fn drop_table_without_credentials_still_qualifies_the_table() {
    let mut params = host_params(UNUSED);
    params["extra"]["auth_method"] = json!("service_account");
    let response = call(
        "get_drop_table_sql",
        json!({
            "params": params,
            "table": "orders",
            "schema": "sales",
            "database": "analytics-project"
        }),
    )
    .await;
    assert_eq!(
        result(&response),
        &json!("DROP TABLE IF EXISTS `analytics-project`.`sales`.`orders`")
    );
}

#[tokio::test]
async fn drop_table_uses_the_database_field_when_params_have_none() {
    let server = MockServer::start().await;
    mount_kind(&server, "daily_snapshot", Some("SNAPSHOT")).await;
    let mut params = host_params(&server.uri());
    params["database"] = json!("");
    let response = call(
        "get_drop_table_sql",
        json!({
            "params": params,
            "table": "daily_snapshot",
            "schema": "sales",
            "database": "analytics-project"
        }),
    )
    .await;
    assert_eq!(
        result(&response),
        &json!("DROP SNAPSHOT TABLE IF EXISTS `analytics-project`.`sales`.`daily_snapshot`")
    );
}
