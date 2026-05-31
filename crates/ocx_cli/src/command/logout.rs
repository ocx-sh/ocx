// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::auth::login::logout;
use ocx_lib::auth::store::{DockerCredentialStore, StoreOptions};

use crate::api::data::login::LogoutResult;

/// Remove credentials for a registry from the docker-compatible store.
///
/// No-op when no credential is stored — exits `Success(0)` to match
/// `docker`/`oras`/`helm`/`crane` convention (CI cleanup must not fail).
#[derive(Parser, Clone, Debug)]
pub struct Logout {
    /// Registry hostname (e.g. `ghcr.io`).
    /// Optional - falls back to `OCX_DEFAULT_REGISTRY`.
    registry: Option<String>,
}

// `--format` inherited from `Cli` root (see Login note).

impl Logout {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let registry = self
            .registry
            .clone()
            .unwrap_or_else(|| context.default_registry().to_string());

        let ui = context.ui();
        ui.status("Logging out", &registry);

        // No store can be constructed (no HOME, no $DOCKER_CONFIG) -> there is
        // nothing to log out from. True noop. Exit 0 to match `docker logout` /
        // `oras logout` / `helm registry logout` for absent-state.
        let store = match DockerCredentialStore::new(StoreOptions {
            allow_plaintext_put: false,
            detect_default_native_store: false,
        }) {
            Ok(s) => s,
            Err(err) => {
                tracing::debug!(%err, "logout: no credential store to act on, treating as noop");
                context.api().report(&LogoutResult { registry })?;
                return Ok(ExitCode::SUCCESS);
            }
        };

        // Surface real failures. `auth::logout` returns Ok(()) for the
        // "not-logged-in" case (delegated to the store's `delete`, which
        // mirrors oras-go semantics). Any Err here is a genuine helper /
        // I/O failure that means revocation did NOT complete.
        logout(&registry, &store).await?;

        ui.success(format!("Logged out of {registry}"));
        context.api().report(&LogoutResult { registry })?;
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_clap_registry_is_optional() {
        // Falls back to OCX_DEFAULT_REGISTRY at runtime; clap parse succeeds.
        Logout::try_parse_from(["logout"]).expect("registry must be optional at parse");
    }

    #[test]
    fn logout_clap_accepts_registry() {
        Logout::try_parse_from(["logout", "ghcr.io"]).expect("positional registry must parse");
    }
}
