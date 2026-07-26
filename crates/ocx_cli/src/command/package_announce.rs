// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package announce` — publish an owner-curated tag set into the index.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::forge::{ForgeToken, GitHubForge, RepoCoordinate};
use ocx_lib::{
    announce::{self, AnnounceRequest, AnnounceTarget, TagSelection},
    cli, oci,
    publisher::Publisher,
};

use crate::{api::data::announce::AnnounceReport, app::CommandError, conventions, options};

/// The announce credential. Ambient environment variable only — never stored
/// in the registry credential store.
const OCX_ANNOUNCE_TOKEN: &str = "OCX_ANNOUNCE_TOKEN";

/// Observe an owner-curated set of registry tags and publish the rebuilt
/// package entry into the index.
///
/// Reads the currently-committed index entry, re-observes the given tags on
/// the registry, and either writes the rebuilt entry to a local directory
/// (`--out`) or opens a pull request against a fork of the index repository
/// (`--fork`). A run that produces no change reports as unchanged and makes
/// no commit or pull request.
///
/// Opening a pull request needs a GitHub credential in the `OCX_ANNOUNCE_TOKEN`
/// environment variable; writing to `--out` works without one.
#[derive(Parser)]
pub struct PackageAnnounce {
    /// Package to announce, as `<namespace>/<package>` (e.g. `acme/widget`).
    #[clap(long = "package", required = true)]
    package: options::Identifier,

    /// Replace the curated tag set with this comma-separated list. A
    /// currently-committed tag that is not named here is dropped. So is a
    /// reserved tag named here: a canonical `sha256.<hex>` tag or an `__ocx`
    /// one is not a version, so the run still succeeds and reports the drops.
    #[clap(
        long = "tags",
        value_name = "TAGS",
        value_delimiter = ',',
        conflicts_with_all = ["tags_file", "refresh"],
        required_unless_present_any = ["tags_file", "refresh"],
    )]
    tags: Vec<String>,

    /// Add the tags listed in this file to the already-committed curated set.
    /// The file holds comma- or newline-separated tag names. Never removes a
    /// committed tag; use `--tags` for that.
    #[clap(long = "tags-file", value_name = "PATH", conflicts_with = "refresh")]
    tags_file: Option<PathBuf>,

    /// Re-observe every already-committed tag, picking up a digest that moved
    /// (e.g. `latest`) without changing which tags are curated.
    #[clap(long = "refresh")]
    refresh: bool,

    /// Write the rebuilt index entry under this directory instead of opening
    /// a pull request.
    #[clap(long = "out", value_name = "DIRECTORY", conflicts_with = "fork")]
    out: Option<PathBuf>,

    /// Open (or update) a pull request against this fork, as `<owner>/<repo>`.
    /// Requires `OCX_ANNOUNCE_TOKEN`.
    #[clap(
        long = "fork",
        value_name = "OWNER/REPO",
        conflicts_with = "out",
        required_unless_present = "out"
    )]
    fork: Option<RepoCoordinate>,

    /// Index repository the pull request targets, as `<owner>/<repo>`.
    #[clap(long = "index-repo", value_name = "OWNER/REPO", default_value = "ocx-sh/index")]
    index_repo: RepoCoordinate,

    /// Mark a tag as yanked. Repeat for multiple tags. Requires
    /// `--yank-reason`; only applies to a tag already in the curated set.
    #[clap(long = "yank", value_name = "TAG", requires = "yank_reason")]
    yank: Vec<String>,

    /// Clear the yanked marker from a tag. Repeat for multiple tags.
    #[clap(long = "unyank", value_name = "TAG")]
    unyank: Vec<String>,

    /// Reason recorded on every tag named by `--yank` in this run.
    #[clap(long = "yank-reason", value_name = "TEXT")]
    yank_reason: Option<String>,
}

impl PackageAnnounce {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let package = self.package.with_domain(context.default_registry())?;

