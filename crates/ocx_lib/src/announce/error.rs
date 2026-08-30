// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the announce orchestration.
//!
//! Variants are structured so the CLI (`ocx package announce`) can classify
//! them to the existing sysexits without a new code (design register C13): an
//! SSRF refusal maps to `ConfigError` (78), a DNS resolution failure reaching
//! the physical registry maps to `Unavailable` (69), and a curated tag that
//! does not resolve on the physical registry maps to `NotFound` (79). A forge
//! auth failure (401/403 — a bad or missing `OCX_ANNOUNCE_TOKEN`) maps to
//! `AuthError` (80) and a forge transport failure maps to `Unavailable` (69),
//! both via [`ForgeError`](crate::forge::ForgeError)'s own classification. The
//! missing-token case for `--fork` with no token at all is a CLI-boundary
//! check (never reaches this type) and also maps to `AuthError` (80) there.
//!
//! [`ClassifyExitCode`] is implemented here (rather than left to source-chain
//! walking) for two independent reasons:
//!
//! - [`Self::Ssrf`] and [`Self::Forge`] are `#[error(transparent)]`:
//!   thiserror's transparent forwarding makes `Error::source()` skip straight
//!   past the wrapped [`SsrfError`] / [`ForgeError`](crate::forge::ForgeError)
//!   to *its own* source, so the generic chain walker in `cli::classify_error`
//!   would never see it.
//! - [`Self::Observe`]'s `#[source]` field is `Box<ClientError>` (a concrete
//!   boxed type, not `Box<dyn Error>`): thiserror's generated `source()`
//!   exposes it through the blanket `AsDynError` impl keyed on the field's
//!   *declared* type, so the resulting trait object's `Any` identity is
//!   `Box<ClientError>`, not `ClientError` — a `downcast_ref::<ClientError>()`
//!   in the generic walker would silently fail to match.
//!
//! Delegating explicitly here keeps both mappings correct regardless of
//! those thiserror/`Any` details.

