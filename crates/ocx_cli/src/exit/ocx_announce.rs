// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the announce / claim / forge error family — the `ocx_announce` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_announce::announce::AnnounceError;
use ocx_announce::claim::ClaimError;
use ocx_announce::forge::{ForgeError, is_server_fault};

use super::{ClassifyErrorKind, ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for AnnounceError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Delegated explicitly — see the module doc for why the generic
            // source-chain walker cannot reach any of these on its own.
            Self::Ssrf { source, .. } => source.classify(),
            Self::Forge(inner) => inner.classify(),
            Self::Observe { source, .. } => source.classify(),
            Self::ObserveDesc { source, .. } => source.classify(),
            Self::ListTags { source, .. } => source.classify(),
            // The description tag resolved once (it is recorded in the
            // committed root) and does not now. Nothing is malformed on the
            // wire and nothing the publisher typed is at fault, but the two
            // sides of the announce genuinely disagree — the malformed-input
            // category, same as `TagIsNotAnImageIndex`, and discriminable from
            // an unclassified crash.
            Self::DescDisappeared { .. } => Some(ExitCode::DataError),
            // The identifier parses, the root exists and is well-formed: the
            // two sides simply name different packages, and only a human can
            // say which one is right. `DescDisappeared`'s category exactly, and
            // deliberately not `UsageError` (64) — nothing about the command
            // line is malformed, and 64 has to keep meaning "your flags are
            // wrong" for the comments above it to stay true.
            Self::RootNameMismatch { .. } => Some(ExitCode::DataError),
            // The two sides disagree and only a human can say which is right —
            // the same category and the same precedent as `DescDisappeared`.
            // Never `TempFail`: a rerun reproduces it exactly, so inviting a
            // retry would loop a publisher on a defect that needs reporting.
            Self::CommittedTagsDropped { .. } => Some(ExitCode::DataError),
            // A publisher typo — the tag genuinely does not exist on the
            // physical registry. Same category as `ClientError::ManifestNotFound`.
            Self::UnresolvedTag { .. } => Some(ExitCode::NotFound),
            // The index root genuinely does not exist yet — the same
            // absent-resource shape as `UnresolvedTag`, and the likeliest
            // first-run outcome for a new publisher. Left unclassified it
            // exits 1, indistinguishable from a crash, so a release wrapper
            // cannot tell "claim the package first" (a one-time human
            // action, register R3) from an unclassified failure.
            Self::UnclaimedPackage { .. } => Some(ExitCode::NotFound),
            // The tag resolved and the artifact exists — its *shape* is wrong.
            // `NotFound` (79) would be a lie (nothing is absent) and leaving it
            // unclassified exits 1, which a release wrapper cannot tell apart
            // from a crash. `EX_DATAERR` is the malformed-input category.
            Self::TagIsNotAnImageIndex { .. } => Some(ExitCode::DataError),
            // The tag selection is operator input — a `--tags` list, a tags
            // file, or a committed root the publisher curated. Nothing is
            // absent and nothing is malformed on the wire: the invocation
            // named no version. `EX_USAGE` is that category, and it keeps the
            // all-reserved collapse discriminable from an unclassified crash.
            Self::NoCuratedTags { .. } => Some(ExitCode::UsageError),
            // The branch was rebuilt onto the current base and the request
            // still will not merge: nothing is malformed and nothing is absent,
            // the two sides genuinely disagree and only a human clears it —
            // `DescDisappeared`'s category exactly. `TempFail` (75) would invite
            // a retry that can never succeed, and an unclassified 1 is the crash
            // code, which is how #399 stayed invisible in the first place.
            Self::PullRequestUnmergeable { .. } => Some(ExitCode::DataError),
            // Writing the `--out` tree failed: a full disk, an `ENOTDIR`, a
            // read-only mount. `cli/classify.rs`'s bare-`io::Error` walker
            // special-cases only `PermissionDenied`, so every other kind lands
            // on `Failure` (1) — indistinguishable from a crash to a release
            // wrapper. `EX_IOERR` is the category the rest of the tool uses for
            // an operator/environment I/O failure.
            Self::OutputWrite { .. } => Some(ExitCode::IoError),
            // Every other variant falls through to `ExitCode::Failure`.
            _ => None,
        }
    }
}

