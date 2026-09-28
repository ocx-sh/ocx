// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::{conventions, options};

/// Inspect what sits at one or more package references. Read-only - nothing
/// is installed and no symlinks are created. Accepts a tag or an `@digest`.
///
/// Output adapts to each reference's shape: an image index lists its platform
/// candidates, a single manifest emits its metadata and layers, `--resolve`
/// adds the resolution chain and `--closure` the dependency closure. Several
/// packages render as a JSON object keyed by identifier. A script testing
/// whether `--closure` ran checks the `closure` key, not `resolution`, which
/// follows the reference's shape. Exits 65 when `--closure` finds a conflict,
/// still reported in full.
#[derive(Parser)]
pub struct PackageInspect {
    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    env: options::EnvOverride,

    /// Platform-select through the index and emit the OCI resolution chain
    /// (pinned identifier and walk-order chain digests: index, manifest,
    /// config) alongside the metadata and layers. Without `--resolve` or
    /// `--closure`, an image-index reference lists its platform candidates
    /// instead.
    #[clap(long)]
    resolve: bool,

    /// Compute the metadata-only dependency closure without installing.
    #[arg(long_help = "\
        Compute the metadata-only dependency closure without installing.\n\n\
        Adds one `closure` object to the output holding `deps` (the transitive dependencies in \
        transitive-closure order, each with its effective visibility) and `surface` (the \
        `interface` and `private` projections - the binaries, entrypoints and env keys that would \
        land on PATH for a consumer versus internally, plus each side's declared integration \
        namespaces). Plain output adds a `closure` branch with a flat dependency list and the two \
        surface summaries.\n\n\
        For a multi-platform reference, `--closure` first selects a platform (honoring \
        `-p/--platform`, the host platform by default) to read the declared dependencies - the same \
        selection `--resolve` performs.")]
    #[clap(long)]
    closure: bool,

    /// Package identifiers to inspect (each a tag or `@digest`).
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl PackageInspect {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        use ocx_package_manager::InspectOptions;

        use crate::api::data::package_inspect::{InspectReport, PackageInspect};

        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        options::Identifier::reject_duplicate_references(&identifiers)?;
        let platform = conventions::platform_or_default(self.platform.platform.clone());

        // A relative `:path` anchors to the invocation directory, the only base a calling script can compute.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        let inspect_options = InspectOptions {
            resolve: self.resolve,
            closure: self.closure,
        };

        // The zip below relies on `inspect_all` preserving input order.
        let results = context
            .manager()
            .inspect_all(identifiers.clone(), platform.clone(), inspect_options)
            .await?;

        let packages: Vec<PackageInspect> = self
            .packages
            .iter()
            .zip(identifiers)
            .zip(results)
            .map(|((raw, identifier), result)| {
                PackageInspect::new(raw.raw().to_string(), identifier, platform.clone(), result)
            })
            .collect();

        // Default mode selects no platform, so reporting one would make `-p` observable where its help calls it inert.
        let selected_platform = (self.resolve || self.closure).then_some(&platform);
        let report = InspectReport::new(selected_platform, packages, conventions::env_entries(&env_overrides));
        context.api().report(&report)?;

        Ok(conventions::inspect_exit_code(&report))
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