        let curated = if self.refresh {
            TagSelection::Refresh
        } else if let Some(path) = &self.tags_file {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading tags file {}", path.display()))?;
            TagSelection::UnionFile(conventions::parse_tags_file(&bytes))
        } else {
            TagSelection::Replace(self.tags.clone())
        };

        let target = if let Some(directory) = &self.out {
            AnnounceTarget::Out(directory.clone())
        } else {
            // clap's `conflicts_with`/`required_unless_present` pair on
            // `--out`/`--fork` guarantees exactly one is present.
            let coordinate = self
                .fork
                .clone()
                .expect("clap guarantees --fork is present when --out is absent");
            AnnounceTarget::Fork(coordinate)
        };

        // The SSRF escape hatch is sourced exclusively from the selected
        // `[registries."<ns>"]` entry for the package's namespace — the same
        // config source the index read path resolves through. There is no
        // CLI flag to widen it (design register X2).
        let trusted_hosts = context
            .config()
            .registries
            .as_ref()
            .and_then(|registries| registries.get(package.registry()))
            .and_then(|entry| entry.trusted_hosts.clone())
            .unwrap_or_default();

        let request = AnnounceRequest {
            package: package.clone(),
            curated,
            target,
            index_repo: self.index_repo.clone(),
            yank: self.yank.clone(),
            unyank: self.unyank.clone(),
            yank_reason: self.yank_reason.clone().unwrap_or_default(),
            trusted_hosts: trusted_hosts.clone(),
        };

        // A forge is needed for both modes (`--out` reads the committed root
        // over the contents API too); the token is only required for `--fork`.
        let token = std::env::var(OCX_ANNOUNCE_TOKEN).ok().filter(|value| !value.is_empty());
        if self.fork.is_some() && token.is_none() {
            return Err(CommandError::new(
                format!("ocx package announce --fork requires the {OCX_ANNOUNCE_TOKEN} environment variable"),
                cli::ExitCode::AuthError,
            )
            .into());
        }
        let forge = GitHubForge::new(ForgeToken::new(token.unwrap_or_default()))?;

        let publisher = Publisher::new(announce_client(&context, trusted_hosts)?);

        let outcome = announce::announce(&publisher, Some(&forge), request).await?;

        // Reserved tags are dropped, not refused — the run succeeded, so the
        // notice is a diagnostic on stderr and the drops also ride out in the
        // report on stdout.
        if !outcome.reserved_tags_dropped.is_empty() {
            context.ui().warn(format!(
                "not a version, dropped from the curated set: {}",
                outcome.reserved_tags_dropped.join(", ")
            ));
        }

