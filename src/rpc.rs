use serde_json::{json, Value};

use crate::handlers;

pub async fn handle_line(line: &str) -> Value {
    match serde_json::from_str(line) {
        Ok(request) => handle_request(request).await,
        Err(error) => error_response(Value::Null, -32700, &format!("parse error: {error}")),
    }
}

pub async fn handle_request(request: Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return error_response(id, -32600, "invalid JSON-RPC request");
    }
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return error_response(id, -32600, "missing JSON-RPC method");
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => handlers::connection::initialize(id, &params).await,
        "ping" => handlers::connection::ping(id, &params).await,
        "test_connection" => handlers::connection::test_connection(id, &params).await,
        "shutdown" => handlers::connection::shutdown(id, &params).await,
        "get_databases" => handlers::metadata::get_databases(id, &params).await,
        "get_schemas" => handlers::metadata::get_schemas(id, &params).await,
        "get_tables" => handlers::metadata::get_tables(id, &params).await,
        "get_columns" => handlers::metadata::get_columns(id, &params).await,
        "get_foreign_keys" => handlers::metadata::get_foreign_keys(id, &params).await,
        "get_indexes" => handlers::metadata::get_indexes(id, &params).await,
        "get_views" => handlers::metadata::get_views(id, &params).await,
        "get_view_definition" => handlers::metadata::get_view_definition(id, &params).await,
        "get_view_columns" => handlers::metadata::get_view_columns(id, &params).await,
        "create_view" => handlers::metadata::create_view(id, &params).await,
        "alter_view" => handlers::metadata::alter_view(id, &params).await,
        "drop_view" => handlers::metadata::drop_view(id, &params).await,
        "get_materialized_views" => handlers::metadata::get_materialized_views(id, &params).await,
        "get_materialized_view_columns" => {
            handlers::metadata::get_materialized_view_columns(id, &params).await
        }
        "get_materialized_view_definition" => {
            handlers::metadata::get_materialized_view_definition(id, &params).await
        }
        "refresh_materialized_view" => {
            handlers::metadata::refresh_materialized_view(id, &params).await
        }
        "get_routines" => handlers::routines::get_routines(id, &params).await,
        "get_routine_parameters" => handlers::routines::get_routine_parameters(id, &params).await,
        "get_routine_definition" => handlers::routines::get_routine_definition(id, &params).await,
        "get_routine_edit_script" => handlers::routines::get_routine_edit_script(id, &params).await,
        "build_routine_call_sql" => handlers::routines::build_routine_call_sql(id, &params).await,
        "routine_create_template" => handlers::routines::routine_create_template(id, &params).await,
        "drop_routine" => handlers::routines::drop_routine(id, &params).await,
        "get_table_ddl" => handlers::metadata::get_table_ddl(id, &params).await,
        "get_schema_snapshot" => handlers::metadata::get_schema_snapshot(id, &params).await,
        "get_all_columns_batch" => handlers::metadata::get_all_columns_batch(id, &params).await,
        "get_all_foreign_keys_batch" => {
            handlers::metadata::get_all_foreign_keys_batch(id, &params).await
        }
        "get_ai_schema_context" => handlers::metadata::get_ai_schema_context(id, &params).await,
        "execute_query" => handlers::query::execute_query(id, &params).await,
        "execute_query_batch" => handlers::query::execute_query_batch(id, &params).await,
        "explain_query" => handlers::query::explain_query(id, &params).await,
        "release_session" => handlers::query::release_session(id, &params).await,
        "insert_record" => handlers::crud::insert_record(id, &params).await,
        "update_record" => handlers::crud::update_record(id, &params).await,
        "delete_record" => handlers::crud::delete_record(id, &params).await,
        "get_create_table_sql" => handlers::ddl::get_create_table_sql(id, &params).await,
        "get_add_column_sql" => handlers::ddl::get_add_column_sql(id, &params).await,
        "get_alter_column_sql" => handlers::ddl::get_alter_column_sql(id, &params).await,
        "get_create_index_sql" => handlers::ddl::get_create_index_sql(id, &params).await,
        "get_create_foreign_key_sql" => {
            handlers::ddl::get_create_foreign_key_sql(id, &params).await
        }
        "get_drop_table_sql" => handlers::ddl::get_drop_table_sql(id, &params).await,
        "drop_index" => handlers::ddl::drop_index(id, &params).await,
        "drop_foreign_key" => handlers::ddl::drop_foreign_key(id, &params).await,
        "save_blob_to_file" => handlers::blob::save_blob_to_file(id, &params).await,
        "fetch_blob_as_data_url" => handlers::blob::fetch_blob_as_data_url(id, &params).await,
        other => not_implemented(id, other),
    }
}

pub fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "result": result, "id": id})
}

pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "error": {"code": code, "message": message}, "id": id})
}

pub fn not_implemented(id: Value, method: &str) -> Value {
    error_response(id, -32601, &format!("method '{method}' is not implemented"))
}

#[cfg(test)]
mod tests {
    use super::handle_line;

    #[tokio::test]
    async fn rejects_invalid_json_rpc_version() {
        let response = handle_line(r#"{"jsonrpc":"1.0","method":"ping","id":7}"#).await;
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], 7);
    }

    #[tokio::test]
    async fn reports_unknown_methods() {
        let response = handle_line(r#"{"jsonrpc":"2.0","method":"unknown","id":2}"#).await;
        assert_eq!(response["error"]["code"], -32601);
    }
}
