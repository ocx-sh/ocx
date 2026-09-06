// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::cli::{ExitCode, UsageError, UserInterface};
// The canonical spelling, never a local copy: `env::keys` is where the ladder's
// reader and the `CREDENTIAL_KEYS` scrub list both take it from, so the name in
// this refusal cannot drift from the name that is read.
use ocx_lib::env::keys::OCX_ANNOUNCE_TOKEN;
use ocx_lib::forge::{ForgeCredentials, ForgeError, ForgeKind, RepoCoordinate, WriteTransport};

use crate::app::CommandError;

/// C-064's notice: inside a GitLab job, an `OCX_ANNOUNCE_TOKEN` set for the REST
/// half silently becomes the **push** credential too, because rung 2 of C-063's
/// push ladder is the resolved API credential. The merge request is then
/// authored by that token's owner rather than by the pipeline — a difference
/// that shows up as an unexpected author on a review the operator did not
/// expect to sign, and nowhere in the exit code.
///
/// It lives on the shared flatten because the state it describes belongs to the
/// flatten: every conjunct of [`ForgeWriteOptions::warn_push_identity`]'s guard
/// reads `--transport` or the resolved credential pair, and nothing about the
/// command that carried them. `ocx package claim` reaches the identical state
/// through the identical `ForgeCredentials::resolve`, so a copy in each command
/// file would be two sentences free to drift — which is the divergence
/// `subsystem-cli.md` forbids, in the one place a divergence would tell two
/// operators two different things about the same run.
const PUSH_IDENTITY_NOTICE: &str = "in a GitLab job, --transport git pushes with the OCX_ANNOUNCE_TOKEN credential (push credential kind token), not this job's CI_JOB_TOKEN: the merge request is authored by that token's owner rather than by the pipeline, and ocx cannot name them; set OCX_ANNOUNCE_GIT_TOKEN to push with another credential";

/// The write-target flags every forge-writing command shares.
///
/// Flatten into a command with `#[command(flatten)]` to add `--index-repo`,
/// `--forge`, `--transport`, `--fork` and `--out`. `ocx package claim` and
/// `ocx package announce` both carry it, so the two commands expose one
/// grammar rather than two that can drift apart.
///
/// Arg ids: `index_repo`, `forge`, `transport`, `fork`, `out`.
///
/// An exclusion **between two of these five** is declared here, on the fields
/// themselves — `--out` ⟂ `--fork` is the one C-058 names, both write commands
/// need it, and the outer command has no field to hang a `conflicts_with` on
/// once they live inside the flatten (DX-58). Only a *command-specific* flag
/// declares a conflict against a flattened arg id, and it does so in its own
/// command file.
///
/// Value-conditional refusals are not clap attributes at all — clap's
/// `blacklist` holds arg ids, never predicates — so `--transport git` against
/// `--fork`, against `--out` and against a resolved GitHub forge are runtime
/// checks. [`Self::validate`] owns all of them, so the rule that decides which
/// host receives a credential has one home rather than one copy per command.
///
/// Fields are public rather than accessor-wrapped because every one of them is
/// read verbatim by its command: there is no resolution rule to protect, unlike
/// [`Pull`](super::Pull) or [`TagsOpt`](super::tags::TagsOpt).
/// [`PlatformOption`](super::PlatformOption) is the existing precedent for a
/// flag group with nothing to resolve.
///
/// # Callers
///
/// **Every command flattening this must call [`Self::validate`] before it
/// constructs a forge client.** `ForgeKind::client` repeats exactly one of the
/// refusals below — `validate_transport` — so a forgotten call loses **every
/// other one**: `--transport git` with `--fork`, `--transport git` with `--out`,
/// the fork/index host agreement, and `validate_coordinate` on the index
/// repository *and* on the fork (a nested namespace on a GitHub coordinate,
/// `NestedNamespaceUnsupported`, 64), which lives nowhere else at the CLI. The
/// fields are public, so the type cannot enforce this; the obligation is stated
/// here instead of hardened into a typestate for one consumer (two after WP-15).
#[derive(clap::Args, Clone, Debug)]
pub struct ForgeWriteOptions {
    /// Index repository the request targets, as `[HOST/]NAMESPACE/PROJECT`.
    ///
    /// Give the host for a self-hosted GitHub Enterprise Server or GitLab
    /// instance; omit it for github.com. The namespace may be a nested GitLab
    /// group path. Defaults to `ocx-sh/index`.
    //
    // The default stays at stub time although the rest of the clap semantics
    // does not: the field is not an `Option`, so removing it would make
    // `--index-repo` *required*, and a required flag is exactly the grammar
    // rule the Specify phase must find absent. See the module-level note in
    // `command/package_claim.rs`.
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
    // Same reason as `--index-repo` above for keeping the default at stub time.
    #[clap(long = "transport", value_name = "TRANSPORT", default_value_t = WriteTransport::default())]
    pub transport: WriteTransport,

