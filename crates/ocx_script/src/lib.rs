// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded Starlark test-runner — engine-swap firewall.
//!
//! The only crate that may name `starlark*` symbols; its public surface and docs stay engine-neutral.

mod arch_value;
mod engine;
mod expect_module;
mod guard;
mod host;
mod ocx_module;
mod os_value;
mod platform_value;
mod run_result;
mod sl_error;
mod variant_parity;

use std::path::Path;
use std::time::Duration;

/// Resource-guard inputs for a single script run.
///
/// Nothing preempts a pure-compute Starlark loop: such a script hangs until the process is killed.
pub struct ScriptLimits {
    /// Recursion-depth bound.
    pub max_callstack_size: usize,
    /// Per-`ocx.run` child deadline; the child is killed and the outcome times out on elapse.
    pub wall_clock: Duration,
}

/// Engine-neutral result of interpreting a script.
pub struct ScriptOutcome {
    /// What ultimately happened.
    pub kind: ScriptOutcomeKind,
}

/// Identity of the `expect.*` assertion (or `fail()`) that terminated a run.
///
/// A stable contract: its [`as_str`](Self::as_str) token appears verbatim in the JSON report.
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
    /// Stable snake_case wire token. Tooling matches on this.
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

/// Engine-neutral script outcome; variant docs never name Starlark error kinds.
#[non_exhaustive]
pub enum ScriptOutcomeKind {
    /// Script ran to completion; all assertions passed.
    Passed,
    /// An assertion failed, `expect.fail`/`fail()` was called, or a host
    /// function reported a runtime failure.
    Failed {
        /// Which assertion terminated the run; `None` when no assertion is to blame.
        kind: Option<AssertionKind>,
        /// Human-readable failure detail; the prose is not stable.
        message: String,
        /// Where the failure occurred, when known.
        location: Option<ScriptLocation>,
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
        /// Where the error occurred, when known.
        location: Option<ScriptLocation>,
    },
    /// A sandboxed filesystem operation failed for I/O reasons.
    Io {
        /// Human-readable I/O failure detail.
        message: String,
    },
    /// The wall-clock budget elapsed before the script finished.
    Timeout,
}

/// Where in the script a failure happened, as the engine reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLocation {
    /// The label the script source was parsed under.
    pub file: String,
    /// 1-indexed line of the span start.
    pub line: usize,
    /// 1-indexed column of the span start.
    pub column: usize,
}

/// Library error for unrecoverable host setup or abort failures; script failures are [`ScriptOutcomeKind`].
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

/// Interprets `source` against the composed package environment; `source_label` names it in diagnostics.
///
/// Must run inside `block_in_place` on a multi-thread Tokio runtime, or `ocx.run` panics.
///
/// # Errors
///
/// [`ScriptError`] only for unrecoverable host setup or abort; every script-level result is a [`ScriptOutcomeKind`].
pub fn run_script(
    source: &str,
    source_label: &str,
    package_root: &Path,
    scratch_root: &Path,
    platform: &ocx_oci::Platform,
    env: ocx_config::env::Env,
    limits: ScriptLimits,
) -> Result<ScriptOutcome, ScriptError> {
    let state = host::HostState {
        package_root: package_root.to_path_buf(),
        content_root: package_root.join("content"),
        scratch_root: scratch_root.to_path_buf(),
        platform: platform.clone(),
        env,
        wall_clock: limits.wall_clock,
        last_run: None,
    };
    engine::evaluate(source, source_label, &limits, state)
}

/// Takes the last `ocx.run` result of the most recent [`run_script`]; `None` when the script never called it.
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
    // appear OUTSIDE this crate (`crates/ocx_script/src/`). This locks the
    // engine swap to a single crate.

    /// Engine-crate import tokens that must not leak out of the firewall.
    const ENGINE_TOKENS: &[&str] = &[
        "use starlark",
        "starlark::",
        "starlark_syntax",
        "starlark_map",
        "starlark_derive",
    ];

    fn crates_root() -> std::path::PathBuf {
        // CARGO_MANIFEST_DIR = crates/ocx_script → parent = crates/.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate manifest dir has a parent (crates/)")
            .to_path_buf()
    }

    fn is_allowed(path: &Path) -> bool {
        let s = path.to_string_lossy().replace('\\', "/");
        // This crate's own sources, and nothing else.
        s.contains("/ocx_script/src/")
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
        // Floored on the *reader*, not on the finding: this walk is anchored at
        // `CARGO_MANIFEST_DIR`'s parent, so a crate move that left it pointing
        // somewhere thin would read a handful of files and report the firewall
        // intact. A firewall that scans almost nothing is a firewall that is
        // open, and it looks exactly like a clean one.
        assert!(
            files.len() > 200,
            "the workspace scan read only {} file(s) under {root:?} — too few for this to mean \
             anything",
            files.len()
        );

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
