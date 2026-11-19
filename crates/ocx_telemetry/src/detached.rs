// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Blocking work on a thread that is never joined.

use crate::client::Failure;

/// Runs `work` on a plain thread named `name` and waits for its result.
///
/// On the blocking pool a hung call would hold the runtime's shutdown past the export deadline,
/// since an aborted task cannot cancel the work it is waiting on; a thread is abandoned instead and
/// dies with the process.
///
/// # Errors
///
/// [`Failure::Thread`] when the thread cannot start, [`Failure::Panic`] when `work` panics.
pub(crate) async fn run<Work, Output>(name: &'static str, work: Work) -> Result<Output, Failure>
where
    Work: FnOnce() -> Output + Send + 'static,
    Output: Send + 'static,
{
    run_with(name, work, |name, job| {
        std::thread::Builder::new().name(name.to_owned()).spawn(job).map(drop)
    })
    .await
}

/// [`run`] with the thread start supplied, so a start that fails can be tested.
async fn run_with<Work, Output, Spawn>(name: &'static str, work: Work, spawn: Spawn) -> Result<Output, Failure>
where
    Work: FnOnce() -> Output + Send + 'static,
    Output: Send + 'static,
    Spawn: FnOnce(&'static str, Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let spawned = spawn(
        name,
        Box::new(move || {
            // The receiver is gone once the deadline passed.
            let _ = sender.send(work());
        }),
    );
    if let Err(error) = spawned {
        tracing::debug!(target: "ocx_telemetry", kind = ?error.kind(), thread = name, "thread not started");
        return Err(Failure::Thread);
    }
    // A panic drops the sender.
    receiver.await.map_err(|_| Failure::Panic)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    fn real_thread(name: &'static str, job: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        std::thread::Builder::new().name(name.to_owned()).spawn(job).map(drop)
    }

    #[tokio::test]
    async fn a_thread_that_cannot_start_is_not_a_panic() {
        let refused = run_with("ocx-test", || 1, |_, _| Err(io::Error::other("no threads left"))).await;
        assert_eq!(refused, Err(Failure::Thread));

        let panicked = run_with("ocx-test", || -> u8 { panic!("work that breaks") }, real_thread).await;
        assert_eq!(panicked, Err(Failure::Panic));

        assert_eq!(run_with("ocx-test", || 7, real_thread).await, Ok(7));
    }

    #[tokio::test]
    async fn the_work_runs_on_the_named_thread() {
        let name = run("ocx-test-named", || std::thread::current().name().map(str::to_owned)).await;
        assert_eq!(name, Ok(Some("ocx-test-named".to_owned())));
    }
}
