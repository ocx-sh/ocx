// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch sync` — refresh descriptors and companions.

use std::process::ExitCode;

use clap::Args;

use crate::options;

/// Arguments for `ocx patch sync`.
#[derive(Args)]
pub struct PatchSyncArgs {
    #[clap(flatten)]
    platform: options::PlatformOption,
}

impl PatchSyncArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_concrete_matrix(self.platform.platform.clone());

        let report = context
            .manager()
            .sync_patches(&platforms)
            .await
            .map_err(anyhow::Error::new)?;

        context
            .api()
            .report(&crate::api::data::patch_sync::PatchSyncReport::new(report))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Returns `[explicit]`, else the full concrete ship matrix, not the host: a synced set is team-shared like `ocx lock`.
///
/// This is the single-platform exception `adr_platform_model_unification.md` names.
fn platforms_or_concrete_matrix(explicit: Option<ocx_oci::Platform>) -> Vec<ocx_oci::Platform> {
    match explicit {
        Some(platform) => vec![platform],
        None => concrete_ship_platforms(),
    }
}

/// The five OS/architecture pairs OCX ships; keep in sync with `product-context.md` "Platform support".
fn concrete_ship_platforms() -> Vec<ocx_oci::Platform> {
    [
        "linux/amd64",
        "linux/arm64",
        "darwin/amd64",
        "darwin/arm64",
        "windows/amd64",
    ]
    .iter()
    .map(|platform| {
        platform
            .parse()
            .expect("literal ship-target platform strings are valid")
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An omitted `--platform` must expand to the full concrete ship matrix,
    /// not just the host platform — regression guard for a host-only default
    /// that would silently miss non-host companions.
    #[test]
    fn absent_platform_covers_full_concrete_matrix() {
        let resolved = platforms_or_concrete_matrix(None);
        assert_eq!(resolved.len(), 5, "must cover all five concrete platforms");
        let displayed: Vec<String> = resolved.iter().map(ToString::to_string).collect();
        for expected in ["darwin/arm64", "windows/amd64"] {
            assert!(
                displayed.iter().any(|p| p == expected),
                "resolved platform set must include non-host platform '{expected}'; got {displayed:?}"
            );
        }
    }

    /// An explicit `--platform` value narrows the fan-out to that single
    /// platform (no expansion).
    #[test]
    fn explicit_platform_narrows_to_single_value() {
        let explicit: ocx_oci::Platform = "linux/amd64".parse().unwrap();
        let resolved = platforms_or_concrete_matrix(Some(explicit.clone()));
        assert_eq!(resolved, vec![explicit]);
    }
}
