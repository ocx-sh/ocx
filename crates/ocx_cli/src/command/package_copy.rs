// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    cli::UsageError,
    log, oci,
    publisher::{CopyRequest, Publisher},
};

use crate::api::data::package_copy::{CopyReport, DescriptionOutcome};
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
    platform: Vec<oci::Platform>,

    /// Recompute the rolling tags (`1.4`, `1`, `latest`) at the target.
    ///
    /// Computed against the target's own tag list, not the source's: whether
    /// `1.4` should move depends on what the target already publishes, and a
    /// staging registry ahead of production has a different answer.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    // No doc comment on either flattened field: clap renders the *flattened
    // struct's* own field docs, so anything written here reaches nobody and the
    // struct's text reaches the user (`quality-cli-help.md`, render source).
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

        // Everything below this line is argument-shaped and decided before a
        // single request goes out — an invocation that cannot succeed must not
        // first authenticate against a production registry.
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
        let publisher = Publisher::new(context.remote_client()?.clone());
        // No pre-emptive `ensure_auth` on the target. The source-form refusals
        // that exit 64 are raised inside `Publisher::copy`, by
        // `resolve_source_leaves`, which runs before the first target contact —
        // so authenticating here made "the target registry is provably never
        // contacted" false for exactly the invocations the ADR promises it for.
        // Nothing is lost: every write authenticates itself first, at
        // `oci/copy.rs` (`copy_blob`, and the leaf manifest PUT) and in
        // `merge_platform_into_index`, so a bad target credential still fails
        // before a single byte is transferred.

        // Layers spool here on their way through, and the description artifact
        // downloads here too. Under the OCX home rather than `$TMPDIR`, because
        // a memory-backed `$TMPDIR` turns the spool's byte cap into a bound on
        // how much RAM one promotion eats — the cap bounds the file, not the
        // medium. Created once; `copy_leaf` makes its own subdirectory per leaf.
        let scratch_root = context.file_structure().temp.root().to_path_buf();
        tokio::fs::create_dir_all(&scratch_root)
            .await
            .map_err(|e| ocx_lib::error::file_error(&scratch_root, e))?;

        // Says what this run will do, not what a copy does: `--dry-run -l info`
        // asserting "copying" is a log line the run then contradicts.
        if self.dry_run {
            log::info!("planning a copy of {source} to {target}");
        } else {
            log::info!("copying {source} to {target}");
        }
        let outcome = publisher
            .copy(CopyRequest {
                source: &source,
                target: &target,
                platforms: self.platform.clone(),
                cascade: self.cascade,
                keep_tag: self.keep_tag.enabled(),
                referrers: self.referrers.enabled(),
                annotations: &annotations,
                dry_run: self.dry_run,
                scratch_root: &scratch_root,
            })
            .await?;

        // The description is repository-level and independent of the version, so
        // it is copied after the package landed and never instead of it. The
        // outcome is a reported field rather than a stderr warning: `--format
        // json` is how a CI job learns whether the catalog page travelled, and
        // a dry run that silently dropped the flag printed a plan missing the
        // one thing the flag asked for.
        let description = if !self.description {
            None
        } else if self.dry_run {
            Some(DescriptionOutcome::SkippedDryRun)
        } else {
            let temp = tempfile::tempdir_in(&scratch_root)?;
            match publisher.pull_description(&source, temp.path()).await? {
                Some(description) => {
                    publisher.push_description(&target, &description).await?;
                    Some(DescriptionOutcome::Copied)
                }
                None => Some(DescriptionOutcome::Absent),
            }
        };

        let report = CopyReport::from_outcome(outcome, description);
        // The receipt goes to stderr, leaving stdout to the per-platform rows —
        // the single-table rule, and the Channel Rules' "receipts are
        // diagnostics" (`subsystem-cli-api.md`).
        context.ui().status(report.action(), report.summary());
        context.api().report(&report)?;
        // Reported first, then failed. A sidecar tag the target already holds
        // under a different manifest is refused rather than overwritten — a
        // `.sig` accumulates signatures as layers within itself, so a verbatim
        // PUT would destroy every one the target has and the source does not.
        // The leaf and the other sidecars landed, so this is a data fault in
        // what the *target* holds, not a failed promotion: 65, the code
        // registry-supplied state this build declines already carries. Exiting
        // before the report would swallow the tag names, which are the only
        // part of this an operator can act on.
        Ok(match report.sidecar_conflicts.is_empty() {
            true => ExitCode::SUCCESS,
            false => ExitCode::from(ocx_lib::cli::ExitCode::DataError),
        })
    }

    /// Resolves where the copy lands.
    ///
    /// `--to` rewrites the host and keeps everything else, which is the
    /// promotion shape; `--identifier` states the whole reference. Neither means
    /// the source repository at the default registry, which is only useful when
    /// the source named a different one.
    fn resolve_target(&self, source: &oci::Identifier, default_registry: &str) -> anyhow::Result<oci::Identifier> {
        if let Some(identifier) = &self.identifier {
            return Ok(identifier.with_domain(default_registry)?);
        }
        let registry = self.to.as_deref().unwrap_or(default_registry);
        let target = oci::Identifier::new_registry(source.repository(), registry);
        Ok(match source.tag() {
            Some(tag) => target.clone_with_tag(tag),
            None => target,
        })
    }
}
