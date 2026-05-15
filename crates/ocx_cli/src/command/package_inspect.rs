// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::{conventions::platforms_or_default, options};

/// Inspect what sits at a package reference. Read-only — nothing is
/// installed and no symlinks are created. Accepts a tag or an `@digest`.
///
/// Output adapts to the reference shape:
///
/// - Default, reference is an image index: list the platform **candidates**
///   (platform, child digest, media type, size). No metadata is loaded and
///   no platform is selected.
/// - Default, reference is a single image manifest (flat tag or `@digest`):
///   emit the declared **metadata** (bundle version, strip_components, env
///   vars, dependencies, entrypoints). No resolution chain.
/// - `--resolve`: platform-select through the index (honoring
///   `-p/--platform`), then emit metadata plus the **resolution** chain —
///   the pinned identifier, the walk-order chain blob digests, and the
///   platform-selected manifest's layers.
///
/// `-p/--platform` applies only with `--resolve`. Honors the global
/// `--offline` / `--remote` / `--format` flags. JSON is the primary consumer
/// surface (OCX is a backend tool).
#[derive(Parser)]
pub struct PackageInspect {
    #[clap(flatten)]
    platforms: options::Platforms,

    /// Platform-select through the index and emit metadata plus the OCI
    /// resolution chain (pinned identifier, walk-order chain digests, and the
    /// platform-selected manifest's layers). Without this, an image-index
    /// reference lists its platform candidates instead.
    #[clap(long)]
    resolve: bool,

    /// The package identifier to inspect (tag or `@digest`).
    identifier: options::Identifier,
}

impl PackageInspect {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;
        let platforms = platforms_or_default(self.platforms.as_slice());

        let result = context
            .manager()
            .inspect(&identifier, platforms.clone(), self.resolve)
            .await?;

        let report = crate::api::data::package_inspect::PackageInspect::new(identifier, platforms, result);
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}
