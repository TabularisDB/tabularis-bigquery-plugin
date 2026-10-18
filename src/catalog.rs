//! Short-lived metadata cache and bounded-concurrency catalog fetches.
//!
//! Opening a table makes Tabularis ask for columns, keys and indexes in quick
//! succession, and every one of them is answered from the same `tables.get`
//! resource. Only per-object resources are cached, for [`METADATA_TTL`];
//! object lists are always fetched so created and dropped objects show up
//! at once. Every DDL statement issued by this plugin, and every DDL builder
//! call, clears the cache.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::client::BigQueryClient;

pub const METADATA_TTL: Duration = Duration::from_secs(5);
/// Concurrent `tables.get` calls for bulk metadata.
pub const FETCH_CONCURRENCY: usize = 8;
const MAX_ENTRIES: usize = 4_096;

type Entries = HashMap<String, (Instant, Value)>;

static CACHE: OnceLock<Mutex<Entries>> = OnceLock::new();

fn entries() -> &'static Mutex<Entries> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(client: &BigQueryClient, kind: &str, parts: &[&str]) -> String {
    format!(
        "{:p}\u{1f}{}\u{1f}{kind}\u{1f}{}",
        client as *const BigQueryClient,
        client.project_id,
        parts.join("\u{1f}")
    )
}

fn lookup(key: &str) -> Option<Value> {
    let mut entries = entries().lock().ok()?;
    match entries.get(key) {
        Some((stored, value)) if stored.elapsed() < METADATA_TTL => Some(value.clone()),
        Some(_) => {
            entries.remove(key);
            None
        }
        None => None,
    }
}

fn store(key: String, value: &Value) {
    if let Ok(mut entries) = entries().lock() {
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, (stored, _)| stored.elapsed() < METADATA_TTL);
            if entries.len() >= MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(key, (Instant::now(), value.clone()));
    }
}

/// Drops every cached resource. Called after any DDL statement.
pub fn invalidate() {
    if let Ok(mut entries) = entries().lock() {
        entries.clear();
    }
}

async fn cached<F, Fut>(key: String, load: F) -> Result<Value, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    if let Some(value) = lookup(&key) {
        return Ok(value);
    }
    let value = load().await?;
    store(key, &value);
    Ok(value)
}

pub async fn dataset(
    client: &BigQueryClient,
    project: &str,
    dataset: &str,
) -> Result<Value, String> {
    cached(cache_key(client, "dataset", &[project, dataset]), || {
        client.get_dataset(project, dataset)
    })
    .await
}

/// Dataset location, used to run INFORMATION_SCHEMA jobs in the right region.
pub async fn dataset_location(
    client: &BigQueryClient,
    project: &str,
    name: &str,
) -> Option<String> {
    dataset(client, project, name)
        .await
        .ok()
        .and_then(|resource| {
            resource
                .get("location")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

pub async fn tables(
    client: &BigQueryClient,
    project: &str,
    dataset: &str,
) -> Result<Vec<Value>, String> {
    client.list_tables_in(project, dataset).await
}

pub async fn table(
    client: &BigQueryClient,
    project: &str,
    dataset: &str,
    table: &str,
) -> Result<Value, String> {
    cached(
        cache_key(client, "table", &[project, dataset, table]),
        || client.get_table_in(project, dataset, table),
    )
    .await
}

pub async fn routines(
    client: &BigQueryClient,
    project: &str,
    dataset: &str,
) -> Result<Vec<Value>, String> {
    client.list_routines(project, dataset).await
}

pub async fn routine(
    client: &BigQueryClient,
    project: &str,
    dataset: &str,
    routine: &str,
) -> Result<Value, String> {
    cached(
        cache_key(client, "routine", &[project, dataset, routine]),
        || client.get_routine(project, dataset, routine),
    )
    .await
}

/// Fetches each table resource once with at most `concurrency` requests in
/// flight. Results keep the order of `names`.
pub async fn tables_batch(
    client: Arc<BigQueryClient>,
    project: &str,
    dataset: &str,
    names: Vec<String>,
    concurrency: usize,
) -> Vec<(String, Result<Value, String>)> {
    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut tasks = JoinSet::new();
    for (index, name) in names.iter().cloned().enumerate() {
        let client = client.clone();
        let semaphore = semaphore.clone();
        let project = project.to_string();
        let dataset = dataset.to_string();
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            let result = table(&client, &project, &dataset, &name).await;
            (index, result)
        });
    }
    let mut results: Vec<Option<Result<Value, String>>> = names.iter().map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        if let Ok((index, result)) = joined {
            results[index] = Some(result);
        }
    }
    names
        .into_iter()
        .zip(results)
        .map(|(name, result)| {
            let result = result
                .unwrap_or_else(|| Err(format!("Metadata request for '{name}' was cancelled")));
            (name, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{cached, invalidate, lookup};

    #[tokio::test]
    async fn caches_until_invalidated() {
        let key = "test\u{1f}cache".to_string();
        let first = cached(key.clone(), || async { Ok(json!(1)) })
            .await
            .unwrap();
        let second = cached(key.clone(), || async { Ok(json!(2)) })
            .await
            .unwrap();
        assert_eq!(first, json!(1));
        assert_eq!(second, json!(1));
        invalidate();
        assert!(lookup(&key).is_none());
        let third = cached(key, || async { Ok(json!(3)) }).await.unwrap();
        assert_eq!(third, json!(3));
    }

    #[tokio::test]
    async fn failed_loads_are_not_cached() {
        let key = "test\u{1f}failure".to_string();
        assert!(cached(key.clone(), || async { Err("boom".to_string()) })
            .await
            .is_err());
        assert!(lookup(&key).is_none());
    }
}
