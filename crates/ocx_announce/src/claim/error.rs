// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the claim orchestration (C-052, C-073).
//!
//! `ClassifyExitCode` (in the binary, `ocx::exit`) is implemented **explicitly**
//! and the match is **wildcard-free**, for two reasons the announce taxonomy
//! learned the hard way:
//!
//! - [`ClaimError::Forge`] is `#[error(transparent)]`, and thiserror's transparent
//!   forwarding makes `Error::source()` skip straight past the wrapped
//!   [`ForgeError`](crate::forge::ForgeError) to *its own* source. The generic
//!   chain walker in `cli::classify_error` therefore never sees a `ForgeError`
//!   node at all, which is why `ClaimError` must both be registered in that
//!   module's `try_downcast!` ladder **and** delegate here. C-071's plan text
//!   claims the forge-derived codes survive the row's deletion because
//!   `ForgeError` is separately registered; that is false under this design —
//!   deleting the row collapses them to exit 1 as well. Do not "fix" the
//!   asymmetry by making [`ClaimError::Forge`] non-transparent: transparency is
//!   the whole reason C-052 exists.
//! - A wildcard arm (`_ => None`, which `AnnounceError::classify` is stuck with
//!   because its variants predate the rule) makes a variant added later default
//!   silently to exit 1. `ClaimError` is new, so the compiler can be the arity
//!   control from day one: every variant is listed, and adding one without a
//!   decision is an `E0004` build failure rather than a shipped exit 1.

use crate::forge::ForgeError;

