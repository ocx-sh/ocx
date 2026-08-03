// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{
    ci::CiFlavor,
    cli::{MetadataResolutionError, UsageError},
    oci,
    package::cascade::apply::WriteOutcome,
    package::metadata::env::entry::Entry,
    publisher::LayerRef,
    shell::Shell,
};

/// Infers a metadata file path based on the archive file path.
/// For example, if the content path is `/path/to/package.tar.gz`, this function will return `/path/to/package-metadata.json`.
pub fn infer_metadata_file(content: &std::path::Path) -> Result<std::path::PathBuf, MetadataResolutionError> {
    let content_parent = content
        .parent()
        .ok_or_else(|| MetadataResolutionError::InvalidLayerPath {
            layer: content.to_path_buf(),
            reason: "no parent directory".into(),
        })?;
    let mut content_name = content
        .file_stem()
        .ok_or_else(|| MetadataResolutionError::InvalidLayerPath {
            layer: content.to_path_buf(),
            reason: "no file stem".into(),
        })?
        .to_string_lossy()
        .to_string();
    let known_archive_extensions = [".tar", ".tar.gz", ".tgz", ".zip"];
    for extension in known_archive_extensions {
        if content_name.ends_with(extension) {
            content_name.truncate(content_name.len() - extension.len());
            break;
        }
    }
    Ok(content_parent.join(format!("{}-metadata.json", content_name)))
}

