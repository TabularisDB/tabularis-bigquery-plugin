//! Cancellation of in-flight requests.
//!
//! Every JSON-RPC request runs inside [`scope`], which registers it under its
//! request id. Query code registers the BigQuery job it started with
//! [`register_job`]; a host `cancel` notification for that id then stops the
//! local wait and cancels the job on the server.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use tokio::sync::Notify;

pub type CancelFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
pub type JobCanceller = Arc<dyn Fn() -> CancelFuture + Send + Sync>;

#[derive(Default)]
pub struct RequestHandle {
    cancelled: AtomicBool,
    notify: Notify,
    job: Mutex<Option<JobCanceller>>,
}

impl RequestHandle {
    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        self.notify.notify_waiters();
        let canceller = self.job.lock().ok().and_then(|job| job.clone());
        if let Some(canceller) = canceller {
            spawn_cancel(canceller);
        }
    }
}

tokio::task_local! {
    static CURRENT: Arc<RequestHandle>;
}

static ACTIVE: OnceLock<Mutex<HashMap<String, Arc<RequestHandle>>>> = OnceLock::new();

fn active() -> &'static Mutex<HashMap<String, Arc<RequestHandle>>> {
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(id: &Value) -> String {
    id.to_string()
}

/// Run `future` as the request identified by `id`, so it can be cancelled.
pub async fn scope<F: Future>(id: Value, future: F) -> F::Output {
    let handle = Arc::new(RequestHandle::default());
    let id = key(&id);
    if let Ok(mut active) = active().lock() {
        active.insert(id.clone(), handle.clone());
    }
    let output = CURRENT.scope(handle.clone(), future).await;
    if let Ok(mut active) = active().lock() {
        if active
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, &handle))
        {
            active.remove(&id);
        }
    }
    output
}

/// Handle a host `cancel` notification. Unknown ids are ignored.
pub fn cancel(id: &Value) -> bool {
    let handle = active()
        .lock()
        .ok()
        .and_then(|active| active.get(&key(id)).cloned());
    match handle {
        Some(handle) => {
            handle.cancel();
            true
        }
        None => false,
    }
}

/// Whether the current request has been cancelled.
pub fn is_cancelled() -> bool {
    CURRENT
        .try_with(|handle| handle.cancelled.load(Ordering::SeqCst))
        .unwrap_or(false)
}

/// Resolves when the current request is cancelled; never outside a scope.
pub async fn cancelled() {
    let Ok(handle) = CURRENT.try_with(Arc::clone) else {
        return std::future::pending().await;
    };
    loop {
        let notified = handle.notify.notified();
        if handle.cancelled.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

/// Attach the server-side job of the current request. When the request was
/// already cancelled the job is cancelled immediately.
pub fn register_job(canceller: JobCanceller) {
    let _ = CURRENT.try_with(|handle| {
        if let Ok(mut job) = handle.job.lock() {
            *job = Some(canceller.clone());
        }
        if handle.cancelled.load(Ordering::SeqCst) {
            spawn_cancel(canceller);
        }
    });
}

/// Detach the job once it has finished, so a late cancel is a no-op.
pub fn clear_job() {
    let _ = CURRENT.try_with(|handle| {
        if let Ok(mut job) = handle.job.lock() {
            *job = None;
        }
    });
}

fn spawn_cancel(canceller: JobCanceller) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(canceller());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;

    use super::{cancel, cancelled, is_cancelled, register_job, scope, JobCanceller};

    #[tokio::test]
    async fn cancel_wakes_the_request_and_cancels_its_job() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let canceller: JobCanceller = Arc::new(move || {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        });
        let request = tokio::spawn(scope(json!(41), async move {
            register_job(canceller);
            tokio::select! {
                _ = cancelled() => is_cancelled(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => false,
            }
        }));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(cancel(&json!(41)));
        assert!(request.await.unwrap());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!cancel(&json!(41)), "finished requests are unregistered");
    }

    #[tokio::test]
    async fn unknown_ids_and_unscoped_code_are_ignored() {
        assert!(!cancel(&json!(999_999)));
        assert!(!is_cancelled());
        let waited = tokio::time::timeout(Duration::from_millis(20), cancelled()).await;
        assert!(waited.is_err());
    }
}
