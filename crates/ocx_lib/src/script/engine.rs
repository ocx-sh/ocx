// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Starlark evaluation driver + terminal-error classification.
//!
//! ALL `starlark*` symbols stay inside the firewall directory. This module
//! builds the globals, parses + evaluates the script, and classifies the
//! terminal Starlark error into the engine-neutral
//! [`super::ScriptOutcomeKind`]. No Starlark concept escapes the firewall.

use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::{Error as StarlarkError, ErrorKind};

use super::host::{self, HostState};
use super::{ScriptError, ScriptLimits, ScriptOutcome, ScriptOutcomeKind};

/// Deterministic stdlib extensions enabled for OCX test scripts.
///
/// All are pure/deterministic (no host network/time/random — the sandbox
/// guarantee is unaffected). `Breakpoint` (interactive console) and `Internal`
/// (explicitly "not for production use") are deliberately excluded. Without an
/// explicit set, `GlobalsBuilder::standard()` lacks `print`/`json`/`map`/etc.,
/// which test scripts routinely use.
pub(super) const SCRIPT_EXTENSIONS: &[LibraryExtension] = &[
    LibraryExtension::StructType,
    LibraryExtension::RecordType,
    LibraryExtension::EnumType,
    LibraryExtension::NamespaceType,
    LibraryExtension::Map,
    LibraryExtension::Filter,
    LibraryExtension::Partial,
    LibraryExtension::Debug,
    LibraryExtension::Print,
    LibraryExtension::Pprint,
    LibraryExtension::Pstr,
    LibraryExtension::Prepr,
    LibraryExtension::Json,
    LibraryExtension::SetType,
];

/// Builds the script globals: the Starlark standard library + the curated
/// deterministic extensions + the two host modules (`ocx.*`, `expect.*`).
fn build_globals() -> Globals {
    GlobalsBuilder::extended_by(SCRIPT_EXTENSIONS)
        .with(super::ocx_module::ocx_module)
        .with(super::expect_module::expect_module)
        .build()
}

/// The dialect for OCX test scripts: standard Starlark with `load()` disabled
/// (scripts are single-file by contract).
fn dialect() -> Dialect {
    Dialect {
        enable_load: false,
        ..Dialect::Standard
    }
}

/// Parses + evaluates `source` with `state` installed, classifying the
/// terminal error into an engine-neutral outcome.
pub(super) fn evaluate(
    source: &str,
    source_label: &str,
    limits: &ScriptLimits,
    state: HostState,
) -> Result<ScriptOutcome, ScriptError> {
    let dialect = dialect();
    let ast = match AstModule::parse(source_label, source.to_owned(), &dialect) {
        Ok(ast) => ast,
        // Parse failure is a script-source error (syntax). Classify and return
        // as an outcome — never a host `Err`.
        Err(e) => return Ok(ScriptOutcome { kind: classify(&e) }),
    };

    let globals = build_globals();
    let module = Module::new();

    // Install per-run host state for the `ocx.*` host fns. The RAII guard
    // clears it on drop so a reused worker thread never sees stale state.
    let _scope = host::scoped(state);

    let outcome = {
        let mut eval = Evaluator::new(&module);
        // The ONLY in-process bound starlark 0.13.0 exposes (recursion depth).
        // A `HostSetup` error here is an unrecoverable host failure.
        eval.set_max_callstack_size(limits.max_callstack_size)
            .map_err(|e| ScriptError::HostSetup(e.to_string()))?;

        match eval.eval_module(ast, &globals) {
            Ok(_) => ScriptOutcome {
                kind: ScriptOutcomeKind::Passed,
            },
            Err(e) => ScriptOutcome { kind: classify(&e) },
        }
    };

    // Capture the surfaced `ocx.run` result before the scope drops so the
    // report layer can read it after evaluation.
    host::stash_last_run(host::with(|s| s.last_run.clone()));
    drop(_scope);

    Ok(outcome)
}

