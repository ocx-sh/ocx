// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `$OCX_HOME` — the one resolution of the data root every store is rooted at.

use std::path::PathBuf;

/// Returns the OCX data root directory.
///
/// `OCX_HOME` if set and non-empty, else the home a parent ocx recorded in the state it handed down,
/// else `~/.ocx`; every caller resolves it here so one invocation never sees two data roots.
pub fn default_ocx_root() -> Option<PathBuf> {
    if let Some(home) = ocx_env::OCX_HOME.get() {
        return Some(PathBuf::from(home));
    }
    // Header-borne, so it survives a payload this version cannot read.
    if let Some(home) = crate::exec_state::Inherited::from_process_env().home() {
        return Some(home.to_path_buf());
    }
    ocx_env::home_dir().map(|home| home.join(".ocx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unset `OCX_HOME` falls back to the home in the received state's header, and an explicit one wins.
    #[test]
    fn the_received_header_home_stands_in_for_an_unset_ocx_home() {
        let env = ocx_env::overrides::lock();
        let home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_HOME);
        let received = ocx_test_support::paths::abs("/received/home");
        let header = crate::exec_state::header::emit(
            crate::exec_state::MAJOR,
            &crate::exec_state::Narrowing::default(),
            Some(&received),
        );
        env.set(&ocx_env::__OCX_ENV_STATE, &header);
        assert_eq!(default_ocx_root().as_deref(), Some(received.as_path()));

        let explicit = ocx_test_support::paths::abs("/explicit/home");
        env.set(&ocx_env::OCX_HOME, explicit.to_str().expect("utf-8"));
        assert_eq!(default_ocx_root().as_deref(), Some(explicit.as_path()));
        drop(home);
    }

    /// The `$OCX_HOME` default has one definition, and the
    /// config loader's accessors resolve through it.
    ///
    /// The two used to be separate resolvers reading the fallback home through
    /// different APIs (`std::env::home_dir` here, `dirs::home_dir` there),
    /// which can name different directories — so this asserts the *paths*
    /// agree, not that both functions merely return `Some`.
    #[test]
    fn the_ocx_home_default_has_one_definition() {
        let env = ocx_env::overrides::lock();
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
        env.set(&ocx_env::OCX_HOME, "");
        let fallback = ocx_env::home_dir().map(|h| h.join(".ocx"));
        assert_eq!(default_ocx_root(), fallback, "an empty OCX_HOME must fall back");
        assert_eq!(
            crate::loader::ConfigLoader::home_path(),
            fallback.map(|d| d.join("config.toml")),
            "the two must still agree on the fallback"
        );
    }
}
