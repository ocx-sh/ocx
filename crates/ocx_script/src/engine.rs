// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Starlark evaluation driver + terminal-error classification.

use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::{Error as StarlarkError, ErrorKind};

use super::host::{self, HostState};
use super::{ScriptError, ScriptLimits, ScriptLocation, ScriptOutcome, ScriptOutcomeKind};

/// Stdlib extensions enabled for OCX test scripts; each must stay free of host network, time and randomness.
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

fn build_globals() -> Globals {
    GlobalsBuilder::extended_by(SCRIPT_EXTENSIONS)
        .with(super::ocx_module::ocx_module)
        .with(super::expect_module::expect_module)
        .build()
}

/// Standard Starlark with `load()` disabled: scripts are single-file.
fn dialect() -> Dialect {
    Dialect {
        enable_load: false,
        ..Dialect::Standard
    }
}

/// Parses and evaluates `source` with `state` installed; script failures are outcomes, never `Err`.
pub(super) fn evaluate(
    source: &str,
    source_label: &str,
    limits: &ScriptLimits,
    state: HostState,
) -> Result<ScriptOutcome, ScriptError> {
    let dialect = dialect();
    let ast = match AstModule::parse(source_label, source.to_owned(), &dialect) {
        Ok(ast) => ast,
        Err(e) => return Ok(ScriptOutcome { kind: classify(&e) }),
    };

    // Before `build_globals`, or `ocx.target_platform` freezes as `Platform::Any` on a `-p` run.
    let _scope = host::scoped(state);

    let globals = build_globals();
    let module = Module::new();

    let outcome = {
        let mut eval = Evaluator::new(&module);
        eval.set_max_callstack_size(limits.max_callstack_size)
            .map_err(|e| ScriptError::HostSetup(e.to_string()))?;

        match eval.eval_module(ast, &globals) {
            Ok(_) => ScriptOutcome {
                kind: ScriptOutcomeKind::Passed,
            },
            Err(e) => ScriptOutcome { kind: classify(&e) },
        }
    };

    // Before the scope drops, or the report loses the last `ocx.run` result.
    host::stash_last_run(host::with(|s| s.last_run.clone()));
    drop(_scope);

    Ok(outcome)
}

/// Maps a terminal Starlark error to the engine-neutral outcome kind; the only place naming `ErrorKind` variants.
fn classify(error: &StarlarkError) -> ScriptOutcomeKind {
    use super::AssertionKind;
    let message = error.to_string();
    let location = source_location(error);
    // First, or a deadline kill classifies as the host `fail()` error it raised.
    if host::timed_out() {
        return ScriptOutcomeKind::Timeout;
    }
    let recorded = host::last_assertion();
    match error.kind() {
        ErrorKind::Fail(_) => ScriptOutcomeKind::Failed {
            kind: Some(recorded.unwrap_or(AssertionKind::Fail)),
            message,
            location,
        },
        ErrorKind::StackOverflow(_) => ScriptOutcomeKind::Failed {
            kind: None,
            message,
            location,
        },
        // Syntax, arity and type errors: exit 65, not 1.
        ErrorKind::Parser(_) | ErrorKind::Function(_) | ErrorKind::Value(_) | ErrorKind::Scope(_) => {
            ScriptOutcomeKind::ScriptError { message, location }
        }
        ErrorKind::Native(_) => ScriptOutcomeKind::Failed {
            kind: Some(recorded.unwrap_or(AssertionKind::Other)),
            message,
            location,
        },
        ErrorKind::Internal(_) | ErrorKind::Freeze(_) | ErrorKind::Other(_) => ScriptOutcomeKind::Failed {
            kind: None,
            message,
            location,
        },
        // A future upstream variant maps to `Failed`, never to a new exit code.
        _ => ScriptOutcomeKind::Failed {
            kind: None,
            message,
            location,
        },
    }
}

/// Lifts the terminal error's span into a 1-indexed [`ScriptLocation`]; starlark resolves 0-indexed.
fn source_location(error: &StarlarkError) -> Option<ScriptLocation> {
    let resolved = error.span()?.resolve();
    Some(ScriptLocation {
        file: resolved.file,
        line: resolved.span.begin.line + 1,
        column: resolved.span.begin.column + 1,
    })
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
                        &ocx_oci::Platform::any(),
                        ocx_config::env::Env::clean(),
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

    // ── source location: captured from the span, never parsed from prose ─────
    //
    // The location is what makes a JUnit `<testcase file= line=>` (and hence a
    // GitLab MR annotation) point at the failing line. It is read from
    // `Error::span()`; asserting the numbers here is what stops a regression
    // to "scrape `file:line` out of `Display`".

    /// Runs `source` under `label` through the real engine.
    fn outcome_of(source: &str, label: &str) -> ScriptOutcomeKind {
        let scratch = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        let limits = super::super::ScriptLimits {
            max_callstack_size: 50,
            wall_clock: std::time::Duration::from_secs(30),
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::task::block_in_place(|| {
                super::super::run_script(
                    source,
                    label,
                    package.path(),
                    scratch.path(),
                    &ocx_oci::Platform::any(),
                    ocx_config::env::Env::clean(),
                    limits,
                )
            })
        })
        .expect("host setup must not fail")
        .kind
    }

    #[test]
    fn failure_carries_the_1_indexed_line_of_the_failing_statement() {
        // `fail()` sits on the third line; starlark resolves spans 0-indexed,
        // so the contract is "3", not "2".
        let kind = outcome_of("x = 1\ny = 2\nfail(\"boom\")\n", "tests/smoke.star");
        let ScriptOutcomeKind::Failed { location, .. } = kind else {
            panic!("fail() must classify as Failed");
        };
        let location = location.expect("a `fail()` carries a source span");
        assert_eq!(location.file, "tests/smoke.star", "file is the source label verbatim");
        assert_eq!(location.line, 3, "line is 1-indexed");
        assert_eq!(location.column, 1, "column is 1-indexed");
    }

    #[test]
    fn script_error_carries_the_location_of_the_bad_syntax() {
        let kind = outcome_of("x = 1\ny = = 2\nz = 3\n", "<stdin>");
        let ScriptOutcomeKind::ScriptError { location, .. } = kind else {
            panic!("a syntax error must classify as ScriptError");
        };
        let location = location.expect("a parse error carries a source span");
        assert_eq!(location.file, "<stdin>");
        assert_eq!(location.line, 2, "the stray `=` is on line 2");
    }

    #[test]
    fn a_span_less_error_reports_no_location() {
        // An error synthesized without a diagnostic has no span; inventing a
        // location for it would put a JUnit reporter on a line that never ran.
        let e = err(ErrorKind::Fail(anyhow::anyhow!("boom")));
        let ScriptOutcomeKind::Failed { location, .. } = classify(&e) else {
            panic!("Fail must classify as Failed");
        };
        assert_eq!(location, None);
    }
}
