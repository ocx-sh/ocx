// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Structural parity gate: every Rust `VARIANTS` entry for
//! [`OperatingSystem`] / [`Architecture`] must be present as a constant in the
//! matching `ocx.{os,arch}.*` Starlark namespace, and `str()` of that constant
//! must equal the Rust enum's `Display`.
//!
//! The gate breaks the build when a new variant lands Rust-side without a
//! matching Starlark constant — locking out the silent-drift class of bug
//! that motivated codifying the style rule.

#![cfg(test)]

use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

use crate::oci::platform::{Architecture, OperatingSystem};

/// Builds the script globals with ONLY the `ocx` module (no expect, no host
/// state) — enough to assert the typed namespaces and their constants.
fn parity_globals() -> Globals {
    GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(super::ocx_module::ocx_module)
        .build()
}

/// Wraps `body` in a single `def _check(): ... _check()` so module-level
/// statements like `if` are legal (standard Starlark forbids `if` at module
/// top level).
fn eval_check(body: &str) -> Result<(), String> {
    let indented: String = body
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!("def _check():\n{indented}\n_check()\n");
    let ast = AstModule::parse(
        "<variants_parity>",
        source,
        &Dialect {
            enable_load: false,
            ..Dialect::Standard
        },
    )
    .map_err(|e| format!("parse failed: {e}"))?;
    let globals = parity_globals();
    let module = Module::new();
    let mut eval = Evaluator::new(&module);
    eval.eval_module(ast, &globals)
        .map_err(|e| format!("eval failed: {e}"))?;
    Ok(())
}

#[test]
fn every_operating_system_variant_is_in_ocx_os_namespace() {
    for variant in OperatingSystem::VARIANTS {
        // Variant attribute name = PascalCase Rust variant; str() = OCI
        // lowercase Display.
        let pascal = format!("{variant:?}");
        let expected_display = variant.to_string();
        let body = format!(
            r#"v = ocx.os.{pascal}
if str(v) != "{expected_display}":
    fail("ocx.os.{pascal} display mismatch: got " + str(v) + ", expected {expected_display}")"#
        );
        eval_check(&body).unwrap_or_else(|e| panic!("OperatingSystem::{variant:?} parity gate failed: {e}"));
    }
}

#[test]
fn every_architecture_variant_is_in_ocx_arch_namespace() {
    for variant in Architecture::VARIANTS {
        let pascal = format!("{variant:?}");
        let expected_display = variant.to_string();
        let body = format!(
            r#"v = ocx.arch.{pascal}
if str(v) != "{expected_display}":
    fail("ocx.arch.{pascal} display mismatch: got " + str(v) + ", expected {expected_display}")"#
        );
        eval_check(&body).unwrap_or_else(|e| panic!("Architecture::{variant:?} parity gate failed: {e}"));
    }
}

#[test]
fn os_constants_equal_themselves() {
    eval_check(
        r#"if ocx.os.Linux != ocx.os.Linux:
    fail("ocx.os.Linux must equal itself")
if ocx.os.Linux == ocx.os.Darwin:
    fail("distinct OS variants must not compare equal")"#,
    )
    .unwrap();
}

#[test]
fn arch_constants_equal_themselves() {
    eval_check(
        r#"if ocx.arch.Amd64 != ocx.arch.Amd64:
    fail("ocx.arch.Amd64 must equal itself")
if ocx.arch.Amd64 == ocx.arch.Arm64:
    fail("distinct arch variants must not compare equal")"#,
    )
    .unwrap();
}

#[test]
fn cross_type_wall_os_vs_arch() {
    // The cross-type wall codified in subsystem-script.md: comparing an OS
    // constant against an arch constant or a plain string is `false`, never
    // an error.
    eval_check(
        r#"if ocx.os.Linux == ocx.arch.Amd64:
    fail("ocx.os.Linux must not equal ocx.arch.Amd64")
if ocx.os.Linux == "linux":
    fail("ocx.os.Linux must not equal the string 'linux'")"#,
    )
    .unwrap();
}
