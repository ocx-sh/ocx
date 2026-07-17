// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch sync` — refresh descriptors and companions.
//!
//! Re-fetches every patch descriptor for all installed packages and the global
//! descriptor, then installs any newly-referenced companion packages. Requires
//! network access. Also picks up patches for packages installed before patch
//! configuration was added.

use std::process::ExitCode;

use clap::Args;
use ocx_lib::oci;

use crate::options;

/// Arguments for `ocx patch sync`.
#[derive(Args)]
pub struct PatchSyncArgs {
    #[clap(flatten)]
    platform: options::PlatformOption,
}

impl PatchSyncArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the platform(s) to sync. ──
        //
        // Unlike most commands (host-only default via
        // `conventions::platform_or_default`), an omitted `--platform` here
        // fans out over the FULL concrete ship matrix. A synced
        // descriptor/companion set is shareable across a team like `ocx
        // lock`: it pins a manifest per platform a team runs, not a single
        // host-scoped variant. This is the one sanctioned multi-platform
        // fan-out (D4 exception, `adr_platform_model_unification.md`) — an
        // explicit enumeration loop over concrete platforms, never a
        // selection tier list.
        let platforms = platforms_or_concrete_matrix(self.platform.platform.clone());

        // ── Step 2: Run the sync. ──
        let report = context
            .manager()
            .sync_patches(&platforms)
            .await
            .map_err(anyhow::Error::new)?;

        // ── Step 3: Report. ──
        context
            .api()
            .report(&crate::api::data::patch_sync::PatchSyncReport::new(report))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Returns `[explicit]` when `--platform` was given; otherwise the full
/// concrete ship-target matrix `ocx patch sync` fans out over by default.
///
/// This is the fan-out site the D4 exception in
/// `adr_platform_model_unification.md` names: the concrete enumeration
/// belongs to this single caller, not a general-purpose "supported set"
/// helper. `Any` is deliberately absent from the matrix: an
/// `any`-published companion satisfies every one of the concrete
/// requirements below by construction (D1's `Any`-offer rule), so a trailing
/// pseudo-`Any` requirement tier is redundant.
fn platforms_or_concrete_matrix(explicit: Option<oci::Platform>) -> Vec<oci::Platform> {
    match explicit {
        Some(platform) => vec![platform],
        None => concrete_ship_platforms(),
    }
}

/// The five concrete OS/architecture combinations OCX ships and tests, kept
/// in sync with `product-context.md` "Platform support".
fn concrete_ship_platforms() -> Vec<oci::Platform> {
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
    /// that would silently miss non-host companions (C6).
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
        let explicit: oci::Platform = "linux/amd64".parse().unwrap();
        let resolved = platforms_or_concrete_matrix(Some(explicit.clone()));
        assert_eq!(resolved, vec![explicit]);
    }
}