impl ClassifyExitCode for ForgeError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // 401/403 — the bearer `OCX_ANNOUNCE_TOKEN` is missing, revoked,
            // or lacks scope. The fix is a credential, not a config file or a
            // retry (design register C13).
            Self::Status { status, .. } if *status == 401 || *status == 403 => Some(ExitCode::AuthError),
            // Same class as the 403 above, reached by a probe instead of a
            // rejected write: the fix is a credential with more permission.
            Self::PushAccessDenied { .. } => Some(ExitCode::AuthError),
            // 429 — a secondary rate limit. The request was well-formed and the
            // credential is fine; the same call succeeds after a backoff, so a
            // CI wrapper must be able to tell it apart from bad input.
            Self::Status { status, .. } if *status == 429 => Some(ExitCode::TempFail),
            // 5xx — a forge-side incident. The request completed, so it is not
            // `Transport`, but the forge is just as unavailable and a retry is
            // just as reasonable; without this it is indistinguishable from
            // malformed input at exit 1.
            Self::Status { status, .. } if is_server_fault(*status) => Some(ExitCode::Unavailable),
            // A request never completed (connect, TLS, timeout, DNS, or read
            // failure) — the forge itself is unreachable.
            Self::Transport { .. } => Some(ExitCode::Unavailable),
            // A persistent non-fast-forward (heavy branch contention the one
            // in-announce retry did not clear) is a transient failure the
            // caller may retry (design register C4). A stale lease is the git
            // transport's spelling of the same race, and an unconfirmed merge
            // request is a server that was simply slower than the poll — all
            // three are answered by running the command again.
            Self::NonFastForward { .. } | Self::StaleLease { .. } | Self::MergeRequestUnconfirmed { .. } => {
                Some(ExitCode::TempFail)
            }
            // `git` is missing or too old. Nothing about the invocation is
            // wrong and no credential is involved: the host lacks a tool the
            // run needs, which is exactly what `EX_UNAVAILABLE` means. Raised
            // before any network call.
            Self::GitUnavailable { .. } => Some(ExitCode::Unavailable),
            // A protected branch or a pre-receive hook said no. The credential
            // authenticated fine and the request was well-formed; the caller
            // simply may not write there — the same reading `EX_NOPERM` carries
            // for a filesystem `EPERM`.
            Self::PushRefused { .. } => Some(ExitCode::PermissionDenied),
            // A capability the transport needs is disabled on the project or
            // absent from the instance. Deliberately not 80 (the credential is
            // valid), not 81 (no local policy refused anything) and not 69 (the
            // forge is up and answering): the remedy is an administrator
            // changing a setting, which is a state a pipeline must be able to
            // tell apart from all three.
            Self::WriteCapabilityUnavailable { .. } => Some(ExitCode::ForgeCapabilityUnavailable),
            // A malformed invocation, not a failure of the run: the operator
            // named a self-hosted host without saying which forge runs there,
            // asked GitHub for a nested namespace it cannot express, or pointed
            // `--fork` at the namespace that already owns the index. Each is
            // fixed by editing the command line, which is what `EX_USAGE` means
            // — and a CI wrapper must be able to tell "your flags are wrong"
            // from "the forge said no".
            //
            // The three transport-shaped refusals join them for the same
            // reason. `TransportUnsupported` and `TransportOperationUnsupported`
            // are both fixed by changing `--transport` or dropping `--fork`, and
            // `UsersApiUnavailable` is fixed by writing the owner as `LOGIN:ID`
            // — none of them is a failure of the forge or of the credential.
            Self::ForgeKindUnknown { .. }
            | Self::NestedNamespaceUnsupported { .. }
            | Self::SelfForkRefused { .. }
            | Self::ForkHostMismatch { .. }
            | Self::InvalidRepoCoordinate { .. }
            | Self::TransportUnsupported { .. }
            | Self::TransportOperationUnsupported { .. }
            | Self::UsersApiUnavailable => Some(ExitCode::UsageError),
            // Every other status code and every other variant is not yet
            // classified beyond the sysexits default (`ExitCode::Failure`).
            // `GitCommandFailed` and `GitPushFailed` land here **deliberately**:
            // each is an unrecognised failure of a plumbing step, with no remedy
            // a caller could branch on, so inventing a code for either would be
            // worse than exit 1 with git's own message attached.
            _ => None,
        }
    }
}

