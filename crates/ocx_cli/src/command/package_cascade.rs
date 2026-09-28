// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package cascade` - dispatcher for the rolling-tag audit commands.
//!
//! Both leaves audit through this module, or `check` and `repair` disagree about what is broken.

use std::process::ExitCode;

use crate::error::UsageError;
use clap::Subcommand;
use futures::stream::{self, StreamExt, TryStreamExt};
use ocx_index::{Jurisdiction, OcxIndex};
use ocx_package::cascade::{gather, graph};

use crate::options;

/// Packages audited at once; kept narrow because it multiplies `gather`'s own 64-wide fan-out.
const CASCADE_PACKAGE_CONCURRENCY: usize = 4;

/// Dispatcher for `ocx package cascade`.
// Variants stay undocumented: a variant doc would replace the leaf struct's help in `--help`.
#[derive(Subcommand)]
pub enum CascadeGroup {
    Check(super::package_cascade_check::PackageCascadeCheck),
    Repair(super::package_cascade_repair::PackageCascadeRepair),
}

impl CascadeGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            CascadeGroup::Check(check) => check.execute(context).await,
            CascadeGroup::Repair(repair) => repair.execute(context).await,
        }
    }
}

/// One package's audit.
pub struct PackageAudit {
    /// The package the user named, as named.
    pub package: ocx_oci::PackageRef,
    pub observation: graph::TagGraphObservation,
    pub expected: graph::ExpectedGraph,
    pub report: graph::CascadeReport,
    /// True when a claiming index source serves no root yet, so empty `index_findings`
    /// means "not compared" rather than "agrees".
    pub index_layer_skipped: bool,
}

impl PackageAudit {
    /// The name to report this package under.
    pub fn package(&self) -> &ocx_oci::PackageRef {
        &self.package
    }
}

/// The packages whose index staleness layer had no root to compare against.
pub fn index_layer_skipped(audits: &[PackageAudit]) -> Vec<ocx_oci::PackageRef> {
    audits
        .iter()
        .filter(|audit| audit.index_layer_skipped)
        .map(|audit| audit.package().clone())
        .collect()
}

/// Reads and diffs every named package's tag graph, one audit per package in first-named order;
/// identifiers naming the same package union their tags into one scope.
///
/// # Errors
///
/// Exit 64 for a digest-pinned identifier or a non-version scope tag, raised before any registry
/// read; otherwise any registry or index read failure.
pub async fn audit_all(
    context: &crate::app::Context,
    packages: &[options::Identifier],
) -> anyhow::Result<Vec<PackageAudit>> {
    let identifiers = options::Identifier::transform_all(packages.to_vec(), context.default_registry())?;
    let requested = group_requests(identifiers)?;

    let mut audits: Vec<(usize, PackageAudit)> = stream::iter(requested.into_iter().enumerate())
        .map(|(position, (package, requests))| async move {
            audit_one(context, package, requests)
                .await
                .map(|audit| (position, audit))
        })
        .buffer_unordered(CASCADE_PACKAGE_CONCURRENCY)
        .try_collect()
        .await?;

    // Sorted back, or report order follows whichever registry read finished first.
    audits.sort_by_key(|(position, _)| *position);
    Ok(audits.into_iter().map(|(_, audit)| audit).collect())
}

/// Collapses identifiers into one scope-request list per package, in first-named order,
/// parsing every request before any registry read.
fn group_requests(
    identifiers: Vec<ocx_oci::PackageRef>,
) -> anyhow::Result<Vec<(ocx_oci::PackageRef, Vec<Option<graph::AliasTag>>)>> {
    let mut grouped: Vec<(ocx_oci::PackageRef, Vec<Option<graph::AliasTag>>)> = Vec::new();
    for identifier in identifiers {
        let request = graph::scope_request(&identifier)
            .map_err(|error| UsageError::with_source(format!("cannot audit '{identifier}'"), error))?;
        let package = identifier.without_specifiers();
        match grouped.iter_mut().find(|(existing, _)| existing == &package) {
            Some((_, requests)) => requests.push(request),
            None => grouped.push((package, vec![request])),
        }
    }
    Ok(grouped)
}

