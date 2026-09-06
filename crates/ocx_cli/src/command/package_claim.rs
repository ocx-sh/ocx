// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package claim` — claim a package in the index.
//!
//! # Where each refusal lives
//!
//! C-058 spreads seven rules over two sites, and the site is part of the
//! contract. clap owns the ones it can express — `--out` ⟂ `--fork` (declared
//! on [`options::ForgeWriteOptions`] itself, so both write commands inherit one
//! rule) and each `--upstream-*` optional's `requires` on its anchor.
//! [`options::ForgeWriteOptions::validate`] owns the value-conditional ones,
//! because clap's `blacklist` holds arg ids and never predicates.
//!
//! [`PackageClaim::execute`]'s order is contracted too: every pure refusal
//! precedes anything that spawns or dials, so a malformed command line costs no
//! network call and no operator round trip through a token they did not need.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::claim::{self, ClaimRequest, ClaimTarget, Upstream};

use crate::api::data::claim::ClaimReport;
use crate::options;

/// Claim a package in the index so its tags can be announced.
///
/// Renders the package's index entry and opens a pull or merge request
/// against the index repository, or writes the entry to a local directory with
/// `--out`. A package that is already claimed is refused: announce it with
/// `ocx package announce` instead.
///
/// Owners default to the CI environment's user variables, else to the identity
/// behind the credential. Name them explicitly with `--owner`, which replaces
/// the detected list rather than adding to it.
///
/// Opening a request needs a forge credential: `OCX_ANNOUNCE_TOKEN`, or the job
/// token under `--transport git` inside a GitLab job. Writing to `--out` works
/// without one.
#[derive(Parser)]
pub struct PackageClaim {
    /// Where the request is written, and how it gets there.
    #[command(flatten)]
    forge: options::ForgeWriteOptions,

    /// The physical OCI repository the package's bytes live in, as
    /// `oci://HOST/PATH`.
    ///
    /// This is the pointer every later `ocx package announce` resolves tags
    /// against, so it names the registry repository, not the index.
    //
    // Deliberately a bare `String` with no `value_parser`: the pointer is parsed
    // by the existing `oci::index::parse_physical_repository`, which
    // `claim::root` already calls and which names the refused value back
    // (`ClaimError::MalformedRepository`, exit 64). A clap `value_parser` would
    // move that refusal to `ValueValidation` -- 64 as well, so the exit code
    // cannot tell them apart -- and leave the library guard with no production
    // caller (R-13).
    #[clap(long = "repository", value_name = "REPOSITORY", required = true)]
    repository: String,

    /// An owner of the package, as `LOGIN` or `LOGIN:ID`. Repeat for
    /// several, in the order they should be recorded.
    ///
    /// Giving any `--owner` replaces the detected list; the invoking identity
    /// is not added to it. A bare `LOGIN` is resolved against the forge's users
    /// API and needs it reachable; `LOGIN:ID` is taken on your word and is the
    /// form to use when it is not.
    #[clap(long = "owner", value_name = "OWNER")]
    owner: Vec<String>,

    /// The upstream organization this package mirrors or repackages.
    ///
    /// Give it for a third-party package so the index entry records who the
    /// software actually comes from. It is the anchor for the other two
    /// `--upstream-*` flags: neither is accepted without it.
    #[clap(long = "upstream-org", value_name = "ORGANIZATION")]
    upstream_org: Option<String>,

    /// The upstream project's repository URL, as an absolute `http` or `https`
    /// URL carrying no embedded credentials.
    ///
    /// Any other form is a usage error, including a URL with a username or
    /// password in it, which is what a forwarded `CI_REPOSITORY_URL` looks
    /// like. The value is written verbatim into the index entry, which a
    /// catalog may render as a link, so neither a scheme ocx cannot vouch for
    /// nor a secret ever reaches it.
    //
    // Anchored independently of its sibling below (exit 64): one `requires` on
    // one flag would leave the other unanchored and still satisfy a singular
    // test name.
    #[clap(long = "upstream-repository-url", value_name = "URL", requires = "upstream_org")]
    upstream_repository_url: Option<String>,

    /// A disclaimer recorded on the index entry, for a package that is not
    /// operated by the upstream project.
    ///
    /// The text reaches the index entry only. It is never interpolated into
    /// the pull or merge request, so it fires no mentions and renders no
    /// markdown in a repository humans review.
    //
    // Anchored independently, as above.
    #[clap(long = "upstream-disclaimer", value_name = "TEXT", requires = "upstream_org")]
    upstream_disclaimer: Option<String>,

    /// Namespace and package to claim, as `<namespace>/<package>`
    /// (e.g. `acme/widget`).
    ///
    /// The positional is the only form; flags come before it.
    #[clap(value_name = "PACKAGE")]
    package: options::Identifier,
}

