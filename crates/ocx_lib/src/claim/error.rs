// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the claim orchestration (C-052, C-073).
//!
//! [`ClassifyExitCode`](crate::cli::ClassifyExitCode) is implemented **explicitly**
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

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::forge::ForgeError;

/// Failures raised by [`claim`](super::claim).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    #[error("namespace already claimed: {path} exists on {base_ref} for {package} — use `ocx package announce`")]
    NamespaceAlreadyClaimed {
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

    /// Any forge failure, classified by [`ForgeError`] itself.
    #[error(transparent)]
    Forge(#[from] ForgeError),
}

impl ClassifyExitCode for ClaimError {
    /// The contracted mapping, **wildcard-free** so a later variant is an
    /// `E0004` rather than a silent exit 1:
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | `ForgeRequired`, `MissingBaseRef`, `MissingHeadRoot` | `None` — broken invariant, exit 1 |
    /// | `MalformedRepository`, `NoActingIdentity`, `InvalidOwnerLogin`, `DuplicateOwner`, `OwnerIdMismatch`, `BotIdentity` | `UsageError` (64) |
    /// | `NamespaceAlreadyClaimed` | `DataError` (65) — S-037's "already claimed, go announce" |
    /// | `OwnerUnknown` | `NotFound` (79) |
    /// | `OutputWrite` | `IoError` (74) |
    /// | `Forge(inner)` | `inner.classify()` — explicit, see the module doc |
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // A broken invariant, not an operator error — exit 1, by decision.
            Self::ForgeRequired | Self::MissingBaseRef { .. } | Self::MissingHeadRoot { .. } => None,
            Self::MalformedRepository { .. }
            | Self::NoActingIdentity
            | Self::InvalidOwnerLogin { .. }
            | Self::DuplicateOwner { .. }
            | Self::OwnerIdMismatch { .. }
            | Self::BotIdentity { .. } => Some(ExitCode::UsageError),
            Self::NamespaceAlreadyClaimed { .. } => Some(ExitCode::DataError),
            Self::OwnerUnknown { .. } => Some(ExitCode::NotFound),
            Self::OutputWrite { .. } => Some(ExitCode::IoError),
            // Explicit, never inherited: `#[error(transparent)]` forwards
            // `source()` past `ForgeError`, so the generic chain walker never
            // sees this node.
            Self::Forge(inner) => inner.classify(),
        }
    }
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
            ClaimError::NamespaceAlreadyClaimed {
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
            ClaimError::Forge(ForgeError::UsersApiUnavailable),
        ]
    }

    /// C-052 — the three deliberately **unclassified** variants answer `None`,
    /// asserted rather than omitted.
    ///
    /// The `GitCommandFailed` / `GitPushFailed` precedent: an unclassified variant
    /// is a decision, and a decision that is never asserted is indistinguishable
    /// from an oversight. All three describe a broken invariant — a `None` forge,
    /// a missing base ref, a missing head root after a retry — which is an
    /// internal error, so exit 1 is the honest code.
    ///
    /// The compiler holds the other half: `ClaimError::classify` is wildcard-free,
    /// so a variant added later without an arm is an `E0004` build failure. That
    /// arity control is not a test row — a mutation that breaks the build is not a
    /// red — and this is its runtime complement.
    ///
    /// Reds on: classifying any of the three (say `MissingBaseRef` → 65).
    #[test]
    fn unclassified_variants_stay_unclassified() {
        for error in [
            ClaimError::ForgeRequired,
            ClaimError::MissingBaseRef {
                repo: "ocx-sh/index".to_string(),
                base_ref: "main".to_string(),
            },
            ClaimError::MissingHeadRoot {
                branch: "indexbot-claim-acme-widget".to_string(),
                path: "p/acme/widget.json".to_string(),
            },
        ] {
            assert_eq!(
                error.classify(),
                None,
                "a broken invariant exits 1 by decision, not by omission: {error}"
            );
        }
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
            13,
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
    /// Two defences carried over verbatim. The literal is **split**, so the line
    /// defining the scan does not match the scan. And the positive half pins
    /// `error.rs` at exactly **one** — without it, a zero everywhere is equally
    /// the answer a scanner that recognises nothing returns, and an arity of one
    /// is the only way a filename-scoped guard notices a second exempt-by-accident
    /// enum moving into the same file.
    ///
    /// Reds on: adding the attribute to any non-error claim enum (negative half),
    /// removing it from `ClaimError` (positive half), and declaring a second
    /// `#[non_exhaustive]` enum in `error.rs` (positive half, arity).
    #[test]
    fn non_exhaustive_policy_holds_for_claim() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let claim_root = source_root.join("claim");
        let mut files = rust_sources(&claim_root);
        files.push(source_root.join("claim.rs"));
        files.sort();

        // Split so this line does not match the scan it defines.
        let attribute = concat!("#[non_", "exhaustive]");
        let count = |path: &Path| -> usize {
            std::fs::read_to_string(path)
                .expect("a claim source file is readable")
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| line.contains(attribute))
                .count()
        };

        let error_source = claim_root.join("error.rs");
        assert_eq!(
            count(&error_source),
            1,
            "`claim/error.rs` holds exactly one exempt enum, `ClaimError`. Zero means it lost the attribute or the \
             scanner stopped recognising it, and the zero counts below then prove nothing; more than one means a \
             non-error enum is being exempted by filename"
        );

        let offenders: Vec<&Path> = files
            .iter()
            .filter(|path| path.as_path() != error_source.as_path())
            .filter(|path| count(path) > 0)
            .map(PathBuf::as_path)
            .collect();
        assert!(
            offenders.is_empty(),
            "internal claim enums stay closed so every match over one is total: {offenders:?}"
        );

        assert!(
            files.len() > 1
                && files.contains(&error_source)
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
