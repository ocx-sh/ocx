// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the `ocx_announce` error family.

use ocx_exit::ExitCode;

use ocx_announce::announce::AnnounceError;
use ocx_announce::claim::ClaimError;
use ocx_announce::forge::{ForgeError, is_server_fault};
use ocx_oci::transport_policy::{is_transient_status, is_transient_transport_error};

use super::{ClassifyErrorKind, ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for AnnounceError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Delegated explicitly: `Observe`/`ObserveDesc`/`ListTags` box their source (never downcasts) and
            // `Forge` is transparent; `Ssrf` delegates for uniformity.
            Self::Ssrf { source, .. } => source.classify(),
            Self::Forge(inner) => inner.classify(),
            Self::Observe { source, .. } => source.classify(),
            Self::ObserveDesc { source, .. } => source.classify(),
            Self::ListTags { source, .. } => source.classify(),
            Self::DescDisappeared { .. } => Some(ExitCode::DataError),
            Self::RootNameMismatch { .. } => Some(ExitCode::DataError),
            // Never `TempFail`: a rerun reproduces it exactly, so a retrying publisher would loop.
            Self::CommittedTagsDropped { .. } => Some(ExitCode::DataError),
            Self::UnresolvedTag { .. } => Some(ExitCode::NotFound),
            // A push landed between the two reads; a rerun observes the tag as present.
            Self::ObserveRaced { .. } => Some(ExitCode::TempFail),
            Self::UnclaimedPackage { .. } => Some(ExitCode::NotFound),
            // Not `NotFound`: the artifact exists, only its shape is wrong.
            Self::TagIsNotAnImageIndex { .. } => Some(ExitCode::DataError),
            Self::NoCuratedTags { .. } => Some(ExitCode::UsageError),
            // Not `TempFail`: a retry can never succeed; only a human clears the conflict.
            Self::PullRequestUnmergeable { .. } => Some(ExitCode::DataError),
            // The generic `io::Error` walker maps only `PermissionDenied`; any other kind would exit 1.
            Self::OutputWrite { .. } => Some(ExitCode::IoError),
            _ => None,
        }
    }
}

impl ClassifyExitCode for ForgeError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::Status { status, .. } if *status == 401 || *status == 403 => Some(ExitCode::AuthError),
            Self::PushAccessDenied { .. } => Some(ExitCode::AuthError),
            // 75 means a rerun may succeed, 69 that it will not; the registry's own split.
            Self::Status { status, .. } if is_transient_status(*status) => Some(ExitCode::TempFail),
            Self::Status { status, .. } if is_server_fault(*status) => Some(ExitCode::Unavailable),
            Self::Transport { source, .. } if is_transient_transport_error(source) => Some(ExitCode::TempFail),
            Self::Transport { .. } => Some(ExitCode::Unavailable),
            // All three are races a rerun clears.
            Self::NonFastForward { .. } | Self::StaleLease { .. } | Self::MergeRequestUnconfirmed { .. } => {
                Some(ExitCode::TempFail)
            }
            Self::GitUnavailable { .. } => Some(ExitCode::Unavailable),
            Self::PushRefused { .. } => Some(ExitCode::PermissionDenied),
            Self::WriteCapabilityUnavailable { .. } => Some(ExitCode::ForgeCapabilityUnavailable),
            Self::ForgeKindUnknown { .. }
            | Self::NestedNamespaceUnsupported { .. }
            | Self::SelfForkRefused { .. }
            | Self::ForkHostMismatch { .. }
            | Self::InvalidRepoCoordinate { .. }
            | Self::TransportUnsupported { .. }
            | Self::TransportOperationUnsupported { .. }
            | Self::UsersApiUnavailable => Some(ExitCode::UsageError),
            // `GitCommandFailed`/`GitPushFailed` stay exit 1: no remedy a caller could branch on.
            _ => None,
        }
    }
}

impl ClassifyExitCode for ClaimError {
    /// Wildcard-free, so a new variant is an `E0004` rather than a silent exit 1.
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::ForgeRequired | Self::MissingBaseRef { .. } | Self::MissingHeadRoot { .. } => None,
            Self::MalformedRepository { .. }
            | Self::NoActingIdentity
            | Self::InvalidOwnerLogin { .. }
            | Self::DuplicateOwner { .. }
            | Self::OwnerIdMismatch { .. }
            | Self::BotIdentity { .. } => Some(ExitCode::UsageError),
            Self::RootNameMismatch { .. } | Self::RepositoryMismatch { .. } => Some(ExitCode::DataError),
            Self::OwnerUnknown { .. } => Some(ExitCode::NotFound),
            Self::OutputWrite { .. } => Some(ExitCode::IoError),
            // `#[error(transparent)]` hides both nodes from the chain walker, so delegate explicitly.
            Self::Description(inner) => inner.classify(),
            Self::Forge(inner) => inner.classify(),
        }
    }
}

impl ClassifyErrorKind for ClaimError {
    fn exit_code(&self) -> ExitCode {
        self.classify().unwrap_or(ExitCode::Failure)
    }

