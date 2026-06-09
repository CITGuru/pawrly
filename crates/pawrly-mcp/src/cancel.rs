//! In-flight query tracking for `cancel_query`. A query that carries a
//! client-supplied `query_id` registers a cancellation token here; a concurrent
//! `cancel_query` for the same id aborts it. This is only effective on
//! transports that handle requests concurrently (HTTP) — stdio serializes
//! requests, so nothing is ever in-flight to cancel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `id` and return its cancellation token.
    pub(crate) fn register(&self, id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.lock().insert(id.to_string(), token.clone());
        token
    }

    /// Drop `id`'s entry once its query has finished.
    pub(crate) fn finish(&self, id: &str) {
        self.lock().remove(id);
    }

    /// Cancel `id`. Returns whether a query with that id was in-flight.
    pub fn cancel(&self, id: &str) -> bool {
        match self.lock().remove(id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_unknown_id_is_false() {
        let reg = CancelRegistry::new();
        assert!(!reg.cancel("nope"));
    }

    #[test]
    fn register_then_cancel_is_true_and_signals() {
        let reg = CancelRegistry::new();
        let token = reg.register("q1");
        assert!(!token.is_cancelled());
        assert!(reg.cancel("q1"));
        assert!(token.is_cancelled());
        // A second cancel finds nothing: the entry was removed.
        assert!(!reg.cancel("q1"));
    }

    #[test]
    fn finish_removes_without_cancelling() {
        let reg = CancelRegistry::new();
        let token = reg.register("q1");
        reg.finish("q1");
        assert!(!token.is_cancelled());
        assert!(!reg.cancel("q1"));
    }
}
