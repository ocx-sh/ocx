// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded version string accessor.

/// Effective ocx version: `__OCX_BUILD_VERSION` (set by dev-deploy CI to the published tag),
/// else `CARGO_PKG_VERSION`.
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
