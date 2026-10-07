// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package prune` - delete snapshot tags from a registry, guarded by the index.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package::prune;
use ocx_package::version::Version;

use crate::options;

/// Delete tags from a package's registry repository, guarded by the index.
#[derive(Parser)]
#[command(
    override_usage = "ocx package prune [OPTIONS] <PACKAGE> [TAG]...",
    long_about = "\
    Delete tags from a package's registry repository, guarded by the index.\n\n\
    Name the tags to delete, or pass `--prerelease` to select every build of one pre-release \
    (`0.5.0-canary_<build>`) plus its rolling tag (`0.5.0-canary`). A pre-release build cascades \
    only into its own pre-release, so a family never touches `x.y`, `x` or `latest`. \
    `--keep-builds N` keeps the newest N builds and the rolling tag. Newest is version order, \
    which compares build ids as text, so build ids must be fixed-width: push with \
    `--build-timestamp=datetime`. `=date` is not enough, since two builds on one day share a tag.\n\n\
    For a namespace with an index, prune reads the package's root from the configured index URL, \
    never a mirror, to find the registry repository. Before any delete it refuses the whole run \
    if a selected tag is listed without the ephemeral marker (exit 81) or is in the registry but \
    not yet in the index (exit 75). `--force` deletes such tags anyway and is required for a \
    namespace with no index. A tag already gone from the registry is reported as absent.\n\n\
    Prune deletes registry tags only: never a digest, a platform manifest, a keep tag or a \
    referrer, and it never writes the index. A keep tag pins the manifest past the tag's deletion: \
    push ephemeral builds with `--no-keep-tag`, or prune frees no storage. Pass `--tags-file PATH` \
    to append each gone tag the index lists, then publish the removal with \
    `ocx package announce --tags-file PATH`.\n\n\
    Exits 0 when every selected tag is gone or kept; 64 on a usage error; 69 when the index or \
    registry fails in a way a rerun will not change; 74 when the tags file cannot be written; 75 \
    when either is briefly unreachable, or a tag is not in the index yet or is still present after \
    its delete; 78 when the index points at a forbidden host; \
    79 when the index has no such package; 80 when the credential cannot delete; 81 for a durable \
    tag, a namespace with no index, or `--offline`; 82 when the registry does not delete tags. \
    A dry run exits 81 or 75 exactly as the real run would; 80 and 82 surface only on a real delete.\n\n\
    Details: <https://ocx.sh/docs/reference/command-line#package-prune>"
)]
// Exactly one selection mode: both is a conflict, neither a missing argument, each exit 64.
#[clap(group(
    clap::ArgGroup::new("selection")
        .args(["tags", "prerelease"])
        .required(true),
))]
// `requires` must name this one-member group, not `prerelease`: clap skips a required arg that
// conflicts with a present one, so `TAG --keep-builds` would parse clean instead of exiting 64.
#[clap(group(clap::ArgGroup::new("family").args(["prerelease"])))]
pub struct PackagePrune {
    /// Select every build of this pre-release and its rolling tag, e.g. `0.5.0-canary`.
    #[arg(long, value_name = "VERSION", value_parser = prune::parse_prerelease_family)]
    prerelease: Option<Version>,

    /// With `--prerelease`: keep the newest N builds and the rolling tag (N >= 1).
    #[arg(
        long = "keep-builds",
        value_name = "N",
        requires = "family",
        value_parser = clap::value_parser!(u32).range(1..),
    )]
    keep_builds: Option<u32>,

    /// Delete tags the index does not mark ephemeral, or with no index to ask.
    #[arg(long)]
    force: bool,

    /// Append each gone tag the index lists, one per line, for `ocx package announce --tags-file`.
    /// Creates the file if absent and keeps the tags already in it. Written on every run that
    /// gets past selection, empty included, never on a dry run.
    #[arg(long = "tags-file", value_name = "PATH")]
    tags_file: Option<PathBuf>,

    /// Run the safeguard and report; delete nothing, write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Package whose tags to delete, e.g. `ocx.acme.example/acme/tool`.
    #[arg(value_name = "PACKAGE")]
    package: options::Identifier,

    /// Delete exactly these tags.
    #[arg(value_name = "TAG", value_parser = prune::parse_tag)]
    tags: Vec<String>,
}

