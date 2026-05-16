// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded Starlark test-runner — engine-swap firewall.
//!
//! This module and its submodules are the ONLY place in the workspace that may
//! reference `starlark*` symbols. The public surface here is engine-neutral by
//! contract: no Starlark concept (`Evaluator`, `ErrorKind`, …) appears in any
//! public type, signature, or doc comment. Terminal-error classification
//! happens *inside* the firewall (`engine`), so an engine swap stays a
//! single-directory change.
//!
//! The firewall caps the *internal maintenance* blast radius of an engine
//! change only. It is NOT a reversibility argument — the published exit-code
//! contract ([`ScriptOutcomeKind`] → exit code) and the corpus of
//! author-written `.star` scripts are not reversible regardless of it.
//!
//! Entry point: [`run_script`] — a **sync** function (the Starlark `Evaluator`
//! is `!Send + !Sync`). The async command layer wraps the call in
//! `tokio::task::block_in_place`.

mod engine;
mod expect_module;
mod guard;
mod host;
mod lsp;
mod ocx_module;
mod run_result;
mod sl_error;

pub use lsp::run_lsp_server;

use std::path::Path;
use std::time::Duration;

/// Resource-guard inputs for a single script run.
///
/// starlark 0.13.0 exposes ONLY `Evaluator::set_max_callstack_size`. There is
/// NO `set_max_tick_count`, NO `set_max_heap_size`, and NO eval-deadline /
/// cancellation / tick hook of any kind. The v1 guard story is therefore
/// exactly two in-scope mechanisms and one accepted gap:
///
/// 1. `max_callstack_size` — the ONLY in-process bound (recursion depth).
/// 2. `wall_clock` — a best-effort I/O bound. It works by killing the in-flight
///    `ocx.run` child process when its per-run deadline elapses. This reliably
///    bounds the common case (scripts spend wall-time inside subprocesses).
/// 3. ACCEPTED v1 LIMITATION: a pathological pure-compute Starlark loop cannot
///    be preempted. starlark 0.13.0 gives no in-eval cancellation; the
///    `Evaluator` is `!Send` with non-`'static` borrowed roots so it cannot be
///    moved to a killable task; and `tokio::time::timeout` around
///    `block_in_place` does NOT fire (the surrounding future never yields while
///    the inline blocking closure runs). Such a script hangs until the OS
///    process is externally killed. Documented, not silently mitigated.
pub struct ScriptLimits {
    /// Recursion-depth bound — the only in-process limit in starlark 0.13.0.
    pub max_callstack_size: usize,
    /// Best-effort I/O bound: per-`ocx.run` child-process deadline. The host
    /// function kills the child on elapse and marks the outcome timed-out. NOT
    /// a pure-compute guard (see struct note 3).
    pub wall_clock: Duration,
}

/// Engine-neutral result of interpreting a script.
pub struct ScriptOutcome {
    /// What ultimately happened.
    pub kind: ScriptOutcomeKind,
}

/// Identity of the `expect.*` assertion (or `fail()`) that terminated a run.
///
/// This is a STABLE v1 contract (plan C5): the failing assertion's kind is
/// surfaced verbatim in the JSON report so tooling can branch on *which*
/// assertion failed without parsing the (non-stable) prose message. `Fail`
/// covers the builtin `fail()` / `expect.fail`; `Other` covers a host-side
/// runtime/sandbox failure that is not an assertion (e.g. a guard rejection).
/// `#[non_exhaustive]` so adding a kind is not a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssertionKind {
    /// `expect.ok`
    Ok,
    /// `expect.eq`
    Eq,
    /// `expect.ne`
    Ne,
    /// `expect.true`
    True,
    /// `expect.false`
    False,
    /// `expect.contains`
    Contains,
    /// `expect.matches`
    Matches,
    /// `expect.fail` / builtin `fail()`
    Fail,
    /// A non-assertion host failure (sandbox rejection, runtime host error).
    Other,
}

impl AssertionKind {
    /// Stable snake_case wire token (plan C5). Tooling matches on this.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::True => "true",
            Self::False => "false",
            Self::Contains => "contains",
            Self::Matches => "matches",
            Self::Fail => "fail",
            Self::Other => "other",
        }
    }
}

/// Engine-neutral script outcome.
///
/// This enum IS a documented public contract; `#[non_exhaustive]` so adding a
/// variant is not a breaking change. Variant docs describe OCX-level meaning
/// only — they MUST NOT name Starlark error-kind variants (that is an engine
/// internal, classified in `engine`).
#[non_exhaustive]
pub enum ScriptOutcomeKind {
    /// Script ran to completion; all assertions passed.
    Passed,
    /// An assertion failed, `expect.fail`/`fail()` was called, or a host
    /// function reported a runtime failure.
    Failed {
        /// Which `expect.*` assertion (or `fail()`) terminated the run. `None`
        /// when the engine could not attribute the failure to a specific
        /// assertion (e.g. a `StackOverflow`/`Internal` terminal error). This
        /// is the STABLE v1 `kind` contract (plan C5).
        kind: Option<AssertionKind>,
        /// Human-readable failure detail. The prose is non-stable; only its
        /// presence (and the JSON field shape, when rendered) is contractual.
        message: String,
    },
    /// The script could not be used as given (e.g. unreadable script file).
    Usage {
        /// Human-readable usage detail.
        message: String,
    },
    /// The script source is invalid: syntax, arity, or type error.
    ScriptError {
        /// Human-readable script-error detail.
        message: String,
    },
    /// A sandboxed filesystem operation failed for I/O reasons.
    Io {
        /// Human-readable I/O failure detail.
        message: String,
    },
    /// The wall-clock budget elapsed before the script finished.
    Timeout,
}