    /// Open (or update) the request from this fork, as
    /// `[HOST/]NAMESPACE/PROJECT`.
    ///
    /// Omit it to push the branch straight to `--index-repo` and open the
    /// request from there, which needs push access on that repository.
    /// Whichever of the two you choose, the change lands as a pull or merge
    /// request, never as a direct commit to the index's default branch.
    /// `--out` writes locally instead and opens neither.
    //
    // The reciprocal of the note on `out` below. Declared on both fields the way
    // `package_announce.rs` already declares this exact pair, which WP-15 then
    // deletes in favour of the flatten.
    #[clap(long = "fork", value_name = "REPOSITORY", conflicts_with = "out")]
    pub fork: Option<RepoCoordinate>,

    /// Write the rendered index entry under this directory instead of opening
    /// a request. Works without a credential.
    //
    // C-058, exit 64. The rule belongs **here**, on the flattened pair, and not
    // in each command file (DX-58): both args live inside the flatten, so the
    // outer command has no field to hang the attribute on, and the plan names no
    // command with a divergent rule. Only a *command-specific* flag declares a
    // conflict against a flattened arg id.
    #[clap(long = "out", value_name = "DIRECTORY", conflicts_with = "fork")]
    pub out: Option<PathBuf>,
}

impl ForgeWriteOptions {
    /// Every argv fault that is decidable from these five flags alone, and the
    /// forge kind the run resolved (C-058).
    ///
    /// Pure: no environment read, no process spawn, no network. It runs at the
    /// head of the command's argv-fault block, before any credential is
    /// resolved, so a malformed command line reports what is wrong with it
    /// rather than a missing token the operator would set only to hit the real
    /// error next run.
    ///
    /// Returns the resolved [`ForgeKind`] because there is exactly **one**
    /// producer of it: the report's `forge` key renders what this returned, and
    /// a second `ForgeKind::resolve` at the report layer would be free to
    /// disagree.
    ///
    /// The refusals, and where each one's rule lives:
    ///
    /// - `--transport git` with `--fork`, and with `--out` — here, naming
    ///   **both** flags (S-020). No library error names a flag pair.
    /// - a self-hosted `--index-repo` host with no `--forge` —
    ///   `ForgeKind::resolve`.
    /// - `--fork` on a different host from `--index-repo` — `same_host`, never
    ///   `Option` equality: an omitted host *means* the forge's canonical host,
    ///   so `ocx-sh/index` with `--fork github.com/me/index` names one instance
    ///   twice and must be accepted.
    /// - `--transport git` against a resolved GitHub forge —
    ///   `ForgeKind::validate_transport`, whose message names the forge and the
    ///   remedy. Re-spelling that check here would drift from the copy
    ///   `ForgeKind::client` makes anyway.
    ///
    /// `--out` with `--fork` is not here: it is a clap `conflicts_with` on the
    /// two fields above, and clap refuses it before this is reached.
    ///
    /// # Errors
    ///
    /// Every refusal classifies to exit 64.
    pub fn validate(&self) -> anyhow::Result<ForgeKind> {
        // The two flag-pair refusals come first, and they must: on the default
        // github.com index `--transport git` also trips `validate_transport`,
        // whose message names the forge rather than the second flag S-020 owes.
        if self.transport == WriteTransport::Git {
            if self.fork.is_some() {
                return Err(UsageError::new(
                    "--transport git cannot be combined with --fork; the git transport writes the claim branch to --index-repo itself",
                )
                .into());
            }
            if self.out.is_some() {
                return Err(UsageError::new(
                    "--transport git cannot be combined with --out; --out writes the entry locally and opens no request",
                )
                .into());
            }
        }

        // The forge is resolved from the index coordinate, never from the fork:
        // a fork always lives on the same instance as the repository it forks,
        // and letting the two disagree would send the credential to one host
        // while addressing repositories on another.
        let kind = ForgeKind::resolve(self.forge, &self.index_repo)?;
        kind.validate_coordinate(&self.index_repo)?;
        if let Some(fork) = &self.fork {
            kind.validate_coordinate(fork)?;
            // Compared through `same_host`, never by `Option` equality: an
            // omitted host MEANS the forge's canonical host, so `ocx-sh/index`
            // with `--fork github.com/me/index` names one instance twice and must
            // not be refused. Case folds for the same reason.
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
        // The library's own rule, called rather than re-spelled: `ForgeKind::client`
        // makes this check anyway, so a hand-written copy here could only drift.
        kind.validate_transport(self.transport)?;
        Ok(kind)
    }

    /// Whether the run needs a local `git` (C-065).
    ///
    /// The `git --version` gate is conditional on **this**, and nothing else
    /// can observe the condition: `ForgeKind::client` raises the same exit 69
    /// when the binary is missing, so a probe that runs unconditionally is
    /// invisible in the exit code and only shows up as an ordinary `api` claim
    /// failing on a host that has no `git` and never needed one.
    #[must_use]
    pub fn needs_git(&self) -> bool {
        self.transport == WriteTransport::Git
    }

    /// C-063's terminal-rung refusal: a write mode with no resolved API
    /// credential is exit 80, naming `OCX_ANNOUNCE_TOKEN`; `--out` proceeds
    /// unauthenticated (S-011).
    ///
    /// Takes the **resolved** [`ForgeCredentials`] rather than reading the
    /// environment itself, and that is the whole point of the signature. The
    /// shape the sibling announce command carried before this migration is
    /// `std::env::var(OCX_ANNOUNCE_TOKEN).ok().filter(|v| !v.is_empty())`, which
    /// a builder would reasonably copy — and which refuses, at exit 80, exactly
    /// the state C-063 says must succeed: a GitLab job with an empty ocx
    /// variable and a perfectly good `CI_JOB_TOKEN`, picked up by the ladder's
    /// second rung. Only the ladder knows which rung answered, so only the
    /// ladder's answer may be branched on.
    ///
    /// # Errors
    ///
    /// [`crate::app::CommandError`] at [`ocx_lib::cli::ExitCode::AuthError`].
    pub fn require_credential(&self, credentials: &ForgeCredentials) -> anyhow::Result<()> {
        // `--out` reads the forge but writes nothing, so it proceeds
        // unauthenticated (S-011).
        if self.out.is_some() || credentials.api_is_present() {
            return Ok(());
        }
        Err(CommandError::new(
            format!(
                "a forge write needs a credential in {OCX_ANNOUNCE_TOKEN}; use --out to write the entry locally instead"
            ),
            ExitCode::AuthError,
        )
        .into())
    }

    /// C-064: warn, before the write, that the push authenticates as the
    /// operator rather than as the pipeline.
    ///
    /// Emitted from **here** and nowhere else, so `ocx package claim` and
    /// `ocx package announce` render one sentence on one stream. Each command
    /// calls this unconditionally at the same point in its own sequence —
    /// after `require_credential`, before the forge is built — so the state
    /// the notice describes is the state the write is about to use.
    ///
    /// stderr via [`UserInterface::warn`], never stdout: a `--format json` run
    /// stays one parseable document (S-034). Takes the `UserInterface` rather
    /// than the `Context` it came from because that is the whole capability
    /// needed, and it keeps this module free of per-invocation app state.
    ///
    /// Four of the five conjuncts of the guard are load-bearing, which is why
    /// it is assembled here rather than hidden behind one predicate:
    ///
    /// - **`--transport git`** — the API transport pushes nothing, so there is
    ///   no authoring identity to be surprised by.
    /// - **inside `GITLAB_CI`** — outside a job there is no job token this run
    ///   could have used instead, so nothing was displaced and the operator is
    ///   already authenticating as themselves on purpose. Firing here anyway
    ///   would warn every local operator about a run behaving exactly as asked.
    /// - **no `OCX_ANNOUNCE_GIT_TOKEN`** — an operator who named a push
    ///   credential chose the identity, and being told about their own choice
    ///   is noise.
    /// - **the push secret is not the job token** — when it is, the pipeline
    ///   *is* the author and there is nothing to say.
    ///
    /// The fifth, **a push credential was resolved at all**, is the one no
    /// reachable state falsifies, and it is kept as a precondition of the
    /// sentence rather than as a filter: the notice asserts what the push
    /// authenticates with, so it must not be printed for a run that injects
    /// nothing. `push` is `None` only at rung 3, i.e. an empty API half —
    /// which [`Self::require_credential`] has already refused at exit 80
    /// unless `--out` was given, and `--out` with `--transport git` is already
    /// exit 64 from [`Self::validate`]. Both run before this guard in both
    /// commands, so no mutation of this conjunct can red a test.
    ///
    /// The credential kind named in the sentence is the one
    /// [`crate::api::data::forge_report::push_credential_kind`] reports for
    /// exactly this state — `token`, its "a push secret ocx injected but
    /// cannot classify further" arm — so the stderr line and the report's
    /// `push_credential_kind` key use one word. Written out rather than
    /// rendered from the enum because the guard proves which arm applies, and
    /// the wire spelling is already pinned by
    /// `push_credential_kind_wire_spellings`.
    ///
    /// **What the notice may name, and what it may not (DX-86).** It names the
    /// push credential *kind* and the *variable* the secret came from, and it
    /// says outright that ocx cannot name the person behind it. It must never
    /// print the HTTP Basic username instead. In exactly the state this fires,
    /// [`GitPushCredential::username`](ocx_lib::forge::GitPushCredential::username)
    /// is `gitlab-ci-token` — `OCX_ANNOUNCE_GIT_USERNAME`'s default, a protocol
    /// artifact GitLab ignores the value of. Printing it here would name the
    /// *job* as the author in the one warning whose entire subject is that the
    /// job is not the author. Where ocx cannot observe an identity, saying so
    /// is the honest answer; substituting a constant is a confident wrong one.
    pub fn warn_push_identity(&self, ui: &UserInterface, credentials: &ForgeCredentials) {
        if self.transport == WriteTransport::Git
            && credentials.in_gitlab_ci()
            && !credentials.push_is_explicit()
            && !credentials.push_is_job_token()
            && credentials.push().is_some()
        {
            ui.warn(PUSH_IDENTITY_NOTICE);
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{Args as _, CommandFactory as _, Parser as _};

    use ocx_lib::cli::ExitCode;
    use ocx_lib::forge::{ForgeCredentials, ForgeToken, WriteTransport};

    use super::ForgeWriteOptions;
    use crate::app::classify_error;
    use crate::command::package_claim::PackageClaim;

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

    /// C-059: `ocx package claim` exposes the whole shared grammar, and the
    /// expected set is **derived** from the struct.
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
    /// into the shared struct and land on announce in WP-15 as a flag that
    /// command has no use for.
    ///
    /// The both-commands parity half is WP-15's (C-059): the announce flatten is
    /// that package's edit, and a parity test owned here would be red at this
    /// package's own merge gate by construction.
    ///
    /// **Green on arrival** — the struct is complete at the stub, so this is a
    /// characterization test of C-059's shape.
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
            vec!["index-repo", "forge", "transport", "fork", "out"],
            "C-059's five shared flags, and nothing command-specific"
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

    /// C-058 / DX-58: `--out` ⟂ `--fork` is declared on the shared struct, so
    /// both write commands inherit one rule.
    ///
    /// Asserted through the flatten alone rather than through a command, which
    /// is what makes WP-15's inheritance real: announce carries this pair as two
    /// `conflicts_with` attributes today and must delete them, not duplicate
    /// them.
    ///
    /// Red at the stub: no `conflicts_with` is declared, so the parse succeeds.
    /// Mutation once implemented: delete **both** `conflicts_with` attributes.
    /// Deleting either one alone leaves this green — clap's conflict check is
    /// symmetric (`Conflicts::gather_conflicts` tests each argument's blacklist
    /// against the other's), so one surviving attribute still refuses the pair.
    /// Keeping both is correct anyway: `package_announce.rs` declares the same
    /// pair today and WP-15 deletes its copies in favour of this flatten. Two
    /// independent guards over one property is the case `quality-core.md`
    /// § Unchecked Green describes, not a weak check.
    #[test]
    fn out_and_fork_conflict_is_declared_on_the_shared_struct() {
        assert!(
            probe(&["--out", "d", "--fork", "o/r"]).is_err(),
            "--out and --fork together must be a clap usage error, declared on the flatten"
        );
        assert!(probe(&["--out", "d"]).is_ok(), "--out alone parses");
        assert!(probe(&["--fork", "o/r"]).is_ok(), "--fork alone parses");
    }

    /// C-057 / R-50: `--transport` is backed by `WriteTransport`'s own
    /// `ValueEnum`, so the flag's accepted values and the report's rendered
    /// value are one vocabulary.
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

    /// C-065: the `git --version` gate runs **iff** `--transport git`.
    ///
    /// The only property of C-065 a unit test can hold. `ForgeKind::client`
    /// raises the same exit 69 when the binary is missing, so an unconditional
    /// probe is invisible in the exit code — it shows up only as an ordinary
    /// `api` claim failing on a host that has no `git` and never needed one,
    /// which no CI runner reproduces. The version cases belong to WP-6 and the
    /// end-to-end 69s to WP-16.
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

    /// C-063 / S-011: the exit-80 refusal is keyed on the credential the
    /// **ladder** resolved, and `--out` is exempt.
    ///
    /// This is the CLI-boundary half of C-063; the ladder itself is WP-6's and
    /// is already tested there
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
    /// **Residual, recorded rather than hidden.** DX-55 named *two*
    /// CLI-boundary properties for this test; only the second is reachable
    /// here. *Which transport the CLI hands `ForgeCredentials::resolve`* is
    /// **not** asserted at any scope in this package, so R-15's first named
    /// mutation — hardcoding `WriteTransport::Api` at
    /// `package_claim.rs`'s `resolve` call — reds nothing. No red is reachable
    /// at unit scope: `ocx_cli`'s test binary links a non-`cfg(test)`
    /// `ocx_lib`, so `crate::env::var`'s override map cannot be reached from
    /// here and the job-token rung cannot be staged without mutating the real
    /// process environment. The control is WP-17's
    /// `::test_job_token_pickup_headers_and_push_user`, which observes the
    /// resolved credential end to end.
    ///
    /// Red at the stub: `require_credential` is `unimplemented!()`.
    /// Mutation once implemented: key the refusal on
    /// `std::env::var(OCX_ANNOUNCE_TOKEN)` (the shape `package_announce.rs`
    /// carried before this migration); or drop the `--out` carve-out.
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
            "C-063's terminal rung is exit 80"
        );
        assert!(
            error.to_string().contains("OCX_ANNOUNCE_TOKEN"),
            "the refusal names the variable to set, got: {error}"
        );

        let out = probe(&["--out", "d"]).expect("parses");
        out.require_credential(&nothing)
            .expect("--out reads the forge but writes nothing, so it proceeds unauthenticated (S-011)");
    }

    /// C-060's vocabulary reaches stderr and the report with one spelling.
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

    /// DX-86: the notice names the credential's **source**, never a person it
    /// cannot observe.
    ///
    /// The tempting way to make a warning about authorship concrete is to print
    /// the identity the push authenticates as. ocx has exactly one string that
    /// looks like one — [`GitPushCredential::username`](ocx_lib::forge::GitPushCredential::username)
    /// — and in the state this notice fires it is `gitlab-ci-token`, the
    /// constant default GitLab ignores the value of. It names the pipeline,
    /// which is precisely the party the warning exists to say is *not* the
    /// author, so printing it would invert the sentence while reading as a
    /// helpful detail.
    ///
    /// The username needle is read off
    /// [`GitPushCredential::DEFAULT_USERNAME`](ocx_lib::forge::GitPushCredential::DEFAULT_USERNAME),
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
            !notice.contains(ocx_lib::forge::GitPushCredential::DEFAULT_USERNAME),
            "the HTTP Basic username is a protocol constant naming the pipeline, which is the one \
             party this notice says did NOT author the request: {notice}"
        );
        assert!(
            notice.contains(ocx_lib::env::keys::OCX_ANNOUNCE_TOKEN),
            "the notice must name the variable the push secret came from: {notice}"
        );
        assert!(
            notice.contains("cannot name"),
            "where ocx cannot observe a person for the credential it must say so, not substitute \
             the username: {notice}"
        );
    }
}