/// Resolves the metadata path used by `ocx package push` and `ocx package
/// test`.
///
/// When `explicit` is `Some`, it wins. Otherwise the helper walks the file
/// layers, infers a candidate metadata path for each, and dedups: zero file
/// layers → [`MetadataResolutionError::Required`], multiple distinct
/// candidates → [`MetadataResolutionError::Ambiguous`].
pub fn resolve_metadata_path(
    layers: &[LayerRef],
    explicit: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, MetadataResolutionError> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for layer in layers {
        if let LayerRef::File { path: file, .. } = layer {
            let candidate = infer_metadata_file(file)?;
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    match candidates.len() {
        0 => Err(MetadataResolutionError::Required),
        1 => Ok(candidates.into_iter().next().unwrap()),
        _ => Err(MetadataResolutionError::Ambiguous { candidates }),
    }
}

/// Resolves an explicit `--platform` value, falling back to the current host
/// platform when the flag was omitted.
///
/// The single source of truth for "which platform does this command resolve
/// against" — every resolution command (`ocx package install/pull/exec`,
/// `ocx run`, `ocx env`, ...) applies the same default.
pub fn platform_or_default(platform: Option<oci::Platform>) -> oci::Platform {
    platform.unwrap_or_else(|| oci::Platform::current().unwrap_or_else(oci::Platform::any))
}

/// Emit shell-sourceable export lines for a slice of env entries.
///
/// This is the single shared emit helper consumed by:
/// - `ocx env` (toolchain-tier, new Phase 2 command)
/// - `ocx package env` (OCI-tier, delegates here for `--shell` output)
/// - `ocx direnv export` (delegates here instead of inlining the loop)
///
/// Wraps [`Shell::export_path`] / [`Shell::export_constant`] and skips entries
/// whose key fails POSIX validation (emitting a `# ocx:` note to stderr so the
/// caller is informed without aborting the full output).
///
/// `Shell::Bash` is the fixed shell for `direnv export` (direnv always evals
/// `.envrc` in a bash sub-shell — no `--shell` flag on that command). For
/// `ocx env` / `ocx package env` the caller passes the user-selected shell.
///
/// # Panics
///
/// This function is infallible — `None` from `export_path` / `export_constant`
/// is handled by a stderr note.
pub fn emit_lines(shell: Shell, entries: &[Entry]) {
    use ocx_lib::package::metadata::env::modifier::ModifierKind;
    for entry in entries {
        let line = match entry.kind {
            ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
            ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
        };
        match line {
            Some(line) => println!("{line}"),
            None => eprintln!("# ocx: skipping invalid env-var key {:?}", entry.key),
        }
    }
}

/// Resolve a `--shell` clap argument to an explicit [`Shell`], or `None`
/// when the default-format (JSON / `--format plain`) path should be taken.
///
/// `--shell` is declared as `Option<Option<Shell>>` with
/// `num_args=0..=1, require_equals=true` (clap 4.x produces `Some(None)` for
/// a bare `--shell`, `Some(Some(s))` for `--shell=NAME`, `None` when absent —
/// `require_equals` keeps a following positional from being swallowed):
///
/// - `None` (flag absent) → `Ok(None)`: caller uses the default-format path.
/// - `Some(None)` (bare `--shell`) → autodetect from `$SHELL`/parent; a
///   [`UsageError`] (exit 64) when undetectable.
/// - `Some(Some(s))` (explicit `--shell=NAME`) → `Ok(Some(s))`.
///
/// Shared by `ocx env` and `ocx package env` so the bare-shell autodetect and
/// its identical undetectable-shell `UsageError` exist exactly once.
pub fn resolve_shell_arg(shell: Option<Option<Shell>>) -> anyhow::Result<Option<Shell>> {
    match shell {
        None => Ok(None),
        Some(Some(s)) => Ok(Some(s)),
        Some(None) => {
            let s = Shell::detect().ok_or_else(|| {
                UsageError::new(
                    "could not autodetect shell from $SHELL or parent process; \
                     pass --shell=NAME explicitly. \
                     Legal values: bash, zsh, fish, ash, dash, ksh, sh, \
                     pwsh, elvish, nushell, batch (sh == dash POSIX alias)",
                )
            })?;
            Ok(Some(s))
        }
    }
}

/// Resolve a `--ci` clap argument to an explicit [`CiFlavor`], or `None` when
/// the flag is absent and the caller should take the non-CI path.
///
/// `--ci` is declared as `Option<Option<CiFlavor>>` with
/// `num_args=0..=1, require_equals=true` (mirroring `--shell`):
///
/// - `None` (flag absent) → `Ok(None)`: caller uses the structured-report /
///   `--shell` path.
/// - `Some(None)` (bare `--ci`) → autodetect from CI env vars
///   (`$GITHUB_ACTIONS`, `$GITLAB_CI`); a [`UsageError`] (exit 64) when no
///   provider is detected.
/// - `Some(Some(provider))` (explicit `--ci=NAME`) → `Ok(Some(provider))`.
///
/// Shared by `ocx env` and `ocx package env` so the bare-`--ci` autodetect and
/// its identical undetectable-provider `UsageError` exist exactly once.
pub fn resolve_ci_arg(ci: Option<Option<CiFlavor>>) -> anyhow::Result<Option<CiFlavor>> {
    match ci {
        None => Ok(None),
        Some(Some(provider)) => Ok(Some(provider)),
        Some(None) => {
            let provider = CiFlavor::detect()
                .ok_or_else(|| UsageError::new("could not autodetect CI provider; pass --ci=github or --ci=gitlab"))?;
            Ok(Some(provider))
        }
    }
}

/// Splits tag-list bytes on commas and newlines into trimmed, non-empty tag
/// names, preserving order.
///
/// Shared wire format for `ocx package announce --tags-from-file` (read) and
/// `ocx package push --announce-file` (write) — parses whatever either side
/// writes, and is byte-compatible with `indexbot --tags-from-file`.
pub fn parse_tags_file(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(['\n', '\r', ','])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

/// Appends `tags` onto `existing`, deduping (first occurrence wins) while
/// preserving order, and returns the comma-joined content to write back.
pub fn merge_tags_file(existing: &[String], tags: &[String]) -> String {
    let mut merged = existing.to_vec();
    for tag in tags {
        if !merged.contains(tag) {
            merged.push(tag.clone());
        }
    }
    merged.join(",")
}

/// Export resolved env entries into a CI system's persistence channel.
///
/// Shared by `ocx env` and `ocx package env`. Rejects `--export-file` for
/// GitHub Actions (which infers its two-file sink from `$GITHUB_ENV` /
/// `$GITHUB_PATH`); GitLab uses `export_file` as its output path, falling back
/// to stdout when `None`.
pub fn export_ci(provider: CiFlavor, export_file: Option<std::path::PathBuf>, entries: &[Entry]) -> anyhow::Result<()> {
    if provider == CiFlavor::GitHubActions && export_file.is_some() {
        return Err(UsageError::new(
            "--export-file is not supported with --ci=github; GitHub infers $GITHUB_ENV/$GITHUB_PATH",
        )
        .into());
    }
    provider.export(entries, export_file)?;
    Ok(())
}

/// Project composed env entries into the inspect report's wire shape.
///
/// Shared by `ocx package inspect` (where the entries are the `--env`
/// overrides alone) and `ocx inspect` (where they are `[env]`, the selected
/// groups' `[group.<name>.env]`, then `--env`, already in application order).
/// The report keeps entries in that order rather than merging them, so a
/// consumer sees every contributing declaration instead of a collapsed result;
/// `ocx env` is what answers "what is the final value".
pub fn env_entries(entries: &[Entry]) -> Vec<crate::api::data::env::EnvEntry> {
    entries
        .iter()
        .map(|entry| crate::api::data::env::EnvEntry {
            key: entry.key.clone(),
            value: entry.value.clone(),
            kind: entry.kind.clone(),
            // Patch provenance is an `ocx env` concern — the inspect report
            // carries no package-composed entries to attribute.
            source: None,
        })
        .collect()
}

/// Exit code for an inspect run: 65 when the closure walk found a conflict.
///
/// Shared by both inspect commands. `DataError` is the code compose already
/// returns for the identical condition (`DependencyError::Conflict`,
/// `PackageErrorKind::EntrypointCollision`), so `ocx inspect --closure` exits
/// exactly where `ocx run` over the same set would. The conflict detail stays
/// in the payload — the exit code is the machine-readable half, not a
/// replacement for it.
pub fn inspect_exit_code(report: &crate::api::data::package_inspect::InspectReport) -> std::process::ExitCode {
    if report.has_conflicts() {
        ocx_lib::cli::ExitCode::DataError.into()
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// Exit code for `ocx package cascade check`: 65 when any package reported a
/// finding.
///
/// `DataError` is the same code every other "the run succeeded, and what it
/// found is not clean" verdict uses ([`inspect_exit_code`]), so a job that
/// branches on 65 need not know which ocx command produced it. Index staleness
/// counts: check's contract is whole-graph consistency, and the report carries
/// the finding class a script needs to tell it from registry drift.
///
/// Returns the typed code rather than [`std::process::ExitCode`] (which
/// [`inspect_exit_code`] above does): the process type is opaque and compares
/// against nothing, so the verdict would only be assertable by running the
/// binary. The caller converts.
pub fn cascade_check_exit_code(
    report: &crate::api::data::package_cascade_check::PackageCascadeCheck,
) -> ocx_lib::cli::ExitCode {
    if report.reports.iter().any(|report| report.has_findings()) {
        ocx_lib::cli::ExitCode::DataError
    } else {
        ocx_lib::cli::ExitCode::Success
    }
}

/// Exit code for `ocx package cascade repair`: 65 when a finding survived the
/// run.
///
/// Deliberately not [`cascade_check_exit_code`]'s question. Index staleness is
/// the expected residue of a repair — closing it is `ocx package announce`'s
/// hop, and failing on it would make every healthy logical repair look broken.
/// What counts is work this run could have done and did not: a write the
/// registry rejected, an alias refused before it was attempted, and one the
/// fold cannot rebuild without new content being published. A preview counts
/// its whole plan, because a preview writes nothing — a `--dry-run` exiting 0
/// while naming repairs would be useless as a gate.
pub fn cascade_repair_exit_code(
    report: &crate::api::data::package_cascade_repair::PackageCascadeRepair,
) -> ocx_lib::cli::ExitCode {
    let remains = report.entries.iter().any(|entry| {
        !entry.report.unrepairable.is_empty()
            || entry
                .outcomes
                .iter()
                .any(|outcome| !matches!(outcome.outcome, WriteOutcome::Written { .. }))
            || (report.dry_run && !entry.planned.is_empty())
    });
    if remains {
        ocx_lib::cli::ExitCode::DataError
    } else {
        ocx_lib::cli::ExitCode::Success
    }
}

#[cfg(test)]
mod tests {
    use super::{export_ci, merge_tags_file, parse_tags_file, resolve_ci_arg, resolve_shell_arg};
    use ocx_lib::ci::CiFlavor;
    use ocx_lib::cli::UsageError;
    use ocx_lib::package::metadata::env::{entry::Entry, modifier::ModifierKind};
    use ocx_lib::shell::Shell;

    #[test]
    fn shell_arg_absent_is_default_format() {
        assert!(resolve_shell_arg(None).expect("absent is ok").is_none());
    }

    #[test]
    fn shell_arg_explicit_is_passed_through() {
        let resolved = resolve_shell_arg(Some(Some(Shell::Bash))).expect("explicit is ok");
        assert!(matches!(resolved, Some(Shell::Bash)));
    }

    #[test]
    fn ci_arg_absent_is_none() {
        assert!(resolve_ci_arg(None).expect("absent is ok").is_none());
    }

    #[test]
    fn ci_arg_explicit_is_passed_through() {
        // Both providers pass through deterministically (no env reads). The
        // bare-`--ci` autodetect branch reads real CI env vars and is exercised
        // by the acceptance suite, not here (cf. `resolve_shell_arg`).
        assert_eq!(
            resolve_ci_arg(Some(Some(CiFlavor::GitHubActions))).expect("explicit is ok"),
            Some(CiFlavor::GitHubActions)
        );
        assert_eq!(
            resolve_ci_arg(Some(Some(CiFlavor::GitLab))).expect("explicit is ok"),
            Some(CiFlavor::GitLab)
        );
    }

    #[test]
    fn export_ci_github_rejects_export_file() {
        let result = export_ci(
            CiFlavor::GitHubActions,
            Some(std::path::PathBuf::from("/tmp/whatever")),
            &[],
        );
        let error = result.expect_err("github + --export-file must be rejected");
        assert!(
            error.downcast_ref::<UsageError>().is_some(),
            "rejection must be a UsageError (exit 64), got: {error:#}"
        );
    }

    #[test]
    fn export_ci_gitlab_writes_json_lines_to_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let export = tmp.path().join("export.env");
        let entries = vec![Entry {
            key: "JAVA_HOME".to_string(),
            value: "/pkg/java".to_string(),
            kind: ModifierKind::Constant,
        }];

        export_ci(CiFlavor::GitLab, Some(export.clone()), &entries).expect("gitlab export ok");

        let content = std::fs::read_to_string(&export).expect("read export");
        assert_eq!(content, "{\"name\":\"JAVA_HOME\",\"value\":\"/pkg/java\"}\n");
    }

    // ── announce tag-file wire format (design register C2) ──────────────────

    #[test]
    fn parse_tags_file_splits_on_commas_and_newlines_and_trims() {
        let tags = parse_tags_file(b"3.28.1,3.28,3\nlatest\r\n , 1.0.0 ,");
        assert_eq!(
            tags,
            vec![
                "3.28.1".to_string(),
                "3.28".to_string(),
                "3".to_string(),
                "latest".to_string(),
                "1.0.0".to_string(),
            ]
        );
    }

    #[test]
    fn parse_tags_file_is_empty_for_empty_input() {
        assert!(parse_tags_file(b"").is_empty());
    }

    #[test]
    fn merge_tags_file_pushes_the_pushed_tag_and_cascade() {
        let merged = merge_tags_file(&[], &["3.28.1".to_string(), "3.28".to_string(), "3".to_string()]);
        assert_eq!(merged, "3.28.1,3.28,3");
    }

    #[test]
    fn merge_tags_file_dedupes_overlapping_appends_preserving_order() {
        let existing = parse_tags_file(b"3.28.1,3.28,3,latest");
        let merged = merge_tags_file(&existing, &["3.28.2".to_string(), "latest".to_string()]);
        assert_eq!(merged, "3.28.1,3.28,3,latest,3.28.2");
    }

    // ── cascade exit codes ──────────────────────────────────────────────────
    //
    // The two commands answer different questions of the same graph, and the
    // pair that matters most is index staleness: `check` fails on it because
    // its contract is whole-graph consistency, `repair` does not because
    // closing it is `announce`'s hop. Both directions are asserted below so
    // neither code can quietly become the other's.

    mod cascade {
        use ocx_lib::cli::ExitCode;
        use ocx_lib::oci;
        use ocx_lib::package::cascade::apply::{RepairOutcome, WriteOutcome};
        use ocx_lib::package::cascade::graph::{
            AliasState, AliasTag, CascadeReport, IndexFinding, PlannedWrite, SlotRow, SlotStatus, Unrepairable,
        };
        use ocx_lib::package::version::Version;

        use crate::api::data::package_cascade_check::PackageCascadeCheck;
        use crate::api::data::package_cascade_repair::{PackageCascadeRepair, RepairEntry};
        use crate::conventions::{cascade_check_exit_code, cascade_repair_exit_code};

        fn version(text: &str) -> Version {
            Version::parse(text).expect("fixture version parses")
        }

        fn tag(text: &str) -> AliasTag {
            AliasTag::Version(version(text))
        }

        fn digest() -> oci::Digest {
            oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
                .expect("fixture digest parses")
        }

        /// A second digest, so a staleness fixture's committed and live sides
        /// actually differ - one value on both would describe an index that
        /// agrees, which is the opposite of the case under test.
        fn other_digest() -> oci::Digest {
            oci::Digest::try_from("sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
                .expect("fixture digest parses")
        }

        fn report() -> CascadeReport {
            CascadeReport {
                identifier: oci::Identifier::parse("registry.test/acme/cmake").expect("fixture parses"),
                logical: None,
                aliases: [(tag("3.28"), AliasState::Present)].into_iter().collect(),
                rows: Vec::new(),
                index_findings: Vec::new(),
                ignored_tags: Vec::new(),
                unrepairable: Vec::new(),
            }
        }

        fn stale_row() -> SlotRow {
            SlotRow {
                tag: tag("3.28"),
                platform: oci::native::Platform {
                    os: oci::native::Os::Linux,
                    architecture: oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None,
                },
                status: SlotStatus::Stale,
                observed: None,
                expected: None,
                source: None,
                observed_source: None,
            }
        }

        fn planned_write() -> PlannedWrite {
            PlannedWrite {
                tag: tag("3.28"),
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

        fn repair(entry: RepairEntry, dry_run: bool) -> PackageCascadeRepair {
            PackageCascadeRepair {
                entries: vec![entry],
                dry_run,
                announce_tags_path: None,
                index_layer_skipped: Vec::new(),
            }
        }

        fn entry(report: CascadeReport) -> RepairEntry {
            RepairEntry {
                report,
                planned: Vec::new(),
                outcomes: Vec::new(),
                announce_tags: Vec::new(),
            }
        }

        // ── check ───────────────────────────────────────────────────────────

        #[test]
        fn check_on_a_clean_graph_succeeds() {
            let check = PackageCascadeCheck::new(vec![report()]);
            assert_eq!(cascade_check_exit_code(&check), ExitCode::Success);
        }

        #[test]
        fn check_on_a_registry_finding_is_a_data_error() {
            let mut clean = report();
            clean.rows.push(stale_row());
            let check = PackageCascadeCheck::new(vec![clean]);

            assert_eq!(cascade_check_exit_code(&check), ExitCode::DataError);
        }

        #[test]
        fn check_on_index_staleness_alone_is_a_data_error() {
            let mut clean = report();
            clean.index_findings.push(IndexFinding::Stale {
                tag: tag("3.28"),
                committed: digest(),
                live: other_digest(),
            });
            let check = PackageCascadeCheck::new(vec![clean]);

            assert_eq!(
                cascade_check_exit_code(&check),
                ExitCode::DataError,
                "check's contract is the whole graph, index copy included"
            );
        }

        #[test]
        fn check_reports_a_finding_from_any_package_in_the_batch() {
            let mut broken = report();
            broken.rows.push(stale_row());
            let check = PackageCascadeCheck::new(vec![report(), broken]);

            assert_eq!(cascade_check_exit_code(&check), ExitCode::DataError);
        }

        // ── repair ──────────────────────────────────────────────────────────

        #[test]
        fn repair_that_wrote_everything_succeeds() {
            let mut written = entry(report());
            written.planned = vec![planned_write()];
            written.outcomes = vec![RepairOutcome {
                tag: tag("3.28"),
                outcome: WriteOutcome::Written {
                    digest: digest(),
                    verified: true,
                    dropped: Vec::new(),
                },
            }];

            assert_eq!(cascade_repair_exit_code(&repair(written, false)), ExitCode::Success);
        }

        #[test]
        fn repair_leaves_index_staleness_to_announce() {
            let mut stale_index = report();
            stale_index.index_findings.push(IndexFinding::Stale {
                tag: tag("3.28"),
                committed: digest(),
                live: other_digest(),
            });

            assert_eq!(
                cascade_repair_exit_code(&repair(entry(stale_index), false)),
                ExitCode::Success,
                "the index hop is announce's job, not a repair failure"
            );
        }

        #[test]
        fn repair_fails_on_a_rejected_write() {
            let mut failed = entry(report());
            failed.planned = vec![planned_write()];
            failed.outcomes = vec![RepairOutcome {
                tag: tag("3.28"),
                outcome: WriteOutcome::Failed {
                    message: "registry said no".to_string(),
                },
            }];

            assert_eq!(cascade_repair_exit_code(&repair(failed, false)), ExitCode::DataError);
        }

        #[test]
        fn repair_fails_on_a_refused_alias() {
            let mut refused = entry(report());
            refused.planned = vec![planned_write()];
            refused.outcomes = vec![RepairOutcome {
                tag: tag("3.28"),
                outcome: WriteOutcome::Refused(Unrepairable::ChildManifestMissing {
                    tag: tag("3.28"),
                    digest: digest().to_string(),
                }),
            }];

            assert_eq!(cascade_repair_exit_code(&repair(refused, false)), ExitCode::DataError);
        }

        #[test]
        fn repair_fails_on_something_no_write_can_fix() {
            let mut unrepairable = report();
            unrepairable
                .unrepairable
                .push(Unrepairable::WouldEmptyIndex { tag: tag("3.28") });

            assert_eq!(
                cascade_repair_exit_code(&repair(entry(unrepairable), false)),
                ExitCode::DataError,
                "an alias needing new content published is a finding that remains"
            );
        }

        #[test]
        fn a_preview_with_repairs_to_make_is_a_data_error() {
            let mut preview = entry(report());
            preview.planned = vec![planned_write()];

            assert_eq!(
                cascade_repair_exit_code(&repair(preview, true)),
                ExitCode::DataError,
                "a preview writes nothing, so everything it planned still needs doing"
            );
        }

        #[test]
        fn a_preview_with_nothing_to_do_succeeds() {
            assert_eq!(
                cascade_repair_exit_code(&repair(entry(report()), true)),
                ExitCode::Success
            );
        }
    }
}
