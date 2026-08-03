// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package cascade repair` - rewrite the rolling tags a check found wrong.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::package::cascade::{apply, graph};

use crate::api::data::package_cascade_repair::RepairEntry;
use crate::options;

/// Re-point a package's rolling tags at the content its versions imply.
///
/// Rebuilds the whole index for every rolling tag `cascade check` reports as
/// wrong and pushes it, using only content the registry already serves - a
/// repair publishes nothing new, it re-points. The full plan is computed
/// first and every manifest it references is checked to still exist, so a run
/// that cannot be completed writes nothing at all.
///
/// Repairing the registry does not update the public index. Pass
/// `--announce-tags PATH` to record the tags this run moved, then hand that
/// file to `ocx package announce --tags-from-file PATH`.
///
/// Exits 0 when every attempted write succeeded, 65 when a finding remains
/// (including a tag that cannot be fixed without publishing new content), and
/// 64 when a package names a digest or a tag that is not a version, or when
/// `--announce-tags` is given more than one package.
#[derive(Parser)]
pub struct PackageCascadeRepair {
    /// Compute and print the same plan without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Write the rolling tags this run moved or created to this file, one per
    /// line, for `ocx package announce --tags-from-file`. Takes one package
    /// per run, since the file names no package and `announce` publishes it
    /// against one. Written on a dry run too, holding the whole plan it would
    /// have repaired; empty when nothing changed.
    #[arg(long = "announce-tags", value_name = "PATH")]
    announce_tags: Option<PathBuf>,

    /// Packages to repair.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl PackageCascadeRepair {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let audits = super::package_cascade::audit_all(&context, &self.packages).await?;

        // `announce --tags-from-file` takes one `--package`, and the file it
        // reads is a bare list of tag names. Two packages' tags in one file
        // would be announced against whichever package the follow-up names,
        // moving the other's index entries to digests it never published.
        if self.announce_tags.is_some() && audits.len() > 1 {
            return Err(ocx_lib::cli::UsageError::new(format!(
                "--announce-tags holds one package's tags per file, and this run covers {}; \
                 repair each package in its own run",
                audits.len()
            ))
            .into());
        }

        let skipped = super::package_cascade::index_layer_skipped(&audits);

        // The report starts as the run that attempted nothing - which is
        // exactly what a clean graph and a preview both end as - and each
        // entry then gains the plan its findings imply and, unless this is a
        // preview, the outcomes of putting that plan on the wire.
        let (reports, graphs): (Vec<_>, Vec<_>) = audits
            .into_iter()
            .map(|audit| (audit.report, (audit.observation, audit.expected)))
            .unzip();
        let mut report =
            crate::api::data::package_cascade_repair::PackageCascadeRepair::from_reports(reports, self.dry_run);
        report.index_layer_skipped = skipped;
        report.announce_tags_path = self.announce_tags.clone();

        for (entry, (observation, expected)) in report.entries.iter_mut().zip(graphs) {
            let planned = graph::plan_repairs(&entry.report, &observation, &expected);

            let attempted = self.writes_to_attempt(&planned);
            entry.outcomes = if attempted.is_empty() {
                Vec::new()
            } else {
                let package = observation.logical.as_ref().unwrap_or(&observation.identifier);
                let _spinner = context.progress().spinner(format!("Repairing {package}"));
                apply::apply(context.remote_client()?, &observation.identifier, attempted).await?
            };
            entry.announce_tags = announce_tags(&entry.report, &planned, &entry.outcomes, self.dry_run);
            entry.planned = planned;
        }

        // Reported before the file is written. A repair mutates a registry;
        // the report is the only record of which aliases moved, and losing it
        // to a failed local write would leave the user unable to tell what
        // already landed (CWE-755).
        context.api().report(&report)?;

        if let Some(path) = &self.announce_tags {
            // Written on every run, empty included: a workflow that chains
            // into `announce --tags-from-file` unconditionally must find a
            // file there whether or not this run had anything to move.
            tokio::fs::write(path, announce_tags_body(&report.entries))
                .await
                .with_context(|| format!("writing announce tags to {}", path.display()))?;
        }

