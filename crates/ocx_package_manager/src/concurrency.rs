// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Concurrency cap for parallel package operations.
//!
//! Used by [`PackageManager::pull_all`](super::PackageManager::pull_all) and
//! its call sites to limit how many root packages dispatch in parallel. The
//! cap is enforced via a shared `tokio::sync::Semaphore` acquired at the
//! outer dispatch only — inner dependency and layer setup remain unbounded
//! to prevent deadlock when transitive permits would block on the same pool.
//!
//! `--jobs 0` resolves to logical-core count at `Context::try_init` time
//! (snapshot semantics — runtime CPU-affinity changes do not re-read).

use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Outer-dispatch concurrency cap for `pull_all`.
///
/// `Unbounded` is the legacy default — every requested root package spawns
/// immediately and singleflight + per-package file locks already protect
/// the registry. `Limit(N)` adds a semaphore so at most N root pulls run
/// concurrently, useful for registry rate limiting and CI matrices.
#[derive(Debug, Clone, Copy, Default)]
pub enum Concurrency {
    #[default]
    Unbounded,
    Limit(NonZeroUsize),
}

impl Concurrency {
    /// Resolves `--jobs 0` (= "use all logical cores") to an explicit `Limit`.
    /// Falls back to a one-permit cap if the platform cannot report a count.
    pub fn cores() -> Self {
        let n = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
        Self::Limit(n)
    }

    /// Builds the shared semaphore used to gate outer dispatch, or `None`
    /// when no cap is configured. Callers clone the returned `Arc` per
    /// spawned task and acquire owned permits before doing work.
    pub fn semaphore(self) -> Option<Arc<Semaphore>> {
        match self {
            Self::Unbounded => None,
            Self::Limit(n) => Some(Arc::new(Semaphore::new(n.get()))),
        }
    }
}

/// Acquires an owned permit on the optional semaphore. `None` means
/// unbounded — returns `None` immediately so the caller proceeds without
/// gating. Panics only if the semaphore was closed, which never happens
/// in the package-manager pipeline.
pub async fn acquire_permit(semaphore: &Option<Arc<Semaphore>>) -> Option<OwnedSemaphorePermit> {
    match semaphore {
        None => None,
        Some(sem) => Some(
            sem.clone()
                .acquire_owned()
                .await
                .expect("pull semaphore is never closed"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_returns_no_semaphore() {
        assert!(Concurrency::Unbounded.semaphore().is_none());
    }

    #[test]
    fn limit_returns_semaphore_with_n_permits() {
        let n = NonZeroUsize::new(3).unwrap();
        let sem = Concurrency::Limit(n).semaphore().expect("limit yields semaphore");
        assert_eq!(sem.available_permits(), 3);
    }

    #[test]
    fn cores_resolves_to_at_least_one_permit() {
        let Concurrency::Limit(n) = Concurrency::cores() else {
            panic!("cores must resolve to Limit");
        };
        assert!(n.get() >= 1);
    }

    #[tokio::test]
    async fn acquire_permit_unbounded_yields_none() {
        let permit = acquire_permit(&None).await;
        assert!(permit.is_none());
    }

    #[tokio::test]
    async fn acquire_permit_limit_yields_some() {
        let sem = Concurrency::Limit(NonZeroUsize::new(1).unwrap()).semaphore();
        let permit = acquire_permit(&sem).await;
        assert!(permit.is_some());
    }
}