/// Failures raised by [`claim`](super::claim).
#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    /// Claim reads the committed index root through a forge in every mode,
    /// `--out` included (C-050); a `None` forge cannot read it.
    ///
    /// Deliberately unclassified (exit 1): a `None` forge is an internal wiring
    /// fault, not an operator error — the CLI boundary refuses a credential-less
    /// write before this is reachable.
    #[error("claim requires a forge to read the committed index root")]
    ForgeRequired,

    /// `--repository` is not a well-formed `oci://host/path` pointer.
    ///
    /// **Not** `#[error(transparent)]` over the index error: that type classifies
    /// to `DataError` (65), and a malformed *flag value* is operator input, which
    /// is `EX_USAGE` (64). C-047's "verbatim" half has no reachable red — the
    /// parse demands an exact `Identifier` round-trip, so every accepted value
    /// reconstructs byte-identically — and this refusal is the half that does.
    #[error("malformed --repository {value}: expected oci://host/path")]
    MalformedRepository { value: String },

    /// A root is already committed at `path` on `base_ref` (C-050).
    ///
    /// The **base ref** is what is read, never the claim branch: an idempotent
    /// re-run of an unmerged claim must report `unchanged`, not 65. Holds in
    /// every mode, `--out` included, and before anything is written there.
    #[error("package already claimed: {path} exists on {base_ref} for {package} — use `ocx package announce`")]
    PackageAlreadyClaimed {
        package: String,
        path: String,
        base_ref: String,
    },

    /// The owner ladder reached its terminal rung with nothing to write (C-048).
    #[error("no acting identity: the credential has no account and the CI environment named none — pass --owner")]
    NoActingIdentity,

    /// A supplied login carries a character outside `[A-Za-z0-9._-]` (C-067).
    ///
    /// The login is the **only** operator free-text channel into a request body a
    /// human merges under G-04: the logical name and the physical repository are
    /// already constrained to `[a-z0-9._-]`, and the `--upstream-*` values reach
    /// the root file alone. Refusing the charset at the point the login enters
    /// the body is what makes C-067's "no operator-supplied string is
    /// interpolated" literally true rather than a provenance-label argument.
    #[error("invalid owner login {login}: expected only letters, digits, dot, underscore or hyphen")]
    InvalidOwnerLogin { login: String },

    /// The same login was supplied twice, verbatim, on either path.
    ///
    /// The refusal runs over the *supplied* spellings, beside the bot-shape and
    /// charset guards, so the exit code does not depend on whether the users API
    /// happens to be reachable — a duplicated literal in a governance field that
    /// gates auto-merge is an operator error either way. Two spellings that differ
    /// only in case are not this: a confirmed list collapses them by resolved id,
    /// which is C-048's canonical-login override doing its job.
    #[error("duplicate owner {login}: each owner is named once")]
    DuplicateOwner { login: String },

    /// The forge has no account with this login (C-048).
    ///
    /// Reserved for a login the forge genuinely does not know. A supplied id that
    /// disagrees is [`Self::OwnerIdMismatch`] — the login lookup runs first and is
    /// case-insensitive, so case can never be the discriminator.
    #[error("unknown owner {login}: the forge has no such account")]
    OwnerUnknown { login: String },

    /// A supplied `LOGIN:ID` whose id disagrees with the forge's (C-048).
    #[error("owner {login} has id {actual} on the forge, not {supplied}")]
    OwnerIdMismatch { login: String, supplied: u64, actual: u64 },

    /// The acting identity is a bot (C-049).
    ///
    /// Refused on **every** list, explicit or detected: an operator typing
    /// `--owner dependabot[bot]` is the more likely mistake, not the less, and
    /// under the narrower "detected only" reading the weak login-shape form would
    /// be dead code — unconfirmed lists are overwhelmingly `--owner`-supplied.
    #[error("owner {login} is a bot account; name a human owner with --owner")]
    BotIdentity { login: String },

    /// The index base ref does not exist.
    ///
    /// Deliberately unclassified (exit 1), the `GitCommandFailed` /
    /// `GitPushFailed` precedent: the base ref is a constant, so a fire means an
    /// upstream invariant broke. Asserted as `None` rather than omitted.
    #[error("index base ref {base_ref} does not exist on {repo}")]
    MissingBaseRef { repo: String, base_ref: String },

    /// The `NonFastForward` retry re-read the winning head and found no root
    /// there.
    ///
    /// Unclassified for the same reason as [`Self::MissingBaseRef`]. Never
    /// defaulted to the freshly-rendered root: that silently clobbers the
    /// concurrent writer whose commit won the race.
    #[error("no root at {path} on the winning head of {branch} after a non-fast-forward retry")]
    MissingHeadRoot { branch: String, path: String },

    /// Writing the `--out` tree failed.
    ///
    /// `cli/classify.rs`'s bare-`io::Error` walker special-cases only
    /// `PermissionDenied`, so every other kind would land on `Failure` (1) —
    /// indistinguishable from a crash to a release wrapper.
    #[error("failed to write {path}")]
    OutputWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A committed root whose `name` disagrees with the identifier the re-claim
    /// names ([#477]).
    ///
    /// Reachable on the **re-claim** path alone: a fresh claim renders `name`
    /// from the identifier and cannot disagree with itself. Announce's sibling
    /// is [`AnnounceError::RootNameMismatch`](crate::announce::AnnounceError::RootNameMismatch),
    /// and both read the expected value from [`root_name`](super::root_name) —
    /// one spelling, two callers.
    ///
    /// [#477]: https://github.com/ocx-sh/ocx/issues/477
    #[error("committed root names {committed}, not the {expected} this claim names")]
    RootNameMismatch { committed: String, expected: String },

    /// A re-claim supplied a `--repository` other than the committed one.
    ///
    /// Refused rather than updated: the pointer decides where a package's bytes
    /// come from, and repointing it must not be a side effect of adding an
    /// owner. The flag is required on every claim, so an operator adding an
    /// owner types the current value anyway.
    #[error("committed root points at {committed}, not the supplied {supplied}")]
    RepositoryMismatch { committed: String, supplied: String },

    /// A description observation failed while rendering the claim's root.
    ///
    /// Transparent over the announce taxonomy rather than a parallel one: claim
    /// runs `announce::pipeline::observe_desc` verbatim, so `Ssrf`,
    /// `ObserveDesc` and `DescDisappeared` mean exactly what they mean under
    /// announce — and classify to exactly the same codes, because
    /// `ClaimError::classify` delegates to the inner error.
    #[error(transparent)]
    Description(#[from] crate::announce::AnnounceError),

    /// Any forge failure, classified by [`ForgeError`] itself.
    #[error(transparent)]
    Forge(#[from] ForgeError),
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Every [`ClaimError`] variant, constructed once and reused by both guards
    /// below so the two cannot cover different sets.
    fn every_variant() -> Vec<ClaimError> {
        vec![
            ClaimError::ForgeRequired,
            ClaimError::MalformedRepository {
                value: "ghcr.io/acme/widget".to_string(),
            },
            ClaimError::PackageAlreadyClaimed {
                package: "acme/widget".to_string(),
                path: "p/acme/widget.json".to_string(),
                base_ref: "main".to_string(),
            },
            ClaimError::NoActingIdentity,
            ClaimError::InvalidOwnerLogin {
                login: "@alice".to_string(),
            },
            ClaimError::DuplicateOwner {
                login: "alice".to_string(),
            },
            ClaimError::OwnerUnknown {
                login: "nobody".to_string(),
            },
            ClaimError::OwnerIdMismatch {
                login: "alice".to_string(),
                supplied: 8,
                actual: 7,
            },
            ClaimError::BotIdentity {
                login: "dependabot[bot]".to_string(),
            },
            ClaimError::MissingBaseRef {
                repo: "ocx-sh/index".to_string(),
                base_ref: "main".to_string(),
            },
            ClaimError::MissingHeadRoot {
                branch: "indexbot-claim-acme-widget".to_string(),
                path: "p/acme/widget.json".to_string(),
            },
            ClaimError::OutputWrite {
                path: "/out/p/acme/widget.json".to_string(),
                source: std::io::Error::other("no space left on device"),
            },
            ClaimError::RootNameMismatch {
                committed: "ocx.sh/acme/widget".to_string(),
                expected: "ghcr.io/acme/widget".to_string(),
            },
            ClaimError::RepositoryMismatch {
                committed: "oci://ghcr.io/acme/widget".to_string(),
                supplied: "oci://quay.io/acme/widget".to_string(),
            },
            ClaimError::Description(crate::announce::AnnounceError::DescDisappeared {
                repository: "oci://ghcr.io/acme/widget".to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
            }),
            ClaimError::Forge(ForgeError::UsersApiUnavailable),
        ]
    }

    /// C-073 — every message is lowercase-initial, unpunctuated, carries no
    /// redundant `error:` prefix, and names no credential.
    ///
    /// `new_error_messages_follow_style` scans `forge/error.rs` and is a different
    /// package's test; this is its `claim/error.rs` sibling. The arity assertion
    /// against [`every_variant`] is what stops a variant added later from escaping
    /// the style rule silently.
    ///
    /// An all-uppercase first word is admitted as an acronym; a proper noun would
    /// red here, which is the safe direction — a false red an author must answer,
    /// rather than a false green that ships.
    ///
    /// Reds on: sentence-casing any message, giving one a trailing period, or
    /// adding a variant without extending [`every_variant`].
    #[test]
    fn claim_error_messages_follow_style() {
        let variants = every_variant();
        assert_eq!(
            variants.len(),
            16,
            "every ClaimError variant owes a style-checked message; extend this list when one is added"
        );

        for error in &variants {
            let message = error.to_string();
            assert!(!message.is_empty(), "a variant rendered nothing");
            assert!(
                !message.ends_with(['.', '!', '?']),
                "C-GOOD-ERR wants no trailing punctuation: {message}"
            );
            let first_word = message.split_whitespace().next().unwrap_or_default();
            let mut characters = first_word.chars();
            let sentence_case = characters.next().is_some_and(char::is_uppercase) && characters.any(char::is_lowercase);
            assert!(
                !sentence_case,
                "C-GOOD-ERR wants a lowercase first word (an all-caps acronym is fine): {message}"
            );
            assert!(
                !message.to_ascii_lowercase().starts_with("error:"),
                "`Error` already categorises the line: {message}"
            );
            for secret in ["ghp_", "glpat-", "Bearer ", "JOB-TOKEN"] {
                assert!(!message.contains(secret), "no credential shape in a message: {message}");
            }
        }
    }

    /// C-073 — every wrapping variant exposes its inner error through `source()`.
    ///
    /// Without it the chain walk breaks for logging and diagnostics, and the two
    /// wrapping variants here are exactly the ones a reader would expect to walk.
    /// [`ClaimError::Forge`] is `#[error(transparent)]`, so its `source()`
    /// forwards past `ForgeError` to *its* source — which for a unit variant is
    /// `None`, and that is the shape C-052 exists to compensate for, not a defect.
    ///
    /// Reds on: dropping `#[source]` from `OutputWrite`.
    #[test]
    fn wrapping_variants_expose_their_source() {
        use std::error::Error as _;

        let write = ClaimError::OutputWrite {
            path: "/out/p/acme/widget.json".to_string(),
            source: std::io::Error::other("no space left on device"),
        };
        assert!(
            write.source().is_some(),
            "OutputWrite must expose the io::Error it wraps"
        );
    }

    /// C-070 — the `#[non_exhaustive]` policy over `src/claim/**`.
    ///
    /// The existing `non_exhaustive_policy_holds` is scoped to `src/forge/**` and
    /// `forge.rs`, so it cannot see this module tree at all. Without this sibling,
    /// `ClaimError`, `OwnerIdentitySource`, `ClaimTarget`, `ClaimStatus` and
    /// `OwnerSpec` ship unguarded.
    ///
    /// **The invariant changed, and `ClaimError` is no longer exempt.** The
    /// error-enum exemption was written while nothing outside `ocx_lib` matched
    /// these values, because `#[non_exhaustive]` is inert inside the declaring
    /// crate. The exit-code ladder now lives in the binary, so the attribute
    /// stopped being inert and started buying the one thing it must not buy
    /// here: a new variant reaching a `_ =>` arm and being classified by
    /// nobody. Zero exempt enums anywhere under `claim`, `error.rs` included.
    ///
    /// **A count of zero cannot be its own positive half.** Zero is exactly
    /// what a scanner whose needle stopped matching returns, so the needle is
    /// first demonstrated against text this test owns: it must find the
    /// attribute in a line that has it, and must not find it in a commented-out
    /// one. Only a scanner shown working makes the zeros below evidence. The
    /// old positive half — `error.rs` counts exactly one — cannot serve, because
    /// the correct answer there is now zero too.
    ///
    /// The literal stays **split**, so the line defining the scan does not match
    /// the scan, and the walk assertion stays, so a scan over no files cannot
    /// pass either.
    ///
    /// Reds on: adding the attribute to any claim enum including `ClaimError`
    /// (negative half), and a needle that stops matching (positive control).
    /// Both were run.
    #[test]
    fn non_exhaustive_policy_holds_for_claim() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let claim_root = source_root.join("claim");
        let mut files = rust_sources(&claim_root);
        files.push(source_root.join("claim.rs"));
        files.sort();

        // Split so this line does not match the scan it defines.
        let attribute = concat!("#[non_", "exhaustive]");
        let count_text = |text: &str| -> usize {
            text.lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| line.contains(attribute))
                .count()
        };
        let count = |path: &Path| -> usize {
            count_text(&std::fs::read_to_string(path).expect("a claim source file is readable"))
        };

        // The positive control, and the reason it is not a count of one
        // somewhere: every real answer below is now zero, which is also what a
        // needle that stopped matching returns. So the needle is proved against
        // text this test owns before any zero is read as evidence.
        let probe = format!("{attribute}\npub enum Probe {{}}\n    // {attribute}\n");
        assert_eq!(
            count_text(&probe),
            1,
            "the scanner must find the attribute on a real line and ignore a commented one; a broken needle would \
             report zero here and zero for every file below, and the two are indistinguishable"
        );

        let offenders: Vec<&Path> = files
            .iter()
            .filter(|path| count(path) > 0)
            .map(PathBuf::as_path)
            .collect();
        assert!(
            offenders.is_empty(),
            "every claim enum stays closed, `ClaimError` included, so each match over one is total and a new variant \
             is a compile error in the binary's exit-code ladder rather than an unclassified value: {offenders:?}"
        );

        assert!(
            files.len() > 1
                && files.contains(&claim_root.join("error.rs"))
                && files.contains(&claim_root.join("request.rs"))
                && files.contains(&claim_root.join("owners.rs"))
                && files.contains(&claim_root.join("root.rs")),
            "the walk did not reach the claim module, so it scanned nothing: {files:?}"
        );
    }

    /// Every `.rs` file under `root`, recursing so a submodule directory added
    /// later cannot silently drop out of the scan.
    fn rust_sources(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory)
                .expect("the claim source tree is readable")
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }
        files
    }
}
