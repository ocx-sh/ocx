// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Build script for `ocx_cli`.
//!
//! Bakes git + build + CI provenance into the binary via
//! `cargo:rustc-env` instructions. The CLI side reads them through
//! `option_env!()` from `crate::app::build_info` so a `cargo build` from
//! a tarball without a `.git/` directory produces a binary that still
//! compiles — every emitted variable is optional at consumer-side.
//!
//! Three groups of env vars are exposed to the binary:
//!
//! 1. `VERGEN_*` — git, build, rustc, cargo metadata (via `vergen-gix`).
//! 2. `__OCX_BUILD_VERSION` / `__OCX_BUILD_CHANNEL` — implementation-detail
//!    pass-throughs (the double-underscore prefix marks them internal: not
//!    a public CLI/contract surface, just the seam dev-deploy uses to
//!    override the embedded version string).
//! 3. `GITHUB_*` — GitHub Actions context (server URL, repository, run id,
//!    workflow, ref, sha). Used to derive the `ci.run_url` JSON field.
//!
//! All groups are best-effort: if the source env var is absent at build
//! time, the corresponding compile-time `option_env!()` resolves to `None`
//! and the binary omits the field from `ocx version --format json`.

use std::env;

use vergen_gix::{BuildBuilder, CargoBuilder, Emitter, GixBuilder, RustcBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── 1. Vergen-gix git + build + rustc + cargo metadata ──────────────
    //
    // Failure here is non-fatal: a tarball checkout without `.git/` will
    // fail the gix step. We log and proceed so the build still succeeds.
    // build_timestamp(true) emits the current UTC time every invocation,
    // changing VERGEN_BUILD_TIMESTAMP and forcing a relink of ocx_cli on
    // every `cargo build`. Only enable under CI where the artifact is
    // published and needs an authentic bake-time. Local builds opt out so
    // incremental builds stay incremental.
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

    // Gix metadata is the only step that can fail on a tarball build.
    // Build it separately so we can degrade gracefully.
    match GixBuilder::default()
        .sha(false) // long SHA — short variant derived in build_info.rs
        .describe(true, true, None) // dirty marker + tags
        .dirty(true)
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

    // ── 2. OCX build-time overrides (implementation detail) ─────────────
    //
    // Double-underscore prefix marks these as internal seams, not a
    // documented CLI contract surface. dev-deploy CI sets them so the
    // binary self-reports the artifact tag it was published as.
    pass_through_env("__OCX_BUILD_VERSION");
    pass_through_env("__OCX_BUILD_CHANNEL");

    // ── 3. GitHub Actions context — used for `ci.run_url` in JSON ───────
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

/// Pass a build-time env var through to the binary as a compile-time
/// constant readable by `option_env!()`. If the source var is absent, the
/// consumer's `option_env!()` resolves to `None` and the field is omitted
/// from JSON output.
fn pass_through_env(name: &str) {
    println!("cargo:rerun-if-env-changed={name}");
    if let Ok(value) = env::var(name) {
        // Guard against cargo:rustc-env instruction injection via
        // newline in the env value. Today's callers (dev-deploy CI +
        // GitHub Actions runner) cannot produce one, but the guard
        // keeps `pass_through_env` safe to extend.
        if value.contains('\n') || value.contains('\r') {
            println!("cargo:warning=skipping {name}: value contains newline");
            return;
        }
        println!("cargo:rustc-env={name}={value}");
    }
}
