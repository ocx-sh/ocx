// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one seam for secret reads, which may be a `file://` reference on a slow share.

/// Runs `read` on the blocking pool, so a read does not stall the tasks scheduled on a worker thread.
///
/// A panic resumes on the caller's task; a cancellation (runtime shutdown) is `None`.
pub(crate) async fn off_runtime<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    match tokio::task::spawn_blocking(read).await {
        Ok(value) => Some(value),
        Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under a current-thread runtime the worker is the test thread, so an inline read would answer the same id.
    #[tokio::test]
    async fn the_read_runs_off_the_runtime_thread() {
        let worker = std::thread::current().id();
        let reader = off_runtime(|| std::thread::current().id())
            .await
            .expect("the read completes");
        assert_ne!(reader, worker, "the read ran on the runtime's worker thread");
    }
}
