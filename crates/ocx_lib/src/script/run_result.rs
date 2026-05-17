// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `RunResult` value returned by `ocx.run`.
//!
//! Exposed to scripts with attributes `exit_code`, `stdout`, `stderr`,
//! `duration_ms`, `truncated`. Immutable. `truncated` is `true` iff stdout or
//! stderr hit the capture cap. A signal-killed child reports the conventional
//! `128 + signal` exit code.

/// Maximum bytes captured per stream (stdout/stderr) before truncation.
///
/// Output beyond this cap is discarded and [`RunResult::truncated`] is set so
/// a runaway child can never OOM the interpreter.
pub(super) const OUTPUT_CAP_BYTES: usize = 10 * 1024 * 1024;

/// Captured outcome of one `ocx.run` invocation.
///
/// The fields are the v1 contract surface; the Starlark value wrapper that
/// exposes them as attributes is built in the implementation phase.
#[derive(Debug, Clone)]
pub(super) struct RunResult {
    /// Child exit code (or `128 + signal` when signal-killed).
    pub exit_code: i32,
    /// Captured stdout (UTF-8 lossy), capped at [`OUTPUT_CAP_BYTES`].
    pub stdout: String,
    /// Captured stderr (UTF-8 lossy), capped at [`OUTPUT_CAP_BYTES`].
    pub stderr: String,
    /// Wall-clock duration of the spawn, in milliseconds.
    pub duration_ms: u64,
    /// `true` iff stdout or stderr hit [`OUTPUT_CAP_BYTES`].
    pub truncated: bool,
}

impl RunResult {
    /// Builds a captured run result. Callers cap `stdout`/`stderr` at
    /// [`OUTPUT_CAP_BYTES`] and pass `truncated=true` when either stream hit
    /// the cap (the cap + truncation flag are computed by the spawn path, not
    /// re-derived here — this constructor only carries the values).
    pub(super) fn new(exit_code: i32, stdout: String, stderr: String, duration_ms: u64, truncated: bool) -> Self {
        Self {
            exit_code,
            stdout,
            stderr,
            duration_ms,
            truncated,
        }
    }

    /// Allocates this result as a Starlark struct value with attribute access
    /// (`r.exit_code`, `r.stdout`, …) so `expect.ok(r)` can read `r.exit_code`.
    pub(super) fn alloc<'v>(&self, heap: &'v starlark::values::Heap) -> starlark::values::Value<'v> {
        use starlark::values::structs::AllocStruct;
        heap.alloc(AllocStruct([
            ("exit_code", heap.alloc(self.exit_code)),
            ("stdout", heap.alloc(self.stdout.as_str())),
            ("stderr", heap.alloc(self.stderr.as_str())),
            (
                "duration_ms",
                heap.alloc(i64::try_from(self.duration_ms).unwrap_or(i64::MAX)),
            ),
            ("truncated", heap.alloc(self.truncated)),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RunResult — value contract (C6) ──────────────────────────────────────
    //
    // Spec source: plan_package_test_scripting.md C6 + Edge Cases "Output
    // truncation cap". The fields are the v1 contract surface; the cap constant
    // is fixed (~10 MiB) and `truncated` is true iff a stream hit it. These
    // FAIL against the `unimplemented!()` constructor (panic).

    #[test]
    fn output_cap_is_ten_mib() {
        // Edge Cases: builder picks one constant (~10 MiB) and documents it.
        assert_eq!(OUTPUT_CAP_BYTES, 10 * 1024 * 1024);
    }

    #[test]
    fn exposes_fields_verbatim_when_not_truncated() {
        // C6: exit_code/stdout/stderr/duration_ms/truncated round-trip; a
        // non-truncated result carries truncated=false.
        let r = RunResult::new(0, "out".into(), "err".into(), 42, false);
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "out");
        assert_eq!(r.stderr, "err");
        assert_eq!(r.duration_ms, 42);
        assert!(!r.truncated);
    }

    #[test]
    fn truncated_flag_is_carried() {
        // C6: truncated=true iff stdout or stderr hit OUTPUT_CAP_BYTES.
        let r = RunResult::new(1, String::new(), String::new(), 0, true);
        assert!(r.truncated);
    }

    #[test]
    fn signal_killed_child_uses_128_plus_signal_convention() {
        // C6 edge: a signal-killed child reports `128 + signal` (e.g. SIGTERM
        // = 15 → 143). The constructor must carry that value unchanged.
        let r = RunResult::new(143, String::new(), String::new(), 0, false);
        assert_eq!(r.exit_code, 143);
    }
}