impl PackageClaim {
    /// The `--owner` values, parsed into the library's own vocabulary.
    ///
    /// **A design gap this seam records rather than hides.** No contract in the
    /// plan names the CLI-side `--owner` parser: `OwnerSpec` has no `FromStr`
    /// anywhere in the workspace, and every malformed wire form is unruled. The
    /// ruling proposed here, and the one the tests pin, is: split on the
    /// **first** `:`; the head must be non-empty and the tail must parse as
    /// `u64`; a failure is a usage error (exit 64). `alice:7:8` therefore fails
    /// on `7:8` — deterministic, and no forge login can contain `:`. Splitting
    /// on the last `:` instead would silently read `alice:7:8` as login
    /// `alice:7`, and `:7` names nobody at all.
    ///
    /// Two things this must **not** do, because both would make a library
    /// governance rule unreachable:
    ///
    /// - filter an empty login. `--owner ""` reaches
    ///   `claim::owners::resolve_owners` as `OwnerSpec::Login("")`, which
    ///   refuses it — treating it as absence would silently write whoever the CI
    ///   environment names into a governance field.
    /// - deduplicate. `resolve_owners` owns that rule and raises
    ///   `ClaimError::DuplicateOwner`; a CLI that dedups first makes it dead
    ///   code.
    ///
    /// # Errors
    ///
    /// A usage error (exit 64) naming the refused value and the `LOGIN:ID` form.
    fn owner_specs(&self) -> anyhow::Result<Vec<ocx_lib::claim::OwnerSpec>> {
        self.owner.iter().map(|value| parse_owner_spec(value)).collect()
    }

    /// Every refusal decidable from argv alone, in contract order, and the
    /// forge kind the run resolved.
    ///
    /// One function rather than four statements in [`Self::execute`] so that
    /// "this is the whole set of zero-I/O refusals" is a property a test can
    /// hold: everything here must outrank C-063's exit-80 credential refusal,
    /// which is decidable only after the environment is read. A malformed
    /// `--repository` that reached the credential check first would report 80,
    /// sending an operator to provision a forge token for a typo.
    ///
    /// `--repository` is parsed **and the value discarded**: the library
    /// re-parses it inside `claim::claim`, so `claim::root`'s
    /// `malformed_repository_*` tests keep their production caller and R-13's
    /// objection does not apply.
    ///
    /// # Errors
    ///
    /// Every refusal here classifies to exit 64.
    fn argv_faults(&self) -> anyhow::Result<ocx_lib::forge::ForgeKind> {
        // `validate` carries C-058's value-conditional exclusions, the `git`
        // transport's GitHub refusal and the fork/index host agreement, and it
        // is the single producer of the resolved kind the report renders.
        let kind = self.forge.validate()?;
        self.owner_specs()?;
        claim::parse_repository(&self.repository)?;
        validate_upstream_repository_url(self.upstream_repository_url.as_deref())?;
        Ok(kind)
    }
}

/// Refuse an `--upstream-repository-url` that may not be published.
///
/// The value is the one argv channel that reaches the index root without any
/// grammar of its own — the logical name and the physical repository are both
/// constrained by the identifier grammar, and the login by
/// `claim::owners::login_charset_is_valid`. The rule itself lives in the library
/// beside the field it protects
/// ([`ocx_lib::claim::upstream_repository_url_is_publishable`], which states why
/// each half exists); this function owns only the flag name and the exit code.
///
/// **The message names the flag and the rule, never the value.** The refusal's
/// most likely input is a forwarded `CI_REPOSITORY_URL`, whose userinfo is a
/// live job token — echoing it back would move the secret from the index root
/// into the CI job log (CWE-532), which is the same disclosure one sink over.
/// That is why this refusal reads differently from `parse_owner_spec`'s, which
/// does name its value: a `--owner` login carries no secret.
fn validate_upstream_repository_url(value: Option<&str>) -> anyhow::Result<()> {
    match value {
        Some(url) if !claim::upstream_repository_url_is_publishable(url) => Err(ocx_lib::cli::UsageError::new(
            "--upstream-repository-url must be an http or https url without embedded credentials",
        )
        .into()),
        _ => Ok(()),
    }
}

/// One `--owner` value, by the split-on-first-colon rule
/// [`PackageClaim::owner_specs`] records.
///
/// The `LOGIN:ID` form additionally requires a **non-empty login**: `:7` names
/// nobody, so it is not a wire form. That is not the empty-login filter the
/// method's doc forbids — `--owner ""` carries no colon, so it reaches the
/// library as `OwnerSpec::Login("")` and is refused there, where the governance
/// rule is documented.
fn parse_owner_spec(value: &str) -> anyhow::Result<ocx_lib::claim::OwnerSpec> {
    let Some((login, id)) = value.split_once(':') else {
        return Ok(ocx_lib::claim::OwnerSpec::Login(value.to_string()));
    };
    let refuse = || {
        ocx_lib::cli::UsageError::new(format!(
            "--owner {value} is neither a LOGIN nor a LOGIN:ID pair; an id is a non-negative whole number"
        ))
    };
    if login.is_empty() {
        return Err(refuse().into());
    }
    // `u64`, never `i64`: `alice:-1` is not an account id, and the last-colon
    // split would silently read `alice:7:8` as login `alice:7`.
    let id: u64 = id.parse().map_err(|_| refuse())?;
    Ok(ocx_lib::claim::OwnerSpec::Resolved {
        login: login.to_string(),
        id,
    })
}

