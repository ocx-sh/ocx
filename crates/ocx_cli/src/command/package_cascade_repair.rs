// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package cascade repair` - rewrite the rolling tags a check found wrong.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package::cascade::{apply, graph};

use crate::api::data::package_cascade_repair::RepairEntry;
use crate::options;

/// Re-point a package's rolling tags at the content its versions imply.
#[derive(Parser)]
#[command(long_about = "\
    Re-point a package's rolling tags at the content its versions imply.\n\n\
    Rebuilds the whole index for every rolling tag `cascade check` reports as wrong and pushes \
    it, using only content the registry already serves. A repair publishes nothing new and only \
    re-points. The full plan is computed first, and every manifest it references is checked to \
    still exist. A run that cannot be completed therefore writes nothing at all.\n\n\
    Repairing the registry does not update the public index. Pass `--tags-file PATH` to append \
    the tags this run moved, one per line, then hand that file to `ocx package announce \
    --tags-file PATH`.\n\n\
    Exits 0 when every attempted write succeeded. Exits 65 when a finding remains, including a tag \
    that cannot be fixed without publishing new content. Exits 64 when a package names a digest \
    or a tag that is not a version, or when `--tags-file` is given more than one package.\n\n\
    `repair` writes by default, since `check` is already the read-only preview; `--dry-run` \
    computes and reports the same plan. Identifier forms and scope are those of `check`, except that a build tag \
    writes only the aliases that build owns: a pre-release build never moves a stable alias.\n\n\
    For each broken alias it rebuilds the whole platform index entry from the same fold \
    `check` diffs against. It preserves every observed entry the fold does not supersede. That includes annotations already on the index, non-platform entries such as attestations, and \
    orphaned alias tags whose child manifests still exist. A missing referenced manifest is \
    dropped only if every entry naming that digest is an orphan slot. Otherwise the whole \
    alias is refused and reported while other aliases still write.\n\n\
    Writes are batched after the whole plan is built and run concurrently per tag.\n\n\
    After each write `repair` re-reads the tag and warns when the digest disagrees, which is \
    evidence of a concurrent publisher. An outcome is `raced` only when nothing was written \
    because the tag moved between read and write.\n\n\
    There is no conditional-request guard, so avoid repairing a repository with a publish in \
    flight. An index-staleness finding is never something `repair` writes. Reaching the public \
    index with the fix is a second hop through `ocx package announce`. Refreshing one \
    machine's local copy is `ocx index update`'s job.\n\n\
    `--tags-file` records the tags whose write landed (not `raced`, `refused` or \
    `failed` ones) and the tags a build-tag scope writes that already carry the expected content (never a tag it would not write, and none for an alias or whole-graph request), unioned with \
    every tag an index-staleness finding names, so one file covers both hops and a retry after a \
    failed announce still lists the tags the first run moved. It is written on every run, empty included, so a workflow can feed it to \
    `announce` unconditionally. A preview contributes no landed tag, and its plan is in the \
    JSON report as each entry's `planned`.\n\n\
    A follow-up `announce` updates or creates the listed aliases; a repair never produces a \
    removal.")]
pub struct PackageCascadeRepair {
    /// Compute and print the same plan without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Append the rolling tags this run moved or created to this file, one per
    /// line, for `ocx package announce --tags-file`. Creates the file if
    /// absent and keeps the tags already in it. Takes one package
    /// per run, since the file names no package and `announce` publishes it
    /// against one. Written on a dry run too - a preview moves nothing, so
    /// it adds only the tags an index finding already names.
    #[arg(long = "tags-file", value_name = "PATH")]
    tags_file: Option<PathBuf>,

    /// Packages to repair.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl PackageCascadeRepair {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let audits = super::package_cascade::audit_all(&context, &self.packages).await?;