/// Failures raised by [`announce`](super::announce).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnnounceError {
    /// Announce reads the committed index root through a forge in every mode
    /// (design register C10); a `None` forge cannot read it, and a fork target
    /// additionally needs the forge to commit.
    #[error("announce requires a forge to read the committed index root")]
    ForgeRequired,

    /// The resolved curated tag set was empty (design register C3/C5).
    ///
    /// `reserved_dropped` carries the reserved names the D7 filter removed on
    /// the way to empty. Without them the message would claim nothing was
    /// given for a selection that named only reserved tags — and this is the
    /// one D7 path with no [`AnnounceOutcome`](super::AnnounceOutcome) to
    /// carry the drop notice, so the error message is where the names surface.
    #[error("{}", no_curated_tags_message(reserved_dropped))]
    NoCuratedTags { reserved_dropped: Vec<String> },

    /// No committed root exists for the package at `base_ref` — a new package
    /// goes through the human namespace-claim lane, never announce.
    #[error(
        "unclaimed namespace: no committed root at {path} on {base_ref} for {package} — new packages go through the human lane"
    )]
    UnclaimedNamespace {
        package: String,
        path: String,
        base_ref: String,
    },

    /// The committed root bytes are not valid JSON.
    #[error("committed root at {path} is not valid JSON")]
    RootParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    /// The committed root parsed but is not a JSON object.
    #[error("committed root at {path} is not a JSON object")]
    RootNotObject { path: String },

    /// The committed root is missing a field announce needs to proceed.
    #[error("committed root is missing the {field} field")]
    RootMissingField { field: &'static str },

    /// The root's `repository` pointer is not a well-formed `oci://host/path`
    /// reference (design register C3, strict one-way-door parse).
    #[error("malformed physical repository pointer {value}")]
    MalformedPhysicalRepository { value: String },

    /// The physical host resolved to a forbidden address or could not be
    /// resolved (design register X1-X3, SSRF pre-flight).
    #[error(transparent)]
    Ssrf(#[from] crate::oci::ssrf::SsrfError),

    /// A curated tag does not resolve on the physical repository — a publisher
    /// typo, never silently dropped (reference parity).
    #[error("tag {tag} does not resolve on {repository} — check for a typo")]
    UnresolvedTag { tag: String, repository: String },

    /// Fetching a curated tag's manifest from the physical registry failed.
    ///
    /// The source is boxed so an otherwise-small [`AnnounceError`] does not
    /// inherit `ClientError`'s large footprint (`clippy::result_large_err`).
    #[error("failed to observe tag {tag} on {repository}")]
    Observe {
        tag: String,
        repository: String,
        #[source]
        source: Box<crate::oci::client::error::ClientError>,
    },

    /// Fetching or decoding the `__ocx.desc` artifact failed (D6): a transport
    /// failure, a manifest that is not a description artifact, or one carrying
    /// no markdown readme layer.
    ///
    /// Boxed for the same reason as [`Self::Observe`].
    #[error("failed to observe the description of {repository}")]
    ObserveDesc {
        repository: String,
        #[source]
        source: Box<crate::oci::client::error::ClientError>,
    },

    /// The committed root records a description the physical repository no
    /// longer serves. Retraction semantics are unspecified, so announce stops
    /// loudly rather than silently clearing `desc` back to null (reference
    /// parity).
    #[error("__ocx.desc disappeared from {repository} (was {digest})")]
    DescDisappeared { repository: String, digest: String },

    /// Listing the physical repository's tags failed (`--tags-from-registry`).
    ///
    /// Boxed for the same reason as [`Self::Observe`].
    #[error("failed to list the tags on {repository}")]
    ListTags {
        repository: String,
        #[source]
        source: Box<crate::Error>,
    },

    /// A curated tag resolves to a bare OCI image manifest. The index records
    /// image indices only, and `ocx package push` always publishes one — so the
    /// artifact behind this tag was not published by ocx.
    #[error("tag {tag} on {repository} resolves to an OCI image manifest; the index records image indices only")]
    TagIsNotAnImageIndex { tag: String, repository: String },

    /// A tag was named to both `--yank` and `--unyank` (design register C7).
    #[error("tag(s) {tags:?} given to both yank and unyank")]
    YankUnyankOverlap { tags: Vec<String> },

    /// A `--yank` named a tag outside the curated set (design register C7).
    #[error("cannot yank {tag}: not in the curated tag set")]
    YankTagNotCurated { tag: String },

    /// A `--unyank` named a tag outside the curated set (design register C7).
    #[error("cannot unyank {tag}: not in the curated tag set")]
    UnyankTagNotCurated { tag: String },

    /// A forge operation (fork ensure, commit, or pull-request) failed.
    #[error(transparent)]
    Forge(#[from] crate::forge::ForgeError),

    /// No base ref was found on the fork to commit the announce branch onto.
    #[error("no base ref found on {repo} to commit onto")]
    MissingBaseRef { repo: String },

    /// The C4 retry re-read the branch head that won the race, but the package
    /// root is absent from it — the winning commit deleted or never carried it.
    /// Distinct from [`Self::MissingBaseRef`]: the ref resolved fine, the FILE
    /// at it did not.
    #[error("no committed root at {path} on {repo}@{sha} to retry the announce against")]
    MissingHeadRoot { repo: String, path: String, sha: String },

    /// Writing a root or CAS object to the `--out` directory failed.
    #[error("failed to write {path}")]
    OutputWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// `Display` body for [`AnnounceError::NoCuratedTags`] — thiserror's format
/// string cannot branch, and the two cases are genuinely different failures.
fn no_curated_tags_message(reserved_dropped: &[String]) -> String {
    if reserved_dropped.is_empty() {
        return "no curated tags given".to_string();
    }
    format!(
        "no curated tags left: every tag given is reserved and is not a version ({})",
        reserved_dropped.join(", ")
    )
}

impl crate::cli::ClassifyExitCode for AnnounceError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // Delegated explicitly — see the module doc for why the generic
            // source-chain walker cannot reach any of these on its own.
            Self::Ssrf(inner) => inner.classify(),
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
            Self::DescDisappeared { .. } => Some(crate::cli::ExitCode::DataError),
            // A publisher typo — the tag genuinely does not exist on the
            // physical registry. Same category as `ClientError::ManifestNotFound`.
            Self::UnresolvedTag { .. } => Some(crate::cli::ExitCode::NotFound),
            // The index root genuinely does not exist yet — the same
            // absent-resource shape as `UnresolvedTag`, and the likeliest
            // first-run outcome for a new publisher. Left unclassified it
            // exits 1, indistinguishable from a crash, so a release wrapper
            // cannot tell "claim your namespace first" (a one-time human
            // action, register R3) from an unclassified failure.
            Self::UnclaimedNamespace { .. } => Some(crate::cli::ExitCode::NotFound),
            // The tag resolved and the artifact exists — its *shape* is wrong.
            // `NotFound` (79) would be a lie (nothing is absent) and leaving it
            // unclassified exits 1, which a release wrapper cannot tell apart
            // from a crash. `EX_DATAERR` is the malformed-input category.
            Self::TagIsNotAnImageIndex { .. } => Some(crate::cli::ExitCode::DataError),
            // The tag selection is operator input — a `--tags` list, a tags
            // file, or a committed root the publisher curated. Nothing is
            // absent and nothing is malformed on the wire: the invocation
            // named no version. `EX_USAGE` is that category, and it keeps the
            // all-reserved collapse discriminable from an unclassified crash.
            Self::NoCuratedTags { .. } => Some(crate::cli::ExitCode::UsageError),
            // Writing the `--out` tree failed: a full disk, an `ENOTDIR`, a
            // read-only mount. `cli/classify.rs`'s bare-`io::Error` walker
            // special-cases only `PermissionDenied`, so every other kind lands
            // on `Failure` (1) — indistinguishable from a crash to a release
            // wrapper. `EX_IOERR` is the category the rest of the tool uses for
            // an operator/environment I/O failure.
            Self::OutputWrite { .. } => Some(crate::cli::ExitCode::IoError),
            // Every other variant falls through to `ExitCode::Failure`.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};

    #[test]
    fn ssrf_variant_classifies_via_the_inner_error() {
        let error = AnnounceError::Ssrf(crate::oci::ssrf::SsrfError::ForbiddenTarget {
            host: "127.0.0.1".to_string(),
            ip: "127.0.0.1".parse().expect("valid ip literal"),
        });
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
    }

    #[test]
    fn forge_variant_classifies_via_the_inner_error() {
        let error = AnnounceError::Forge(crate::forge::ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 401,
            detail: String::new(),
        });
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    /// Observed live on the publisher E2E (run 30133426034): announcing into a
    /// namespace with no committed root is the likeliest first-run outcome for
    /// a new publisher, and register R3 makes claiming it a one-time human
    /// action. It must be discriminable from a generic failure so a release
    /// wrapper can say so, rather than surfacing exit 1.
    #[test]
    fn unclaimed_namespace_classifies_as_not_found() {
        let error = AnnounceError::UnclaimedNamespace {
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
    #[test]
    fn list_tags_variant_classifies_via_the_inner_error() {
        let error = AnnounceError::ListTags {
            repository: "oci://ghcr.io/acme/widget".to_string(),
            source: Box::new(crate::Error::OfflineMode),
        };
        assert_eq!(error.classify(), crate::Error::OfflineMode.classify());
        let message = error.to_string();
        assert!(
            message.contains("oci://ghcr.io/acme/widget"),
            "the message must name the repository whose listing failed: {message}"
        );
    }

    /// The D4(a) refusal is a verdict a release wrapper must be able to act on.
    /// Left unclassified it exits 1 — indistinguishable from a crash, the same
    /// defect the `UnclaimedNamespace` comment above records.
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
        let inner = crate::oci::client::error::ClientError::ManifestNotFound("x".to_string());
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
}
