// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-run host state shared with the `#[starlark_module]` host functions, held in thread-locals.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use ocx_config::env::Env;
use ocx_oci::Platform;

/// Host state available to the `ocx.*` host functions during one script run.
pub(super) struct HostState {
    /// Read-only package store directory, not the bundle's files.
    pub package_root: PathBuf,
    /// Read-only root of the bundle's own files, and the read-side fallback base.
    pub content_root: PathBuf,
    /// Read-write sandbox root (sibling of the package root).
    pub scratch_root: PathBuf,
    /// Target platform from the `-p` flag, not the host's.
    pub platform: Platform,
    /// The composed package env.
    pub env: Env,
    /// Per-`ocx.run` child-process wall-clock kill deadline.
    pub wall_clock: Duration,
    /// The most recent `ocx.run` result, surfaced in the report envelope.
    pub last_run: Option<super::run_result::RunResult>,
}

thread_local! {
    static HOST: RefCell<Option<HostState>> = const { RefCell::new(None) };
    /// Outlives the [`HostScope`] so the report layer can read it after evaluation.
    static LAST_RUN: RefCell<Option<super::run_result::RunResult>> = const { RefCell::new(None) };
    /// The kind of the most recently failing `expect.*` assertion; the `fail()` builtin leaves it unset.
    static LAST_ASSERTION: RefCell<Option<super::AssertionKind>> = const { RefCell::new(None) };
    /// Set when `ocx.run` kills a child on its deadline, so the outcome is `Timeout`, not `Failed`.
    static TIMED_OUT: RefCell<bool> = const { RefCell::new(false) };
}

pub(super) fn note_timeout() {
    TIMED_OUT.with(|cell| *cell.borrow_mut() = true);
}

/// True iff the most recent run killed a child on the wall-clock deadline.
pub(super) fn timed_out() -> bool {
    TIMED_OUT.with(|cell| *cell.borrow())
}

pub(super) fn note_assertion(kind: super::AssertionKind) {
    LAST_ASSERTION.with(|cell| *cell.borrow_mut() = Some(kind));
}

pub(super) fn last_assertion() -> Option<super::AssertionKind> {
    LAST_ASSERTION.with(|cell| *cell.borrow())
}

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

/// Installs `state` for the current thread for the lifetime of the returned guard.
pub(super) fn scoped(state: HostState) -> HostScope {
    HOST.with(|cell| *cell.borrow_mut() = Some(state));
    // Reset per run, or a reused worker thread reports the previous run's kind and timeout.
    LAST_ASSERTION.with(|cell| *cell.borrow_mut() = None);
    TIMED_OUT.with(|cell| *cell.borrow_mut() = false);
    HostScope
}

impl Drop for HostScope {
    fn drop(&mut self) {
        HOST.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Runs `f` with the installed host state; panics outside a [`scoped`] region.
pub(super) fn with<R>(f: impl FnOnce(&HostState) -> R) -> R {
    HOST.with(|cell| {
        let borrow = cell.borrow();
        let state = borrow
            .as_ref()
            .expect("host state must be installed before a host fn runs");
        f(state)
    })
}

/// Runs `f` with the installed host state, or returns `None` outside a [`scoped`] region.
///
/// The globals builder also runs with no scope installed (LSP, tests), so it must use this, not [`with`].
pub(super) fn try_with<R>(f: impl FnOnce(&HostState) -> R) -> Option<R> {
    HOST.with(|cell| cell.borrow().as_ref().map(f))
}

/// Runs `f` with the installed host state mutably; panics outside a [`scoped`] region.
pub(super) fn with_mut<R>(f: impl FnOnce(&mut HostState) -> R) -> R {
    HOST.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let state = borrow
            .as_mut()
            .expect("host state must be installed before a host fn runs");
        f(state)
    })
}
