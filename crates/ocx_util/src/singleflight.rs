// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Watch-based singleflight for async work deduplication.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};

/// Clonable `Arc` wrapper around an error, preserving its source chain.
#[derive(Clone)]
pub struct SharedError(Arc<dyn std::error::Error + Send + Sync>);

impl SharedError {
    /// Test-only constructor; `__testing` because the exit-code tests that need it live in `ocx_cli`.
    #[cfg(any(test, feature = "__testing"))]
    pub fn for_test<E: std::error::Error + Send + Sync + 'static>(error: E) -> Self {
        Self(Arc::new(error))
    }
}

impl fmt::Debug for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // The wrapped error itself, so exit-code classification can downcast to the leader's typed error.
        Some(&*self.0)
    }
}

/// Error from a singleflight wait.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// The leader failed; transparent, so a deduplicated call reports the message a direct call would.
    #[error(transparent)]
    Failed(SharedError),
    /// The leader's [`Handle`] was dropped without completing.
    #[error("singleflight leader abandoned")]
    Abandoned,
    #[error("singleflight wait timed out")]
    Timeout,
    #[error("singleflight capacity exceeded (max {max})")]
    CapacityExceeded { max: usize },
}

/// Result of [`Group::try_acquire`].
#[allow(clippy::large_enum_variant)]
pub enum Acquisition<V> {
    /// This task produces the value and calls [`Handle::complete`].
    Leader(Handle<V>),
    /// Another task already produced the value — reuse it.
    Resolved(V),
}

impl<V: fmt::Debug> fmt::Debug for Acquisition<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Leader(_) => f.debug_tuple("Leader").field(&"...").finish(),
            Self::Resolved(v) => f.debug_tuple("Resolved").field(v).finish(),
        }
    }
}

/// The leader's handle; dropping it without completing broadcasts [`Error::Abandoned`].
pub struct Handle<V> {
    sender: Option<watch::Sender<Option<Result<V, Error>>>>,
}

impl<V: Clone> Handle<V> {
    /// Broadcast the value to all waiters.
    pub fn complete(mut self, value: V) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Some(Ok(value)));
        }
    }

    /// Broadcast an error to all waiters, returning it for the leader's own result.
    pub fn fail<E: std::error::Error + Send + Sync + 'static>(mut self, error: E) -> SharedError {
        let shared = SharedError(Arc::new(error));
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Some(Err(Error::Failed(shared.clone()))));
        }
        shared
    }
}

impl<V> Drop for Handle<V> {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Some(Err(Error::Abandoned)));
        }
    }
}

type WatchValue<V> = Option<Result<V, Error>>;

/// A keyed singleflight group: a success is cached for the group's lifetime, a failure is retried by the next caller.
pub struct Group<K, V> {
    entries: Arc<Mutex<HashMap<K, watch::Receiver<WatchValue<V>>>>>,
    max_entries: usize,
    timeout: Duration,
}

// Manual Clone: `Arc` clone does not require `K: Clone` or `V: Clone`.
impl<K, V> Clone for Group<K, V> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            max_entries: self.max_entries,
            timeout: self.timeout,
        }
    }
}

