// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `$OCX_HOME` — the one resolution of the data root every store is rooted at.
//!
//! It sits in the config tier and not beside the stores it locates because it
//! is the resolution of an **environment variable with a documented fallback**,
//! which is configuration, not layout: `file_structure` joins subdirectories
//! onto the root it is handed, and the loader's own `home_*_path()` accessors
//! need the same root before any store exists. Owning it here is what lets the
//! loader stop reaching into `ocx_store::file_structure` for it.

use std::path::PathBuf;

/// Returns the OCX data root directory.
///
/// Resolution order:
/// 1. `OCX_HOME` environment variable (if set and non-empty)
/// 2. `~/.ocx` (fallback, via [`ocx_util::env::home_dir`])
///
/// **The one definition of that default.** `$OCX_HOME` has to name a single
/// directory for a whole invocation, so every caller resolves it here —
/// including the config loader's `home_*_path()` accessors, which used to
/// re-derive it and could land somewhere else.
///
/// Read through [`ocx_util::env::var`], the project-wide shim, so a test injects
/// `OCX_HOME` the same way it does for every other variable.
pub fn default_ocx_root() -> Option<PathBuf> {
    if let Some(home) = ocx_util::env::var("OCX_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    ocx_util::env::home_dir().map(|home| home.join(".ocx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-002 (#381): the `$OCX_HOME` default has one definition, and the
    /// config loader's accessors resolve through it.
    ///
    /// The two used to be separate resolvers reading the fallback home through
    /// different APIs (`std::env::home_dir` here, `dirs::home_dir` there),
    /// which can name different directories — so this asserts the *paths*
    /// agree, not that both functions merely return `Some`.
    #[test]
    fn the_ocx_home_default_has_one_definition() {
        let env = ocx_util::env::overrides::lock();
        let home = env.isolate_project_home();

        assert_eq!(
            default_ocx_root().as_deref(),
            Some(home.path()),
            "OCX_HOME must be read through the shared shim, not std::env::var"
        );
        assert_eq!(
            crate::loader::ConfigLoader::home_path(),
            Some(home.path().join("config.toml")),
            "the loader's home tier must sit under the same root"
        );
        assert_eq!(
            crate::loader::ConfigLoader::home_sigstore_trusted_root_path(),
            Some(home.path().join("sigstore").join("trusted-root.json")),
            "the trust-root convention path must sit under the same root"
        );

        // An empty value is not a root. Falling back keeps `$OCX_HOME=""` from
        // resolving every store to the process working directory.
        env.set("OCX_HOME", "");
        let fallback = ocx_util::env::home_dir().map(|h| h.join(".ocx"));
        assert_eq!(default_ocx_root(), fallback, "an empty OCX_HOME must fall back");
        assert_eq!(
            crate::loader::ConfigLoader::home_path(),
            fallback.map(|d| d.join("config.toml")),
            "the two must still agree on the fallback"
        );
    }
}
