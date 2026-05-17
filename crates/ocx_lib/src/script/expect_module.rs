// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `expect.*` host module exposed to test scripts.
//!
//! `#[starlark_module]`-defined and registered under the `expect` namespace.
//! NOTE (LDR-7): the namespace is `expect`, NOT `assert` — `assert` is a
//! hardcoded reserved keyword in the starlark 0.13.0 lexer (a script using
//! `assert.ok(...)` fails to *parse*), so the assertion DSL lives under
//! `expect`. An assertion failure is a host fn returning an error that
//! `engine` classifies as a host-fn failure → exit 1 (`Failed`). The builtin
//! Starlark `fail()` is exposed by the standard globals.
//!
//! - `expect.ok(result, msg=None)` — ERGONOMIC: asserts `result.exit_code ==
//!   0`; on failure auto-builds the message from `result.stderr`. `msg`
//!   overrides/prepends the auto message.
//! - `expect.eq(actual, expected, msg=None)` — fail if `actual != expected`.
//! - `expect.ne(actual, expected, msg=None)` — fail if `actual == expected`.
//! - `expect.true(cond, msg=None)` — fail if not truthy.
//! - `expect.false(cond, msg=None)` — fail if truthy.
//! - `expect.contains(haystack, needle, msg=None)` — substring / membership.
//! - `expect.matches(text, pattern, msg=None)` — regex via the `regex` crate;
//!   an invalid pattern is a `ScriptError` (exit 65), not an assertion failure.
//! - `expect.fail(msg)` — unconditional failure with `msg`.

use starlark::environment::GlobalsBuilder;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::none::NoneType;

use super::AssertionKind;
use super::sl_error::{fail, script_type};

/// Records the failing assertion's identity (plan C5 stable `kind`) and builds
/// the host-fn failure error. Single choke point so every `expect.*` failure
/// path attributes itself — `engine::classify` reads the recorded kind back.
fn assertion_fail(kind: AssertionKind, message: String) -> starlark::Error {
    super::host::note_assertion(kind);
    fail(message)
}

/// Optional `msg=` override → a leading prefix on the auto-built message.
fn prefix(msg: Value<'_>) -> Option<String> {
    msg.unpack_str().map(|s| s.to_string())
}

/// Builds the final assertion-failure message: caller `msg` (if any) joined
/// with the auto-built detail.
fn message(user: Option<String>, auto: String) -> String {
    match user {
        Some(m) => format!("{m}: {auto}"),
        None => auto,
    }
}

/// Members of the `expect` namespace.
#[starlark_module]
fn expect_members(globals: &mut GlobalsBuilder) {
    /// `expect.ok(result, msg=None)` — assert `result.exit_code == 0`.
    fn ok<'v>(
        #[starlark(require = pos)] result: Value<'v>,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
        eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        let exit = result
            .get_attr("exit_code", eval.heap())?
            .ok_or_else(|| script_type("expect.ok expects a RunResult (missing .exit_code)"))?;
        let code = exit
            .unpack_i32()
            .ok_or_else(|| script_type("expect.ok: result.exit_code is not an int"))?;
        if code == 0 {
            return Ok(NoneType);
        }
        let stderr = result
            .get_attr("stderr", eval.heap())?
            .and_then(|v| v.unpack_str().map(|s| s.to_string()))
            .unwrap_or_default();
        Err(assertion_fail(
            AssertionKind::Ok,
            message(
                prefix(msg),
                format!("expected exit_code 0, got {code}; stderr: {stderr}"),
            ),
        ))
    }

    /// `expect.eq(actual, expected, msg=None)`
    fn eq<'v>(
        #[starlark(require = pos)] actual: Value<'v>,
        #[starlark(require = pos)] expected: Value<'v>,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
    ) -> starlark::Result<NoneType> {
        if actual.equals(expected)? {
            return Ok(NoneType);
        }
        Err(assertion_fail(
            AssertionKind::Eq,
            message(
                prefix(msg),
                format!("expected {} == {}", actual.to_str(), expected.to_str()),
            ),
        ))
    }

    /// `expect.ne(actual, expected, msg=None)`
    fn ne<'v>(
        #[starlark(require = pos)] actual: Value<'v>,
        #[starlark(require = pos)] expected: Value<'v>,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
    ) -> starlark::Result<NoneType> {
        if !actual.equals(expected)? {
            return Ok(NoneType);
        }
        Err(assertion_fail(
            AssertionKind::Ne,
            message(
                prefix(msg),
                format!("expected {} != {}", actual.to_str(), expected.to_str()),
            ),
        ))
    }

    /// `expect.true(cond, msg=None)`
    fn r#true<'v>(
        #[starlark(require = pos)] cond: Value<'v>,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
    ) -> starlark::Result<NoneType> {
        if cond.to_bool() {
            return Ok(NoneType);
        }
        Err(assertion_fail(
            AssertionKind::True,
            message(prefix(msg), "expected a truthy value".to_string()),
        ))
    }

    /// `expect.false(cond, msg=None)`
    fn r#false<'v>(
        #[starlark(require = pos)] cond: Value<'v>,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
    ) -> starlark::Result<NoneType> {
        if !cond.to_bool() {
            return Ok(NoneType);
        }
        Err(assertion_fail(
            AssertionKind::False,
            message(prefix(msg), "expected a falsey value".to_string()),
        ))
    }

    /// `expect.contains(haystack, needle, msg=None)`
    fn contains<'v>(
        #[starlark(require = pos)] haystack: Value<'v>,
        #[starlark(require = pos)] needle: Value<'v>,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
        eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        let found = if let Some(h) = haystack.unpack_str() {
            let n = needle
                .unpack_str()
                .ok_or_else(|| script_type("expect.contains: needle must be a string when haystack is a string"))?;
            h.contains(n)
        } else {
            // List / iterable membership.
            let mut hit = false;
            for item in haystack.iterate(eval.heap())? {
                if item.equals(needle)? {
                    hit = true;
                    break;
                }
            }
            hit
        };
        if found {
            return Ok(NoneType);
        }
        Err(assertion_fail(
            AssertionKind::Contains,
            message(
                prefix(msg),
                format!("expected {} to contain {}", haystack.to_str(), needle.to_str()),
            ),
        ))
    }

    /// `expect.matches(text, pattern, msg=None)`
    fn matches<'v>(
        #[starlark(require = pos)] text: &str,
        #[starlark(require = pos)] pattern: &str,
        #[starlark(require = named, default = NoneType)] msg: Value<'v>,
    ) -> starlark::Result<NoneType> {
        // Invalid regex is a script-value error (exit 65), NOT an assertion.
        let re = regex::Regex::new(pattern)
            .map_err(|e| script_type(format!("expect.matches: invalid regex '{pattern}': {e}")))?;
        if re.is_match(text) {
            return Ok(NoneType);
        }
        Err(assertion_fail(
            AssertionKind::Matches,
            message(prefix(msg), format!("expected '{text}' to match /{pattern}/")),
        ))
    }

    /// `expect.fail(msg)` — unconditional failure.
    fn fail(#[starlark(require = pos)] msg: &str) -> starlark::Result<NoneType> {
        Err(assertion_fail(AssertionKind::Fail, msg.to_string()))
    }
}

/// Registers the `expect` namespace on the globals builder.
pub(super) fn expect_module(globals: &mut GlobalsBuilder) {
    globals.namespace("expect", expect_members);
}