        // The file names no package, so a second package's tags would be announced against the
        // first, moving its index entries to digests it never published.
        if self.tags_file.is_some() && audits.len() > 1 {
            return Err(crate::error::UsageError::new(format!(
                "--tags-file holds one package's tags per file, and this run covers {}; \
                 repair each package in its own run",
                audits.len()
            ))
            .into());
        }

        let skipped = super::package_cascade::index_layer_skipped(&audits);

        let (reports, graphs): (Vec<_>, Vec<_>) = audits
            .into_iter()
            .map(|audit| (audit.report, (audit.observation, audit.expected, audit.announce_scope)))
            .unzip();
        let mut report =
            crate::api::data::package_cascade_repair::PackageCascadeRepair::from_reports(reports, self.dry_run);
        report.index_layer_skipped = skipped;
        report.tags_file = self.tags_file.clone();

        for (entry, (observation, expected, announce_scope)) in report.items.iter_mut().zip(graphs) {
            let planned = graph::plan_repairs(&entry.report, &observation, &expected, announce_scope.as_ref());

            let attempted = self.writes_to_attempt(&planned);
            entry.outcomes = if attempted.is_empty() {
                Vec::new()
            } else {
                let package = observation
                    .logical
                    .as_ref()
                    .map_or_else(|| observation.identifier.to_string(), ToString::to_string);
                let _spinner = context.progress().spinner(format!("Repairing {package}"));
                apply::apply(context.remote_client()?, &observation.identifier, attempted).await?
            };
            entry.tags = announce_tags(&entry.report, &entry.outcomes, announce_scope.as_ref());
            entry.planned = planned;
        }

        // Reported before the file write, or a failed local write loses the only record of which aliases moved.
        context.api().report(&report)?;

        if let Some(path) = &self.tags_file {
            // Appended like `push --tags-file`, so a pipeline's earlier tags survive. Written on every
            // run, empty included, or an unconditional `announce --tags-file` finds no file.
            crate::conventions::append_tags_file(path, &run_tags(&report.items)).await?;
        }

        Ok(crate::conventions::cascade_repair_exit_code(&report).into())
    }

    /// The writes this run puts on the wire: the whole plan, or nothing on a preview.
    // A function, not an inline `if`, so the `--dry-run` gate stays unit-testable without a registry.
    fn writes_to_attempt<'a>(&self, planned: &'a [graph::PlannedWrite]) -> &'a [graph::PlannedWrite] {
        if self.dry_run { &[] } else { planned }
    }
}

