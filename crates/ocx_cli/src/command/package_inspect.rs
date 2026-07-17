// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::{conventions, options};

/// Inspect what sits at one or more package references. Read-only — nothing
/// is installed and no symlinks are created. Accepts a tag or an `@digest`.
///
/// With multiple packages, JSON output is an object keyed by the requested
/// identifier (`{"<id>": {...}}`); plain output renders each package's tree in
/// order.
///
/// Output adapts to each reference's shape:
///
/// - Default, reference is an image index: list the platform **candidates**
///   (platform, child digest, media type, size). No metadata is loaded and
///   no platform is selected.
/// - Default, reference is a single image manifest (flat tag or `@digest`):
///   emit the declared **metadata** (bundle version, strip_components, env
///   vars, dependencies, entrypoints) plus the manifest's **layers**. No
///   resolution chain.
/// - `--resolve`: platform-select through the index (honoring
///   `-p/--platform`), then emit metadata and layers plus the **resolution**
///   chain — the pinned identifier and the walk-order chain blob digests
///   (index, manifest, config).
///
/// `-p/--platform` applies only with `--resolve`. Honors the global
/// `--offline` / `--remote` / `--format` flags. JSON is the primary consumer
/// surface (OCX is a backend tool).
#[derive(Parser)]
pub struct PackageInspect {
    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Platform-select through the index and emit the OCI resolution chain
    /// (pinned identifier and walk-order chain digests: index, manifest,
    /// config) alongside the metadata and layers. Without this, an image-index
    /// reference lists its platform candidates instead.
    #[clap(long)]
    resolve: bool,

    /// Package identifiers to inspect (each a tag or `@digest`).
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl PackageInspect {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        use crate::api::data::package_inspect::{PackageInspect, PackageInspects};

        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        options::Identifier::reject_duplicate_references(&identifiers)?;
        let platform = conventions::platform_or_default(self.platform.platform.clone());

        // `inspect_all` preserves input order, so zipping the results back with
        // `self.packages` (the raw request strings) is sound.
        let results = context
            .manager()
            .inspect_all(identifiers.clone(), platform.clone(), self.resolve)
            .await?;

        let entries: Vec<(String, PackageInspect)> = self
            .packages
            .iter()
            .zip(identifiers)
            .zip(results)
            .map(|((raw, identifier), result)| {
                (
                    raw.raw().to_string(),
                    PackageInspect::new(identifier, platform.clone(), result),
                )
            })
            .collect();

        context.api().report(&PackageInspects::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_multiple_positionals() {
        let cmd = PackageInspect::try_parse_from(["inspect", "a", "b", "c"]).unwrap();
        assert_eq!(cmd.packages.len(), 3);
    }

    #[test]
    fn single_positional_still_parses() {
        let cmd = PackageInspect::try_parse_from(["inspect", "a"]).unwrap();
        assert_eq!(cmd.packages.len(), 1);
    }

    #[test]
    fn zero_positionals_is_rejected() {
        assert!(PackageInspect::try_parse_from(["inspect"]).is_err());
    }
}
