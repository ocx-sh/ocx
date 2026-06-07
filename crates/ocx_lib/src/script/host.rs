// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-run host state shared with the `#[starlark_module]` host functions.
//!
//! `starlark::Evaluator` is `!Send` and its `extra` slot requires a
//! `'static`-lifetime `AnyLifetime` payload, which would force the borrowed
//! sandbox roots / composed `Env` to be cloned with non-trivial lifetime
//! gymnastics. `run_script` is sync and single-threaded (invoked via
//! `tokio::task::block_in_place`), so a thread-local scoped to the call is the
//! simplest sound channel for host state. The [`scoped`] guard installs the
//! state for the duration of one evaluation and removes it on drop (RAII), so
//! state never leaks across runs on a reused worker thread.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use crate::env::Env;
use crate::oci::Platform;

/// Host state available to the `ocx.*` host functions during one script run.
pub(super) struct HostState {
    /// Read-only materialized package root.
    pub package_root: PathBuf,
    /// Read-write sandbox root (sibling of the package root).
    pub scratch_root: PathBuf,
    /// Target platform reflecting the `-p` flag (NOT the host).
    pub platform: Platform,
    /// The composed package env (already through `Env::apply_ocx_config`).
    pub env: Env,
    /// Per-`ocx.run` child-process wall-clock kill deadline.
    pub wall_clock: Duration,
    /// The most recent `ocx.run` result, surfaced in the report envelope.
    pub last_run: Option<super::run_result::RunResult>,
}

thread_local! {
    static HOST: RefCell<Option<HostState>> = const { RefCell::new(None) };
    /// Survives the [`HostScope`] drop so the report layer can read the last
    /// `ocx.run` result after evaluation finishes.
    static LAST_RUN: RefCell<Option<super::run_result::RunResult>> = const { RefCell::new(None) };
    /// The `expect.*` assertion identity recorded by the most recently failing
    /// assertion host fn. Read by `engine::classify` to attribute a `Failed`
    /// outcome to a specific assertion kind (plan C5 stable contract). Set just
    /// before an `expect.*` fn returns its error; `Fail`-builtin path leaves it
    /// unset (engine defaults attribution accordingly).
    static LAST_ASSERTION: RefCell<Option<super::AssertionKind>> = const { RefCell::new(None) };
    /// Set by the `ocx.run` per-child wall-clock kill branch (Codex C4) so
    /// `engine::classify` can map the resulting terminal error to the
    /// documented [`super::ScriptOutcomeKind::Timeout`] instead of collapsing
    /// it into the generic `Failed` bucket. Reset by [`scoped`] every run.
    static TIMED_OUT: RefCell<bool> = const { RefCell::new(false) };
}

/// Records that an `ocx.run` child was killed for exceeding its per-call
/// wall-clock deadline (Codex C4). Read back by `engine::classify` to surface
/// the typed `Timeout` outcome/status.
pub(super) fn note_timeout() {
    TIMED_OUT.with(|cell| *cell.borrow_mut() = true);
}

/// True iff the most recent run killed a child on the wall-clock deadline.
pub(super) fn timed_out() -> bool {
    TIMED_OUT.with(|cell| *cell.borrow())
}

/// Records the assertion identity for the failing `expect.*` host fn so the
/// engine can attribute the terminal `Failed` outcome to it (plan C5).
pub(super) fn note_assertion(kind: super::AssertionKind) {
    LAST_ASSERTION.with(|cell| *cell.borrow_mut() = Some(kind));
}

/// Reads the last-recorded assertion identity without clearing it. The
/// [`HostScope`] drop clears `HOST`; `LAST_ASSERTION` is reset by [`scoped`]
/// at the start of every run so a reused worker thread never sees a stale kind.
pub(super) fn last_assertion() -> Option<super::AssertionKind> {
    LAST_ASSERTION.with(|cell| *cell.borrow())
}

/// Stashes the final `ocx.run` result so it outlives the [`HostScope`].
/// Called by `engine::evaluate` immediately before the scope drops.
pub(super) fn stash_last_run(run: Option<super::run_result::RunResult>) {
    LAST_RUN.with(|cell| *cell.borrow_mut() = run);
}

/// Takes the stashed last-run result (engine-neutral), clearing it.
pub fn take_last_run() -> Option<super::RunSummary> {
    LAST_RUN.with(|cell| {
        cell.borrow_mut().take().map(|r| super::RunSummary {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
            duration_ms: r.duration_ms,
            truncated: r.truncated,
        })
    })
}

/// RAII guard: installs `state` for the current thread and clears it on drop.
pub(super) struct HostScope;

/// Installs `state` for the current thread for the lifetime of the returned
/// guard. Dropping the guard removes it (so a reused Tokio worker thread never
/// observes stale state from a previous run).
pub(super) fn scoped(state: HostState) -> HostScope {
    HOST.with(|cell| *cell.borrow_mut() = Some(state));
    // Reset the per-run assertion attribution so a reused worker thread never
    // observes a stale kind from a previous run.
    LAST_ASSERTION.with(|cell| *cell.borrow_mut() = None);
    // Same for the wall-clock timeout flag (Codex C4).
    TIMED_OUT.with(|cell| *cell.borrow_mut() = false);
    HostScope
}

impl Drop for HostScope {
    fn drop(&mut self) {
        HOST.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Runs `f` with a shared reference to the installed host state.
///
/// Panics only if called outside a [`scoped`] region — that is a host bug, not
/// a script-reachable path (every host fn runs inside `evaluate`).
pub(super) fn with<R>(f: impl FnOnce(&HostState) -> R) -> R {
    HOST.with(|cell| {
        let borrow = cell.borrow();
        let state = borrow
            .as_ref()
            .expect("host state must be installed before a host fn runs");
        f(state)
    })
}

/// Runs `f` with a shared reference to the installed host state if one is
/// present, returning `None` when called outside a [`scoped`] region.
///
/// Unlike [`with`], this never panics. The globals builder runs in two
/// contexts: inside a script run (a host scope is installed, so the per-run
/// `ocx.target_platform` reflects the `-p` flag) and outside one (LSP globals
/// build and the structural variant-parity test, where there is no run and the
/// attribute falls back to `Platform::Any`). The fallible accessor lets the
/// builder degrade gracefully in the latter rather than abort.
pub(super) fn try_with<R>(f: impl FnOnce(&HostState) -> R) -> Option<R> {
    HOST.with(|cell| cell.borrow().as_ref().map(f))
}

/// Runs `f` with a mutable reference to the installed host state.
pub(super) fn with_mut<R>(f: impl FnOnce(&mut HostState) -> R) -> R {
    HOST.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let state = borrow
            .as_mut()
            .expect("host state must be installed before a host fn runs");
        f(state)
    })
}
