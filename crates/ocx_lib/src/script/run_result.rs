// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `RunResult` value returned by `ocx.run`.
//!
//! Exposed to scripts as a typed Starlark value with attributes `exit_code`,
//! `stdout`, `stderr`, `duration_ms`, `truncated`. Immutable. `truncated` is
//! `true` iff stdout or stderr hit the capture cap. A signal-killed child
//! reports the conventional `128 + signal` exit code.
//!
//! Two pieces:
//! - [`RunResult`] — internal data carrier, used by the host (`HostState`,
//!   `RunSummary`).
//! - [`RunResultValue`] — Starlark wrapper. `#[starlark_value]` impl exposes
//!   the five attributes via `get_attr` with declared shape; replaces the
//!   anonymous `AllocStruct` path the host fn used previously.

use std::fmt;

use allocative::{Allocative, Visitor};
use starlark::any::ProvidesStaticType;
use starlark::values::{AllocValue, Heap, NoSerialize, StarlarkValue, Value, starlark_value};

/// Maximum bytes captured per stream (stdout/stderr) before truncation.
///
/// Output beyond this cap is discarded and [`RunResult::truncated`] is set so
/// a runaway child can never OOM the interpreter.
pub(super) const OUTPUT_CAP_BYTES: usize = 10 * 1024 * 1024;

/// Captured outcome of one `ocx.run` invocation.
///
/// The fields are the v1 contract surface; the Starlark wrapper that exposes
/// them as typed attributes is [`RunResultValue`].
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

    /// Allocates this result as a typed Starlark value with attribute access
    /// (`r.exit_code`, `r.stdout`, …). Wraps a [`RunResultValue`] — the
    /// declared shape replaces the previous anonymous `AllocStruct` path.
    pub(super) fn alloc<'v>(&self, heap: &'v starlark::values::Heap) -> starlark::values::Value<'v> {
        heap.alloc(RunResultValue {
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            duration_ms: self.duration_ms,
            truncated: self.truncated,
        })
    }
}

/// Starlark-facing wrapper around a captured [`RunResult`].
///
/// Exposes the five v1 contract attributes via `get_attr`; `dir_attr` lists
/// them for tooling that wants to enumerate. `Allocative` is implemented
/// manually (cheap: a `String` already reports its own allocation via
/// `enter_field`, but the strings are author-supplied script output and the
/// whole value lives for one `ocx.run`-to-end-of-script duration — the simple
/// `enter_self_sized` shape is enough for the firewall use case).
#[derive(Clone, Debug, ProvidesStaticType, NoSerialize)]
pub(super) struct RunResultValue {
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    truncated: bool,
}

impl RunResultValue {
    /// Starlark type tag (the result of `type()` in a script).
    pub(super) const TYPE: &'static str = "RunResult";
}

impl Allocative for RunResultValue {
    fn visit<'a, 'b: 'a>(&self, visitor: &'a mut Visitor<'b>) {
        let mut v = visitor.enter_self_sized::<Self>();
        v.visit_field(allocative::Key::new("stdout"), &self.stdout);
        v.visit_field(allocative::Key::new("stderr"), &self.stderr);
        v.exit();
    }
}

impl fmt::Display for RunResultValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RunResult(exit_code={})", self.exit_code)
    }
}

#[starlark_value(type = RunResultValue::TYPE)]
impl<'v> StarlarkValue<'v> for RunResultValue {
    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attribute {
            "exit_code" => Some(heap.alloc(self.exit_code)),
            "stdout" => Some(heap.alloc(self.stdout.as_str())),
            "stderr" => Some(heap.alloc(self.stderr.as_str())),
            "duration_ms" => Some(heap.alloc(i64::try_from(self.duration_ms).unwrap_or(i64::MAX))),
            "truncated" => Some(Value::new_bool(self.truncated)),
            _ => None,
        }
    }

    fn dir_attr(&self) -> Vec<String> {
        ["exit_code", "stdout", "stderr", "duration_ms", "truncated"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}

impl<'v> AllocValue<'v> for RunResultValue {
    fn alloc_value(self, heap: &'v Heap) -> Value<'v> {
        heap.alloc_simple(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RunResult — value contract (C6) ──────────────────────────────────────
    //
    // Spec source: plan_package_test_scripting.md C6 + Edge Cases "Output
    // truncation cap". The fields are the v1 contract surface; the cap constant
    // is fixed (~10 MiB) and `truncated` is true iff a stream hit it.

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

    // ── RunResultValue — typed Starlark attribute access ─────────────────────
    //
    // The typed value replaces the anonymous AllocStruct path. Tests cover
    // every declared attribute and confirm the type tag.

    #[test]
    fn alloc_exposes_all_five_attributes() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let r = RunResult::new(7, "out\n".into(), "warn\n".into(), 123, true);
        let value = r.alloc(heap);
        let exit_code = value.get_attr("exit_code", heap).unwrap().unwrap();
        assert_eq!(exit_code.unpack_i32(), Some(7));
        let stdout = value.get_attr("stdout", heap).unwrap().unwrap();
        assert_eq!(stdout.unpack_str(), Some("out\n"));
        let stderr = value.get_attr("stderr", heap).unwrap().unwrap();
        assert_eq!(stderr.unpack_str(), Some("warn\n"));
        let duration = value.get_attr("duration_ms", heap).unwrap().unwrap();
        assert_eq!(duration.unpack_i32().map(i64::from), Some(123));
        let truncated = value.get_attr("truncated", heap).unwrap().unwrap();
        assert_eq!(truncated.unpack_bool(), Some(true));
    }

    #[test]
    fn alloc_type_tag_is_run_result() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let r = RunResult::new(0, String::new(), String::new(), 0, false);
        let value = r.alloc(heap);
        assert_eq!(value.get_type(), RunResultValue::TYPE);
    }

    #[test]
    fn unknown_attribute_returns_none() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let r = RunResult::new(0, String::new(), String::new(), 0, false);
        let value = r.alloc(heap);
        // `Value::get_attr` returns `Result<Option<Value>>`; an unknown
        // attribute on a typed value with `get_attr -> None` surfaces as
        // `Ok(None)` (no Starlark `AttributeError` raised here — the engine
        // decides whether to raise based on the call site).
        let outcome = value.get_attr("nonexistent", heap).unwrap();
        assert!(outcome.is_none(), "unknown attribute must be None, got {outcome:?}");
    }
}
