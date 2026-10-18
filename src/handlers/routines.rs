//! Routine metadata and management backed by the free `routines.list` and
//! `routines.get` REST calls.

use serde_json::{json, Value};

use crate::catalog;
use crate::handlers::metadata::{required_any, required_schema, respond, Target};
use crate::models::{inner_params, ConnectionParams};
use crate::routines::{self, CallArg, RoutineKind};

pub async fn get_routines(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let target = Target::from_params(params).await?;
        let mut routines = catalog::routines(&target.client, &target.project, schema)
            .await?
            .iter()
            .filter_map(routines::routine_info)
            .collect::<Vec<_>>();
        routines.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        Ok(json!(routines))
    })
    .await
}

async fn routine_resource(params: &Value) -> Result<(Target, String, String, Value), String> {
    let schema = required_schema(params)?.to_string();
    let name = required_any(params, &["routine_name", "routine"])?.to_string();
    let target = Target::from_params(params).await?;
    let resource = catalog::routine(&target.client, &target.project, &schema, &name).await?;
    Ok((target, schema, name, resource))
}

pub async fn get_routine_parameters(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (_, _, _, resource) = routine_resource(params).await?;
        Ok(json!(routines::routine_parameters(&resource)))
    })
    .await
}

pub async fn get_routine_definition(id: Value, params: &Value) -> Value {
    respond(id, async {
        let (target, schema, name, resource) = routine_resource(params).await?;
        Ok(json!(routines::routine_ddl(
            &resource,
            &target.path(&schema, &name),
            true
        )?))
    })
    .await
}

pub async fn get_routine_edit_script(id: Value, params: &Value) -> Value {
    get_routine_definition(id, params).await
}

pub async fn build_routine_call_sql(id: Value, params: &Value) -> Value {
    respond(id, async {
        let args: Vec<CallArg> = match params.get("args") {
            Some(Value::Null) | None => Vec::new(),
            Some(args) => serde_json::from_value(args.clone())
                .map_err(|error| format!("Invalid routine arguments: {error}"))?,
        };
        let (target, schema, name, resource) = routine_resource(params).await?;
        Ok(json!(routines::routine_call_sql(
            &resource,
            &target.path(&schema, &name),
            &args
        )?))
    })
    .await
}

pub async fn routine_create_template(id: Value, params: &Value) -> Value {
    respond(id, async {
        let routine_type = params
            .get("routine_type")
            .and_then(Value::as_str)
            .unwrap_or("FUNCTION");
        let schema = params.get("schema").and_then(Value::as_str);
        // Only hosts that attach connection parameters to SQL builders send
        // `params`; without them the template is dataset-qualified.
        let connection = params
            .get("params")
            .map(|_| ConnectionParams::from_value(inner_params(params)));
        let project = connection
            .as_ref()
            .and_then(|connection| connection.project_id().ok().map(str::to_owned));
        Ok(json!(routines::create_template(
            routine_type,
            project.as_deref(),
            schema
        )))
    })
    .await
}

pub async fn drop_routine(id: Value, params: &Value) -> Value {
    respond(id, async {
        let schema = required_schema(params)?;
        let name = required_any(params, &["routine_name", "routine"])?;
        let target = Target::from_params(params).await?;
        // The host only knows FUNCTION / PROCEDURE; table functions need
        // their own keyword, so the stored routine type wins.
        let kind = match catalog::routine(&target.client, &target.project, schema, name).await {
            Ok(resource) => RoutineKind::from_resource(&resource),
            Err(_) => RoutineKind::from_name(
                params
                    .get("routine_type")
                    .and_then(Value::as_str)
                    .unwrap_or("FUNCTION"),
            ),
        };
        let sql = routines::drop_routine_sql(kind, &target.path(schema, name));
        target.run_ddl(&sql, schema).await?;
        Ok(Value::Null)
    })
    .await
}