        Ok(crate::conventions::cascade_repair_exit_code(&report).into())
    }

    /// The writes this run will put on the wire: the whole plan, or nothing at
    /// all when the run is a preview.
    ///
    /// A named function rather than an `if` around the apply call so "a
    /// preview writes nothing" is assertable without a registry - this branch
    /// is the only thing between `--dry-run` and a live PUT.
    fn writes_to_attempt<'a>(&self, planned: &'a [graph::PlannedWrite]) -> &'a [graph::PlannedWrite] {
        if self.dry_run { &[] } else { planned }
    }
}

/// The alias tags one package's run hands to `ocx package announce`.
///
/// A real run lists only the aliases whose write actually landed. Announce
/// re-observes each tag it is given and commits what the registry serves, so
/// naming a tag whose write raced, failed or was refused would publish a
/// digest this run did not put there - the index would then be wrong in a new
/// way instead of the old one.
///
/// A preview lists its whole plan instead: it wrote nothing, so "what landed"
/// is empty, and a file holding nothing would be a useless preview of the
/// real run's handoff.
///
/// Index findings join either list. They are the one class of drift a repair
/// cannot close itself, and the announce hop is what closes them.
fn announce_tags(
    report: &graph::CascadeReport,
    planned: &[graph::PlannedWrite],
    outcomes: &[apply::RepairOutcome],
    dry_run: bool,
) -> Vec<String> {
    if dry_run {
        return graph::announce_tags(report, planned);
    }
    outcomes
        .iter()
        .filter(|outcome| matches!(outcome.outcome, apply::WriteOutcome::Written { .. }))
        .map(|outcome| outcome.tag.to_string())
        .chain(report.index_findings.iter().map(|finding| match finding {
            graph::IndexFinding::Stale { tag, .. } | graph::IndexFinding::NotCommitted { tag } => tag.to_string(),
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The `--announce-tags` file body for a whole run.
///
/// One tag per line, sorted and deduped. A run reaching here covers exactly
/// one package - `--announce-tags` rejects more, because the file names no
/// package and `announce` publishes it against one - so the fold over
/// `entries` is a fold over a single entry; it stays general because the sort
/// and the dedup are what the format needs either way.
fn announce_tags_body(entries: &[RepairEntry]) -> String {
    let tags: BTreeSet<&str> = entries
        .iter()
        .flat_map(|entry| entry.announce_tags.iter().map(String::as_str))
        .collect();
    if tags.is_empty() {
        return String::new();
    }
    let mut body = tags.into_iter().collect::<Vec<_>>().join("\n");
    body.push('\n');
    body
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use ocx_lib::oci;
    use ocx_lib::package::cascade::graph::{AliasTag, CascadeReport, PlannedWrite};
    use ocx_lib::package::version::Version;

    use super::*;

    fn parse(argv: &[&str]) -> PackageCascadeRepair {
        PackageCascadeRepair::try_parse_from(argv).expect("valid invocation parses")
    }

    fn planned(tag: &str) -> PlannedWrite {
        PlannedWrite {
            tag: AliasTag::Version(Version::parse(tag).expect("fixture version parses")),
            index: oci::ImageIndex {
                schema_version: 2,
                media_type: None,
                manifests: Vec::new(),
                artifact_type: None,
                annotations: None,
            },
            observed_digest: None,
            referenced_digests: Vec::new(),
            reasons: Vec::new(),
        }
    }

    fn report() -> CascadeReport {
        CascadeReport {
            identifier: oci::Identifier::parse("registry.test/acme/cmake").expect("fixture parses"),
            logical: None,
            aliases: Default::default(),
            rows: Vec::new(),
            index_findings: Vec::new(),
            ignored_tags: Vec::new(),
            unrepairable: Vec::new(),
        }
    }

    fn entry(announce_tags: &[&str]) -> RepairEntry {
        RepairEntry {
            report: report(),
            planned: Vec::new(),
            outcomes: Vec::new(),
            announce_tags: announce_tags.iter().map(|tag| (*tag).to_string()).collect(),
        }
    }

    fn digest() -> oci::Digest {
        oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
            .expect("fixture digest parses")
    }

    fn outcome(tag: &str, outcome: apply::WriteOutcome) -> apply::RepairOutcome {
        apply::RepairOutcome {
            tag: AliasTag::Version(Version::parse(tag).expect("fixture version parses")),
            outcome,
        }
    }

    fn landed(tag: &str) -> apply::RepairOutcome {
        outcome(
            tag,
            apply::WriteOutcome::Written {
                digest: digest(),
                verified: true,
                dropped: Vec::new(),
            },
        )
    }

    // ── --dry-run gates the write, structurally ──────────────────────────

    #[test]
    fn a_preview_attempts_none_of_its_plan() {
        let repair = parse(&["repair", "--dry-run", "acme/cmake"]);
        let plan = vec![planned("3.28"), planned("3")];

        assert!(
            repair.writes_to_attempt(&plan).is_empty(),
            "a preview must reach the registry with nothing, however large its plan"
        );
    }

    #[test]
    fn a_real_run_attempts_its_whole_plan() {
        // The negative control: the same call on the same plan, with the flag
        // off, must hand the whole plan through - otherwise the assertion
        // above would pass on a function that always returns empty.
        let repair = parse(&["repair", "acme/cmake"]);
        let plan = vec![planned("3.28"), planned("3")];

        assert_eq!(
            repair.writes_to_attempt(&plan).len(),
            2,
            "without --dry-run every planned alias is attempted"
        );
    }

    // ── --announce-tags round-trips through the announce reader ──────────

    #[test]
    fn the_tags_body_round_trips_through_the_announce_parser() {
        let entries = vec![entry(&["3.28", "3", "latest"]), entry(&["latest", "1"])];

        let parsed = crate::conventions::parse_tags_file(announce_tags_body(&entries).as_bytes());

        assert_eq!(
            parsed,
            ["1", "3", "3.28", "latest"],
            "every tag survives the file, sorted and deduped across packages"
        );
    }

    #[test]
    fn a_run_with_nothing_to_move_writes_an_empty_body() {
        assert!(
            announce_tags_body(&[entry(&[])]).is_empty(),
            "an empty file is a safe no-op for the announce follow-up"
        );
        assert!(crate::conventions::parse_tags_file(b"").is_empty());
    }

    // ── a real run announces only what landed ────────────────────────────

    #[test]
    fn a_real_run_announces_only_the_aliases_whose_write_landed() {
        let plan = vec![planned("3.28"), planned("3"), planned("1")];
        let outcomes = vec![
            landed("3.28"),
            outcome(
                "3",
                apply::WriteOutcome::Failed {
                    message: "registry said no".to_string(),
                },
            ),
            outcome(
                "1",
                apply::WriteOutcome::Raced {
                    expected: Some(digest()),
                    live: None,
                },
            ),
        ];

        assert_eq!(
            announce_tags(&report(), &plan, &outcomes, false),
            ["3.28"],
            "announcing a failed or raced alias would commit a digest this run never wrote"
        );
    }

    #[test]
    fn a_preview_announces_its_whole_plan() {
        // The negative control for the filter above: the same plan, with the
        // preview flag on, must hand every planned alias through - a preview
        // has no outcomes to filter by, and its file is the documented
        // preview of what the real run would announce.
        let plan = vec![planned("3.28"), planned("3")];

        assert_eq!(announce_tags(&report(), &plan, &[], true), ["3", "3.28"]);
    }

    #[test]
    fn an_index_finding_is_announced_even_when_nothing_was_written() {
        let mut stale = report();
        stale.index_findings.push(graph::IndexFinding::NotCommitted {
            tag: AliasTag::Root { variant: None },
        });

        assert_eq!(
            announce_tags(&stale, &[], &[], false),
            ["latest"],
            "an alias the index never committed is the one drift a repair cannot close itself"
        );
    }
}