/// Maps a terminal Starlark error to the engine-neutral outcome kind.
///
/// This is the SOLE place Starlark `ErrorKind` variant names appear. The match
/// is exhaustive over the real 0.13.0 variant set (probed by
/// [`tests::error_kind_variants_compile`]); `#[non_exhaustive]` upstream forces
/// the wildcard arm, mapped to `Failed` (no exit code `2` is invented).
fn classify(error: &StarlarkError) -> ScriptOutcomeKind {
    use super::AssertionKind;
    let message = error.to_string();
    // Codex C4: a child killed on the per-`ocx.run` wall-clock deadline raises
    // a host (`fail()`) error that would otherwise collapse into `Failed`,
    // leaving the documented `Timeout` outcome unreachable. The kill branch
    // records a typed flag; surface it here so the JSON envelope/status can
    // report a timeout. (Timeout→exit-code mapping is unchanged: a separately
    // deferred decision.)
    if host::timed_out() {
        return ScriptOutcomeKind::Timeout;
    }
    // The `expect.*` host fns record their identity before returning a
    // host-fn error; read it back to attribute the failure (plan C5 stable
    // `kind`). The builtin `fail()` (ErrorKind::Fail) does not flow through an
    // `expect.*` fn — default it to `Fail`.
    let recorded = host::last_assertion();
    match error.kind() {
        // Builtin `fail()` → failure (exit 1). Attribute to the recorded
        // assertion if `expect.fail` set one, else the bare `Fail` builtin.
        ErrorKind::Fail(_) => ScriptOutcomeKind::Failed {
            kind: Some(recorded.unwrap_or(AssertionKind::Fail)),
            message,
        },
        // Recursion past `max_callstack_size` → failure (exit 1). Not
        // attributable to a single assertion.
        ErrorKind::StackOverflow(_) => ScriptOutcomeKind::Failed { kind: None, message },
        // Syntax / arity / type errors → script error (exit 65).
        ErrorKind::Parser(_) | ErrorKind::Function(_) | ErrorKind::Value(_) | ErrorKind::Scope(_) => {
            ScriptOutcomeKind::ScriptError { message }
        }
        // Host-fn failures → failure (exit 1). `expect.*` records its kind; a
        // non-assertion host failure (sandbox rejection) records nothing → `Other`.
        ErrorKind::Native(_) => ScriptOutcomeKind::Failed {
            kind: Some(recorded.unwrap_or(AssertionKind::Other)),
            message,
        },
        // Engine internals / freeze / fallback → failure (exit 1; no code 2).
        // Not attributable to a single assertion.
        ErrorKind::Internal(_) | ErrorKind::Freeze(_) | ErrorKind::Other(_) => {
            ScriptOutcomeKind::Failed { kind: None, message }
        }
        // `ErrorKind` is `#[non_exhaustive]` upstream — unclassified variants
        // map to `Failed` (locked here, never silently to a new code).
        _ => ScriptOutcomeKind::Failed { kind: None, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compiling probe: confirms the REAL starlark =0.13.0 `ErrorKind` variant
    /// set (Fix#13 / Contradiction #1). If a family bump changes the variants,
    /// this fails to compile at stub time — not in a later phase.
    #[test]
    fn error_kind_variants_compile() {
        fn _exhaustive(kind: &ErrorKind) {
            match kind {
                ErrorKind::Fail(_) => {}
                ErrorKind::StackOverflow(_) => {}
                ErrorKind::Value(_) => {}
                ErrorKind::Function(_) => {}
                ErrorKind::Scope(_) => {}
                ErrorKind::Parser(_) => {}
                ErrorKind::Freeze(_) => {}
                ErrorKind::Internal(_) => {}
                ErrorKind::Native(_) => {}
                ErrorKind::Other(_) => {}
                _ => {}
            }
        }
        let _ = _exhaustive as fn(&ErrorKind);
        let _ = classify as fn(&StarlarkError) -> ScriptOutcomeKind;
    }

    // ── classify: ErrorKind → ScriptOutcomeKind (C3 / Error Taxonomy) ────────
    //
    // Spec source: plan_package_test_scripting.md C3 + Error Taxonomy table +
    // engine.rs `classify` doc. Each starlark 0.13.0 `ErrorKind` variant must
    // map to the documented outcome. No exit code `2` is ever produced (the
    // existing `ExitCode` enum has no `2`).

    /// Builds a real `StarlarkError` from an `ErrorKind` so `classify` runs on
    /// genuine engine values (no mock). `Error::new_kind` is the public
    /// constructor in starlark 0.13.0.
    fn err(kind: ErrorKind) -> StarlarkError {
        StarlarkError::new_kind(kind)
    }

    fn is_failed(o: &ScriptOutcomeKind) -> bool {
        matches!(o, ScriptOutcomeKind::Failed { .. })
    }

    fn is_script_error(o: &ScriptOutcomeKind) -> bool {
        matches!(o, ScriptOutcomeKind::ScriptError { .. })
    }

    #[test]
    fn classify_fail_maps_to_failed() {
        // Error Taxonomy: assertion / `expect.fail` / `fail()` → Failed (exit 1).
        let e = err(ErrorKind::Fail(anyhow::anyhow!("boom")));
        assert!(is_failed(&classify(&e)));
    }

    #[test]
    fn classify_stack_overflow_maps_to_failed() {
        // C3 edge: recursion past `max_callstack_size` → engine error → Failed (1).
        let e = err(ErrorKind::StackOverflow(anyhow::anyhow!("deep")));
        assert!(is_failed(&classify(&e)));
    }

    #[test]
    fn classify_parser_maps_to_script_error() {
        // Error Taxonomy: syntax / arity / type error → ScriptError (exit 65).
        let e = err(ErrorKind::Parser(anyhow::anyhow!("syntax")));
        assert!(is_script_error(&classify(&e)));
    }

    #[test]
    fn classify_function_maps_to_script_error() {
        let e = err(ErrorKind::Function(anyhow::anyhow!("arity")));
        assert!(is_script_error(&classify(&e)));
    }

    #[test]
    fn classify_value_maps_to_script_error() {
        let e = err(ErrorKind::Value(anyhow::anyhow!("type")));
        assert!(is_script_error(&classify(&e)));
    }

    #[test]
    fn classify_scope_maps_to_script_error() {
        let e = err(ErrorKind::Scope(anyhow::anyhow!("scope")));
        assert!(is_script_error(&classify(&e)));
    }

    #[test]
    fn classify_native_maps_to_failed() {
        // Error Taxonomy: host fn reports a runtime failure → Failed (exit 1).
        let e = err(ErrorKind::Native(anyhow::anyhow!("host")));
        assert!(is_failed(&classify(&e)));
    }

    #[test]
    fn classify_internal_maps_to_failed_not_code_two() {
        // ADR Exit Code Scheme + C3 edge: engine-internal error → Failed (1);
        // explicitly NOT a new code `2` (the enum has no `2`).
        let e = err(ErrorKind::Internal(anyhow::anyhow!("internal")));
        assert!(is_failed(&classify(&e)));
    }

    #[test]
    fn classify_freeze_maps_to_failed() {
        let e = err(ErrorKind::Freeze(anyhow::anyhow!("freeze")));
        assert!(is_failed(&classify(&e)));
    }

    #[test]
    fn classify_other_maps_to_failed() {
        let e = err(ErrorKind::Other(anyhow::anyhow!("other")));
        assert!(is_failed(&classify(&e)));
    }

    // ── C-4: per-child wall-clock kill is reported as Timeout ────────────────
    //
    // A child exceeding the per-`ocx.run` wall-clock deadline previously
    // collapsed into the generic `Failed` bucket, making the documented
    // `ScriptOutcomeKind::Timeout` unreachable. The kill branch now records a
    // typed flag; `classify` surfaces it as `Timeout`. Generous margins keep
    // the test non-flaky (50 ms deadline vs a 30 s child).

    #[test]
    #[cfg(unix)]
    fn run_child_exceeding_wall_clock_yields_timeout_outcome() {
        let scratch = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        let source = r#"ocx.run("sh", "-c", "sleep 30")"#;
        let limits = super::super::ScriptLimits {
            max_callstack_size: 50,
            wall_clock: std::time::Duration::from_millis(50),
        };

        // run_script is sync + uses Handle::current().block_on inside
        // block_in_place → requires a multi-thread runtime (its documented
        // precondition).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let outcome = rt
            .block_on(async {
                tokio::task::block_in_place(|| {
                    super::super::run_script(
                        source,
                        "<test>",
                        package.path(),
                        scratch.path(),
                        &crate::oci::Platform::any(),
                        crate::env::Env::clean(),
                        limits,
                    )
                })
            })
            .expect("host setup must not fail");

        assert!(
            matches!(outcome.kind, ScriptOutcomeKind::Timeout),
            "a child exceeding the per-call wall-clock must yield Timeout, got a different outcome kind"
        );
    }
}
