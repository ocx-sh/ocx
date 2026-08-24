// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package cascade` - dispatcher for the rolling-tag audit commands.
//!
//! The read half of both subcommands lives here rather than in either leaf:
//! `check` reports the diff and `repair` plans writes from it, but they must
//! audit a package identically or the two would disagree about what is broken.

use std::process::ExitCode;

use clap::Subcommand;
use futures::stream::{self, StreamExt, TryStreamExt};
use ocx_lib::cli::UsageError;
use ocx_lib::oci;
use ocx_lib::oci::index::{Jurisdiction, OcxIndex};
use ocx_lib::package::cascade::{gather, graph};

use crate::options;

/// How many packages are audited at once.
///
/// Deliberately narrow. Each audit already fans its own manifest reads out
/// 64-wide inside `gather`, so this bound multiplies that one, and both
/// commands are normally given a single package anyway.
const CASCADE_PACKAGE_CONCURRENCY: usize = 4;

/// Dispatcher for `ocx package cascade`.
///
/// The variants carry no doc comment on purpose: clap renders a variant's own
/// doc as the subcommand's help and ignores the argument struct's when one is
/// present, and the user-facing text for these two lives with the flags it
/// describes, in `package_cascade_check.rs` and `package_cascade_repair.rs`.
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
///
/// Carries the observation and the fold alongside the report because `repair`
/// plans from all three: a report alone says what is wrong, not what content
/// already exists to fix it with.
pub struct PackageAudit {
    pub observation: graph::TagGraphObservation,
    pub expected: graph::ExpectedGraph,
    pub report: graph::CascadeReport,
    /// True when a configured index source claims this package but has no root
    /// document for it yet - a package nobody has announced. The staleness
    /// layer then produces no findings because it had nothing to compare
    /// against, which a report of empty `index_findings` cannot tell apart
    /// from an index that agrees.
    pub index_layer_skipped: bool,
}

impl PackageAudit {
    /// The name to report this package under: the logical one when the
    /// identifier was rewritten, otherwise the repository itself.
    pub fn package(&self) -> &oci::Identifier {
        self.observation
            .logical
            .as_ref()
            .unwrap_or(&self.observation.identifier)
    }
}

/// The packages in `audits` whose index staleness layer had no root to run
/// against - the plain-mode note both commands print.
pub fn index_layer_skipped(audits: &[PackageAudit]) -> Vec<oci::Identifier> {
    audits
        .iter()
        .filter(|audit| audit.index_layer_skipped)
        .map(|audit| audit.package().clone())
        .collect()
}

/// Reads and diffs every named package's tag graph.
///
/// Identifiers naming the same package produce one audit: their tags union
/// into a single scope, so `cmake:3.28 cmake:2` reports one graph rather than
/// two disagreeing halves of it. Results keep the order each package was first
/// named in.
///
/// # Errors
///
/// A usage error (exit 64) for a digest-pinned identifier or a scope tag that
/// names no version - raised before the first registry read. Otherwise any
/// registry or index read failure, including a logical name whose index root
/// cannot be resolved.
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

    // `buffer_unordered` yields in completion order; a report whose package
    // order depended on which registry read finished first would differ
    // between two runs over the same registry.
    audits.sort_by_key(|(position, _)| *position);
    Ok(audits.into_iter().map(|(_, audit)| audit).collect())
}

/// Collapses the identifier list into one scope-request list per package,
/// keeping first-named order.
///
/// Every scope request is parsed here, ahead of the first registry read: a
/// digest-pinned identifier or a junk tag is a fault the user can fix without
/// waiting on a network round trip, and reporting it after a full graph read
/// would be pure latency.
fn group_requests(
    identifiers: Vec<oci::Identifier>,
) -> anyhow::Result<Vec<(oci::Identifier, Vec<Option<graph::AliasTag>>)>> {
    let mut grouped: Vec<(oci::Identifier, Vec<Option<graph::AliasTag>>)> = Vec::new();
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

/// Resolves one package to its physical repository, reads its whole tag graph,
/// and diffs it against the fold.
async fn audit_one(
    context: &crate::app::Context,
    package: oci::Identifier,
    requests: Vec<Option<graph::AliasTag>>,
) -> anyhow::Result<PackageAudit> {
    let _spinner = context.progress().spinner(format!("Auditing {package}"));

    // A configured source claiming the name is what turns the index-staleness
    // layer on - not the physical rewrite. An indexed package can be hosted
    // under its own name, and a plain registry package can have a locally
    // derived root pointing at itself; only a source that claims the name has
    // a committed root worth comparing against.
    let index = index_source(context, &package);

    // Authoritative-stop: a logical name whose root will not resolve is an
    // error here, never a silent fall-through to a registry repository that
    // happens to share its spelling.
    let physical = context.default_index().physical_reference(&package).await?;
    let repository = physical.unwrap_or_else(|| package.clone());
    let logical = (repository != package).then(|| package.clone());

    let observation = gather::gather(context.remote_client()?, &repository, logical.as_ref(), index).await?;
    let expected = graph::fold_expected(&observation);
    let scope = graph::scope_filter(&observation.versions(), &requests)
        .map_err(|error| UsageError::with_source(format!("cannot audit '{package}'"), error))?;
    let report = graph::diff(&observation, &expected, &scope);

    // A source that claims the name but serves no root has published nothing
    // to compare the registry against, so the staleness layer stayed silent
    // for want of input rather than for want of drift.
    let index_layer_skipped = index.is_some() && observation.index_root.is_none();

    Ok(PackageAudit {
        observation,
        expected,
        report,
        index_layer_skipped,
    })
}

/// The configured index source that publishes `package`, if any.
///
/// Asks each source's own jurisdiction rather than comparing namespaces, the
/// same way `ocx index update` routes a package: a source answers `Outside`
/// for a registry it does not serve, and `Authoritative` for every name in the
/// one it does. The verdict is a registry comparison, so it costs no I/O.
fn index_source<'a>(context: &'a crate::app::Context, package: &oci::Identifier) -> Option<&'a OcxIndex> {
    context
        .index_sources()
        .iter()
        .find(|source| source.jurisdiction(package) != Jurisdiction::Outside)
}

#[cfg(test)]
mod tests {
    use ocx_lib::cli::{ExitCode as OcxExitCode, classify_error};

    use super::*;

    fn identifier(reference: &str) -> oci::Identifier {
        oci::Identifier::parse_with_default_registry(reference, "registry.test").expect("fixture parses")
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
    fn a_canonical_digest_tag_is_a_usage_error() {
        let error = group_requests(vec![identifier("acme/cmake:sha256.aaaa1111")])
            .expect_err("a canonical tag is reserved, never a graph node");

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