/// The alias tags a run hands to `ocx package announce`: writes that landed, every tag an index
/// finding names, and every in-scope alias that already carries exactly the expected content (a
/// retry whose first run moved the aliases but never announced them). Never a raced, failed,
/// refused or unattempted write, since announce commits whatever the registry serves and would
/// publish a digest this run never wrote. A `Some` `scope` bounds the written and current-alias halves to
/// the aliases the requested build tag owns, so a candidate's announce never creates a floating tag and a
/// superseded build (empty set) announces none; `None` (alias or whole-graph request) leaves written
/// aliases unbounded.
fn announce_tags(
    report: &graph::CascadeReport,
    outcomes: &[apply::RepairOutcome],
    scope: Option<&BTreeSet<graph::AliasTag>>,
) -> Vec<String> {
    outcomes
        .iter()
        .filter(|outcome| {
            matches!(outcome.outcome, apply::WriteOutcome::Written { .. })
                && scope.is_none_or(|scope| scope.contains(&outcome.tag))
        })
        .map(|outcome| outcome.tag.to_string())
        .chain(
            report
                .aliases
                .iter()
                .filter(|(tag, state)| {
                    scope.is_some_and(|scope| scope.contains(*tag))
                        && **state == graph::AliasState::Present
                        && report
                            .rows
                            .iter()
                            .all(|row| &row.tag != *tag || row.status == graph::SlotStatus::Ok)
                })
                .map(|(tag, _)| tag.to_string()),
        )
        .chain(report.index_findings.iter().map(|finding| match finding {
            graph::IndexFinding::Stale { tag, .. } | graph::IndexFinding::NotCommitted { tag } => tag.to_string(),
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// This run's tags for the `--tags-file`: sorted and deduped across packages.
fn run_tags(entries: &[RepairEntry]) -> Vec<String> {
    let tags: BTreeSet<&str> = entries
        .iter()
        .flat_map(|entry| entry.tags.iter().map(String::as_str))
        .collect();
    tags.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use ocx_package::cascade::graph::{AliasTag, CascadeReport, PlannedWrite};
    use ocx_package::version::Version;

    use super::*;

    fn parse(argv: &[&str]) -> PackageCascadeRepair {
        PackageCascadeRepair::try_parse_from(argv).expect("valid invocation parses")
    }

    fn planned(tag: &str) -> PlannedWrite {
        PlannedWrite {
            tag: AliasTag::Version(Version::parse(tag).expect("fixture version parses")),
            index: ocx_oci::ImageIndex {
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
            identifier: ocx_oci::OciIdentifier::parse_target("registry.test/acme/cmake", ocx_oci::DEFAULT_REGISTRY)
                .expect("fixture parses"),
            logical: None,
            aliases: Default::default(),
            rows: Vec::new(),
            index_findings: Vec::new(),
            ignored_tags: Vec::new(),
            unrepairable: Vec::new(),
        }
    }

    fn entry(tags: &[&str]) -> RepairEntry {
        RepairEntry {
            report: report(),
            planned: Vec::new(),
            outcomes: Vec::new(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        }
    }

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
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

    // ── --tags-file round-trips through the announce reader ─────────────

    #[test]
    fn the_tags_body_round_trips_through_the_announce_parser() {
        let entries = vec![entry(&["3.28", "3", "latest"]), entry(&["latest", "1"])];

        let parsed = crate::conventions::parse_tags_file(
            crate::conventions::merge_tags_file(&[], &run_tags(&entries)).as_bytes(),
        );

        assert_eq!(
            parsed,
            ["1", "3", "3.28", "latest"],
            "every tag survives the file, sorted and deduped across packages"
        );
    }

    #[test]
    fn the_tags_body_appends_to_what_the_file_already_holds() {
        let existing = vec!["3.28.1".to_string(), "latest".to_string()];

        let body = crate::conventions::merge_tags_file(&existing, &run_tags(&[entry(&["3", "latest"])]));

        assert_eq!(
            body, "3.28.1\nlatest\n3\n",
            "existing tags first, new ones after, no duplicates"
        );
    }

    #[test]
    fn a_run_with_nothing_to_move_keeps_what_the_file_already_holds() {
        let existing = vec!["3.28.1".to_string()];

        assert_eq!(
            crate::conventions::merge_tags_file(&existing, &run_tags(&[entry(&[])])),
            "3.28.1\n"
        );
    }

    #[test]
    fn a_run_with_nothing_to_move_writes_an_empty_body() {
        assert!(
            run_tags(&[entry(&[])]).is_empty(),
            "an empty file is a safe no-op for the announce follow-up"
        );
        assert!(crate::conventions::parse_tags_file(b"").is_empty());
    }

    // ── a real run announces only what landed ────────────────────────────

    #[test]
    fn a_real_run_announces_only_the_aliases_whose_write_landed() {
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
            announce_tags(&report(), &outcomes, None),
            ["3.28"],
            "announcing a failed or raced alias would commit a digest this run never wrote"
        );
    }

    #[test]
    fn an_index_finding_is_announced_even_when_nothing_was_written() {
        // Also the whole of what a preview hands on: it attempts no write, so
        // it contributes no outcome, and its plan is not a parameter here at
        // all - a preview cannot announce a tag it did not put on the wire.
        let mut stale = report();
        stale.index_findings.push(graph::IndexFinding::NotCommitted {
            tag: AliasTag::Root { variant: None },
        });

        assert_eq!(
            announce_tags(&stale, &[], None),
            ["latest"],
            "an alias the index never committed is the one drift a repair cannot close itself"
        );
    }

    #[test]
    fn an_alias_already_correct_is_announced_though_nothing_was_written() {
        // A retry after a publish whose announce failed: the first run moved the aliases, this one finds
        // them current and writes nothing, so announcing only what landed would leave them out of the index.
        let mut current = report();
        current.aliases.insert(
            AliasTag::Version(Version::parse("3.28").expect("fixture version parses")),
            graph::AliasState::Present,
        );
        current
            .aliases
            .insert(AliasTag::Root { variant: None }, graph::AliasState::Present);
        current.aliases.insert(
            AliasTag::Version(Version::parse("3").expect("fixture version parses")),
            graph::AliasState::Absent,
        );

        assert_eq!(
            announce_tags(&current, &[], Some(&scope(&["3.28", "latest"]))),
            ["3.28", "latest"],
            "a current alias belongs in the file, an absent one does not"
        );
    }

    fn present(report: &mut CascadeReport, tags: &[&str]) {
        for tag in tags {
            let alias = match *tag {
                "latest" => AliasTag::Root { variant: None },
                tag => AliasTag::Version(Version::parse(tag).expect("fixture version parses")),
            };
            report.aliases.insert(alias, graph::AliasState::Present);
        }
    }

    fn scope(tags: &[&str]) -> BTreeSet<AliasTag> {
        let mut holder = report();
        present(&mut holder, tags);
        holder.aliases.into_keys().collect()
    }

    #[test]
    fn a_prerelease_build_tag_announces_only_its_own_prerelease() {
        // Check scope climbs through the stable ancestors; a candidate's announce must not.
        let mut current = report();
        present(&mut current, &["1.1.0-beta", "1.0", "1", "latest"]);

        assert_eq!(
            announce_tags(&current, &[], Some(&scope(&["1.1.0-beta"]))),
            ["1.1.0-beta"],
            "a candidate that announced `latest` would create a floating tag row"
        );
    }

    #[test]
    fn a_candidate_request_never_announces_a_drifted_alias_its_repair_rewrote() {
        let latest = apply::RepairOutcome {
            tag: AliasTag::Root { variant: None },
            outcome: apply::WriteOutcome::Written {
                digest: digest(),
                verified: true,
                dropped: Vec::new(),
            },
        };

        assert_eq!(
            announce_tags(
                &report(),
                &[landed("1.1.0-beta"), latest],
                Some(&scope(&["1.1.0-beta"]))
            ),
            ["1.1.0-beta"],
            "a rewritten `latest` outside the candidate's scope would create a floating tag row"
        );
    }

    #[test]
    fn a_superseded_build_tag_whose_leaf_owns_no_alias_announces_no_rewritten_alias() {
        let latest = apply::RepairOutcome {
            tag: AliasTag::Root { variant: None },
            outcome: apply::WriteOutcome::Written {
                digest: digest(),
                verified: true,
                dropped: Vec::new(),
            },
        };
        let owns_nothing = BTreeSet::new();

        assert!(announce_tags(&report(), &[latest], Some(&owns_nothing)).is_empty());
    }

    #[test]
    fn a_dev_build_tag_announces_only_its_own_dev_alias() {
        let mut current = report();
        present(&mut current, &["0.1.0-dev", "0.1", "0", "latest"]);

        assert_eq!(
            announce_tags(&current, &[], Some(&scope(&["0.1.0-dev"]))),
            ["0.1.0-dev"]
        );
    }

    #[test]
    fn a_release_build_tag_never_announces_an_alias_a_newer_release_owns() {
        // Scope `1.0.1_b1` while `1.1.0` holds `1` and `latest`: unbounded, the file named both.
        let mut current = report();
        present(&mut current, &["1.0.1", "1.0", "1", "latest"]);

        assert_eq!(
            announce_tags(&current, &[], Some(&scope(&["1.0.1", "1.0"]))),
            ["1.0", "1.0.1"]
        );
    }
}
