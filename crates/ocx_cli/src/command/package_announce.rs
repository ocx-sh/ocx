// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package announce` — publish an owner-curated tag set into the index.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::forge::{ForgeKind, ForgeToken, RepoCoordinate};
use ocx_lib::{
    announce::{self, AnnounceRequest, AnnounceTarget, TagSelection},
    cli, oci,
    publisher::Publisher,
};

use crate::{api::data::announce::AnnounceReport, app::CommandError, conventions, options};

/// The announce credential. Ambient environment variable only — never stored
/// in the registry credential store.
const OCX_ANNOUNCE_TOKEN: &str = "OCX_ANNOUNCE_TOKEN";

/// The tag-selection flags other than `--tags`. Named once so the
/// mutually-exclusive-and-exactly-one rule is a single list: a fifth mode added
/// to one attribute but not the other would compile, and the gap would only
/// show as a flag silently accepted alongside `--tags`.
const TAG_SELECTION_SIBLINGS_OF_TAGS: [&str; 3] = ["tags_from_file", "tags_from_registry", "refresh"];

/// Observe an owner-curated set of registry tags and publish the rebuilt
/// package entry into the index.
///
/// Reads the currently-committed index entry, re-observes the given tags on
/// the registry, and writes the rebuilt entry to a local directory (`--out`),
/// or opens a pull request against the index repository. The pull request comes
/// from a fork with `--fork`, and from a branch on the index repository itself
/// when `--fork` is omitted, which needs push access there. A run that produces
/// no change reports as unchanged and makes no commit or pull request.
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
        conflicts_with_all = TAG_SELECTION_SIBLINGS_OF_TAGS,
        required_unless_present_any = TAG_SELECTION_SIBLINGS_OF_TAGS,
    )]
    tags: Vec<String>,

    /// Add the tags listed in this file to the already-committed curated set.
    /// The file holds comma- or newline-separated tag names. Never removes a
    /// committed tag; use `--tags` for that.
    #[clap(long = "tags-from-file", value_name = "PATH", conflicts_with_all = ["refresh", "tags_from_registry"])]
    tags_from_file: Option<PathBuf>,

    /// Add every tag the package's registry repository currently holds to the
    /// already-committed curated set. Use it to announce versions that were
    /// published before the package was in the index, or that an earlier
    /// announce missed. Never removes a committed tag, and a yanked tag stays
    /// yanked.
    #[clap(long = "tags-from-registry", conflicts_with = "refresh")]
    tags_from_registry: bool,

    /// Re-observe every already-committed tag, picking up a digest that moved
    /// (e.g. `latest`) without changing which tags are curated.
    #[clap(long = "refresh")]
    refresh: bool,

    /// Write the rebuilt index entry under this directory instead of opening
    /// a pull request. Works without a credential.
    #[clap(long = "out", value_name = "DIRECTORY", conflicts_with = "fork")]
    out: Option<PathBuf>,

    /// Open (or update) the pull request from this fork, as
    /// `[HOST/]NAMESPACE/PROJECT`. Omit it to push the announce branch straight
    /// to `--index-repo` and open the pull request from there, which needs push
    /// access on that repository. Either way the change lands as a pull or merge
    /// request, never a direct commit to the index's default branch.
    #[clap(long = "fork", value_name = "REPOSITORY", conflicts_with = "out")]
    fork: Option<RepoCoordinate>,

    /// Index repository the pull request targets, as
    /// `[HOST/]NAMESPACE/PROJECT`. Give the host for a self-hosted GitHub
    /// Enterprise Server or GitLab instance; omit it for github.com. The
    /// namespace may be a nested GitLab group path.
    #[clap(long = "index-repo", value_name = "REPOSITORY", default_value = "ocx-sh/index")]
    index_repo: RepoCoordinate,

    /// Which forge hosts the index repository. Inferred from the host in
    /// `--index-repo` for github.com and gitlab.com; required for a self-hosted
    /// instance, whose hostname says nothing about which forge runs there.
    #[clap(long = "forge", value_name = "FORGE")]
    forge: Option<ForgeKind>,

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
        } else if self.tags_from_registry {
            TagSelection::FromRegistry
        } else if let Some(path) = &self.tags_from_file {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading tags file {}", path.display()))?;
            TagSelection::UnionFile(conventions::parse_tags_file(&bytes))
        } else {
            TagSelection::Replace(self.tags.clone())
        };

        let target = self.target();

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

        // Argv faults are diagnosed BEFORE the credential check, so a malformed
        // command line reports what is wrong with it (exit 64) rather than
        // reporting a missing token the operator would then go and set only to
        // hit the real error on the next run.
        //
        // The forge is resolved from the index coordinate, never from the fork:
        // a fork always lives on the same instance as the repository it forks,
        // and letting the two disagree would send the credential to one host
        // while addressing repositories on another.
        let kind = ForgeKind::resolve(self.forge, &self.index_repo)?;
        kind.validate_coordinate(&self.index_repo)?;
        if let Some(fork) = &self.fork {
            kind.validate_coordinate(fork)?;
            // The client is built for the index's host, so a fork naming another
            // one was addressed on the index's instance regardless — writing to
            // a repository the operator never named.
            //
            // Compared through `same_host`, never by `Option` equality: an
            // omitted host MEANS the forge's canonical host, so `ocx-sh/index`
            // with `--fork github.com/me/index` names one instance twice and must
            // not be refused. Case folds for the same reason.
            if !kind.same_host(fork, &self.index_repo) {
                let named = |coordinate: &ocx_lib::forge::RepoCoordinate| {
                    coordinate
                        .host
                        .clone()
                        .unwrap_or_else(|| kind.canonical_host().to_string())
                };
                return Err(ocx_lib::forge::ForgeError::ForkHostMismatch {
                    fork_host: named(fork),
                    index_host: named(&self.index_repo),
                }
                .into());
            }
        }

        // A forge is needed for every mode (`--out` reads the committed root
        // over the contents API too); the token is required by every mode that
        // writes, which is every mode except `--out`.
        let token = std::env::var(OCX_ANNOUNCE_TOKEN).ok().filter(|value| !value.is_empty());
        if self.out.is_none() && token.is_none() {
            return Err(CommandError::new(
                format!(
                    "ocx package announce requires the {OCX_ANNOUNCE_TOKEN} environment variable unless --out is given"
                ),
                cli::ExitCode::AuthError,
            )
            .into());
        }
        let forge = kind.client(ForgeToken::new(token.unwrap_or_default()), &self.index_repo)?;

        let publisher = Publisher::new(announce_client(&context, trusted_hosts)?);

        let outcome = announce::announce(&publisher, Some(forge.as_ref()), request).await?;

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

    /// The write target the flag pair selects.
    ///
    /// `--out` and `--fork` are mutually exclusive (clap `conflicts_with`), and
    /// neither given means the announce branch is pushed to `--index-repo`
    /// itself. A method rather than an inline `match` so the mapping — which
    /// decides *which repository gets written to* — is assertable without a forge.
    fn target(&self) -> AnnounceTarget {
        match (&self.out, &self.fork) {
            (Some(directory), _) => AnnounceTarget::Out(directory.clone()),
            (None, Some(coordinate)) => AnnounceTarget::Fork(coordinate.clone()),
            (None, None) => AnnounceTarget::Direct,
        }
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
    let insecure_hosts = context.insecure_hosts().to_vec();
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

    use ocx_lib::announce::AnnounceTarget;

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

    /// The four tag-selection modes, in the argv form each is given.
    const TAG_SELECTION_ARGV: [&[&str]; 4] = [
        &["--tags", "1.0.0"],
        &["--tags-from-file", "tags.txt"],
        &["--tags-from-registry"],
        &["--refresh"],
    ];

    #[test]
    fn tags_selection_is_required() {
        assert!(
            PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--out", "d"]).is_err(),
            "a tag selection is required"
        );
    }

    /// Exactly-one, proven over **every** pair rather than one sampled pair: the
    /// rule is spread across four `conflicts_with*` attributes, so a mode left
    /// out of one of them still compiles and is only observable as a pair that
    /// clap wrongly accepts.
    #[test]
    fn tags_selection_is_mutually_exclusive_over_every_pair() {
        for (index, first) in TAG_SELECTION_ARGV.iter().enumerate() {
            for second in &TAG_SELECTION_ARGV[index + 1..] {
                let mut argv = vec!["announce", "--package", "acme/widget", "--out", "d"];
                argv.extend_from_slice(first);
                argv.extend_from_slice(second);
                assert!(
                    PackageAnnounce::try_parse_from(&argv).is_err(),
                    "{first:?} and {second:?} together must be a clap usage error"
                );
            }
        }
    }

    /// The positive half: each mode on its own parses. Without it the pair test
    /// above would still pass if a flag were misspelled out of existence — every
    /// pair containing it would error for the wrong reason.
    #[test]
    fn every_tags_selection_parses_on_its_own() {
        for selection in &TAG_SELECTION_ARGV {
            let mut argv = vec!["announce", "--package", "acme/widget", "--out", "d"];
            argv.extend_from_slice(selection);
            assert!(
                PackageAnnounce::try_parse_from(&argv).is_ok(),
                "{selection:?} alone must be a valid selection"
            );
        }
    }

    #[test]
    fn tags_from_registry_sets_the_flag() {
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--package",
            "acme/widget",
            "--tags-from-registry",
            "--out",
            "d",
        ])
        .expect("valid invocation parses");
        assert!(args.tags_from_registry);
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

    /// The whole point of the fork-free path: with neither `--out` nor
    /// `--fork`, the announce branch goes to the index repository — NOT to a
    /// fork, and not to a local directory. Asserting on the resolved
    /// `AnnounceTarget` rather than on "clap accepted it" is deliberate: clap
    /// would accept the invocation just as happily if the mapping still built
    /// a `Fork` out of thin air.
    #[test]
    fn omitting_out_and_fork_targets_the_index_repository_itself() {
        let args = PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--tags", "1.0.0"])
            .expect("a target-less invocation is the direct path, not a usage error");
        assert!(
            matches!(args.target(), AnnounceTarget::Direct),
            "no --out and no --fork must resolve to the direct (fork-free) target"
        );
    }

    /// The other two mappings, so the direct case above cannot pass by a
    /// `match` that collapsed everything onto one arm.
    #[test]
    fn out_and_fork_each_resolve_to_their_own_target() {
        let out = PackageAnnounce::try_parse_from([
            "announce",
            "--package",
            "acme/widget",
            "--tags",
            "1.0.0",
            "--out",
            "somewhere",
        ])
        .expect("valid invocation parses");
        assert!(
            matches!(out.target(), AnnounceTarget::Out(directory) if directory == std::path::Path::new("somewhere"))
        );

        let fork = PackageAnnounce::try_parse_from([
            "announce",
            "--package",
            "acme/widget",
            "--tags",
            "1.0.0",
            "--fork",
            "ocx-contrib/index",
        ])
        .expect("valid invocation parses");
        assert!(
            matches!(fork.target(), AnnounceTarget::Fork(coordinate) if coordinate.full_path() == "ocx-contrib/index")
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
    fn fork_parses_a_namespace_and_project() {
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
        assert_eq!(fork.namespace, "ocx-contrib");
        assert_eq!(fork.project, "index");
    }

    #[test]
    fn index_repo_defaults_to_ocx_sh_index() {
        let args =
            PackageAnnounce::try_parse_from(["announce", "--package", "acme/widget", "--tags", "1.0.0", "--out", "d"])
                .expect("valid invocation parses");
        assert_eq!(args.index_repo.host, None);
        assert_eq!(args.index_repo.namespace, "ocx-sh");
        assert_eq!(args.index_repo.project, "index");
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
