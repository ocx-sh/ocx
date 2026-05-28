// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded version string accessor.
//!
//! [`version`] — the *effective* version. Dev-deploy CI sets
//! `__OCX_BUILD_VERSION` to the artifact tag (e.g.
//! `0.3.2-dev+20260528143045`) so the binary self-reports the published
//! identity instead of the stale `Cargo.toml` value. Local `cargo build`
//! leaves the override unset and falls back to `CARGO_PKG_VERSION`. Used
//! everywhere downstream callers want "what version of ocx is this?":
//! `ocx version`, `ocx about`, lock-file `generated_by`, update-check.
//!
//! The raw `Cargo.toml` value is inlined at the single call site in
//! `command/version.rs` via `env!("CARGO_PKG_VERSION")` so it can be
//! compared against `version()` at construction time without a separate
//! accessor surfacing unrelated code paths.

/// Effective ocx version embedded in the binary.
///
/// Resolution order:
///
/// 1. `__OCX_BUILD_VERSION` — set at build time by dev-deploy CI to make
///    the binary's self-reported version match the OCI tag it was
///    published as. Implementation-detail seam (double-underscore prefix),
///    not a user-facing env var.
/// 2. `CARGO_PKG_VERSION` — the `Cargo.toml` value, used by every
///    non-dev-deploy build.
pub fn version() -> &'static str {
    option_env!("__OCX_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `version()` returns a non-empty string. In local builds (no
    /// `__OCX_BUILD_VERSION`) this is a bare `MAJOR.MINOR.PATCH` semver;
    /// in dev-deploy builds it carries pre-release + build metadata
    /// (e.g. `0.3.2-dev+20260528143045`).
    #[test]
    fn version_is_non_empty() {
        let value = version();
        assert!(!value.is_empty(), "effective version must not be empty");
    }
}