async fn audit_one(
    context: &crate::app::Context,
    package: ocx_oci::PackageRef,
    requests: Vec<Option<graph::AliasTag>>,
) -> anyhow::Result<PackageAudit> {
    let _spinner = context.progress().spinner(format!("Auditing {package}"));

    // Keyed on a claiming source, not the physical rewrite: only a claimed name has a committed root to compare.
    let index = index_source(context, &package);

    // `route_for_dial`, since `gather` dials the answer directly: an unheld logical name must fail
    // `NotInIndex`, not fall through to a same-spelled repository, and the dial needs the SSRF floor.
    let repository = context.default_index().route_for_dial(&package).await?;

    let observation = gather::gather(context.remote_client()?, &repository, &package, index).await?;
    let expected = graph::fold_expected(&observation);
    let scope = graph::scope_filter(&observation.versions(), &requests)
        .map_err(|error| UsageError::with_source(format!("cannot audit '{package}'"), error))?;
    let report = graph::diff(&observation, &expected, &scope);

    let index_layer_skipped = index.is_some() && observation.index_root.is_none();

    Ok(PackageAudit {
        package,
        observation,
        expected,
        report,
        index_layer_skipped,
    })
}

/// The configured index source whose jurisdiction covers `package`, if any; costs no I/O.
fn index_source<'a>(context: &'a crate::app::Context, package: &ocx_oci::PackageRef) -> Option<&'a OcxIndex> {
    context
        .index_sources()
        .iter()
        .find(|source| source.jurisdiction(package) != Jurisdiction::Outside)
}

#[cfg(test)]
mod tests {
    use crate::exit::classify_library_error as classify_error;
    use ocx_exit::ExitCode as OcxExitCode;

    use super::*;

    fn identifier(reference: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::parse_with_default_registry(reference, "registry.test").expect("fixture parses")
    }

    fn exit_code(error: &anyhow::Error) -> OcxExitCode {
        classify_error(error.as_ref())
    }

    // ── scope requests: grouping ─────────────────────────────────────────

    #[test]
    fn identifiers_for_one_package_share_a_single_audit() {
        let grouped = group_requests(vec![
            identifier("acme/cmake:3.28"),
            identifier("acme/cmake:2"),
            identifier("acme/ninja"),
        ])
        .expect("version tags are valid scopes");

        assert_eq!(grouped.len(), 2, "two packages, however many identifiers named them");
        assert_eq!(grouped[0].0, identifier("acme/cmake"));
        assert_eq!(
            grouped[0].1.len(),
            2,
            "both cmake tags contribute a request to the one audit"
        );
        assert_eq!(grouped[1].0, identifier("acme/ninja"));
    }

    #[test]
    fn packages_keep_the_order_they_were_first_named_in() {
        let grouped = group_requests(vec![
            identifier("acme/ninja"),
            identifier("acme/cmake"),
            identifier("acme/ninja:1"),
        ])
        .expect("version tags are valid scopes");

        let names: Vec<String> = grouped.iter().map(|(package, _)| package.to_string()).collect();
        assert_eq!(names, ["registry.test/acme/ninja", "registry.test/acme/cmake"]);
    }

    #[test]
    fn a_tagless_identifier_requests_the_whole_graph() {
        let grouped = group_requests(vec![identifier("acme/cmake")]).expect("tagless is valid");
        assert_eq!(grouped[0].1, vec![None], "no tag means no narrowing");
    }

    #[test]
    fn a_tag_requests_its_own_node() {
        let grouped = group_requests(vec![identifier("acme/cmake:latest")]).expect("latest is valid");
        assert_eq!(grouped[0].1, vec![Some(graph::AliasTag::Root { variant: None })]);
    }

    // ── scope requests: usage faults (exit 64) ───────────────────────────

    #[test]
    fn a_digest_reference_is_a_usage_error() {
        let error = group_requests(vec![identifier(
            "acme/cmake@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )])
        .expect_err("a digest names one manifest, not a tag graph");

        assert_eq!(exit_code(&error), OcxExitCode::UsageError);
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_a_usage_error() {
        let error = group_requests(vec![identifier("acme/cmake:nightly-build")])
            .expect_err("a tag naming no version narrows to nothing");

        assert_eq!(exit_code(&error), OcxExitCode::UsageError);
    }

    #[test]
    fn a_keep_tag_is_a_usage_error() {
        let error = group_requests(vec![identifier(&format!(
            "acme/cmake:__ocx.keep.sha256-{}",
            "a".repeat(64)
        ))])
        .expect_err("a keep tag is reserved, never a graph node");

        assert_eq!(exit_code(&error), OcxExitCode::UsageError);
    }

    #[test]
    fn a_valid_batch_raises_nothing() {
        // The negative control for the three above: the same call shape, on
        // input that is fine, must not produce a usage error at all.
        assert!(
            group_requests(vec![identifier("acme/cmake:3.28"), identifier("acme/cmake")]).is_ok(),
            "version tags and a tagless reference are both valid scopes"
        );
    }
}