impl<K, V> Group<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub fn new(max_entries: usize, timeout: Duration) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            max_entries,
            timeout,
        }
    }

    /// Registers `key` as in-flight, replacing a failed entry in place so a retry takes no second capacity slot.
    fn lead(entries: &mut HashMap<K, watch::Receiver<WatchValue<V>>>, key: K) -> Acquisition<V> {
        let (tx, rx) = watch::channel(None);
        entries.insert(key, rx);
        Acquisition::Leader(Handle { sender: Some(tx) })
    }

    /// Acquire leadership for `key`, or wait for the in-flight leader's value.
    pub async fn try_acquire(&self, key: K) -> Result<Acquisition<V>, Error> {
        let mut rx = {
            let mut entries = self.entries.lock().await;
            let existing = entries.get(&key).map(|rx| (rx.borrow().clone(), rx.clone()));
            match existing {
                Some((Some(Ok(value)), _)) => return Ok(Acquisition::Resolved(value)),
                // Retry a failed entry; waiters on the old channel still get its error.
                Some((Some(Err(_)), _)) => return Ok(Self::lead(&mut entries, key)),
                Some((None, rx)) => rx,
                None => {
                    if entries.len() >= self.max_entries {
                        return Err(Error::CapacityExceeded { max: self.max_entries });
                    }
                    return Ok(Self::lead(&mut entries, key));
                }
            }
        };

        // `wait_for` checks the current value first, so a `complete()` since the `borrow()` is not missed.
        let wait_result = tokio::time::timeout(self.timeout, rx.wait_for(|v| v.is_some())).await;
        match wait_result {
            Ok(Ok(ref_guard)) => match ref_guard.as_ref().expect("wait_for guarantees Some") {
                Ok(value) => Ok(Acquisition::Resolved(value.clone())),
                Err(e) => Err(e.clone()),
            },
            Ok(Err(_changed_err)) => Err(Error::Abandoned),
            Err(_elapsed) => Err(Error::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestError(&'static str);

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for TestError {}

    #[test]
    fn failed_display_is_transparent_no_plumbing_prefix() {
        let failed = Error::Failed(SharedError::for_test(TestError(
            "manifest sha256:abc not in the local cache",
        )));
        // Transparent: the wrapped leader error is surfaced verbatim — no
        // "singleflight leader failed" prefix leaking into user-facing output.
        assert_eq!(failed.to_string(), "manifest sha256:abc not in the local cache");
        // The wrapped typed error stays reachable via the source chain for
        // exit-code classification.
        assert!(std::error::Error::source(&failed).is_some());
    }

    fn group(max: usize) -> Group<String, String> {
        Group::new(max, Duration::from_secs(300))
    }

    fn key(s: &str) -> String {
        s.to_owned()
    }

    #[tokio::test]
    async fn first_call_returns_leader() {
        let g = group(10);
        let result = g.try_acquire(key("key-a")).await.unwrap();
        assert!(matches!(result, Acquisition::Leader(_)));
    }

    #[tokio::test]
    async fn completed_leader_returns_resolved() {
        let g = group(10);

        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.complete("hello".to_owned());

        let Acquisition::Resolved(value) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Resolved");
        };
        assert_eq!(value, "hello");
    }

    #[tokio::test]
    async fn concurrent_waiters_receive_result() {
        let g = group(10);

        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };

        let mut waiters = Vec::new();
        for _ in 0..5 {
            let g = g.clone();
            waiters.push(tokio::spawn(async move { g.try_acquire(key("key-a")).await }));
        }

        tokio::task::yield_now().await;
        handle.complete("result".to_owned());

        for jh in waiters {
            let Acquisition::Resolved(value) = jh.await.unwrap().unwrap() else {
                panic!("expected Resolved");
            };
            assert_eq!(value, "result");
        }
    }

    #[tokio::test]
    async fn abandoned_handle_signals_error() {
        let g = group(10);

        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };

        let g2 = g.clone();
        let waiter = tokio::spawn(async move { g2.try_acquire(key("key-a")).await });

        tokio::task::yield_now().await;
        drop(handle);

        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, Error::Abandoned));
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let g: Group<String, String> = Group::new(10, Duration::from_millis(50));

        let Acquisition::Leader(_handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };

        let g2 = g.clone();
        let err = tokio::spawn(async move { g2.try_acquire(key("key-a")).await })
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, Error::Timeout));
    }

    #[tokio::test]
    async fn capacity_exceeded() {
        let g = group(2);

        let _r1 = g.try_acquire(key("a")).await.unwrap();
        let _r2 = g.try_acquire(key("b")).await.unwrap();
        let err = g.try_acquire(key("c")).await.unwrap_err();
        assert!(matches!(err, Error::CapacityExceeded { max: 2 }));
    }

    #[tokio::test]
    async fn failed_leader_propagates_error_to_waiters() {
        let g = group(10);

        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };

        let g2 = g.clone();
        let waiter = tokio::spawn(async move { g2.try_acquire(key("key-a")).await });

        tokio::task::yield_now().await;
        handle.fail(TestError("something broke"));

        let err = waiter.await.unwrap().unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref shared) if shared.to_string() == "something broke"),
            "expected Failed with message, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn subsequent_acquire_after_failure_returns_a_fresh_leader() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.fail(TestError("boom"));

        // A failure is the absence of an answer, not a cached one: the next
        // caller leads a retry rather than inheriting the leader's error.
        let acquisition = g.try_acquire(key("key-a")).await.unwrap();
        assert!(
            matches!(acquisition, Acquisition::Leader(_)),
            "a failed key must be retryable, not poisoned for the group's lifetime"
        );
    }

    #[tokio::test]
    async fn resolved_value_is_durable_across_multiple_acquires() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.complete("durable".to_owned());

        for _ in 0..3 {
            let Acquisition::Resolved(value) = g.try_acquire(key("key-a")).await.unwrap() else {
                panic!("expected Resolved");
            };
            assert_eq!(value, "durable");
        }
    }

    #[tokio::test]
    async fn failed_key_is_retried_by_a_later_acquire() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.fail(TestError("transient outage"));

        let Acquisition::Leader(retry) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("a failed key must be retryable, not poisoned for the group's lifetime");
        };
        retry.complete("recovered".to_owned());

        let Acquisition::Resolved(value) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Resolved");
        };
        assert_eq!(value, "recovered", "the retry's value must be the memoized one");
    }

    #[tokio::test]
    async fn abandoned_key_is_retried_by_a_later_acquire() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        // A cancelled leader (a `select!` loser, an aborted `JoinSet` —
        // `project/resolve.rs`'s `abort_all` — or a `?` higher in the same
        // task) has learned nothing, and must not poison the key for every
        // later caller.
        drop(handle);

        let Acquisition::Leader(retry) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("an abandoned key must be reclaimable by the next caller");
        };
        retry.complete("recovered".to_owned());
    }

    /// A failed key is replaced in place, so it costs the same one capacity
    /// slot a successful key costs — never one slot plus a refusal.
    #[tokio::test]
    async fn a_failed_key_holds_no_capacity_slot() {
        let g = group(1);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.fail(TestError("boom"));

        let acquisition = g.try_acquire(key("key-a")).await.unwrap();
        assert!(
            matches!(acquisition, Acquisition::Leader(_)),
            "retrying a failed key must not trip the capacity limit"
        );
    }

    #[tokio::test]
    async fn complete_between_borrow_and_wait_is_caught() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };

        let g2 = g.clone();
        let waiter = tokio::spawn(async move { g2.try_acquire(key("key-a")).await });

        // Yield to let the waiter enter the wait path (borrow returns None).
        tokio::task::yield_now().await;
        // Complete immediately — waiter must still see the value via wait_for.
        handle.complete("race-safe".to_owned());

        let Acquisition::Resolved(value) = waiter.await.unwrap().unwrap() else {
            panic!("expected Resolved");
        };
        assert_eq!(value, "race-safe");
    }

    #[tokio::test]
    async fn different_keys_independent() {
        let g = group(10);

        let r1 = g.try_acquire(key("key-a")).await.unwrap();
        let r2 = g.try_acquire(key("key-b")).await.unwrap();
        assert!(matches!(r1, Acquisition::Leader(_)));
        assert!(matches!(r2, Acquisition::Leader(_)));
    }
}
