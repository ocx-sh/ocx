// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_console::UserInterface;
use ocx_exit::ExitCode;

use crate::command::deprecated;
use crate::error::UsageError;
use ocx_announce::forge::{ForgeCredentials, ForgeError, ForgeKind, RepoCoordinate, WriteTransport};

use crate::app::CommandError;

/// Why a GitLab job pushing with `OCX_ANNOUNCE_TOKEN` authors the merge request
/// as that token's owner rather than as the pipeline.
const PUSH_IDENTITY_NOTICE: &str = "in a GitLab job, --transport git pushes with the OCX_ANNOUNCE_TOKEN credential (push credential kind token), not this job's CI_JOB_TOKEN: the merge request is authored by that token's owner rather than by the pipeline, and ocx cannot name them; set OCX_ANNOUNCE_GIT_TOKEN to push with another credential";

/// The write-target flags every forge-writing command shares.
///
/// Arg ids: `index_repo`, `forge`, `transport`, `fork`, `output`, `deprecated_out`.
#[derive(clap::Args, Clone, Debug)]
pub struct ForgeWriteOptions {
    /// Index repository the request targets, as `[HOST/]NAMESPACE/PROJECT`.
    ///
    /// Give the host for a self-hosted GitHub Enterprise Server or GitLab
    /// instance; omit it for github.com. The namespace may be a nested GitLab
    /// group path. Defaults to `ocx-sh/index`.
    //
    // Keep the default: the field is not an `Option`, so dropping it makes `--index-repo` required.
    #[clap(long = "index-repo", value_name = "REPOSITORY", default_value = "ocx-sh/index")]
    pub index_repo: RepoCoordinate,

    /// Which forge hosts the index repository.
    ///
    /// Inferred from the host in `--index-repo` for github.com and gitlab.com;
    /// required for a self-hosted instance, whose hostname says nothing about
    /// which forge runs there.
    #[clap(long = "forge", value_name = "FORGE")]
    pub forge: Option<ForgeKind>,

    /// How the request is written.
    ///
    /// `api` opens it through the forge's REST API. `git` clones the index
    /// repository and creates the request from a single authenticated push,
    /// which is the only way a GitLab CI job token can open a merge request:
    /// that credential can push and can read the API, but cannot open a merge
    /// request through it. Defaults to `api`, and `git` is GitLab-only.
    //
    // Keep the default, as for `--index-repo`.
    #[clap(long = "transport", value_name = "TRANSPORT", default_value_t = WriteTransport::default())]
    pub transport: WriteTransport,

    /// Open (or update) the request from this fork, as
    /// `[HOST/]NAMESPACE/PROJECT`.
    ///
    /// Omit it to push the branch straight to `--index-repo` and open the
    /// request from there, which needs push access on that repository.
    /// Whichever of the two you choose, the change lands as a pull or merge
    /// request, never as a direct commit to the index's default branch.
    /// `--output` writes locally instead and opens neither.
    #[clap(long = "fork", value_name = "REPOSITORY", conflicts_with_all = ["output", deprecated::ANNOUNCE_OUT.arg_id()])]
    pub fork: Option<RepoCoordinate>,

    /// Write the rendered index entry under this directory instead of opening
    /// a request. Works without a credential.
    //
    // The `--fork` conflict lives on both fields: the flattening command has no field to hang it on.
    #[clap(long = "output", short = 'o', value_name = "DIRECTORY", conflicts_with = "fork")]
    output: Option<PathBuf>,

    // 0.7 removal: the `--out` spelling of `--output`.
    #[clap(id = deprecated::ANNOUNCE_OUT.arg_id(), long = "out", value_name = "DIRECTORY", hide = true, conflicts_with_all = ["output", "fork"])]
    deprecated_out: Option<PathBuf>,
}

