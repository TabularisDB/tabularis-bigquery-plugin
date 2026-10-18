use bigquery_plugin::rpc::handle_line;
use serde_json::json;

#[tokio::test]
async fn initialize_is_a_valid_protocol_round_trip() {
    let response =
        handle_line(r#"{"jsonrpc":"2.0","method":"initialize","params":{"settings":{}},"id":1}"#)
            .await;
    assert_eq!(response, json!({"jsonrpc": "2.0", "result": null, "id": 1}));
}

#[tokio::test]
async fn create_table_ddl_matches_bigquery_constraints() {
    let request = json!({
        "jsonrpc": "2.0",
        "method": "get_create_table_sql",
        "params": {
            "schema": "analytics",
            "table_name": "events",
            "columns": [
                {
                    "name": "id",
                    "data_type": "INT64",
                    "is_nullable": false,
                    "is_pk": true,
                    "is_auto_increment": false,
                    "default_value": null
                },
                {
                    "name": "payload",
                    "data_type": "JSON",
                    "is_nullable": true,
                    "is_pk": false,
                    "is_auto_increment": false,
                    "default_value": null
                }
            ]
        },
        "id": "ddl"
    });
    let response = handle_line(&request.to_string()).await;
    let sql = response["result"][0].as_str().unwrap();
    assert!(sql.starts_with("CREATE TABLE `analytics`.`events`"));
    assert!(sql.contains("`payload` JSON"));
    assert!(sql.contains("PRIMARY KEY (`id`) NOT ENFORCED"));
}

#[tokio::test]
async fn index_generation_reports_the_bigquery_limitation() {
    let response =
        handle_line(r#"{"jsonrpc":"2.0","method":"get_create_index_sql","params":{},"id":9}"#)
            .await;
    assert_eq!(response["error"]["code"], -32601);
}