impl PackageClaim {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // 1. Argv faults first, before any credential check, so a malformed
        //    command line reports what is wrong with it (64) rather than a
        //    missing token the operator would set only to hit the real error
        //    next run. All of them live in `argv_faults`, so the set is one
        //    function rather than a call sequence a later edit can reorder
        //    piecemeal.
        let kind = self.argv_faults()?;
        let owners = self.owner_specs()?;
        let package = self.package.with_domain(context.default_registry())?;

        // C-065: the `git --version` gate runs beside `validate_transport` and
        // BEFORE the forge is constructed, so a missing or too-old git exits 69
        // with zero *forge* calls. Zero *network* calls is a stronger claim this
        // ordering does not make (DX-62): the ambient self-update check runs
        // upstream of every command, and what keeps it from dialling is
        // `app/update_check.rs`'s "stderr is not a terminal" short-circuit, not
        // anything claim does. Its result travels into the constructor as a
        // `GitBinary` -- one producer, one assembler. Conditional on the
        // transport: `ForgeKind::client` raises the same 69 for an unresolved
        // binary, so an unconditional probe would only show up as an ordinary
        // `api` claim failing on a host that never needed git.
        let git = if self.forge.needs_git() {
            Some(ocx_lib::forge::probe_git_binary().await?)
        } else {
            None
        };

        // 2. The whole C-063 ladder lives in `resolve`, never here, and the
        //    resolved pair travels down as one value. The selected transport is
        //    what opens the job-token rung, so it is passed rather than assumed.
        let credentials = ocx_lib::forge::ForgeCredentials::resolve(self.forge.transport);
        self.forge.require_credential(&credentials)?;

        // C-064: before the write, and only in the one state that surprises.
        // Claim reaches that state through the same `resolve` announce does, so
        // it renders the same sentence on the same stream from the same place —
        // see `options::ForgeWriteOptions::warn_push_identity`.
        self.forge.warn_push_identity(context.ui(), &credentials);

        let request = ClaimRequest {
            package,
            repository: self.repository.clone(),
            owners,
            upstream: self.upstream(),
            target: self.target(),
            index_repo: self.forge.index_repo.clone(),
        };

        let forge = kind.client(self.forge.transport, credentials.clone(), &self.forge.index_repo, git)?;
        let outcome = claim::claim(Some(forge.as_ref()), request).await?;

        context.api().report(&ClaimReport::from_outcome(
            outcome,
            kind,
            self.forge.transport,
            &credentials,
        ))?;