impl ForgeWriteOptions {
    /// The directory `--output` names, under either spelling.
    #[must_use]
    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref().or(self.deprecated_out.as_deref())
    }

    /// Refuses every argv fault decidable from these five flags alone and
    /// returns the resolved [`ForgeKind`]; call it before any credential resolves.
    ///
    /// # Errors
    ///
    /// Every refusal classifies to exit 64.
    // Every flattening command must call this before building a forge client: `ForgeKind::client`
    // repeats only `validate_transport`, so a missed call loses the host and coordinate refusals.
    pub fn validate(&self) -> anyhow::Result<ForgeKind> {
        // First, or `--transport git` on the default github.com index reports the forge, not the flag.
        if self.transport == WriteTransport::Git {
            if self.fork.is_some() {
                return Err(UsageError::new(
                    "--transport git cannot be combined with --fork; the git transport writes the claim branch to --index-repo itself",
                )
                .into());
            }
            if self.output().is_some() {
                return Err(UsageError::new(
                    "--transport git cannot be combined with --output; --output writes the entry locally and opens no request",
                )
                .into());
            }
        }

        // From the index coordinate, never the fork, or the credential goes to one host while
        // addressing repositories on another.
        let kind = ForgeKind::resolve(self.forge, &self.index_repo)?;
        kind.validate_coordinate(&self.index_repo)?;
        if let Some(fork) = &self.fork {
            kind.validate_coordinate(fork)?;
            // `same_host`, never `Option` equality, or `ocx-sh/index` with `--fork github.com/me/index`
            // is refused though an omitted host means the canonical one.
            if !kind.same_host(fork, &self.index_repo) {
                let named = |coordinate: &RepoCoordinate| {
                    coordinate
                        .host
                        .clone()
                        .unwrap_or_else(|| kind.canonical_host().to_string())
                };
                return Err(ForgeError::ForkHostMismatch {
                    fork_host: named(fork),
                    index_host: named(&self.index_repo),
                }
                .into());
            }
        }
        kind.validate_transport(self.transport)?;
        Ok(kind)
    }

    /// Whether the run needs a local `git`; gate the `git --version` probe on
    /// this alone, or an `api` write fails on a host that never needed `git`.
    #[must_use]
    pub fn needs_git(&self) -> bool {
        self.transport == WriteTransport::Git
    }

    /// A write mode with no resolved API credential is exit 80, naming
    /// `OCX_ANNOUNCE_TOKEN`; `--output` proceeds unauthenticated.
    ///
    /// Takes the resolved [`ForgeCredentials`], never the environment, or a
    /// GitLab job the ladder answers with `CI_JOB_TOKEN` is refused.
    ///
    /// # Errors
    ///
    /// [`crate::app::CommandError`] at [`ocx_exit::ExitCode::AuthError`].
    pub fn require_credential(&self, credentials: &ForgeCredentials) -> anyhow::Result<()> {
        if self.output().is_some() || credentials.api_is_present() {
            return Ok(());
        }
        Err(CommandError::new(
            // The declaration's name, never a local copy, so the refusal names the variable the ladder reads.
            format!(
                "a forge write needs a credential in {}; use --output to write the entry locally instead",
                ocx_env::OCX_ANNOUNCE_TOKEN.declaration().name
            ),
            ExitCode::AuthError,
        )
        .into())
    }

    /// Warns on stderr that the git push authenticates as the token's owner, not
    /// the pipeline; call after `require_credential`, before building the forge.
    ///
    /// Never prints the HTTP Basic username: here it is `gitlab-ci-token`, which
    /// would name the job as the author.
    pub fn warn_push_identity(&self, ui: &UserInterface, credentials: &ForgeCredentials) {
        if self.transport == WriteTransport::Git
            // Outside a job no job token was available, so this would warn a run behaving as asked.
            && credentials.in_gitlab_ci()
            && !credentials.push_is_explicit()
            && !credentials.push_is_job_token()
            // The notice names the push credential, so one must exist; `None` only after an earlier
            // refusal, so no test reds this conjunct.
            && credentials.push().is_some()
        {
            // The notice's `token` must match `push_credential_kind`'s wire spelling.
            ui.warn(PUSH_IDENTITY_NOTICE);
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{Args as _, CommandFactory as _, Parser as _};

    use ocx_announce::forge::{ForgeCredentials, ForgeToken, WriteTransport};
    use ocx_exit::ExitCode;

    use super::ForgeWriteOptions;
    use crate::command::package_claim::PackageClaim;
    use crate::exit::classify_error;

    /// A minimal command carrying nothing but the flatten, so the shared
    /// grammar can be parsed without any one command's own flags.
    #[derive(clap::Parser)]
    struct Probe {
        #[command(flatten)]
        forge: ForgeWriteOptions,
    }

    fn probe(argv: &[&str]) -> Result<ForgeWriteOptions, clap::Error> {
        let mut full = vec!["probe"];
        full.extend_from_slice(argv);
        Probe::try_parse_from(full).map(|parsed| parsed.forge)
    }

    /// Every long flag `ForgeWriteOptions` contributes, derived from the struct
    /// rather than hardcoded.
    ///
    /// No filter for clap's own `help`/`version`: `augment_args` on a `Command`
    /// that was never `build()`-ed does not materialize them (measured against
    /// clap 4.6), and if a later clap did, the exact-equality assertion below
    /// reds loudly — which is the outcome a filter would have suppressed.
    fn derived_shared_flags() -> Vec<String> {
        let command = ForgeWriteOptions::augment_args(clap::Command::new("probe"));
        command
            .get_arguments()
            .filter_map(clap::Arg::get_long)
            .map(str::to_string)
            .collect()
    }

    /// `ocx package claim` exposes the whole shared grammar, and the expected
    /// set is **derived** from the struct.
    ///
    /// A hardcoded list never learns about a newly added flag, which is exactly
    /// the drift this exists to catch. The derived set is asserted non-empty so
    /// a refactor that empties the struct reds rather than passing vacuously.
    /// **Proved**: with every field deleted this reds on the non-empty line —
    /// clap 4.6 adds no argument of its own to an un-`build()`-ed `Command`, so
    /// nothing else can be holding the guard up.
    ///
    /// Membership is asserted by **id**, not by count: a count assertion passes
    /// when `claim` declares five different flags. The set is also asserted
    /// exactly, so nothing claim-specific (`--repository`, `--owner`) can drift
    /// into the shared struct and land on announce as a flag that command has
    /// no use for.
    ///
    /// The both-commands parity half belongs to the announce flatten's own
    /// edit; a parity test owned here would be red at this package's own merge
    /// gate by construction.
    ///
    /// **Green on arrival** — the struct is complete at the stub, so this is a
    /// characterization test of the shared-grammar shape.
    /// Mutation: delete every field from `ForgeWriteOptions` (the non-empty
    /// guard reds); rename one flag on `PackageClaim` without renaming it here
    /// (the membership assertion reds).
    #[test]
    fn shared_forge_write_options_derived_set_matches_claim() {
        let derived = derived_shared_flags();
        assert!(
            !derived.is_empty(),
            "the shared write grammar must not be empty — an emptied struct is a silent removal of every flag"
        );
        assert_eq!(
            derived,
            vec!["index-repo", "forge", "transport", "fork", "output", "out"],
            "the five shared flags plus the deprecated `--out`, and nothing command-specific"
        );

        let claim = PackageClaim::command();
        let claim_flags: Vec<&str> = claim.get_arguments().filter_map(clap::Arg::get_long).collect();
        for flag in &derived {
            assert!(
                claim_flags.contains(&flag.as_str()),
                "ocx package claim must expose the shared flag --{flag}"
            );
        }
    }

    /// `--output` ⟂ `--fork` is declared on the shared struct, so both write
    /// commands inherit one rule.
    ///
    /// Asserted through the flatten alone rather than through a command, so
    /// the inheritance is real.
    ///
    /// Red at the stub: no `conflicts_with` is declared, so the parse succeeds.
    /// Mutation once implemented: delete **both** `conflicts_with` attributes.
    /// Deleting either one alone leaves this green — clap's conflict check is
    /// symmetric (`Conflicts::gather_conflicts` tests each argument's blacklist
    /// against the other's), so one surviving attribute still refuses the pair.
    /// Keeping both is correct anyway: two independent guards over one property
    /// is the case `quality-core.md` § Unchecked Green describes, not a weak
    /// check.
    #[test]
    fn out_and_fork_conflict_is_declared_on_the_shared_struct() {
        assert!(
            probe(&["--output", "d", "--fork", "o/r"]).is_err(),
            "--output and --fork together must be a clap usage error, declared on the flatten"
        );
        assert!(probe(&["--output", "d"]).is_ok(), "--output alone parses");
        assert!(
            probe(&["--out", "d", "--fork", "o/r"]).is_err(),
            "the deprecated --out conflicts with --fork too"
        );
        assert!(
            probe(&["--out", "d", "--output", "e"]).is_err(),
            "both spellings together are refused"
        );
        let deprecated = probe(&["--out", "d"]).expect("the deprecated --out still parses");
        assert_eq!(deprecated.output(), Some(std::path::Path::new("d")));
        assert!(probe(&["--fork", "o/r"]).is_ok(), "--fork alone parses");
    }

    /// `--transport` is backed by `WriteTransport`'s own `ValueEnum`, so the
    /// flag's accepted values and the report's rendered value are one
    /// vocabulary.
    ///
    /// The default is asserted in the same function: a `String`-typed flag would
    /// accept `--transport gti` at parse time and fail somewhere later, with a
    /// message about a transport rather than about a typo.
    ///
    /// **Green on arrival** — the stub already declares the `ValueEnum` and the
    /// default.
    /// Mutation: declare `transport: String`; the `gti` row reds.
    #[test]
    fn transport_defaults_to_api_and_accepts_only_the_two_spellings() {
        assert_eq!(
            probe(&[]).expect("a transport-less invocation parses").transport,
            WriteTransport::Api,
            "--transport defaults to api"
        );
        assert_eq!(
            probe(&["--transport", "git"]).expect("git parses").transport,
            WriteTransport::Git
        );
        assert!(
            probe(&["--transport", "gti"]).is_err(),
            "a misspelled transport is refused at parse time, not later"
        );
    }

    /// The `git --version` gate runs **iff** `--transport git`.
    ///
    /// The only property of this a unit test can hold. `ForgeKind::client`
    /// raises the same exit 69 when the binary is missing, so an unconditional
    /// probe is invisible in the exit code — it shows up only as an ordinary
    /// `api` claim failing on a host that has no `git` and never needed one,
    /// which no CI runner reproduces. The version cases and the end-to-end
    /// 69s are exercised elsewhere.
    ///
    /// Red at the stub: `needs_git` is `unimplemented!()`.
    /// Mutation once implemented: return `true` unconditionally.
    #[test]
    fn git_probe_runs_only_under_the_git_transport() {
        assert!(
            !probe(&[]).expect("parses").needs_git(),
            "an api claim must not probe for git"
        );
        assert!(
            probe(&["--transport", "git"]).expect("parses").needs_git(),
            "the git transport needs a git binary before the forge is built"
        );
    }

    /// The exit-80 refusal is keyed on the credential the **ladder** resolved,
    /// and `--output` is exempt.
    ///
    /// This is the CLI-boundary half of that guard; the ladder itself is
    /// already tested where it lives
    /// (`the_api_ladder_prefers_a_non_empty_ocx_token_over_the_job_token`
    /// asserts `OCX_ANNOUNCE_TOKEN=""` falls through to `CI_JOB_TOKEN`), so
    /// re-asserting the fall-through here would be a second green over the same
    /// code. What only this boundary can observe is that the refusal reads the
    /// resolved credential rather than the environment.
    ///
    /// The two halves kill the environment-read mutation under **any** ambient
    /// environment, which is why both are here: with
    /// `std::env::var(OCX_ANNOUNCE_TOKEN)` substituted, the `resolved` row reds
    /// in a process where that variable is unset, and the `empty` row reds in
    /// one where it is set. Correct code passes both regardless, so the test is
    /// deterministic for it.
    ///
    /// **Residual, recorded rather than hidden.** *Which transport the CLI
    /// hands `ForgeCredentials::resolve`* is **not** asserted at any scope in
    /// this package, so hardcoding `WriteTransport::Api` at
    /// `package_claim.rs`'s `resolve` call reds nothing here. No red is
    /// reachable at unit scope: `ocx_cli`'s test binary links a non-`cfg(test)`
    /// `ocx_lib`, so `crate::utility::env::var`'s override map cannot be
    /// reached from here and the job-token rung cannot be staged without
    /// mutating the real process environment. The control that observes the
    /// resolved credential end to end is
    /// `::test_job_token_pickup_headers_and_push_user`.
    ///
    /// Red at the stub: `require_credential` is `unimplemented!()`.
    /// Mutation once implemented: key the refusal on
    /// `std::env::var(OCX_ANNOUNCE_TOKEN)` (the shape `package_announce.rs`
    /// carried before this migration); or drop the `--output` carve-out.
    #[test]
    fn empty_token_resolves_the_job_token_at_the_cli_boundary() {
        let resolved = ForgeCredentials::new(ForgeToken::new("resolved-by-the-ladder".to_string()));
        let nothing = ForgeCredentials::new(ForgeToken::new(String::new()));

        let write = probe(&[]).expect("parses");
        write
            .require_credential(&resolved)
            .expect("a credential the ladder resolved authorizes a write, whichever rung answered");

        let error = write
            .require_credential(&nothing)
            .expect_err("a write mode with no resolved credential is refused");
        assert_eq!(
            classify_error(error.as_ref()),
            ExitCode::AuthError,
            "the terminal rung with no resolved credential is exit 80"
        );
        assert!(
            error.to_string().contains("OCX_ANNOUNCE_TOKEN"),
            "the refusal names the variable to set, got: {error}"
        );

        let out = probe(&["--output", "d"]).expect("parses");
        out.require_credential(&nothing)
            .expect("--output reads the forge but writes nothing, so it proceeds unauthenticated");
    }

    /// The push-credential-kind vocabulary reaches stderr and the report with
    /// one spelling.
    ///
    /// [`PUSH_IDENTITY_NOTICE`] writes the word out rather than rendering it,
    /// because the guard above the emit proves which arm applies. That is fine
    /// only while the two agree, and `push_credential_kind_wire_spellings` pins
    /// the **enum** alone — so if the vocabulary moved, that test and the
    /// acceptance assertion (itself a literal) would move with it and the
    /// notice would drift silently.
    ///
    /// Mutation: change `PushCredentialKind::Token`'s serialized spelling (an
    /// arm-level `#[serde(rename)]`), or the word in the constant; this reds.
    #[test]
    fn the_push_identity_notice_names_the_serialized_push_credential_kind() {
        let wire = serde_json::to_string(&crate::api::data::forge_report::PushCredentialKind::Token)
            .expect("a fieldless enum serializes");
        let spelling = wire.trim_matches('"');
        assert!(
            super::PUSH_IDENTITY_NOTICE.contains(&format!("push credential kind {spelling}")),
            "the notice must name the kind the report renders ({spelling}): {}",
            super::PUSH_IDENTITY_NOTICE
        );
    }

    /// The notice names the credential's **source**, never a person it cannot
    /// observe.
    ///
    /// The tempting way to make a warning about authorship concrete is to print
    /// the identity the push authenticates as. ocx has exactly one string that
    /// looks like one — [`GitPushCredential::username`](ocx_announce::forge::GitPushCredential::username)
    /// — and in the state this notice fires it is `gitlab-ci-token`, the
    /// constant default GitLab ignores the value of. It names the pipeline,
    /// which is precisely the party the warning exists to say is *not* the
    /// author, so printing it would invert the sentence while reading as a
    /// helpful detail.
    ///
    /// The username needle is read off
    /// [`GitPushCredential::DEFAULT_USERNAME`](ocx_announce::forge::GitPushCredential::DEFAULT_USERNAME),
    /// never spelled here: a rename that kept a hardcoded copy would leave this
    /// passing over the new value.
    ///
    /// The two positive halves are what stop the negative from failing
    /// silently — a `!contains` alone is equally satisfied by a notice that
    /// stopped saying anything at all (`quality-rust.md` § Structural guards).
    ///
    /// Mutation: interpolate the username into the notice (the negative reds);
    /// drop the "cannot name" clause, or the variable name (the matching
    /// positive reds).
    #[test]
    fn the_push_identity_notice_names_no_person() {
        let notice = super::PUSH_IDENTITY_NOTICE;

        assert!(
            !notice.contains(ocx_announce::forge::GitPushCredential::DEFAULT_USERNAME),
            "the HTTP Basic username is a protocol constant naming the pipeline, which is the one \
             party this notice says did NOT author the request: {notice}"
        );
        assert!(
            notice.contains(ocx_env::OCX_ANNOUNCE_TOKEN.declaration().name),
            "the notice must name the variable the push secret came from: {notice}"
        );
        assert!(
            notice.contains("cannot name"),
            "where ocx cannot observe a person for the credential it must say so, not substitute \
             the username: {notice}"
        );
    }
}
