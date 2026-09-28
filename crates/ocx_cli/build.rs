// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Bakes git, build and CI provenance into the binary via `cargo:rustc-env`; each
//! variable is optional, so a tarball build without `.git/` still compiles.
//!
//! Under `__testing` the binary depends on source and toolchain only, so its bytes
//! never churn per commit or CI run (`adr_test_speed_tiers.md` § C-PROV).

use std::env;

use vergen_gix::{BuildBuilder, CargoBuilder, Emitter, GixBuilder, RustcBuilder};

/// Provenance a `__testing` build bakes instead of git and CI state; every value stays
/// detectable, or the release provenance check cannot refuse a test build posing as a release.
/// `crates/ocx_cli/BUILD.bazel`'s `_TESTING_PROVENANCE` mirrors these rows, so edit both.
const TESTING_PLACEHOLDERS: &[(&str, &str)] = &[
    ("VERGEN_GIT_SHA", "0000000000000000000000000000000000000000"),
    ("VERGEN_GIT_DESCRIBE", "placeholder-g00000000"),
    ("VERGEN_GIT_DIRTY", "true"),
    ("VERGEN_GIT_COMMIT_TIMESTAMP", "1970-01-01T00:00:00.000000000Z"),
    ("VERGEN_BUILD_TIMESTAMP", "1970-01-01T00:00:00.000000000Z"),
    ("GITHUB_SERVER_URL", "https://ci.invalid"),
    ("GITHUB_REPOSITORY", "placeholder/placeholder"),
    ("GITHUB_RUN_ID", "0"),
    ("GITHUB_WORKFLOW", "placeholder"),
    ("GITHUB_REF", "refs/heads/placeholder"),
    ("GITHUB_SHA", "0000000000000000000000000000000000000000"),
    ("__OCX_BUILD_CHANNEL", "test"),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `CARGO_FEATURE_` + `__TESTING`: the triple underscore is correct.
    if env::var_os("CARGO_FEATURE___TESTING").is_some() {
        return emit_testing_provenance();
    }

    // `build_timestamp` only in CI: it changes every run and would force a relink per local build.
    let in_ci = std::env::var_os("CI").is_some();
    println!("cargo:rerun-if-env-changed=CI");
    let build = BuildBuilder::default().build_timestamp(in_ci).build()?;
    let cargo = CargoBuilder::default().target_triple(true).debug(true).build()?;
    let rustc = RustcBuilder::default().semver(true).build()?;

    let mut emitter = Emitter::default();
    emitter
        .add_instructions(&build)?
        .add_instructions(&cargo)?
        .add_instructions(&rustc)?;

    // Non-fatal: a tarball build without `.git/` warns and continues.
    match GixBuilder::default()
        .sha(false) // long SHA — short variant derived in build_info.rs
        .describe(true, true, None) // dirty marker + tags
        // Tracked-only: the release job writes an untracked `dist-manifest.json` first, which
        // would mark every published binary dirty.
        .dirty(false)
        .commit_timestamp(true)
        .build()
    {
        Ok(gix) => {
            emitter.add_instructions(&gix)?;
        }
        Err(error) => {
            println!("cargo:warning=vergen-gix metadata unavailable (no .git/?): {error}");
        }
    }

    emitter.emit()?;

    // Set by dev-deploy CI so the binary reports the tag it was published as.
    pass_through_env("__OCX_BUILD_VERSION");
    pass_through_env("__OCX_BUILD_CHANNEL");

    // GitHub Actions context for `ci.run_url`.
    for var in [
        "GITHUB_SERVER_URL",
        "GITHUB_REPOSITORY",
        "GITHUB_RUN_ID",
        "GITHUB_WORKFLOW",
        "GITHUB_REF",
        "GITHUB_SHA",
    ] {
        pass_through_env(var);
    }

    Ok(())
}

/// Toolchain metadata, the placeholder table and the `__OCX_BUILD_VERSION` pass-through (it feeds
/// `app::version()`, so never a placeholder); a `GixBuilder` or `CI`/`GITHUB_*` read here would
/// bake checkout or CI state and re-run this script per commit.
fn emit_testing_provenance() -> Result<(), Box<dyn std::error::Error>> {
    let cargo = CargoBuilder::default().target_triple(true).debug(true).build()?;
    let rustc = RustcBuilder::default().semver(true).build()?;
    Emitter::default()
        .add_instructions(&cargo)?
        .add_instructions(&rustc)?
        .emit()?;

    for (name, value) in TESTING_PLACEHOLDERS {
        println!("cargo:rustc-env={name}={value}");
    }
    pass_through_env("__OCX_BUILD_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}

/// Forwards a build-time env var to the binary's `option_env!()`; absent stays `None`.
fn pass_through_env(name: &str) {
    println!("cargo:rerun-if-env-changed={name}");
    if let Ok(value) = env::var(name) {
        // A newline would inject a further `cargo:` instruction.
        if value.contains('\n') || value.contains('\r') {
            println!("cargo:warning=skipping {name}: value contains newline");
            return;
        }
        println!("cargo:rustc-env={name}={value}");
    }
}