impl ClassifyExitCode for ClaimError {
    /// The contracted mapping, **wildcard-free** so a later variant is an
    /// `E0004` rather than a silent exit 1:
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | `ForgeRequired`, `MissingBaseRef`, `MissingHeadRoot` | `None` — broken invariant, exit 1 |
    /// | `MalformedRepository`, `NoActingIdentity`, `InvalidOwnerLogin`, `DuplicateOwner`, `OwnerIdMismatch`, `BotIdentity` | `UsageError` (64) |
    /// | `PackageAlreadyClaimed`, `RootNameMismatch`, `RepositoryMismatch` | `DataError` (65) |
    /// | `OwnerUnknown` | `NotFound` (79) |
    /// | `OutputWrite` | `IoError` (74) |
    /// | `Description(inner)`, `Forge(inner)` | `inner.classify()` — explicit, see the module doc |
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
            Self::PackageAlreadyClaimed { .. } => Some(ExitCode::DataError),
            // The committed root and the command line disagree about which
            // package this is, or about where its bytes come from. Nothing is
            // malformed and nothing is absent; a human decides which side
            // moves — announce's `RootNameMismatch` category exactly. A
            // separate arm rather than an alternative joined onto the one
            // above: that one is a frozen classification row, and widening its
            // pattern re-points the row instead of adding beside it.
            Self::RootNameMismatch { .. } | Self::RepositoryMismatch { .. } => Some(ExitCode::DataError),
            Self::OwnerUnknown { .. } => Some(ExitCode::NotFound),
            Self::OutputWrite { .. } => Some(ExitCode::IoError),
            // Explicit, never inherited, for both: `#[error(transparent)]`
            // forwards `source()` past the wrapped error, so the generic chain
            // walker never sees either node. Delegating rather than minting a
            // code keeps a claim's description failure exiting exactly as the
            // same failure does under announce.
            Self::Description(inner) => inner.classify(),
            Self::Forge(inner) => inner.classify(),
        }
    }
}

impl ClassifyErrorKind for ClaimError {
    /// The one mapping, read through [`ClassifyExitCode`]; the three
    /// unclassified variants surface as the generic exit 1 they already are.
    fn exit_code(&self) -> ExitCode {
        self.classify().unwrap_or(ExitCode::Failure)
    }

