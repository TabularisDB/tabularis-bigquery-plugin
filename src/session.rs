//! Host editor sessions mapped to BigQuery sessions.
//!
//! Tabularis pins an editor tab to a `session_id`. The first statement run for
//! it creates a BigQuery session (`createSession: true`); later statements join
//! it through `connectionProperties`, so variables, temporary tables and
//! multi-statement transactions survive between runs.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tokio::sync::Mutex;

use crate::client::BigQueryClient;

#[derive(Default)]
pub struct SessionState {
    /// BigQuery session id, once the first job created it.
    pub bigquery_id: Option<String>,
    /// Sessions are regional; later jobs must run where the first one ran.
    pub location: Option<String>,
    pub in_transaction: bool,
    pub client: Option<Arc<BigQueryClient>>,
}

type SharedSession = Arc<Mutex<SessionState>>;

static SESSIONS: OnceLock<Mutex<HashMap<String, SharedSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, SharedSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The state for a host session, created on first use. Statements of one
/// session run one at a time by locking the returned mutex.
pub async fn get_or_create(host_id: &str) -> SharedSession {
    sessions()
        .lock()
        .await
        .entry(host_id.to_string())
        .or_default()
        .clone()
}

/// Forget a host session and abort its BigQuery session, if any.
pub async fn release(host_id: &str) -> Result<(), String> {
    let Some(session) = sessions().lock().await.remove(host_id) else {
        return Ok(());
    };
    let mut state = session.lock().await;
    abort(&mut state).await
}

/// Abort every known session, for plugin shutdown.
pub async fn release_all() {
    let all: Vec<SharedSession> = sessions().lock().await.drain().map(|(_, s)| s).collect();
    for session in all {
        let mut state = session.lock().await;
        let _ = abort(&mut state).await;
    }
}

pub async fn abort(state: &mut SessionState) -> Result<(), String> {
    let (Some(client), Some(id)) = (state.client.clone(), state.bigquery_id.take()) else {
        return Ok(());
    };
    state.in_transaction = false;
    client.abort_session(&id, state.location.as_deref()).await
}
