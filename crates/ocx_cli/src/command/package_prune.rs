// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package prune` - delete snapshot tags from a registry, guarded by the index.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_announce::forge::ForgeKind;
use ocx_announce::forge::ForgeToken;
use ocx_announce::forge::github_packages::{GithubPackagesDeleter, api_base_for_registry, forge_for_registry};
use ocx_announce::forge::http::build_forge_http_client;
use ocx_package::prune;
use ocx_package::version::Version;

use crate::error::UsageError;
use crate::options;

/// Per-request ceiling for the packages API.
const FORGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const GITLAB_REFUSAL: &str =
    "GitLab's registry does not accept OCX packages, see https://github.com/ocx-sh/ocx/issues/480";

/// Delete tags from a package's registry repository, guarded by the index.
#[derive(Parser)]
#[command(
    override_usage = "ocx package prune [OPTIONS] <PACKAGE> [TAG]...",
    long_about = "\
    Delete tags from a package's registry repository, guarded by the index.\n\n\
    Name the tags to delete, or pass `--prerelease` to select every build of one pre-release \
    (`0.5.0-canary_<build>`) plus its rolling tag (`0.5.0-canary`). A pre-release build \
    cascades only into its own pre-release, so a family never touches `x.y`, `x` or `latest`. \
    `--prerelease` selects from the registry's tag listing, never from the index.\n\n\
    `--keep-builds N` keeps the newest N builds and the rolling tag. Newest is version order, \
    which compares build ids as text, so build ids must be fixed-width: push with \
    `--build-timestamp=datetime`.\n\n\
    `=date` is not enough, since two builds on one day share a tag.\n\n\
    For a namespace with an index, prune reads the package's root from the configured index \
    URL, never a mirror, to find the registry repository. Before any delete it refuses the \
    whole run when a selected tag is listed without the ephemeral marker (exit 81), or is in \
    the registry but not yet in the index (exit 75).\n\n\
    `--force` deletes such tags anyway and is required for a namespace with no index. A tag \
    already gone from the registry is reported as absent.\n\n\
    On the registry API prune deletes registry tags only: never a digest, a platform manifest, \
    a keep tag or a referrer, and it never writes the index. A keep tag pins the manifest past the tag's \
    deletion: push ephemeral builds with `--no-keep-tag`, or prune frees no storage.\n\n\
    A `ghcr.io` registry cannot delete a tag, so prune deletes through the GitHub packages API \
    with the credential ocx uses for that registry. A GitHub Enterprise `containers.<host>` \
    registry needs `--forge github`. When the index supplies the host, that host must be `ghcr.io` \
    or listed exactly in `trusted_hosts` of the package's namespace in `config.toml`, else the run \
    is refused with exit 64. It deletes the version holding the tag, the platform \
    manifests only that version lists, and the referrer tags of those manifests. A version that \
    also carries a tag outside the selection exits 81. An index row naming another build than \
    the registry serves, a kept version whose manifest is not served, and a package listing \
    that changed since planning each refuse the run before any delete with exit 75. A version \
    entry the API returns without an id or name exits 69. `--dry-run` lists the same set and \
    deletes nothing. `--forge gitlab` is not supported.\n\n\
    Pass `--tags-file PATH` to append each gone tag the index lists, then publish the removal \
    with `ocx package announce --tags-file PATH`.\n\n\
    Builds are deleted oldest first and the rolling tag last. After each delete prune confirms \
    the tag is gone with a manifest read (three tries, 250 ms apart). A tag still present \
    exits 75. Deleting a repository's last tag may remove the repository on some \
    registries, which the confirmation accepts.\n\n\
    A delete is not conditional. A push that cascades into the rolling tag at the same moment \
    can lose to it, so serialize the jobs of one track. Tag deletion is optional in the OCI \
    distribution spec. A registry that refuses it fails the first delete with exit 82 before \
    anything is deleted, and 82 is never worth retrying.\n\n\
    The read of the root commits nothing to the local index.\n\n\
    Under `--format json` each tag row carries an `action`: `deleted`, `absent`, `would_delete`, \
    `kept`, `refused` or `not_attempted`. A kept row's `reason` is `newest` or `rolling`. A refused \
    row's `reason` is `durable`, `not_in_index` or `shared_version`.\n\n\
    Rows are in processing order, builds oldest first and the rolling tag last. A run that failed \
    after entering the safeguard still prints this document alone on stdout, and the exit code \
    is the status. A routing failure prints the envelope alone, since the repository is not yet \
    known.\n\n\
    Exits 0 when every selected tag is gone or kept, and 64 on a usage error. Exits 69 when the \
    index or registry fails in a way a rerun will not change. Exits 74 when the tags file \
    cannot be written. Exits 75 when either is briefly unreachable, or a tag is not in the \
    index yet or is still present after its delete.\n\n\
    Exits 78 when the index points at a forbidden host, and 79 when the index has no such \
    package. Exits 80 when the credential cannot delete. Exits 81 for a durable tag, a \
    namespace with no index, or `--offline`. Exits 82 when the registry does not delete tags. \
    A dry run exits 81 or 75 exactly as the real run would, while 80 and 82 surface only on a \
    real delete. On a GitHub package a dry run also reads the packages API, so it can exit 80 \
    there.\n\n\
    Details: <https://ocx.sh/docs/reference/command-line/package-consuming/#package-prune>"
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
    ///
    /// The value must be a pre-release with no build: `0.5.0` and
    /// `0.5.0-canary_<build>` are usage errors. Conflicts with `[TAG]...`.
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

    /// Delete tags the index does not mark ephemeral, or with no index to ask. A forced tag's index row stays until a reviewed announce removes it.
    #[arg(long)]
    force: bool,

    /// Append each gone tag the index lists, one per line, for `ocx package announce --tags-file`.
    /// Creates the file if absent and keeps the tags already in it. Written on every run that
    /// gets past selection, empty included and even when the run then fails partway, never on a
    /// dry run. A forced delete of a tag the index does not list writes nothing.
    #[arg(long = "tags-file", value_name = "PATH")]
    tags_file: Option<PathBuf>,

    /// Delete through this forge's package API, for a registry that has no tag DELETE (`github`).
    ///
    /// Inferred for `ghcr.io` only; a GitHub Enterprise `containers.<host>` registry needs it named. A host
    /// the index supplies must be `ghcr.io` or listed in `trusted_hosts` of the package's namespace.
    /// `gitlab` is refused: GitLab's registry does not accept OCX packages.
    #[arg(long, value_name = "FORGE")]
    forge: Option<ForgeKind>,

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
        self.refuse_gitlab()?;
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
        // A tags file that cannot be written would lose the record of what the delete removed.
        if let (Some(path), false) = (&self.tags_file, self.dry_run) {
            crate::conventions::check_tags_file_writable(path).await?;
        }
        let run = match self.forge_for(&target.repository) {
            Some(ForgeKind::GitHub) => {
                Self::refuse_unvouched_forge_host(
                    target.index.is_some(),
                    target.repository.registry(),
                    &namespace,
                    |ns| context.trusted_hosts_for(ns),
                )?;
                let deleter =
                    Self::github_deleter(&context, client, &target.repository, &self.selection_label()).await?;
                prune::prune_with(client, &deleter, &target, &request).await
            }
            _ => prune::prune(client, &target, &request).await,
        };

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

    /// The forge to delete through: the one named, else the one the registry host implies.
    fn forge_for(&self, repository: &ocx_oci::OciIdentifier) -> Option<ForgeKind> {
        self.forge.or_else(|| forge_for_registry(repository.registry()))
    }

    /// An index-supplied registry host is deleted through the GitHub API only when it is `ghcr.io` or the
    /// operator listed it in `trusted_hosts` of the package's own namespace: the credential goes to the API
    /// that host names. A host the operator typed themselves (no index) may be any `containers.<host>`.
    fn refuse_unvouched_forge_host(
        indexed: bool,
        registry: &str,
        namespace: &str,
        trusted_hosts_for: impl Fn(&str) -> Vec<String>,
    ) -> anyhow::Result<()> {
        if !indexed || forge_for_registry(registry).is_some() || vouched(registry, &trusted_hosts_for(namespace)) {
            return Ok(());
        }
        let (host, _) = ocx_oci::ssrf::split_host_port(registry);
        Err(UsageError::new(format!(
            "the index names the registry {registry}, and an index-supplied host is deleted through the GitHub packages API only on ghcr.io or when listed in [registries.\"{namespace}\"].trusted_hosts; add \"{host}\" there, or name the registry repository on that host directly"
        ))
        .into())
    }

    /// A GitHub Packages deleter for `repository`, authenticated with the credential ocx already resolves
    /// for its registry host; requests go only to the API base derived from that host.
    async fn github_deleter<'a>(
        context: &crate::app::Context,
        client: &'a ocx_oci::Client,
        repository: &ocx_oci::OciIdentifier,
        label: &str,
    ) -> anyhow::Result<GithubPackagesDeleter<'a>> {
        let registry = repository.registry();
        let api_base = match testing_api_base() {
            Some(url) => url,
            None => api_base_for_registry(registry).ok_or_else(|| {
                UsageError::new(format!(
                    "no GitHub API is known for the registry {registry}, use a ghcr.io package, or name a GitHub Enterprise containers.<host> registry with --forge github"
                ))
            })?,
        };
        let (owner, package) = repository
            .repository()
            .split_once('/')
            .ok_or_else(|| UsageError::new("a GitHub package is named owner/package"))?;
        // Refused before any request: an empty bearer header would only earn the forge's 401 and a confusing hint.
        let token = secret_of(ocx_oci::auth::Auth::new().get_or_fallback(registry).await).ok_or_else(|| {
            prune::PruneError::DeleteDenied {
                repository: repository.to_string(),
                tag: label.to_owned(),
                source: ocx_oci::client::error::ClientError::Authentication(Box::new(std::io::Error::other(format!(
                    "no credential is configured for {registry}, which the GitHub packages API needs; run `ocx login {registry}`"
                )))),
            }
        })?;
        let http = build_forge_http_client(FORGE_TIMEOUT, context.extra_roots_merged())?;
        Ok(GithubPackagesDeleter::new(
            client,
            http,
            api_base,
            owner.to_owned(),
            package.to_owned(),
            ForgeToken::new(token),
        ))
    }

    /// Names the selection in an error that has no single tag to point at.
    fn selection_label(&self) -> String {
        self.tags
            .first()
            .cloned()
            .or_else(|| self.prerelease.as_ref().map(ToString::to_string))
            .unwrap_or_default()
    }

    fn refuse_gitlab(&self) -> anyhow::Result<()> {
        if self.forge == Some(ForgeKind::GitLab) {
            return Err(UsageError::new(GITLAB_REFUSAL).into());
        }
        Ok(())
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

/// Whether a `trusted_hosts` entry names `registry`'s host, or its whole `host:port`, exactly.
///
/// Case and one trailing dot are ignored, as DNS ignores them; never a suffix, wildcard or CIDR match,
/// since this vouch decides where the registry credential is sent.
fn vouched(registry: &str, trusted: &[String]) -> bool {
    let normal = |name: &str| name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    let (host, _) = ocx_oci::ssrf::split_host_port(registry);
    let (host, authority) = (normal(host), normal(registry));
    trusted
        .iter()
        .map(|entry| normal(entry))
        .any(|entry| entry == host || entry == authority)
}

/// The secret of a resolved credential, or `None` for anonymous access or an empty secret.
fn secret_of(auth: ocx_oci::native::Auth) -> Option<String> {
    match auth {
        ocx_oci::native::Auth::Basic(_, secret) | ocx_oci::native::Auth::Bearer(secret) => {
            Some(secret).filter(|secret| !secret.is_empty())
        }
        ocx_oci::native::Auth::Anonymous => None,
    }
}

#[cfg(any(test, feature = "__testing"))]
fn testing_api_base() -> Option<String> {
    ocx_env::__OCX_TESTING_GITHUB_API_URL
        .get_raw()
        .and_then(|url| url.into_string().ok())
}

#[cfg(not(any(test, feature = "__testing")))]
fn testing_api_base() -> Option<String> {
    None
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
    fn forge_gitlab_parses_and_is_refused_with_one_line() {
        let matches = parse(&[PACKAGE, "0.5.0", "--forge", "gitlab"]).expect("the flag parses");
        assert_eq!(
            matches.get_one::<super::ForgeKind>("forge"),
            Some(&super::ForgeKind::GitLab)
        );
        let prune = super::PackagePrune {
            prerelease: None,
            keep_builds: None,
            force: false,
            tags_file: None,
            forge: Some(super::ForgeKind::GitLab),
            dry_run: false,
            package: PACKAGE.parse().expect("identifier"),
            tags: vec![],
        };
        let error = prune.refuse_gitlab().expect_err("gitlab is refused");
        assert_eq!(error.to_string(), super::GITLAB_REFUSAL);
    }

    fn prune_with_forge(forge: Option<super::ForgeKind>) -> super::PackagePrune {
        super::PackagePrune {
            prerelease: None,
            keep_builds: None,
            force: false,
            tags_file: None,
            forge,
            dry_run: false,
            package: PACKAGE.parse().expect("identifier"),
            tags: vec!["0.5.0".to_owned()],
        }
    }

    fn location(registry: &str) -> ocx_oci::OciIdentifier {
        ocx_oci::OciIdentifier::from_parts("org/pkg", registry)
    }

    #[test]
    fn the_forge_is_inferred_from_ghcr_only() {
        let prune = prune_with_forge(None);
        assert_eq!(prune.forge_for(&location("ghcr.io")), Some(super::ForgeKind::GitHub));
    }

    #[test]
    fn a_github_enterprise_host_needs_the_forge_named() {
        let host = location("containers.example.com");
        assert_eq!(prune_with_forge(None).forge_for(&host), None);
        assert_eq!(
            prune_with_forge(Some(super::ForgeKind::GitHub)).forge_for(&host),
            Some(super::ForgeKind::GitHub)
        );
    }

    #[test]
    fn no_forge_is_inferred_for_any_other_host() {
        let prune = prune_with_forge(None);
        for host in ["localhost:5000", "registry.example", "docker.io"] {
            assert_eq!(prune.forge_for(&location(host)), None, "{host}");
        }
    }

    #[test]
    fn an_explicit_forge_wins_over_the_host() {
        let prune = prune_with_forge(Some(super::ForgeKind::GitHub));
        assert_eq!(
            prune.forge_for(&location("localhost:5000")),
            Some(super::ForgeKind::GitHub)
        );
    }

    #[test]
    fn a_shared_version_refusal_carries_its_slug_and_the_policy_exit_code() {
        let error = ocx_package::prune::PruneError::SharedVersion {
            repository: "ghcr.io/acme/tool".to_owned(),
            tag: "0.5.0".to_owned(),
            other_tag: "1.0.0".to_owned(),
        };
        assert_eq!(
            crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&error)),
            "prune_shared_version"
        );
        assert_eq!(crate::exit::classify_error(&error), ocx_exit::ExitCode::PolicyBlocked);
    }

    #[test]
    fn only_a_non_empty_secret_is_a_credential() {
        use ocx_oci::native::Auth;
        assert_eq!(super::secret_of(Auth::Anonymous), None);
        assert_eq!(super::secret_of(Auth::Bearer(String::new())), None);
        assert_eq!(super::secret_of(Auth::Bearer("t".into())).as_deref(), Some("t"));
        assert_eq!(
            super::secret_of(Auth::Basic("u".into(), "p".into())).as_deref(),
            Some("p")
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
            "GitHub packages API",
            "--forge github",
            "trusted_hosts",
            "https://ocx.sh/docs/reference/command-line/package-consuming/#package-prune",
        ] {
            assert!(help.contains(needle), "long help must mention {needle}:\n{help}");
        }
        assert!(help.is_ascii(), "help text must stay ASCII");
    }

    /// `trusted_hosts` per namespace: `acme.example` vouches for `containers.ghe.example`, `other.example`
    /// for `containers.evil.example`.
    fn trusted(namespace: &str) -> Vec<String> {
        match namespace {
            "acme.example" => vec!["containers.ghe.example".to_owned()],
            "other.example" => vec!["containers.evil.example".to_owned()],
            _ => Vec::new(),
        }
    }

    fn refuse(indexed: bool, registry: &str, namespace: &str) -> anyhow::Result<()> {
        super::PackagePrune::refuse_unvouched_forge_host(indexed, registry, namespace, trusted)
    }

    fn usage_error(result: anyhow::Result<()>) -> String {
        let error = result.expect_err("refused");
        assert_eq!(
            crate::exit::classify_error(error.as_ref()),
            ocx_exit::ExitCode::UsageError
        );
        error.to_string()
    }

    #[test]
    fn index_supplied_host_other_than_ghcr_is_refused() {
        let message = usage_error(refuse(true, "containers.evil.example", "acme.example"));
        assert!(
            message.contains(r#"[registries."acme.example"].trusted_hosts"#),
            "the refusal names the key to set: {message}"
        );
        assert!(refuse(true, "ghcr.io", "acme.example").is_ok());
        assert!(refuse(true, "GHCR.io", "unlisted.example").is_ok());
        assert!(refuse(false, "containers.ghe.example", "unlisted.example").is_ok());
    }

    #[test]
    fn an_index_supplied_host_listed_in_trusted_hosts_of_its_namespace_is_accepted() {
        assert!(refuse(true, "containers.ghe.example", "acme.example").is_ok());
        assert!(refuse(true, "Containers.GHE.example.", "acme.example").is_ok());
        assert!(refuse(true, "containers.ghe.example:8443", "acme.example").is_ok());
    }

    #[test]
    fn a_host_listed_under_another_namespace_does_not_vouch() {
        usage_error(refuse(true, "containers.evil.example", "acme.example"));
        usage_error(refuse(true, "containers.ghe.example", "other.example"));
    }

    #[test]
    fn only_an_exact_host_vouches_never_a_suffix_or_prefix() {
        for registry in [
            "evil.containers.ghe.example",
            "containers.ghe.example.evil",
            "containers.ghe.exampl",
            "ghe.example",
        ] {
            usage_error(refuse(true, registry, "acme.example"));
        }
        assert!(!super::vouched("containers.ghe.example", &["*.ghe.example".to_owned()]));
        assert!(!super::vouched("containers.ghe.example", &["ghe.example".to_owned()]));
        assert!(!super::vouched("10.0.0.5", &["10.0.0.0/8".to_owned()]));
        assert!(!super::vouched(
            "containers.ghe.example:8443",
            &["containers.ghe.example:9443".to_owned()]
        ));
    }
}
