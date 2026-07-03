// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch publish` — push a patch descriptor to the registry.
//!
//! Reads a descriptor JSON file, validates it, and pushes it to the configured
//! patch registry under either the reserved global repository (`--global`) or
//! the package-specific sub-path for a given base identifier. The descriptor
//! only references companion packages by identifier — publish those separately
//! with `ocx package push`. Requires network access; fails in offline mode.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Args;
use ocx_lib::{
    oci,
    package_manager::tasks::patch_discovery::{global_descriptor_id, patch_descriptor_id},
};

use crate::options;

/// Arguments for `ocx patch publish`.
#[derive(Args)]
pub struct PatchPublishArgs {
    /// Path to the patch descriptor JSON file to publish.
    #[clap(long = "descriptor", required = true)]
    descriptor: PathBuf,

    /// Publish the descriptor as the global descriptor so it applies to every
    /// base. Stored at the reserved `global` repository in the patch registry.
    /// Mutually exclusive with a base identifier.
    #[clap(long = "global", conflicts_with = "base")]
    global: bool,

    /// Base identifier whose package-specific patch path receives the
    /// descriptor. Omit with `--global` for the global descriptor.
    #[clap(value_name = "BASE-ID", required_unless_present = "global")]
    base: Option<options::Identifier>,

    /// Patch registry to publish to, as `HOST/PATH`
    /// (e.g. registry.corp.example/ocx-patches).
    ///
    /// Overrides the configured `[patches]` tier for this command, so you can
    /// publish to (and thereby bootstrap) a new patch registry without first
    /// adding a `[patches]` config block. Defaults to the configured registry.
    #[clap(long = "registry", value_name = "HOST/PATH")]
    registry: Option<String>,
}

impl PatchPublishArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the effective patch tier (honours --registry). ──
        let patches = crate::command::patch_common::effective_patches(self.registry.as_deref(), &context)?;

        // ── Step 2: Read + validate the descriptor JSON file. ──
        let descriptor_bytes = tokio::fs::read(&self.descriptor)
            .await
            .with_context(|| format!("reading descriptor file {}", self.descriptor.display()))?;
        // Validate up front for a clear error before any network work.
        ocx_lib::patch::PatchDescriptor::from_json_bytes(&descriptor_bytes)
            .with_context(|| format!("validating patch descriptor file {}", self.descriptor.display()))?;

        // ── Step 3: Compute the target patch repo identifier. ──
        //
        // `base` is guaranteed present by `required_unless_present = "global"`
        // when `--global` is absent; resolve its default registry here so the
        // selection helper stays a pure function over already-resolved identifiers.
        let base_id = match &self.base {
            Some(base_raw) => Some(base_raw.with_domain(context.default_registry())?),
            None => None,
        };
        let patch_repo_id = select_publish_target(&patches, self.global, base_id.as_ref());

        // ── Step 4: Publish via the lib orchestration method. ──
        let report = context
            .manager()
            .publish_patch_descriptor(&patch_repo_id, &descriptor_bytes)
            .await
            .map_err(anyhow::Error::new)?;

        // ── Step 5: Report. ──
        context
            .api()
            .report(&crate::api::data::patch_publish::PatchPublishReport::new(report))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Select the patch repo identifier `ocx patch publish` targets.
