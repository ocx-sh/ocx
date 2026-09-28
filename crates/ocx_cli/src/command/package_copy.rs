// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser;
use ocx_package::publisher::{CopyRequest, Publisher};

use crate::api::data::package_copy::{CopyReport, DescriptionOutcome};
use crate::error::UsageError;
use crate::options;

#[derive(Parser)]
pub struct PackageCopy {
    /// Rewrite only the registry host, keeping the repository path and tag.
    ///
    /// The promotion shape: `dev.example.com/team/tool:1.4.2` to
    /// `prod.example.com` lands at `prod.example.com/team/tool:1.4.2`.
    /// Mutually exclusive with `--identifier`.
    #[clap(long = "to", value_name = "REGISTRY", conflicts_with = "identifier")]
    to: Option<String>,

    /// Full target reference, when the repository path or tag changes too.
    ///
    /// Required when the source names a digest: a digest carries no tag for
    /// `--to` to preserve.
    #[clap(short = 'i', long = "identifier")]
    identifier: Option<options::Identifier>,

    /// Platform to copy. Repeatable.
    ///
    /// Against a tag this filters the source index; omit it to copy every
    /// platform the source offers. Against a digest it *declares* the platform,
    /// and exactly one is required - a leaf manifest carries no platform of its
    /// own, so there is nothing to read it from.
    #[clap(short = 'p', long = "platform")]
    platform: Vec<ocx_oci::Platform>,

    /// Recompute the rolling tags (`1.4`, `1`, `latest`) at the target.
    ///
    /// Computed against the target's own tag list, not the source's: whether
    /// `1.4` should move depends on what the target already publishes, and a
    /// staging registry ahead of production has a different answer.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    #[clap(flatten)]
    keep_tag: options::KeepTag,

    #[clap(flatten)]
    referrers: options::Referrers,

    /// Also copy the repository description (`__ocx.desc`): README and logo.
    ///
    /// Off by default, because a description is repository-level prose rather
    /// than part of the version being promoted, and environments legitimately
    /// carry different ones. `ocx package description push --from` copies it alone.
    #[clap(long = "description")]
    description: bool,

    /// Record an OCI annotation on the target's image index. Repeatable.
    ///
    /// Merged into whatever the index already carries; a repeated key keeps the
    /// last value. Leaf manifests are never touched - annotating one would
    /// change its digest, which is the one thing a copy must not do.
    #[clap(long = "annotation", value_name = "KEY=VALUE", value_parser = super::package_push::parse_annotation)]
    annotation: Vec<(String, String)>,

    /// Report what would be copied and write nothing.
    #[clap(long = "dry-run")]
    dry_run: bool,

    /// Package to copy: `registry/repository:tag` or `registry/repository@sha256:...`.
    source: options::Identifier,
}

impl PackageCopy {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let source = self.source.with_domain(context.default_registry())?;
        let target = self.resolve_target(&source, context.default_registry())?;

        // Decided before any request, or an invocation that cannot succeed first authenticates against the target.
        if source.digest().is_some() && source.tag().is_none() {
            if self.platform.len() != 1 {
                return Err(UsageError::new(format!(
                    "{source} names a manifest by digest, which carries no platform; \
                     pass exactly one --platform"
                ))
                .into());
            }
            if self.identifier.is_none() {
                return Err(UsageError::new(format!(
                    "{source} names a manifest by digest, which carries no tag; \
                     pass --identifier with the target reference"
                ))
                .into());
            }
        }
        if target.tag().is_none() {
            return Err(UsageError::new(format!("target {target} has no tag; pass --identifier with one")).into());
        }

        let annotations: BTreeMap<String, String> = self.annotation.iter().cloned().collect();
        // No pre-emptive `ensure_auth` on the target, or it is contacted before `Publisher::copy` raises
        // its exit-64 source-form refusals.
        let publisher = Publisher::new(context.remote_client()?.clone());

        // Under the OCX home, not `$TMPDIR`: a memory-backed `$TMPDIR` turns the spool's byte cap into a RAM bound.
        let scratch_root = context.file_structure().temp.root().to_path_buf();
        tokio::fs::create_dir_all(&scratch_root)
            .await
            .map_err(|e| ocx_util::error::FileError::new(&scratch_root, e))?;

        if self.dry_run {
            log::info!("planning a copy of {source} to {target}");
        } else {
            log::info!("copying {source} to {target}");
        }
        let outcome = publisher
            .copy(
                context.default_index(),
                CopyRequest {
                    source: &source,
                    target: &target,
                    platforms: self.platform.clone(),
                    cascade: self.cascade,
                    keep_tag: self.keep_tag.enabled(),
                    referrers: self.referrers.enabled(),
                    annotations: &annotations,
                    dry_run: self.dry_run,
                    scratch_root: &scratch_root,
                },
            )
            .await?;

        // Copied after the package lands, never instead of it; reported as a field, not a stderr
        // warning, since `--format json` is how CI learns whether the description travelled.
        let description = if !self.description {
            None
        } else if self.dry_run {
            Some(DescriptionOutcome::SkippedDryRun)
        } else {
            let temp = tempfile::tempdir_in(&scratch_root)?;
            match publisher
                .pull_description(&outcome.source_location, temp.path())
                .await?
            {
                Some(description) => {
                    publisher.push_description(&target, &description).await?;
                    Some(DescriptionOutcome::Copied)
                }
                None => Some(DescriptionOutcome::Absent),
            }
        };

        let report = CopyReport::from_outcome(outcome, description);
        context.ui().status(report.action(), report.summary());
        context.api().report(&report)?;
        // Sidecar conflicts exit 65 only after the report, or the conflicting tag names are lost.
        Ok(match report.sidecar_conflicts.is_empty() {
            true => ExitCode::SUCCESS,
            false => ExitCode::from(ocx_exit::ExitCode::DataError),
        })
    }

    /// Where the copy lands: `--identifier` as given, else the source repository and tag at `--to`
    /// or the default registry.
    fn resolve_target(
        &self,
        source: &ocx_oci::PackageRef,
        default_registry: &str,
    ) -> anyhow::Result<ocx_oci::OciIdentifier> {
        if let Some(identifier) = &self.identifier {
            return identifier.as_target(default_registry);
        }
        let registry = self.to.as_deref().unwrap_or(default_registry);
        let target = ocx_oci::OciIdentifier::from_parts(source.repository(), registry);
        Ok(match source.tag() {
            Some(tag) => target.clone_with_tag(tag),
            None => target,
        })
    }
}