    /// Frozen contract C-S1-1: the snake_case parallel of the variant name.
    /// Exhaustive — adding a variant forces a new arm here. `Forge` is one
    /// slug, not the wrapped error's: `#[error(transparent)]` hides the
    /// `ForgeError` node from the envelope's chain walk, so nothing finer
    /// is reachable there.
    fn kind_detail(&self) -> &'static str {
        match self {
            Self::ForgeRequired => "forge_required",
            Self::MalformedRepository { .. } => "malformed_repository",
            Self::PackageAlreadyClaimed { .. } => "package_already_claimed",
            Self::RootNameMismatch { .. } => "root_name_mismatch",
            Self::RepositoryMismatch { .. } => "repository_mismatch",
            Self::Description(_) => "description",
            Self::NoActingIdentity => "no_acting_identity",
            Self::InvalidOwnerLogin { .. } => "invalid_owner_login",
            Self::DuplicateOwner { .. } => "duplicate_owner",
            Self::OwnerUnknown { .. } => "owner_unknown",
            Self::OwnerIdMismatch { .. } => "owner_id_mismatch",
            Self::BotIdentity { .. } => "bot_identity",
            Self::MissingBaseRef { .. } => "missing_base_ref",
            Self::MissingHeadRoot { .. } => "missing_head_root",
            Self::OutputWrite { .. } => "output_write",
            Self::Forge(_) => "forge",
        }
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, ForgeError);
    downcast_arm!(cause, AnnounceError);
    downcast_arm!(cause, ClaimError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_announce::forge::{CapabilityName, ForgeKind, WriteTransport, redact};

    use ocx_announce::claim::INDEX_BASE_REF;

    // ── moved from ocx_announce::announce::error with the impl ──

    #[test]
    fn ssrf_variant_classifies_via_the_inner_error() {
        let error = AnnounceError::Ssrf {
            namespace: "ocx.sh".to_string(),
            source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                host: "127.0.0.1".to_string(),
                ip: "127.0.0.1".parse().expect("valid ip literal"),
            },
        };
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
        // ocx#455: the message names the exact config entry, so an operator
        // who keyed the exemption on the physical host learns which key to use.
        assert!(
            error.to_string().contains("[registries.\"ocx.sh\"].trusted_hosts"),
            "got: {error}"
        );
    }

    #[test]
    fn forge_variant_classifies_via_the_inner_error() {
        let error = AnnounceError::Forge(ocx_announce::forge::ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 401,
            detail: String::new(),
        });
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    /// Observed live on the publisher E2E (run 30133426034): announcing into a
    /// package with no committed root is the likeliest first-run outcome for
    /// a new publisher, and register R3 makes claiming it a one-time human
    /// action. It must be discriminable from a generic failure so a release
    /// wrapper can say so, rather than surfacing exit 1.
    #[test]
    fn unclaimed_package_classifies_as_not_found() {
        let error = AnnounceError::UnclaimedPackage {
            package: "acme/widget".to_string(),
            path: "p/acme/widget.json".to_string(),
            base_ref: "main".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::NotFound));
    }

    /// A `--tags-from-registry` run reaches the registry twice — once to list,
    /// once per tag to observe — and a failure at either point is the same class
    /// of problem. Delegating both to the inner error keeps a listing failure
    /// from collapsing to exit 1, where a caller could not tell "the registry is
    /// unreachable" from a crash.
    ///
    /// The fixture is the value the producer can actually raise, which it was
    /// not before: `AnnounceError::ListTags` carried a `Box<ocx_lib::Error>`
    /// until WP-35 and this test built an `OfflineMode`, a variant
    /// `Publisher::list_tags` never returns. Now it is the two-hop shape —
    /// a transport fault inside an index error inside the package tier's — so
    /// the assertion covers the chain the extraction actually re-pointed, and
    /// the expected code is the `ClientError`'s own rather than a literal
    /// copied down from the arm.
    #[test]
    fn list_tags_variant_classifies_via_the_inner_error() {
        let transport =
            ocx_oci::client::error::ClientError::Authentication(Box::new(std::io::Error::other("bad creds")));
        let expected = transport.classify();
        let error = AnnounceError::ListTags {
            repository: "oci://ghcr.io/acme/widget".to_string(),
            source: Box::new(ocx_package::error::Error::Index(ocx_index::error::Error::OciClient(
                transport,
            ))),
        };
        assert!(
            expected.is_some(),
            "the fixture's own transport fault classifies to nothing, so the delegation below \
             would agree even if it delegated nowhere"
        );
        assert_eq!(error.classify(), expected);
        let message = error.to_string();
        assert!(
            message.contains("oci://ghcr.io/acme/widget"),
            "the message must name the repository whose listing failed: {message}"
        );
    }

    /// The D4(a) refusal is a verdict a release wrapper must be able to act on.
    /// Left unclassified it exits 1 — indistinguishable from a crash, the same
    /// defect the `UnclaimedPackage` comment above records.
    #[test]
    fn tag_is_not_an_image_index_classifies_as_data_error() {
        let error = AnnounceError::TagIsNotAnImageIndex {
            tag: "1.2.3".to_string(),
            repository: "oci://ghcr.io/acme/widget".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let message = error.to_string();
        assert!(message.contains("1.2.3"), "the message must name the tag: {message}");
        assert!(
            message.contains("oci://ghcr.io/acme/widget"),
            "the message must name the repository: {message}"
        );
    }

    /// The D7 filter routed a new failure class into this variant: a selection
    /// made *entirely* of reserved tags. Left as it was, stderr claimed "no
    /// curated tags given" for an invocation that gave several, and the exit
    /// was an unclassified 1. Both halves are pinned here.
    #[test]
    fn all_reserved_selection_names_the_dropped_tags_and_exits_usage_error() {
        let error = AnnounceError::NoCuratedTags {
            reserved_dropped: vec![
                "__ocx.desc".to_string(),
                format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            ],
        };
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
        let message = error.to_string();
        assert!(
            message.contains("__ocx.desc"),
            "the message must name the tags: {message}"
        );
        assert!(
            message.contains(&format!("__ocx.keep.sha256-{}", "a".repeat(64))),
            "the message must name the tags: {message}"
        );
        assert!(
            !message.contains("no curated tags given"),
            "claiming none were given is the defect: {message}"
        );
    }

    /// The genuinely-empty selection keeps the original wording.
    #[test]
    fn an_empty_selection_still_reports_that_none_were_given() {
        let error = AnnounceError::NoCuratedTags {
            reserved_dropped: Vec::new(),
        };
        assert_eq!(error.to_string(), "no curated tags given");
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
    }

    /// The description fetch reaches the same registry as the observe loop, so
    /// its failures classify the same way — an unreachable registry must not
    /// collapse to exit 1 just because it was the description being fetched.
    #[test]
    fn observe_desc_variant_classifies_via_the_inner_error() {
        let inner = ocx_oci::client::error::ClientError::ManifestNotFound("x".to_string());
        let expected = inner.classify();
        let error = AnnounceError::ObserveDesc {
            repository: "oci://ghcr.io/acme/widget".to_string(),
            source: Box::new(inner),
        };
        assert_eq!(error.classify(), expected);
        assert!(
            error.to_string().contains("oci://ghcr.io/acme/widget"),
            "the message must name the repository: {error}"
        );
    }

    /// A vanished description is a disagreement between the committed root and
    /// the registry, not an absence and not a crash — a release wrapper must be
    /// able to tell it apart from both.
    #[test]
    fn desc_disappeared_classifies_as_data_error() {
        let error = AnnounceError::DescDisappeared {
            repository: "oci://ghcr.io/acme/widget".to_string(),
            digest: format!("sha256:{}", "a".repeat(64)),
        };
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let message = error.to_string();
        assert!(
            message.contains("__ocx.desc"),
            "the message must name the tag: {message}"
        );
        assert!(
            message.contains(&format!("sha256:{}", "a".repeat(64))),
            "the message must name the digest the root recorded: {message}"
        );
    }

    /// The #436 tripwire classifies like its `DescDisappeared` sibling, and the
    /// message names every tag it refused to delete.
    ///
    /// The guard's whole point is that nothing reachable today fires it — which
    /// is exactly why the arm needs a row of its own. Without one, both the
    /// exit code a release wrapper branches on and the message an operator has
    /// to act on are green only because no test and no production path ever ran
    /// them. Never `TempFail`: a rerun reproduces it, so a retry would loop.
    #[test]
    fn committed_tags_dropped_classifies_as_data_error() {
        let error = AnnounceError::CommittedTagsDropped {
            path: "p/acme/widget.json".to_string(),
            tags: vec!["0.4.6".to_string(), "1.2.0".to_string()],
        };
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let message = error.to_string();
        assert!(
            message.contains("p/acme/widget.json"),
            "the message must name the root that would have lost them: {message}"
        );
        for tag in ["0.4.6", "1.2.0"] {
            assert!(
                message.contains(tag),
                "the message must name every tag, or the operator cannot tell what was \
                 about to be deleted; {tag} is missing from: {message}"
            );
        }
    }

    /// #477 — a root that names another package is a disagreement between two
    /// statements of the same fact, not a malformed command line.
    ///
    /// `DataError` (65) rather than `UsageError` (64) is the load-bearing half:
    /// a release wrapper branches on 64 to mean "the flags I generated are
    /// wrong", and retrying with different flags cannot fix a root that names
    /// somebody else's package. Unclassified it would exit 1, which is the
    /// crash code.
    ///
    /// Reds on: classifying the variant as `UsageError`, or dropping the arm so
    /// the wildcard answers `None`.
    #[test]
    fn root_name_mismatch_classifies_as_data_error() {
        let error = AnnounceError::RootNameMismatch {
            path: "p/acme/widget.json".to_string(),
            committed: "other.example/acme/widget".to_string(),
            expected: "ocx.sh/acme/widget".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let message = error.to_string();
        for value in ["p/acme/widget.json", "other.example/acme/widget", "ocx.sh/acme/widget"] {
            assert!(
                message.contains(value),
                "the operator cannot act without both names and the path; {value} is missing from: {message}"
            );
        }
    }

    /// The absent-`name` half of the same variant: an empty `committed` renders
    /// as its own sentence, because `names , not …` reads as a defect in the
    /// tool rather than in the file.
    #[test]
    fn an_absent_root_name_still_names_what_was_expected() {
        let error = AnnounceError::RootNameMismatch {
            path: "p/acme/widget.json".to_string(),
            committed: String::new(),
            expected: "ocx.sh/acme/widget".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let message = error.to_string();
        assert!(message.contains("carries no name"), "got: {message}");
        assert!(message.contains("ocx.sh/acme/widget"), "got: {message}");
    }

    /// D2's refusal is the one announce failure a rerun cannot clear, so a
    /// release wrapper must be able to tell it from a crash (exit 1) and from a
    /// transient (75). The message has to name the branch, because deleting it
    /// is one of the two actions that unstick the package.
    #[test]
    fn pull_request_unmergeable_classifies_as_data_error() {
        let error = AnnounceError::PullRequestUnmergeable {
            number: 648,
            url: "https://github.com/ocx-sh/index/pull/648".to_string(),
            branch: "indexbot-announce-acme-widget".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let message = error.to_string();
        assert!(
            message.contains("indexbot-announce-acme-widget"),
            "the message must name the branch to delete: {message}"
        );
        assert!(
            message.contains("648"),
            "the message must name the pull request: {message}"
        );
        assert!(
            message.contains("https://github.com/ocx-sh/index/pull/648"),
            "the message must link the pull request: {message}"
        );
    }

    #[test]
    fn unclassified_variant_defers_to_the_chain_walker() {
        assert_eq!(AnnounceError::ForgeRequired.classify(), None);
    }

    /// C-004/S-003 (#377): an `--out` write failure is an operator/environment
    /// I/O problem, exit 74. `StorageFull` is the case the generic walker
    /// cannot reach — it special-cases only `PermissionDenied`, so before this
    /// arm existed every other kind exited 1, the crash code.
    #[test]
    fn output_write_classifies_as_io_error() {
        for kind in [
            std::io::ErrorKind::StorageFull,
            std::io::ErrorKind::NotADirectory,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let error = AnnounceError::OutputWrite {
                path: "/mnt/ro/index/root.json".to_string(),
                source: std::io::Error::new(kind, "write failed"),
            };
            assert_eq!(
                error.classify(),
                Some(ExitCode::IoError),
                "an --out write failure of kind {kind:?} must exit 74"
            );
        }
    }

    // ── moved from ocx_announce::claim::error with the impl ──

    /// C-S1-1 — the `detail` slugs ship in JSON envelopes and SDKs dispatch
    /// on them, so each string is pinned here; the exhaustive match in
    /// `kind_detail` is what forces a slug for a variant added later, and the
    /// arity pin is what catches a row dropped from this table.
    ///
    /// Reds on: renaming any slug, or a variant whose exit code disagrees
    /// with `classify()`.
    #[test]
    fn kind_detail_values_are_stable() {
        let expected = [
            "forge_required",
            "malformed_repository",
            "package_already_claimed",
            "no_acting_identity",
            "invalid_owner_login",
            "duplicate_owner",
            "owner_unknown",
            "owner_id_mismatch",
            "bot_identity",
            "missing_base_ref",
            "missing_head_root",
            "output_write",
            "root_name_mismatch",
            "repository_mismatch",
            "description",
            "forge",
        ];
        let variants = every_variant();
        assert_eq!(
            variants.len(),
            expected.len(),
            "one slug per variant, in `every_variant` order"
        );
        for (error, slug) in variants.iter().zip(expected) {
            assert_eq!(error.kind_detail(), slug, "{error}");
            assert_eq!(
                error.exit_code(),
                error.classify().unwrap_or(ExitCode::Failure),
                "{error}"
            );
        }
    }

    /// #477 / #481 — a re-claim's two disagreement refusals exit 65, the same
    /// category and the same reading as announce's `RootNameMismatch`.
    ///
    /// Reds on: classifying either as `UsageError`, and on an arm that answers
    /// `None` (the match is wildcard-free, so the *absence* of an arm is a build
    /// failure instead — this pins the value the arm carries).
    #[test]
    fn a_re_claim_disagreement_classifies_as_data_error() {
        for error in [
            ClaimError::RootNameMismatch {
                committed: "ocx.sh/acme/widget".to_string(),
                expected: "ghcr.io/acme/widget".to_string(),
            },
            ClaimError::RepositoryMismatch {
                committed: "oci://ghcr.io/acme/widget".to_string(),
                supplied: "oci://quay.io/acme/widget".to_string(),
            },
        ] {
            assert_eq!(
                error.classify(),
                Some(ExitCode::DataError),
                "two sides of a re-claim disagreeing is 65, not a malformed command line: {error}"
            );
        }
    }

    /// C-003 — a claim's description failure exits exactly as the same failure
    /// does under announce, because the arm delegates rather than minting a
    /// code.
    ///
    /// The expectation is read from the inner error rather than written as a
    /// literal, so the row cannot drift away from the announce taxonomy it is
    /// asserting parity with; the guard above it is what stops the whole
    /// assertion from being `None == None`.
    ///
    /// Reds on: replacing the delegation with any fixed code.
    #[test]
    fn a_claims_description_failure_classifies_through_the_announce_taxonomy() {
        let inner = AnnounceError::Ssrf {
            namespace: "ocx.sh".to_string(),
            source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                host: "127.0.0.1".to_string(),
                ip: "127.0.0.1".parse().expect("valid ip literal"),
            },
        };
        let expected = inner.classify();
        assert!(
            expected.is_some(),
            "the fixture's own inner error classifies to nothing, so the delegation below \
             would agree even if it delegated nowhere"
        );
        let error = ClaimError::Description(inner);
        assert_eq!(error.classify(), expected);
        assert_eq!(
            error.kind_detail(),
            "description",
            "the envelope slug is the claim-side variant's, never the wrapped error's"
        );
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

    // ── moved from ocx_announce::claim::owners with the impl ──

    // ── moved from ocx_announce::claim::root with the impl ──

    // ── moved from ocx_announce::forge::error with the impl ──

    #[test]
    fn status_401_maps_to_auth_error() {
        let error = ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 401,
            detail: String::new(),
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    #[test]
    fn a_malformed_invocation_maps_to_usage_error() {
        for error in [
            ForgeError::ForgeKindUnknown {
                host: "git.example.com".to_string(),
            },
            ForgeError::NestedNamespaceUnsupported {
                forge: "GitHub".to_string(),
                namespace: "acme/platform".to_string(),
            },
            ForgeError::SelfForkRefused {
                upstream: "acme/index".to_string(),
                namespace: "acme".to_string(),
            },
            ForgeError::ForkHostMismatch {
                fork_host: "gitlab.com".to_string(),
                index_host: "gitlab.example.com".to_string(),
            },
            ForgeError::InvalidRepoCoordinate {
                value: "gitlab.com@evil.example/acme/index".to_string(),
            },
        ] {
            assert_eq!(
                error.classify(),
                Some(ExitCode::UsageError),
                "{error} must exit 64 — it is fixed by editing the command line"
            );
        }
    }

    #[test]
    fn status_403_maps_to_auth_error() {
        let error = ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 403,
            detail: String::new(),
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    #[test]
    fn status_429_maps_to_temp_fail() {
        let error = ForgeError::Status {
            url: "https://api.github.com/repos/x/y/git/blobs".to_string(),
            status: 429,
            detail: String::new(),
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    #[test]
    fn server_error_statuses_map_to_unavailable() {
        for status in [500, 502, 503, 599] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
                detail: String::new(),
            };
            assert_eq!(
                error.classify(),
                Some(ExitCode::Unavailable),
                "HTTP {status} is a forge-side incident, retryable"
            );
        }
    }

    #[test]
    fn status_other_is_unclassified() {
        // A 404 in particular stays unclassified: the indeterminate-compare
        // fall-through (C6 amendment) deliberately rides on it.
        for status in [404, 422] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
                detail: String::new(),
            };
            assert_eq!(error.classify(), None, "HTTP {status} has no dedicated exit code");
        }
    }

    #[test]
    fn transport_failure_maps_to_unavailable() {
        let error = ForgeError::Transport {
            url: "https://api.github.com/user".to_string(),
            source: transport_error(),
        };
        assert_eq!(error.classify(), Some(ExitCode::Unavailable));
    }

    #[test]
    fn non_fast_forward_maps_to_temp_fail() {
        let error = ForgeError::NonFastForward {
            branch: "indexbot-announce-acme-widget".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    // ── moved from ocx_announce::announce with the impl ──

    /// The 79 a release wrapper branches on is produced by the **CLI
    /// classifier**, not by `AnnounceError::classify` alone.
    ///
    /// Calling `classify()` directly proves only that the variant carries a
    /// code; the process exits 79 only because `AnnounceError` is registered in
    /// the downcast ladder, and deleting that registration leaves such a test
    /// green while every real run drops to exit 1. Driving `classify_error` over
    /// a **boxed** error is what exercises the registration, because that is the
    /// shape the ladder actually receives.
    #[test]
    fn an_unclaimed_namespace_still_exits_not_found_through_the_cli_classifier() {
        let error: Box<dyn std::error::Error + 'static> = Box::new(AnnounceError::UnclaimedPackage {
            package: "acme/widget".to_string(),
            path: "p/acme/widget.json".to_string(),
            base_ref: INDEX_BASE_REF.to_string(),
        });

        assert_eq!(
            crate::exit::classify_library_error(error.as_ref()),
            ExitCode::NotFound,
            "announcing into an unclaimed package must stay discriminable from a crash"
        );
    }

    // ── moved from ocx_announce::forge::error with the impl ──

    // ── moved from ocx_announce::forge::git_push_options with the impl ──

    // fixture from ocx_lib (claim/error)
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
            ClaimError::Description(AnnounceError::DescDisappeared {
                repository: "oci://ghcr.io/acme/widget".to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
            }),
            ClaimError::Forge(ForgeError::UsersApiUnavailable),
        ]
    }

    // fixture from ocx_lib (forge/error)
    /// `reqwest::Error` exposes no public constructor; a malformed URL fails
    /// `RequestBuilder::build()` synchronously (no network access), giving a
    /// real value to wrap in `Transport` for the classification test.
    fn transport_error() -> reqwest::Error {
        reqwest::Client::new()
            .get("not a valid url")
            .build()
            .expect_err("a malformed URL must fail to build without any network access")
    }

    // recovered from ocx_announce::forge::error
    /// The eleven variants the git write transport adds — C-018's ten, plus
    /// [`ForgeError::PushOptionRefused`] (DX-27) — each beside the exit code the
    /// ADR's exit-code table names for it.
    ///
    /// One fixture, read by two tests. The exit-code table and the message
    /// style guard must cover the same eleven variants, and a second
    /// hand-written list is a second definition of "the eleven", free to drift
    /// from this one.
    fn new_variants_with_exit_codes() -> Vec<(ForgeError, Option<ExitCode>)> {
        vec![
            (
                ForgeError::TransportUnsupported {
                    forge: ForgeKind::GitHub,
                    transport: WriteTransport::Git,
                },
                Some(ExitCode::UsageError),
            ),
            (
                ForgeError::TransportOperationUnsupported {
                    operation: "ensure_fork".to_string(),
                    transport: WriteTransport::Git,
                },
                Some(ExitCode::UsageError),
            ),
            (ForgeError::UsersApiUnavailable, Some(ExitCode::UsageError)),
            (
                ForgeError::GitUnavailable {
                    reason: "git 2.30.9 is below the 2.31 floor the git write transport needs".to_string(),
                },
                Some(ExitCode::Unavailable),
            ),
            (
                ForgeError::GitCommandFailed {
                    command: "fetch".to_string(),
                    status: "exit status 128".to_string(),
                    stderr: redact("fatal: could not read from the remote repository", &[]),
                },
                None,
            ),
            (
                ForgeError::GitPushFailed {
                    status: "exit status: 1".to_string(),
                    stderr: redact("remote: a refusal shape no classifier models", &[]),
                },
                None,
            ),
            // DX-27. Unclassified for the same reason as the two above, and
            // asserted as `None` rather than omitted so that a later hand
            // dropping it into the `UsageError` arm — the tempting wrong answer,
            // because the message reads like bad input — reds here instead of
            // shipping. The `reason` names the key and the codepoint and never
            // the value; see the variant's own doc comment.
            (
                ForgeError::PushOptionRefused {
                    key: "merge_request.title",
                    reason: "byte 12 is U+001B, outside the printable ASCII the wire carries".to_string(),
                },
                None,
            ),
            (
                ForgeError::StaleLease {
                    branch: "indexbot-claim-acme".to_string(),
                },
                Some(ExitCode::TempFail),
            ),
            (
                ForgeError::PushRefused {
                    branch: "main".to_string(),
                    reason: "the branch is protected".to_string(),
                },
                Some(ExitCode::PermissionDenied),
            ),
            (
                ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenPush,
                    repo: "acme/index".to_string(),
                    remedy: "enable Settings > CI/CD > Job token permissions on acme/index".to_string(),
                },
                Some(ExitCode::ForgeCapabilityUnavailable),
            ),
            (
                ForgeError::MergeRequestUnconfirmed { deadline_secs: 30 },
                Some(ExitCode::TempFail),
            ),
        ]
    }

    // recovered from ocx_announce::forge::error
    /// Every one of the git write transport's eleven new variants against the
    /// ADR's exit-code table.
    ///
    /// [`ForgeError::GitCommandFailed`], [`ForgeError::GitPushFailed`] and
    /// [`ForgeError::PushOptionRefused`] are **deliberately unclassified** and
    /// are asserted as `None` rather than omitted: each is an unrecognised
    /// failure of a plumbing step or a broken ocx-side invariant, with no remedy
    /// a caller could branch on, so a later accidental classification must red
    /// here rather than ship silently.
    ///
    /// The two worth reading twice are 86 and 77.
    /// [`ForgeError::WriteCapabilityUnavailable`] is exit 86, a *capability
    /// gate* whose remedy is an administrator's; [`ForgeError::PushRefused`] is
    /// exit 77, a *permission refusal* against a credential that authenticated
    /// fine. Collapsing them would hide the one state a pipeline cannot act on
    /// from the one it can.
    ///
    /// The arity is asserted over the very array the rows are read from — a
    /// count written beside a separate enumeration is a budget, not a pairing,
    /// and would not notice a dropped row.
    ///
    /// Reds on: dropping any row (proved), and on moving a variant to a
    /// different arm of `classify` (proved with `PushRefused` 77 -> 86).
    #[test]
    fn forge_error_exit_code_table() {
        let table = new_variants_with_exit_codes();
        assert_eq!(
            table.len(),
            11,
            "the git write transport adds eleven variants and every one of them is classified here, including the three unclassified"
        );
        for (error, expected) in table {
            assert_eq!(error.classify(), expected, "{error} must classify as {expected:?}");
        }
    }
}
