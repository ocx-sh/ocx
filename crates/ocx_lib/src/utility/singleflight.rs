// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Watch-based singleflight for async work deduplication.
//!
//! When multiple tasks need the same keyed result concurrently, the first
//! caller gets a [`Handle`] (responsibility to produce the value) and
//! subsequent callers block until the result is broadcast.
//!
//! Unlike closure-based singleflight crates, callers only construct the
//! work after confirming they are responsible — waiters pay no setup cost.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};

/// Clonable wrapper around an error source, preserving the full error chain.
///
/// Wraps the original error in an `Arc` so it can be cloned and broadcast
/// to multiple singleflight waiters without erasing the source chain.
#[derive(Clone)]
pub struct SharedError(Arc<dyn std::error::Error + Send + Sync>);

impl SharedError {
    /// Test-only constructor used by classification and chain-walk tests
    /// that need to fabricate a `SharedError` without going through `Group`.
    #[cfg(test)]
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
        // Expose the wrapped error itself as the chain successor so callers
        // walking via `Error::source` (e.g. `cli::classify_error`) can
        // downcast to the leader's typed error and recover its discriminant.
        Some(&*self.0)
    }
}

/// Error from a singleflight wait.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The leader task failed and broadcast its error.
    #[error("singleflight leader failed: {0}")]
    Failed(#[source] SharedError),
    /// The task responsible for producing the value was dropped without
    /// calling [`Handle::complete`] or [`Handle::fail`].
    #[error("singleflight leader abandoned")]
    Abandoned,
    /// Timed out waiting for the leader to produce a value.
    #[error("singleflight wait timed out")]
    Timeout,
    /// The maximum number of in-flight keys has been reached.
    #[error("singleflight capacity exceeded (max {max})")]
    CapacityExceeded { max: usize },
}

/// Result of [`Group::try_acquire`].
#[allow(clippy::large_enum_variant)]
pub enum Acquisition<V> {
    /// This task is responsible for producing the value.
    /// Call [`Handle::complete`] when done.
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

/// Handle returned to the leader task. Broadcasts the result on
/// [`complete`](Self::complete). If dropped without completing,
/// broadcasts [`Error::Abandoned`] to all waiters.
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

    /// Broadcast an error to all waiters.
    ///
    /// The error is wrapped in an `Arc` so it can be cloned to each waiter
    /// while preserving the full source chain. Returns the [`SharedError`]
    /// so the leader can reuse the same wrapped error in its own result.
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

/// A keyed singleflight group.
///
/// Concurrent calls to [`try_acquire`](Self::try_acquire) with the same key
/// are coalesced: the first caller becomes the leader, subsequent callers
/// block on a `tokio::sync::watch` channel until the leader broadcasts.
///
/// Resolved entries are retained for the group's lifetime so that later
/// callers (e.g. diamond dependencies discovered deeper in the tree) get
/// an instant cache hit instead of re-doing work. Scope the group to a
/// single logical operation so entries are freed when the group is dropped.
///
/// # Synchronization
///
/// The `entries` mutex protects map structure only. Value synchronization
/// is handled by the watch channel's internal `RwLock`:
///
/// - `watch::Sender::send()` takes a write-lock on the inner value.
/// - `watch::Receiver::borrow()` takes a read-lock — it sees either the
///   old or new value, never a torn read.
/// - `watch::Receiver::wait_for()` checks the current value before
///   subscribing, so a `complete()` between `borrow()→None` and
///   `wait_for()` is always caught.
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
    /// Creates a group with the given capacity limit and timeout.
    pub fn new(max_entries: usize, timeout: Duration) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            max_entries,
            timeout,
        }
    }

    /// Attempt to acquire leadership for the given key.
    ///
    /// Returns [`Acquisition::Leader`] if this task is responsible for
    /// producing the value, or [`Acquisition::Resolved`] if another task
    /// already produced it (or blocks until that task finishes).
    pub async fn try_acquire(&self, key: K) -> Result<Acquisition<V>, Error> {
        let mut rx = {
            let mut entries = self.entries.lock().await;
            if let Some(rx) = entries.get(&key) {
                let current = rx.borrow().clone();
                match current {
                    Some(Ok(value)) => return Ok(Acquisition::Resolved(value)),
                    Some(Err(e)) => return Err(e),
                    None => rx.clone(),
                }
            } else {
                if entries.len() >= self.max_entries {
                    return Err(Error::CapacityExceeded { max: self.max_entries });
                }
                let (tx, rx) = watch::channel(None);
                entries.insert(key, rx);
                return Ok(Acquisition::Leader(Handle { sender: Some(tx) }));
            }
        };

        // Wait path: entries mutex is dropped, safe to await.
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
    async fn subsequent_acquire_after_failure_returns_error() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.fail(TestError("boom"));

        let err = g.try_acquire(key("key-a")).await.unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref shared) if shared.to_string() == "boom"),
            "expected Failed with message, got: {err:?}"
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
    async fn failed_error_is_durable_across_multiple_acquires() {
        let g = group(10);
        let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
            panic!("expected Leader");
        };
        handle.fail(TestError("persistent failure"));

        for _ in 0..3 {
            let err = g.try_acquire(key("key-a")).await.unwrap_err();
            assert!(
                matches!(err, Error::Failed(ref shared) if shared.to_string() == "persistent failure"),
                "expected durable Failed, got: {err:?}"
            );
        }
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