        context.api().report(&AnnounceReport::from_outcome(outcome))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Builds the OCI client the announce `Publisher` observes tags with, pinned
/// through the same [`oci::ssrf::GuardedResolver`](ocx_lib::oci::ssrf::GuardedResolver)
/// seam the index read path uses (`ClientBuilder::ssrf_guard`) — the physical
/// registry a curated tag resolves against is remote-controlled data (a root
/// `repository` pointer), so the connect-time pin must be wired here too, not
/// only the pre-flight `resolve_and_validate` the announce pipeline already
/// runs. Mirrors and reuses the same mirror-map / plain-HTTP resolution the
/// CLI's own remote client goes through.
fn announce_client(context: &crate::app::Context, trusted_hosts: Vec<String>) -> anyhow::Result<oci::Client> {
    let insecure_hosts = ocx_lib::env::insecure_registries();
    let resolved_mirrors = ocx_lib::resolve_mirror_map(context.config(), ocx_lib::env::mirrors()?, &insecure_hosts)?;
    let mirrors = oci::MirrorMap::new(resolved_mirrors.registry);
    Ok(oci::ClientBuilder::new()
        .plain_http_registries(insecure_hosts)
        .mirrors(mirrors)
        .progress(context.progress().clone())
        .ssrf_guard(trusted_hosts)
        .build())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::PackageAnnounce;

    // ── clap surface ──────────────────────────────────────────────────────

    #[test]
    fn package_is_required() {
        assert!(
            PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d"]).is_err(),
            "missing --package must be a clap usage error"
        );
    }

    #[test]
    fn package_parses_namespace_and_name() {
        let args =
            PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--tags", "1.0.0", "--out", "d"])
                .expect("valid invocation parses");
        assert_eq!(args.package.raw(), "acme/widget");
    }

    #[test]
    fn tags_selection_is_required() {
        assert!(
            PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--out", "d"]).is_err(),
            "at least one of --tags/--tags-file/--refresh is required"
        );
    }

    #[test]
    fn tags_selection_is_mutually_exclusive() {
        assert!(
            PackageAnnounce::try_parse_from([
                "announce",
                "--package",
                "acme/widget",
                "--tags",
                "1.0.0",
                "--refresh",
                "--out",
                "d",
            ])
            .is_err(),
            "--tags and --refresh together must be a clap usage error"
        );
    }

    #[test]
    fn tags_splits_on_commas() {
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--package",
            "acme/widget",
            "--tags",
            "1.0.0,2.0.0",
            "--out",
            "d",
        ])
        .expect("valid invocation parses");
        assert_eq!(args.tags, vec!["1.0.0".to_string(), "2.0.0".to_string()]);
    }

    #[test]
    fn target_selection_is_required() {
        assert!(
            PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--tags", "1.0.0"]).is_err(),
            "at least one of --out/--fork is required"
        );
    }

    #[test]
    fn target_selection_is_mutually_exclusive() {
        assert!(
            PackageAnnounce::try_parse_from([
                "announce",
                "--package",
                "acme/widget",
                "--tags",
                "1.0.0",
                "--out",
                "d",
                "--fork",
                "o/r",
            ])
            .is_err(),
            "--out and --fork together must be a clap usage error"
        );
    }

    #[test]
    fn fork_parses_owner_repo() {
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--package",
            "acme/widget",
            "--tags",
            "1.0.0",
            "--fork",
            "ocx-contrib/index",
        ])
        .expect("valid invocation parses");
        let fork = args.fork.expect("--fork given");
        assert_eq!(fork.owner, "ocx-contrib");
        assert_eq!(fork.repo, "index");
    }

    #[test]
    fn index_repo_defaults_to_ocx_sh_index() {
        let args =
            PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--tags", "1.0.0", "--out", "d"])
                .expect("valid invocation parses");
        assert_eq!(args.index_repo.owner, "ocx-sh");
        assert_eq!(args.index_repo.repo, "index");
    }

    #[test]
    fn yank_requires_yank_reason() {
        assert!(
            PackageAnnounce::try_parse_from([
                "announce",
                "--package",
                "acme/widget",
                "--tags",
                "1.0.0",
                "--out",
                "d",
                "--yank",
                "0.9.0",
            ])
            .is_err(),
            "--yank without --yank-reason must be a clap usage error"
        );
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--package",
            "acme/widget",
            "--tags",
            "1.0.0",
            "--out",
            "d",
            "--yank",
            "0.9.0",
            "--yank-reason",
            "security",
        ])
        .expect("--yank with --yank-reason parses");
        assert_eq!(args.yank, vec!["0.9.0".to_string()]);
        assert_eq!(args.yank_reason.as_deref(), Some("security"));
    }

    /// X2 negative test: the SSRF exemption is config-only (`[registries."<ns>"].trusted_hosts`)
    /// — there must be no CLI flag that could widen a locked source's trust
    /// set. Walks the built `clap::Command` looking for a `trusted-host`
    /// long flag by name, rather than trusting a hand exhaustive `--help`
    /// read.
    #[test]
    fn no_trusted_host_flag_is_registered() {
        let command = PackageAnnounce::command();
        let has_trusted_host_flag = command
            .get_arguments()
            .any(|arg| arg.get_long() == Some("trusted-host"));
        assert!(
            !has_trusted_host_flag,
            "ocx package announce must not expose a --trusted-host flag (design register X2)"
        );
    }
}