impl PackagePrune {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Before any index or registry read: `--offline` refuses a prune outright.
        let remote_client = context.remote_client()?;
        let package = self.package.as_target(context.default_registry())?;
        let namespace = package.registry().to_string();

        let source = context.canonical_index_source(&namespace)?;
        let target = prune::locate(&package, source.as_ref()).await?;

        // An indexed repository's host comes from the served root, so the client is SSRF-pinned
        // with the namespace's exemption; with no index the package names the registry itself.
        let publisher;
        let client = if target.index.is_some() {
            publisher = context.guarded_publisher(&namespace);
            publisher.client()
        } else {
            remote_client
        };

        let request = prune::PruneRequest {
            selection: self.selection(),
            force: self.force,
            dry_run: self.dry_run,
        };
        let run = prune::prune(client, &target, &request).await;

        context.api().report(&run.outcome)?;
        let written = match (&self.tags_file, run.outcome.tags_file_entries()) {
            (Some(path), Some(entries)) => crate::conventions::append_tags_file(path, &entries).await,
            _ => Ok(()),
        };
        let failure = match (run.error, written) {
            (Some(error), Err(write_error)) => {
                log::warn!(
                    "{}",
                    crate::api::data::sanitize_for_terminal(&format!("{write_error:#}"))
                );
                Some(anyhow::Error::from(error))
            }
            (Some(error), Ok(())) => Some(anyhow::Error::from(error)),
            (None, Err(write_error)) => Some(write_error),
            (None, Ok(())) => None,
        };
        match failure {
            None => Ok(ExitCode::SUCCESS),
            Some(error) => Err(error),
        }
    }

    fn selection(&self) -> prune::PruneSelection {
        match &self.prerelease {
            Some(prerelease) => prune::PruneSelection::Prerelease {
                prerelease: prerelease.clone(),
                keep_builds: self.keep_builds,
            },
            None => prune::PruneSelection::Tags {
                tags: self.tags.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;
    use clap::error::ErrorKind;
    use ocx_package::version::Version;

    const PACKAGE: &str = "ocx.example/acme/tool";

    /// Runs `ocx package prune <args>` through the real command tree and returns the `prune` matches.
    fn parse(args: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
        let argv: Vec<&str> = ["ocx", "package", "prune"]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let matches = crate::app::Cli::command().try_get_matches_from(argv)?;
        let package = matches.subcommand_matches("package").expect("package subcommand");
        Ok(package.subcommand_matches("prune").expect("prune subcommand").clone())
    }

    fn tags(matches: &clap::ArgMatches) -> Vec<String> {
        matches
            .get_many::<String>("tags")
            .map(|tags| tags.cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_prerelease_family_alone_selects_by_structure() {
        let matches = parse(&[PACKAGE, "--prerelease", "0.5.0-canary"]).expect("a pre-release family parses");

        let prerelease = matches.get_one::<Version>("prerelease").expect("prerelease is set");
        assert_eq!(prerelease.to_string(), "0.5.0-canary");
        assert!(tags(&matches).is_empty());
        assert_eq!(matches.get_one::<u32>("keep_builds"), None);
    }

    #[test]
    fn explicit_tags_alone_select_by_name_in_input_order() {
        let matches = parse(&[PACKAGE, "0.5.0-canary_20260101000000", "0.5.0-canary_20260102000000"])
            .expect("explicit tags parse");

        assert_eq!(
            tags(&matches),
            vec!["0.5.0-canary_20260101000000", "0.5.0-canary_20260102000000"]
        );
        assert!(matches.get_one::<Version>("prerelease").is_none());
    }

    #[test]
    fn the_run_flags_and_the_tags_file_are_accepted_together() {
        let matches = parse(&[
            PACKAGE,
            "--prerelease",
            "0.5.0-canary",
            "--keep-builds",
            "3",
            "--force",
            "--dry-run",
            "--tags-file",
            "removed.txt",
        ])
        .expect("every flag parses together");

        assert!(matches.get_flag("force"));
        assert!(matches.get_flag("dry_run"));
        assert_eq!(matches.get_one::<u32>("keep_builds"), Some(&3));
        assert_eq!(
            matches
                .get_one::<std::path::PathBuf>("tags_file")
                .map(|path| path.as_path()),
            Some(std::path::Path::new("removed.txt"))
        );
    }

    #[test]
    fn neither_a_tag_nor_a_prerelease_is_a_usage_error() {
        let error = parse(&[PACKAGE]).expect_err("a run must select something");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_prerelease_together_with_tags_is_a_usage_error() {
        let error = parse(&[PACKAGE, "--prerelease", "0.5.0-canary", "0.5.0-canary_20260101000000"])
            .expect_err("the two selection modes must conflict");

        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn keep_builds_without_a_prerelease_is_a_usage_error() {
        let error = parse(&[PACKAGE, "0.5.0-canary_20260101000000", "--keep-builds", "2"])
            .expect_err("--keep-builds only trims a pre-release family");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn keep_builds_below_one_is_a_usage_error() {
        parse(&[PACKAGE, "--prerelease", "0.5.0-canary", "--keep-builds", "0"])
            .expect_err("keeping zero builds is what omitting the flag already means");
    }

    #[test]
    fn a_prerelease_that_is_not_a_bare_family_is_a_usage_error() {
        for value in [
            "0.5.0",
            "0.5.0-canary_20260101000000",
            "0.5.0-canary+build",
            "not-a-version",
        ] {
            parse(&[PACKAGE, "--prerelease", value]).expect_err(&format!("{value:?} names no pre-release family"));
        }
    }

    #[test]
    fn a_digest_in_place_of_a_tag_is_a_usage_error() {
        let digest = format!("sha256:{}", "a".repeat(64));

        parse(&[PACKAGE, &digest]).expect_err("prune deletes tags, never digests");
    }

    #[tokio::test]
    async fn a_tags_file_in_a_missing_directory_exits_74() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-such-directory").join("removed.txt");

        let error = crate::conventions::append_tags_file(&path, &["0.5.0-canary_20260101000000".to_string()])
            .await
            .expect_err("a file under a missing directory cannot be written");

        assert_eq!(
            crate::exit::classify_error(error.as_ref()),
            ocx_exit::ExitCode::IoError,
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn the_tags_file_keeps_its_tags_and_is_written_when_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("removed.txt");

        crate::conventions::append_tags_file(&path, &[])
            .await
            .expect("an empty write succeeds");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "");

        std::fs::write(&path, "a\n").expect("seed");
        crate::conventions::append_tags_file(&path, &["b".to_string(), "a".to_string()])
            .await
            .expect("a merge succeeds");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "a\nb\n");
    }

    #[test]
    fn the_long_help_carries_the_keep_tag_and_build_id_guidance() {
        let mut command = crate::app::Cli::command();
        let prune = command
            .find_subcommand_mut("package")
            .and_then(|package| package.find_subcommand_mut("prune"))
            .expect("`ocx package prune` must exist in the command tree");
        let help = prune.render_long_help().to_string();

        for needle in [
            "--no-keep-tag",
            "--build-timestamp=datetime",
            "--tags-file",
            "--force",
            "80 and 82 surface only on a real delete",
            "https://ocx.sh/docs/reference/command-line#package-prune",
        ] {
            assert!(help.contains(needle), "long help must mention {needle}:\n{help}");
        }
        assert!(help.is_ascii(), "help text must stay ASCII");
    }
}