    /// `Forge` is one slug: `#[error(transparent)]` hides the `ForgeError` node from the envelope's chain walk.
    fn kind_detail(&self) -> &'static str {
        match self {
            Self::ForgeRequired => "forge_required",
            Self::MalformedRepository { .. } => "malformed_repository",
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
        // ocx-sh/ocx#455: the message names the exact config entry, so an operator
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

    /// Announcing into a package with no committed root is the likeliest first-run
    /// outcome for a new publisher, and claiming it is a one-time step. It must be
    /// discriminable from a generic failure so a release wrapper can say so, rather
    /// than surfacing exit 1.
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
    /// The fixture is the value the producer can actually raise: a transport fault inside
    /// an index error inside the package tier's own error, never the `OfflineMode` variant
    /// `Publisher::list_tags` cannot return. The assertion follows that two-hop chain and
    /// reads the expected code off the `ClientError` itself, never a literal copied down
    /// from the arm.
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

    /// This refusal is a verdict a release wrapper must be able to act on. Left
    /// unclassified it exits 1 — indistinguishable from a crash, the same
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

    /// A selection filter routes a new failure class into this variant: a selection
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

    /// This refusal is the one announce failure a rerun cannot clear, so a
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

    /// An `--out` write failure is an operator/environment
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

    /// The `detail` slugs ship in JSON envelopes and SDKs dispatch
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

    /// A claim's description failure exits exactly as the same failure
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

    /// The three deliberately **unclassified** variants answer `None`,
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

    /// A rerun may succeed: the registry's transient status set.
    #[test]
    fn transient_statuses_map_to_temp_fail() {
        for status in [408, 429, 502, 503, 504] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/git/blobs".to_string(),
                status,
                detail: String::new(),
            };
            assert_eq!(error.classify(), Some(ExitCode::TempFail), "HTTP {status}");
        }
    }

    #[test]
    fn terminal_server_error_statuses_map_to_unavailable() {
        for status in [500, 501, 505, 599] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
                detail: String::new(),
            };
            assert_eq!(
                error.classify(),
                Some(ExitCode::Unavailable),
                "HTTP {status} answers the same on a rerun"
            );
        }
    }

    #[test]
    fn status_other_is_unclassified() {
        // A 404 in particular stays unclassified: an indeterminate compare deliberately
        // falls through to it rather than guessing a code.
        for status in [404, 422] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
                detail: String::new(),
            };
            assert_eq!(error.classify(), None, "HTTP {status} has no dedicated exit code");
        }
    }

    /// A request that cannot even be built fails the same way every time.
    #[test]
    fn builder_transport_failure_maps_to_unavailable() {
        let error = ForgeError::Transport {
            url: "https://api.github.com/user".to_string(),
            source: transport_error(),
        };
        assert_eq!(error.classify(), Some(ExitCode::Unavailable));
    }

    /// A peer that accepts and drops: the retry ladder retries it, the exit code calls it terminal.
    #[tokio::test]
    async fn hangup_after_connect_transport_failure_maps_to_unavailable() {
        let addr = ocx_test_support::net::serve_hangup().await;
        let source = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a stock client builds")
            .get(format!("http://{addr}/user"))
            .send()
            .await
            .expect_err("a dropped connection fails");
        assert!(
            ocx_oci::transport_policy::is_retryable_transport_error(&source),
            "precondition: the ladder retries a hang-up, got {source:?}"
        );
        let error = ForgeError::Transport {
            url: "https://api.github.com/user".to_string(),
            source,
        };
        assert_eq!(error.classify(), Some(ExitCode::Unavailable));
    }

    #[tokio::test]
    async fn refused_connect_transport_failure_maps_to_temp_fail() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("bind a free loopback port")
            .port();
        let source = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a stock client builds")
            .get(format!("http://127.0.0.1:{port}/user"))
            .send()
            .await
            .expect_err("a connect to a closed port fails");
        assert!(source.is_connect(), "precondition: a connect failure, got {source:?}");
        let error = ForgeError::Transport {
            url: "https://api.github.com/user".to_string(),
            source,
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    /// A verifier's verdict is the same on every dial, though the error is a connect error.
    #[tokio::test]
    async fn refused_certificate_transport_failure_maps_to_unavailable() {
        use ocx_test_support::pki::{TestPki, serve_https};

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let source = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a stock client builds")
            .get(format!("https://{addr}/user"))
            .send()
            .await
            .expect_err("the default root set does not know the minted root");
        assert!(source.is_connect(), "precondition: a connect failure, got {source:?}");
        let error = ForgeError::Transport {
            url: "https://api.github.com/user".to_string(),
            source,
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
    /// The eleven variants the git write transport adds, each beside the exit code
    /// the ADR's exit-code table names for it.
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
            // Unclassified for the same reason as the two above, and asserted as `None`
            // rather than omitted so that a later hand dropping it into the `UsageError`
            // arm — tempting, because the message reads like bad input — reds here
            // instead of shipping. `reason` names the key and codepoint, never the value.
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