/// Library error for unrecoverable HOST setup/abort failures only.
///
/// Script-level failures are NEVER errors — they are [`ScriptOutcomeKind`]. A
/// permanent `ocx_lib` public API must not erase to `anyhow`
/// (`quality-rust-errors.md` Block-tier).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScriptError {
    /// host-side setup failed before evaluation could begin
    #[error("script host setup failed: {0}")]
    HostSetup(String),
    /// evaluation was aborted by the host (e.g. runtime/join failure)
    #[error("script evaluation aborted: {0}")]
    RuntimeAbort(String),
}

/// Interpret `source` (the script text, already read from a file or stdin)
/// against the composed package environment.
///
/// `source_label` is the filename label used in parse/eval diagnostics
/// (`"<stdin>"` for the `--script -` form, otherwise the script path string).
///
/// SYNC by design: `starlark::Evaluator` is `!Send + !Sync`, so it cannot be
/// held across an `.await` on a multi-thread Tokio runtime. The async caller
/// wraps this in `tokio::task::block_in_place`. `spawn_blocking` is rejected
/// because its `'static` bound would force cloning every borrowed root
/// reference.
///
/// PRECONDITION: the calling future must be running on Tokio's **multi-thread**
/// runtime — the `ocx.run` host fn uses `Handle::current().block_on(...)`
/// inside `block_in_place`, which panics on a `current_thread` runtime. OCX
/// `main.rs` uses the default multi-thread `#[tokio::main]` today; the call
/// site carries a `debug_assert!` on the runtime flavor.
///
/// # Errors
///
/// Returns `Err(ScriptError)` only for unrecoverable host setup/abort. All
/// script-level results — assertion failure, syntax error, sandbox violation,
/// and timeout — are encoded in [`ScriptOutcomeKind`], never `Err`.
pub fn run_script(
    source: &str,
    source_label: &str,
    package_root: &Path,
    scratch_root: &Path,
    platform: &crate::oci::Platform,
    env: crate::env::Env,
    limits: ScriptLimits,
) -> Result<ScriptOutcome, ScriptError> {
    let state = host::HostState {
        package_root: package_root.to_path_buf(),
        scratch_root: scratch_root.to_path_buf(),
        platform: platform.clone(),
        env,
        wall_clock: limits.wall_clock,
        last_run: None,
    };
    engine::evaluate(source, source_label, &limits, state)
}

/// Surfaced `ocx.run` result captured during the most recent [`run_script`]
/// call, for the report envelope.
///
/// Engine-neutral: a plain tuple of the published `RunResult` fields. `None`
/// when the script never called `ocx.run` (or it was cleared between runs).
pub fn last_run_summary() -> Option<RunSummary> {
    host::take_last_run()
}

/// Engine-neutral mirror of the surfaced `ocx.run` result fields.
pub struct RunSummary {
    /// Child exit code (or `128 + signal` when signal-killed).
    pub exit_code: i32,
    /// Captured stdout (possibly truncated).
    pub stdout: String,
    /// Captured stderr (possibly truncated).
    pub stderr: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// `true` iff stdout or stderr hit the capture cap.
    pub truncated: bool,
}

#[cfg(test)]
mod firewall_tests {
    use std::path::Path;

    // ── C-test: engine-isolation firewall (plan Step 3.2 / C-test) ───────────
    //
    // Spec source: plan_package_test_scripting.md "C-test — engine isolation
    // structural test" + module doc. No `starlark`-family crate import path may
    // appear OUTSIDE this firewall directory (`crates/ocx_lib/src/script/`) and
    // the `ocx_cli` `starlark_lsp` command module. This locks the engine swap
    // to a single directory. The `StarlarkLsp` enum variant + `pub mod
    // starlark_lsp;` wiring in `command.rs` is the dispatch arm for the lsp
    // command module and is explicitly permitted (it names the command module,
    // not the engine crate).

    /// Engine-crate import tokens that must not leak out of the firewall.
    const ENGINE_TOKENS: &[&str] = &[
        "use starlark",
        "starlark::",
        "starlark_syntax",
        "starlark_map",
        "starlark_derive",
    ];

    fn crates_root() -> std::path::PathBuf {
        // CARGO_MANIFEST_DIR = crates/ocx_lib → parent = crates/.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate manifest dir has a parent (crates/)")
            .to_path_buf()
    }

    fn is_allowed(path: &Path) -> bool {
        let s = path.to_string_lossy().replace('\\', "/");
        // The firewall directory itself + its named module file.
        s.contains("/ocx_lib/src/script/")
            || s.ends_with("/ocx_lib/src/script.rs")
            // The ocx_cli starlark-lsp command module (allowed per C-test).
            || s.ends_with("/ocx_cli/src/command/starlark_lsp.rs")
            // command.rs only names the StarlarkLsp variant / command module
            // (dispatch wiring), never the engine crate — allowed.
            || s.ends_with("/ocx_cli/src/command.rs")
    }

    fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()) == Some("target") {
                    continue;
                }
                collect_rs(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn no_starlark_import_outside_firewall() {
        let root = crates_root();
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(!files.is_empty(), "expected to find Rust sources under {root:?}");

        let mut violations = Vec::new();
        for f in &files {
            if is_allowed(f) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(f) else {
                continue;
            };
            for token in ENGINE_TOKENS {
                if content.contains(token) {
                    violations.push(format!("{} contains `{}`", f.display(), token));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "starlark engine crate leaked outside the firewall:\n{}",
            violations.join("\n")
        );
    }
}
