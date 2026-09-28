// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Concurrency cap for parallel package operations.
//!
//! Permits are acquired at the outer (root-package) dispatch only; gating inner
//! dependency and layer setup on the same pool deadlocks on transitive permits.

use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Outer-dispatch concurrency cap for `pull_all`.
#[derive(Debug, Clone, Copy, Default)]
pub enum Concurrency {
    #[default]
    Unbounded,
    Limit(NonZeroUsize),
}

impl Concurrency {
    /// A `Limit` of the logical-core count, or one permit if the platform cannot report it.
    pub fn cores() -> Self {
        let n = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
        Self::Limit(n)
    }

    /// The shared semaphore gating outer dispatch, or `None` when unbounded.
    pub fn semaphore(self) -> Option<Arc<Semaphore>> {
        match self {
            Self::Unbounded => None,
            Self::Limit(n) => Some(Arc::new(Semaphore::new(n.get()))),
        }
    }
}

/// Acquires an owned permit, or returns `None` at once when unbounded.
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
