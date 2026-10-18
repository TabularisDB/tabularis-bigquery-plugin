use serde_json::Value;

use crate::client;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};

pub async fn initialize(id: Value, _params: &Value) -> Value {
    ok_response(id, Value::Null)
}

pub async fn ping(id: Value, params: &Value) -> Value {
    test(id, params, true).await
}

pub async fn test_connection(id: Value, params: &Value) -> Value {
    test(id, params, false).await
}

async fn test(id: Value, params: &Value, lightweight: bool) -> Value {
    let params = ConnectionParams::from_value(inner_params(params));
    let result = async {
        let client = client::client_for(&params).await?;
        if lightweight {
            client.ping().await
        } else {
            client
                .execute_query("SELECT 1 AS connected", None, Some(1), 1)
                .await
                .map(|_| ())
        }
    }
    .await;

    match result {
        Ok(()) => ok_response(id, Value::Null),
        Err(error) => error_response(id, -32603, &error),
    }
}

pub async fn shutdown(id: Value, _params: &Value) -> Value {
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::session::release_all(),
    )
    .await;
    client::clear_cache().await;
    ok_response(id, Value::Null)
}