        Ok(ExitCode::SUCCESS)
    }

    /// The write target the flag pair selects.
    ///
    /// `--out` and `--fork` are mutually exclusive on the shared flatten (clap
    /// `conflicts_with`), and neither given means the claim branch is pushed to
    /// `--index-repo` itself. A method rather than an inline `match` so the
    /// mapping — which decides *which repository gets written to* — is
    /// assertable without a forge.
    fn target(&self) -> ClaimTarget {
        match (&self.forge.out, &self.forge.fork) {
            (Some(directory), _) => ClaimTarget::Out(directory.clone()),
            (None, Some(coordinate)) => ClaimTarget::Fork(coordinate.clone()),
            (None, None) => ClaimTarget::Direct,
        }
    }

    /// The `upstream` object, present exactly when its anchor was given.
    ///
    /// The two optionals cannot appear without `--upstream-org` (clap
    /// `requires`), so the anchor alone decides whether the object exists.
    fn upstream(&self) -> Option<Upstream> {
        self.upstream_org.as_ref().map(|org| Upstream {
            org: org.clone(),
            repository_url: self.upstream_repository_url.clone(),
            disclaimer: self.upstream_disclaimer.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use ocx_lib::claim::{ClaimTarget, OwnerSpec, Upstream};
    use ocx_lib::cli::ExitCode;

    use super::PackageClaim;
    use crate::app::classify_error;

    /// A valid invocation with `flags` inserted before the positional.
    fn parse(flags: &[&str]) -> Result<PackageClaim, clap::Error> {
        let mut argv = vec!["claim", "--repository", "oci://ghcr.io/acme/widget"];
        argv.extend_from_slice(flags);
        argv.push("acme/widget");
        PackageClaim::try_parse_from(argv)
    }

    /// The same shape without an injected `--repository`, for the rows that
    /// supply their own.
    fn parse_raw(flags: &[&str]) -> Result<PackageClaim, clap::Error> {
        let mut argv = vec!["claim"];
        argv.extend_from_slice(flags);
        argv.push("acme/widget");
        PackageClaim::try_parse_from(argv)
    }

    /// Where a refusal is expected to come from. C-058 spreads seven rules over
    /// two sites, and the *site* is part of the contract: a rule moved from clap
    /// into a runtime check still exits 64, but stops being reported before the
    /// command starts doing anything.
    #[derive(Debug, Clone, Copy)]
    enum Site {
        /// clap refuses the invocation at parse time, with this error kind.
        ///
        /// The kind is carried rather than a bare `is_err()` so a clap row names
        /// its exit code the way the `Validate` rows do: `cli::clap::parse` maps
        /// **every** `clap::Error` to `EX_USAGE`, so the kind is the only thing
        /// distinguishing "refused by the rule this row is about" from "refused
        /// because the argv the row builds is malformed for some other reason".
        Clap(clap::error::ErrorKind),
        /// The invocation parses; `ForgeWriteOptions::validate` refuses it.
        Validate,
    }

    /// C-058: all seven mutual exclusions, each with its site, its exit code and
    /// what its message must name.
    ///
    /// A table rather than one assertion, because the inventory row's single
    /// name covers seven independent rules — one `try_parse_from` on
    /// `--out --fork` satisfies the name while six rules go unasserted. Deleting
    /// any one `conflicts_with` or any one runtime arm reds exactly one row.
    ///
    /// Every row asserts a **refusal**, not a difference: with the
    /// `--transport git` + `--out` arm deleted the run exits 0, writes the tree
    /// and reports `transport: "git"` for a transport that never ran, which an
    /// exit-code-only table cannot see.
    ///
    /// Red at the stub: no `conflicts_with` is declared and `validate` is
    /// `unimplemented!()`.
    /// Mutation once implemented: delete any one runtime arm. For the
    /// `--out`/`--fork` row the mutation is deleting **both** `conflicts_with`
    /// attributes — clap's conflict check is symmetric
    /// (`Conflicts::gather_conflicts` tests each argument's blacklist against
    /// the other's), so one surviving attribute still refuses the pair. Two
    /// independent guards over one property is the case `quality-core.md`
    /// § Unchecked Green describes, not a weak check.
    #[test]
    fn claim_grammar_conflicts() {
        // (flags, site, a fragment the message must carry)
        let rows: &[(&[&str], Site, &[&str])] = &[
            (
                &["--out", "d", "--fork", "ocx-contrib/index"],
                Site::Clap(clap::error::ErrorKind::ArgumentConflict),
                &[],
            ),
            (
                &["--transport", "git", "--fork", "ocx-contrib/index"],
                Site::Validate,
                &["--transport", "--fork"],
            ),
            (
                &["--transport", "git", "--out", "d"],
                Site::Validate,
                &["--transport", "--out"],
            ),
            // A GitHub index with the git transport, isolated from the two rows
            // above so it is the transport/forge rule that fires. The message is
            // `ForgeError::TransportUnsupported`'s and names the forge and the
            // remedy — a hand-written CLI copy of the rule would exit 64 too, so
            // the text is what discriminates it from the library's own check,
            // which `ForgeKind::client` makes anyway.
            (&["--transport", "git"], Site::Validate, &["is not supported on github"]),
            (
                &["--upstream-repository-url", "https://example.com"],
                Site::Clap(clap::error::ErrorKind::MissingRequiredArgument),
                &[],
            ),
            // A self-hosted host says nothing about which forge runs there.
            (
                &["--index-repo", "git.example.com/acme/index"],
                Site::Validate,
                &["--forge"],
            ),
            // A fork on another instance would be addressed on the index's host
            // regardless — writing to a repository the operator never named.
            (&["--fork", "gitlab.com/me/index"], Site::Validate, &["fork"]),
        ];

        for (flags, site, fragments) in rows {
            match site {
                Site::Clap(expected) => {
                    let Err(error) = parse(flags) else {
                        panic!("{flags:?} must be refused by clap at parse time (exit 64)");
                    };
                    assert_eq!(
                        error.kind(),
                        *expected,
                        "{flags:?} must be refused by clap as {expected:?}; got: {error}"
                    );
                }
                Site::Validate => {
                    let args = parse(flags).unwrap_or_else(|error| {
                        panic!("{flags:?} must parse and be refused by validate, not by clap: {error}")
                    });
                    let error = args
                        .forge
                        .validate()
                        .expect_err(&format!("{flags:?} must be refused (exit 64)"));
                    assert_eq!(
                        classify_error(error.as_ref()),
                        ExitCode::UsageError,
                        "{flags:?} is an argv fault, so it exits 64"
                    );
                    let message = error.to_string();
                    for fragment in *fragments {
                        assert!(
                            message.contains(fragment),
                            "{flags:?} must name {fragment:?}; got: {message}"
                        );
                    }
                }
            }
        }
    }

    /// C-058 / R-10: the six mode cells, so the `git` refusals are proved
    /// **conditional on the transport** rather than on the flag.
    ///
    /// Without the three accepting `api` cells, an implementation that refuses
    /// `--fork` and `--out` unconditionally passes every refusal row above.
    ///
    /// Red at the stub: `validate` is `unimplemented!()`.
    /// Mutation once implemented: make the `git` refusals unconditional; the
    /// three `api` cells red.
    #[test]
    fn transport_mode_matrix_refuses_only_the_git_rows() {
        // GitLab, so `--transport git` is not refused by the forge rule and the
        // matrix isolates the target flags.
        let gitlab = ["--index-repo", "gitlab.com/ocx-sh/index"];
        let cells: &[(&[&str], bool)] = &[
            (&["--transport", "api"], true),
            (&["--transport", "api", "--fork", "gitlab.com/me/index"], true),
            (&["--transport", "api", "--out", "d"], true),
            (&["--transport", "git"], true),
            (&["--transport", "git", "--fork", "gitlab.com/me/index"], false),
            (&["--transport", "git", "--out", "d"], false),
        ];
        for (flags, accepted) in cells {
            let mut argv = gitlab.to_vec();
            argv.extend_from_slice(flags);
            let args = parse(&argv).expect("every cell parses; the refusals are value-conditional");
            assert_eq!(
                args.forge.validate().is_ok(),
                *accepted,
                "{flags:?} on gitlab must be {}",
                if *accepted { "accepted" } else { "refused" }
            );
        }
    }

    /// C-058 / R-08: one instance spelled two ways is one instance.
    ///
    /// The positive half of the fork/index host rule, and the only half that
    /// catches the trap the announce command documents: comparing
    /// `Option<String>` hosts refuses `ocx-sh/index` with
    /// `--fork github.com/me/index`, which names github.com twice. Case folds
    /// for the same reason.
    ///
    /// Red at the stub: `validate` is `unimplemented!()`.
    /// Mutation once implemented: compare `fork.host == index_repo.host`
    /// instead of through `ForgeKind::same_host`.
    #[test]
    fn a_fork_on_the_same_host_spelled_differently_is_accepted() {
        for fork in ["github.com/me/index", "GitHub.COM/me/index"] {
            let args = parse(&["--fork", fork]).expect("parses");
            assert!(
                args.forge.validate().is_ok(),
                "--fork {fork} names the same instance as the default index and must be accepted"
            );
        }
    }

    /// C-057: `--repository` is required.
    ///
    /// Red at the stub: the field is an `Option` with no `required`, so the
    /// invocation parses.
    /// Mutation once implemented: make the field an `Option` again.
    #[test]
    fn claim_requires_repository() {
        assert!(
            PackageClaim::try_parse_from(["claim", "acme/widget"]).is_err(),
            "a claim with no --repository must be a clap usage error"
        );
        assert!(
            PackageClaim::try_parse_from(["claim", "--repository", "oci://ghcr.io/acme/widget", "acme/widget"]).is_ok(),
            "the positive half: with --repository the same invocation parses"
        );
    }

    /// C-057 / S-012: each `--upstream-*` optional independently `requires` the
    /// anchor.
    ///
    /// Three rows, not one: a single `requires` on a single flag satisfies the
    /// inventory's singular name while the sibling flag stays unanchored.
    ///
    /// Red at the stub: no `requires` is declared, so all three parse.
    /// Mutation once implemented: delete `requires` from either flag.
    #[test]
    fn upstream_flags_require_anchor() {
        let rows: &[&[&str]] = &[
            &["--upstream-repository-url", "https://example.com/acme/widget"],
            &["--upstream-disclaimer", "not operated by Acme"],
            &[
                "--upstream-repository-url",
                "https://example.com/acme/widget",
                "--upstream-disclaimer",
                "not operated by Acme",
            ],
        ];
        for flags in rows {
            assert!(
                parse(flags).is_err(),
                "{flags:?} without --upstream-org must be a clap usage error"
            );
        }
    }

    /// S-012: `--upstream-org` alone parses, and carries only the org.
    ///
    /// The positive half of the rule above. Every named row is a refusal, so a
    /// `requires` written in the wrong direction — the anchor requiring its
    /// optionals — refuses this and reds nothing else.
    ///
    /// **Green on arrival** — nothing is declared yet, so it parses for the
    /// wrong reason today.
    /// Mutation: invert the `requires` direction.
    #[test]
    fn upstream_org_alone_parses() {
        let args = parse(&["--upstream-org", "Acme Org"]).expect("the anchor alone is a valid third-party claim");
        assert_eq!(args.upstream_org.as_deref(), Some("Acme Org"));
        assert_eq!(args.upstream_repository_url, None);
        assert_eq!(args.upstream_disclaimer, None);
    }

    /// C-057: `--format` is the root flag, never a subcommand flag.
    ///
    /// Walks the built `Command` rather than reading `--help` by hand, the same
    /// way `package_announce.rs::no_trusted_host_flag_is_registered` walks it.
    ///
    /// **Green on arrival.**
    /// Mutation: add `#[clap(long = "format")]` to any field.
    #[test]
    fn claim_declares_no_format_flag() {
        let command = PackageClaim::command();
        assert!(
            !command.get_arguments().any(|arg| arg.get_long() == Some("format")),
            "ocx package claim must not shadow the root --format flag"
        );
    }

    /// C-057: the rendered usage line carries `[OPTIONS]` before `<PACKAGE>`.
    ///
    /// Read off `render_usage`, which is what an operator sees, rather than off
    /// the struct's field order.
    ///
    /// **No reachable red for the ordering itself.** Measured, not reasoned:
    /// clap's `Usage::write_arg_usage` writes `[OPTIONS]` unconditionally
    /// before it emits any positional, so `find("[OPTIONS]") < find("<PACKAGE>")`
    /// holds for every field order — declaring `package` above the flags does
    /// **not** red this, against clap 4.6. Two reachable reds remain, neither of
    /// them the ordering: removing every optional argument from the command
    /// panics the `[OPTIONS]` `.expect`, and renaming the `<PACKAGE>`
    /// `value_name` panics the second one — DX-63(a)'s recorded discriminating
    /// mutation.
    ///
    /// C-057's sentence describes the rendered grammar, not a parse rule: clap
    /// interleaves positionals and flags freely, so
    /// `ocx package claim acme/widget --repository X` parses today. What this
    /// assertion is worth keeping for is the cheap net it does hold — the
    /// command still has options and still has a `<PACKAGE>` positional.
    /// Control for the ordering: this assertion plus review.
    #[test]
    fn claim_usage_puts_options_before_the_positional() {
        let usage = PackageClaim::command().render_usage().to_string();
        let options = usage.find("[OPTIONS]").expect("the usage line carries [OPTIONS]");
        let positional = usage.find("<PACKAGE>").expect("the usage line carries the positional");
        assert!(options < positional, "flags precede the positional; got usage: {usage}");
    }

    /// C-057: `--repository` reaches the library **unparsed**.
    ///
    /// The physical pointer is parsed by the existing
    /// `oci::index::parse_physical_repository`, which `claim::root` already
    /// calls — mapping the failure to `ClaimError::MalformedRepository` (exit
    /// 64) and naming the refused value back. A clap `value_parser` would move
    /// that refusal to `ValueValidation` (64 as well, so the exit code cannot
    /// tell them apart) and leave the library guard with no production caller.
    ///
    /// **Green on arrival** — the field is a `String` and nothing parses it.
    /// Mutation: add a `value_parser` that parses the pointer; this reds and
    /// `claim::root`'s `malformed_repository_*` tests lose their only caller.
    #[test]
    fn a_malformed_repository_is_not_refused_at_the_cli() {
        assert!(
            PackageClaim::try_parse_from(["claim", "--repository", "not a pointer", "acme/widget"]).is_ok(),
            "the CLI hands --repository through verbatim; the refusal is the library's"
        );
    }

    /// C-057: both `--owner` wire forms, and every malformed one.
    ///
    /// The ruling this pins is recorded on
    /// [`PackageClaim::owner_specs`](super::PackageClaim::owner_specs) — no
    /// contract names the parser, so the split-on-first-colon rule is a decision
    /// this test makes explicit rather than leaving to whoever writes the code.
    /// Each malformed row is satisfied by a refusal at **either** site, because
    /// the ruling does not fix whether the parser is a clap `value_parser` or a
    /// method: both exit 64.
    ///
    /// Red at the stub: `owner_specs` is `unimplemented!()`.
    /// Mutation once implemented: split on the last `:` (`alice:7:8` becomes
    /// login `alice:7`); parse the id as `i64` (`alice:-1` is accepted).
    #[test]
    fn owner_spec_parses_both_wire_forms() {
        let accepted: &[(&str, OwnerSpec)] = &[
            ("alice", OwnerSpec::Login("alice".to_string())),
            (
                "alice:7",
                OwnerSpec::Resolved {
                    login: "alice".to_string(),
                    id: 7,
                },
            ),
        ];
        for (value, expected) in accepted {
            let args = parse(&["--owner", value]).expect("a well-formed owner parses");
            assert_eq!(
                args.owner_specs().expect("a well-formed owner resolves"),
                vec![expected.clone()],
                "--owner {value} is {expected:?}"
            );
        }

        for value in [
            "alice:",
            ":7",
            "alice:x",
            "alice:7:8",
            "alice:-1",
            "alice:18446744073709551616",
        ] {
            let refused = match parse(&["--owner", value]) {
                Err(_) => true,
                Ok(args) => match args.owner_specs() {
                    Err(error) => {
                        assert_eq!(
                            classify_error(error.as_ref()),
                            ExitCode::UsageError,
                            "a malformed --owner is exit 64"
                        );
                        true
                    }
                    Ok(_) => false,
                },
            };
            assert!(refused, "--owner {value} is not a wire form and must be refused");
        }
    }

    /// C-048: `--owner ""` reaches the library as an empty login, unfiltered.
    ///
    /// `claim::owners` refuses it there, and its own test says why: treating an
    /// empty login as absence silently writes whoever the CI environment names
    /// into a governance field. A CLI parser that rejects the empty string is
    /// also exit 64, but it moves a governance rule to a site that does not
    /// document it and leaves the library guard unreachable.
    ///
    /// Red at the stub: `owner_specs` is `unimplemented!()`.
    /// Mutation once implemented: filter empty logins in the parser.
    #[test]
    fn an_empty_owner_reaches_the_library_unfiltered() {
        let args = parse(&["--owner", ""]).expect("an empty owner parses");
        assert_eq!(
            args.owner_specs().expect("the CLI passes it through"),
            vec![OwnerSpec::Login(String::new())],
            "the empty login is the library's to refuse, not the CLI's to drop"
        );
    }

    /// S-006: `--owner` order is preserved and repeats pass through unmerged.
    ///
    /// `resolve_owners` owns the duplicate rule (`ClaimError::DuplicateOwner`);
    /// a CLI that deduplicates first makes that path unreachable.
    ///
    /// Red at the stub: `owner_specs` is `unimplemented!()`.
    /// Mutation once implemented: deduplicate, or sort, in the parser.
    #[test]
    fn owner_order_is_preserved_and_repeats_pass_through() {
        let args = parse(&["--owner", "bob", "--owner", "alice", "--owner", "bob"]).expect("parses");
        assert_eq!(
            args.owner_specs().expect("passes through"),
            vec![
                OwnerSpec::Login("bob".to_string()),
                OwnerSpec::Login("alice".to_string()),
                OwnerSpec::Login("bob".to_string()),
            ],
            "the order given is the order recorded, and the repeat is the library's to refuse"
        );
    }

    /// S-010: the flag pair selects the write target, and each cell maps to its
    /// own `ClaimTarget`.
    ///
    /// [`super::PackageClaim::target`] decides **which repository gets written
    /// to**, and no other WP-14 test observes it: a `target()` returning
    /// `Direct` under `--out` compiles and passes every other assertion here,
    /// turning a local render into a pull request against the real index.
    ///
    /// **Green on arrival** — `target` is implemented.
    /// Mutation: map `--out` to `ClaimTarget::Direct`; the `--out` row reds.
    /// Measured, and worth recording: merely **reordering** the match arms does
    /// **not** red, because `--out` ⟂ `--fork` is a clap `conflicts_with`, so
    /// the both-present cell is unreachable and every arm order agrees on the
    /// three reachable ones. The discriminating mutation is a changed target,
    /// not a changed order.
    #[test]
    fn target_maps_each_flag_pair_to_its_claim_target() {
        let out = parse(&["--out", "d"]).expect("parses").target();
        assert!(
            matches!(&out, ClaimTarget::Out(directory) if directory == std::path::Path::new("d")),
            "--out writes the entry under the directory it names; got {out:?}"
        );

        let fork = parse(&["--fork", "ocx-contrib/index"]).expect("parses").target();
        assert!(
            matches!(&fork, ClaimTarget::Fork(coordinate) if coordinate.full_path() == "ocx-contrib/index"),
            "--fork opens the request from the fork it names; got {fork:?}"
        );

        let direct = parse(&[]).expect("parses").target();
        assert!(
            matches!(direct, ClaimTarget::Direct),
            "neither flag pushes the claim branch to --index-repo itself; got {direct:?}"
        );
    }

    /// S-012: `--upstream-org` alone builds the object, carrying only the org.
    ///
    /// The sibling `upstream_org_alone_parses` asserts the three **struct
    /// fields**; S-012's contracted outcome is the constructed object, and an
    /// `upstream()` returning `None` unless a sibling flag is present satisfies
    /// that test while violating the contract.
    ///
    /// **Green on arrival** — `upstream` is implemented.
    /// Mutation: return `None` unless `upstream_repository_url` or
    /// `upstream_disclaimer` is present.
    #[test]
    fn upstream_org_alone_builds_the_object_with_only_the_org() {
        let anchor_only = parse(&["--upstream-org", "Acme Org"]).expect("parses").upstream();
        assert_eq!(
            anchor_only,
            Some(Upstream {
                org: "Acme Org".to_string(),
                repository_url: None,
                disclaimer: None,
            }),
            "the anchor alone is a complete third-party declaration"
        );

        assert_eq!(parse(&[]).expect("parses").upstream(), None, "no anchor, no object");
    }

    /// S-037 / C-063: a malformed `--repository` is exit 64 **even with no
    /// credential resolved**.
    ///
    /// The defect this pins is an ordering one, and it is a wrong published exit
    /// code rather than a nit: `--repository` is deliberately unparsed by clap
    /// (R-13), so if its refusal runs after `require_credential` then
    /// `ocx package claim --repository "not a pointer" acme/widget` with no
    /// token exits **80**. A script branching on 80 refreshes credentials for a
    /// typo, and an operator provisions a forge PAT they never needed — the
    /// exact round trip this module's own doc says the ordering exists to
    /// prevent.
    ///
    /// Asserted against [`super::PackageClaim::argv_faults`], which is the whole
    /// set of zero-I/O refusals.
    ///
    /// Mutation: drop the `parse_repository` line from `argv_faults` — this reds
    /// with an `Ok`.
    ///
    /// **What this does not hold, stated rather than implied.** It pins the
    /// parse *inside* `argv_faults`; it does **not** pin that
    /// [`super::PackageClaim::execute`] calls `argv_faults()` before
    /// `require_credential()`. Moving that one line below the credential check
    /// reintroduces exit 80 for a typo and reds nothing here, and no red is
    /// reachable at unit scope: `execute` takes a `crate::app::Context`, which is
    /// built only by `Context::try_init` against a real home, config tier and
    /// registry client. The controls for the *ordering* are WP-16's acceptance
    /// row — a malformed `--repository` with no credential, which is the only
    /// scope that observes an exit code — plus review of this file.
    #[test]
    fn a_malformed_repository_is_an_argv_fault_before_any_credential_is_needed() {
        let args = parse_raw(&["--repository", "not a pointer"]).expect("the CLI hands it through verbatim");
        let error = args
            .argv_faults()
            .expect_err("a malformed physical pointer is decidable with zero I/O");
        assert_eq!(
            classify_error(error.as_ref()),
            ExitCode::UsageError,
            "a malformed --repository is a command-line fault (64), never a missing credential (80)"
        );

        parse_raw(&["--repository", "oci://ghcr.io/acme/widget"])
            .expect("parses")
            .argv_faults()
            .expect("the positive half: a well-formed pointer is not an argv fault");
    }

    /// SEC: `--upstream-repository-url` must be an `http`/`https` URL that
    /// carries no credentials, and the refusal is an argv fault (64).
    ///
    /// The value is written verbatim into the **committed** index root, which a
    /// catalog may render as a link — so a `javascript:` or `data:` value would
    /// need a G-04 reviewer to be the only control, and a
    /// `https://gitlab-ci-token:<token>@host/p.git` (GitLab's own
    /// `CI_REPOSITORY_URL`) would commit a live job token into a public
    /// governance artifact. The rule itself is asserted at library scope in
    /// `claim::request`; what this holds is that the CLI *calls* it, from the
    /// zero-I/O block, at exit 64.
    ///
    /// Both polarities in one function: asserting the refusal alone passes for a
    /// guard that refuses everything.
    ///
    /// Mutation: delete the `validate_upstream_repository_url` call from
    /// `argv_faults` — every refused row reds with an `Ok`.
    #[test]
    fn an_upstream_repository_url_must_carry_an_http_scheme_and_no_credentials() {
        for accepted in [
            "https://example.com/acme/widget",
            "http://example.com/acme/widget",
            "HTTPS://example.com/acme/widget",
            // Accepted, and it reads as a typo: WHATWG folds the extra slash
            // away, so this is host `acme`. Measured in `claim::request`'s own
            // test; the prefix guard this replaced refused it.
            "https:///acme/widget",
        ] {
            parse(&["--upstream-org", "acme", "--upstream-repository-url", accepted])
                .expect("parses")
                .argv_faults()
                .unwrap_or_else(|error| panic!("{accepted} is an ordinary repository URL: {error}"));
        }

        for refused in [
            "https://gitlab-ci-token:glcbt-notarealjobtoken@gitlab.example/group/project.git",
            "https://alice@gitlab.example/group/project.git",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "example.com/acme/widget",
            "//example.com/acme/widget",
            "https://",
        ] {
            let error = parse(&["--upstream-org", "acme", "--upstream-repository-url", refused])
                .expect("the value parses; the refusal is ours")
                .argv_faults()
                .unwrap_err();
            assert_eq!(
                classify_error(error.as_ref()),
                ExitCode::UsageError,
                "{refused} may not be published, so it is an argv fault"
            );
        }
    }

    /// SEC (CWE-532): the `--upstream-repository-url` refusal names the flag and
    /// the rule, and **echoes nothing**.
    ///
    /// Separate from the polarity test above because it guards a different
    /// failure: the guard could be perfectly correct and still print the job
    /// token it just refused onto stderr, where CI captures it into a job log
    /// that outlives the process and is readable by more people than the process
    /// table was. The one realistic input — a forwarded `CI_REPOSITORY_URL` —
    /// is exactly the one that carries a secret.
    ///
    /// The needle is the token substring, not the whole URL: a redaction that
    /// kept the host and dropped the userinfo would still be honest, so pinning
    /// the whole value would over-constrain. Mutation: interpolate `{url}` back
    /// into the message — this reds.
    #[test]
    fn the_upstream_repository_url_refusal_does_not_echo_the_credential() {
        let token = "glcbt-notarealjobtoken";
        let url = format!("https://gitlab-ci-token:{token}@gitlab.example/group/project.git");
        let error = parse(&["--upstream-org", "acme", "--upstream-repository-url", &url])
            .expect("the value parses; the refusal is ours")
            .argv_faults()
            .unwrap_err();

        let rendered = format!("{error:#}");
        assert!(
            !rendered.contains(token),
            "the refused userinfo must not reach stderr: {rendered}"
        );
        assert!(
            rendered.contains("--upstream-repository-url"),
            "the operator still learns which flag was refused: {rendered}"
        );
        assert!(
            rendered.contains("without embedded credentials"),
            "and what the rule is: {rendered}"
        );
    }
}
