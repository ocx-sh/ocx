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

use crate::conventions::platforms_or_all_supported;
use crate::options;

/// Arguments for `ocx patch sync`.
#[derive(Args)]
pub struct PatchSyncArgs {
    #[clap(flatten)]
    platforms: options::Platforms,
}

impl PatchSyncArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the platform set to sync. ──
        //
        // Unlike most commands (host-only default via `platforms_or_default`),
        // an empty `--platform` here expands to the FULL supported-platform
        // matrix. A synced descriptor/companion set is shareable across a team
        // like `ocx lock`: it pins multi-platform manifests, not a single
        // GC-prone per-arch index. A host-only default would silently miss
        // non-host companions, breaking an offline or required-patch launch on
        // a teammate's machine running a different platform.
        let platforms = platforms_or_all_supported(self.platforms.as_slice());

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty `--platform` must expand to the full supported matrix, not just
    /// the host platform — regression guard for a host-only default that
    /// would silently miss non-host companions (C6). The matrix ends with the
    /// platform-agnostic `any` fallback so an `any`-published companion still
    /// resolves.
    #[test]
    fn empty_platform_list_covers_full_supported_matrix() {
        let resolved = platforms_or_all_supported(&[]);
        assert_eq!(
            resolved.len(),
            6,
            "must cover all five concrete platforms plus the `any` fallback"
        );
        let displayed: Vec<String> = resolved.iter().map(ToString::to_string).collect();
        for expected in ["darwin/arm64", "windows/amd64"] {
            assert!(
                displayed.iter().any(|p| p == expected),
                "resolved platform set must include non-host platform '{expected}'; got {displayed:?}"
            );
        }
        assert!(
            resolved.last().is_some_and(ocx_lib::oci::Platform::is_any),
            "the matrix must end with the `any` fallback; got {displayed:?}"
        );
    }

    /// An explicit `--platform` list is passed through unchanged (narrows,
    /// never expands).
    #[test]
    fn explicit_platform_list_is_not_expanded() {
        let explicit = vec!["linux/amd64".parse::<ocx_lib::oci::Platform>().unwrap()];
        let resolved = platforms_or_all_supported(&explicit);
        assert_eq!(resolved, explicit);
    }
}