///
/// `--global` targets the reserved global descriptor repository (applies to
/// every base); otherwise the descriptor lands at the package-specific sub-path
/// for `base_id`. `base_id` is `Some` whenever `global` is false (clap's
/// `required_unless_present` guarantees it); when both are absent the global
/// descriptor is used as a safe fallback so the function stays total.
fn select_publish_target(
    patches: &ocx_lib::ResolvedPatchConfig,
    global: bool,
    base_id: Option<&oci::Identifier>,
) -> oci::Identifier {
    match (global, base_id) {
        (false, Some(base)) => patch_descriptor_id(patches, base),
        // `--global`, or (defensively) no base supplied → the global descriptor.
        _ => global_descriptor_id(patches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::ResolvedPatchConfig;

    fn patches() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        }
    }

    /// `--global` selects the reserved `global` repository descriptor.
    #[test]
    fn select_publish_target_global_is_reserved_global_repository() {
        let patches = patches();
        let target = select_publish_target(&patches, true, None);
        // Global descriptor: rooted at the patch registry, reserved single-segment
        // `global` repository, PATCH tag.
        assert_eq!(target.registry(), "patches.corp.com");
        assert_eq!(
            target.repository(),
            "global",
            "global target must use the reserved `global` repository; got '{}'",
            target.repository()
        );
        assert_eq!(
            target,
            global_descriptor_id(&patches),
            "global selection must equal global_descriptor_id"
        );
    }

    /// Without `--global`, a base identifier selects its package-specific
    /// sub-path target — NOT the reserved global repository.
    #[test]
    fn select_publish_target_base_is_package_specific_sub_path() {
        let patches = patches();
        let base = oci::Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let target = select_publish_target(&patches, false, Some(&base));
        assert_eq!(target.registry(), "patches.corp.com");
        assert!(
            target.repository().contains("cmake"),
            "package-specific target must embed the base repository 'cmake'; got '{}'",
            target.repository()
        );
        assert_eq!(
            target,
            patch_descriptor_id(&patches, &base),
            "per-base selection must equal patch_descriptor_id"
        );
        assert_ne!(
            target.repository(),
            global_descriptor_id(&patches).repository(),
            "package-specific target must differ from the global descriptor"
        );
    }

    // --- Clap surface: --descriptor rename (C7), dead --platform removal (C8) ---

    /// `--descriptor` parses and is threaded through to the args struct.
    #[test]
    fn descriptor_flag_parses() {
        use clap::Parser as _;

        let cli = crate::app::Cli::try_parse_from(["ocx", "patch", "publish", "--descriptor", "p.json", "--global"])
            .expect("--descriptor must parse");
        let Some(crate::command::Command::Patch(crate::command::patch::PatchGroup::Publish(args))) = cli.command else {
            panic!("expected Patch(Publish(..)) subcommand");
        };
        assert_eq!(args.descriptor, PathBuf::from("p.json"));
        assert!(args.global);
    }

    /// `--registry` overrides the target patch registry and threads through to
    /// the args struct — the ad-hoc bootstrap path.
    #[test]
    fn registry_flag_parses() {
        use clap::Parser as _;

        let cli = crate::app::Cli::try_parse_from([
            "ocx",
            "patch",
            "publish",
            "--descriptor",
            "p.json",
            "--global",
            "--registry",
            "registry.corp.example/ocx-patches",
        ])
        .expect("--registry must parse");
        let Some(crate::command::Command::Patch(crate::command::patch::PatchGroup::Publish(args))) = cli.command else {
            panic!("expected Patch(Publish(..)) subcommand");
        };
        assert_eq!(args.registry.as_deref(), Some("registry.corp.example/ocx-patches"));
    }

    /// `--descriptor-file` is the OLD flag name — it must be an unknown flag
    /// now that publish uses `--descriptor` (C7).
    #[test]
    fn descriptor_file_flag_is_rejected() {
        use clap::Parser as _;

        let result =
            crate::app::Cli::try_parse_from(["ocx", "patch", "publish", "--descriptor-file", "p.json", "--global"]);
        assert!(
            result.is_err(),
            "--descriptor-file must be rejected; the flag was renamed to --descriptor"
        );
    }

    /// `--platform` was dead on publish (never consumed by
    /// `select_publish_target`) and is removed entirely (C8).
    #[test]
    fn platform_flag_is_rejected() {
        use clap::Parser as _;

        let result = crate::app::Cli::try_parse_from([
            "ocx",
            "patch",
            "publish",
            "--descriptor",
            "p.json",
            "--platform",
            "linux/amd64",
            "--global",
        ]);
        assert!(
            result.is_err(),
            "publish --platform must be rejected; the dead field was removed"
        );
    }
}
