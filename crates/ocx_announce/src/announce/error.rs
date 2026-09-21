// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the announce orchestration.
//!
//! Variants are structured so the CLI (`ocx package announce`) can classify
//! them to the existing sysexits without a new code (design register C13): an
//! SSRF refusal maps to `ConfigError` (78), a DNS resolution failure reaching
//! the physical registry maps to `Unavailable` (69), and a curated tag that
//! does not resolve on the physical registry maps to `NotFound` (79), and an
//! otherwise-unchanged run whose open pull request cannot merge into the index
//! base maps to `DataError` (65). A forge
//! auth failure (401/403 — a bad or missing `OCX_ANNOUNCE_TOKEN`) maps to
//! `AuthError` (80) and a forge transport failure maps to `Unavailable` (69),
//! both via [`ForgeError`](crate::forge::ForgeError)'s own classification. The
//! missing-token case for `--fork` with no token at all is a CLI-boundary
//! check (never reaches this type) and also maps to `AuthError` (80) there.
//!
//! [`ClassifyExitCode`] is implemented here (rather than left to source-chain
//! walking) for two independent reasons:
//!
//! - [`Self::Forge`] is `#[error(transparent)]`: thiserror's transparent
//!   forwarding makes `Error::source()` skip straight past the wrapped
//!   [`ForgeError`](crate::forge::ForgeError) to *its own* source, so the
//!   generic chain walker in `cli::classify_error` would never see it.
//!   [`Self::Ssrf`] delegates the same way for symmetry.
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
    /// goes through the human package-claim lane, never announce.
    #[error(
        "unclaimed package: no committed root at {path} on {base_ref} for {package} — new packages go through the human lane"
    )]
    UnclaimedPackage {
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

    /// The committed root's `name` disagrees with the identifier the run
    /// announces ([#477]).
    ///
    /// The identifier on the command line and the root's `name` are two
    /// statements of the same fact, and until now nothing compared them: a
    /// package whose root said `ocx.sh/acme/widget` accepted an announce of
    /// `ghcr.io/acme/widget` and rewrote it. An absent `name` is a mismatch
    /// carrying an empty `committed` — fail closed, because the index schema
    /// requires the field of every real root, so its absence means the file is
    /// not the root it claims to be.
    ///
    /// `expected` is [`crate::claim::root_name`]'s output; there is no second
    /// spelling of the expected value anywhere.
    ///
    /// [#477]: https://github.com/ocx-sh/ocx/issues/477
    #[error("{}", root_name_mismatch_message(path, committed, expected))]
    RootNameMismatch {
        path: String,
        committed: String,
        expected: String,
    },

    /// The committed root is missing a field announce needs to proceed.
    #[error("committed root is missing the {field} field")]
    RootMissingField { field: &'static str },

    /// The root's `repository` pointer is not a well-formed `oci://host/path`
    /// reference (design register C3, strict one-way-door parse).
    #[error("malformed physical repository pointer {value}")]
    MalformedPhysicalRepository { value: String },

    /// The physical host resolved to a forbidden address or could not be
    /// resolved (design register X1-X3, SSRF pre-flight).
    ///
    /// Names the `[registries."<ns>"]` entry the fix goes into: the exemption is
    /// keyed on the package's *logical* namespace, not on the physical host it
    /// points at, and an operator who keys it on the host sees the inner
    /// refusal with no way to tell which entry it is reading (ocx#455).
    #[error(
        "the physical host of {namespace}/… was refused; list it (bare host, no port) under [registries.\"{namespace}\"].trusted_hosts"
    )]
    Ssrf {
        namespace: String,
        #[source]
        source: ocx_oci::ssrf::SsrfError,
    },

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
        source: Box<ocx_oci::client::error::ClientError>,
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
        source: Box<ocx_oci::client::error::ClientError>,
    },

    /// The committed root records a description the physical repository no
    /// longer serves. Retraction semantics are unspecified, so announce stops
    /// loudly rather than silently clearing `desc` back to null (reference
    /// parity).
    #[error("__ocx.desc disappeared from {repository} (was {digest})")]
    DescDisappeared { repository: String, digest: String },

    /// The regenerated root would delete a tag the root it was built from
    /// carried, under a selection that never deletes.
    ///
    /// The invariant `adr_announce_diverged_branch_rebuild.md` states — *no tag
    /// announced into an open pull request is ever lost* — asserted at the one
    /// place it can be measured cheaply. #228, #399 and #436 are three routes to
    /// breaking it, each found in production after a silent loss; this refuses
    /// the fourth without knowing what it is.
    ///
    /// It reads the base root, so it means something only because
    /// `GitWorkspace::commit_files` now refuses a fast-forward onto a head the
    /// run did not read: that is what makes the root this was built from the
    /// tree the commit lands on. Reserved tags (D7) and
    /// [`TagSelection::Replace`](crate::announce::TagSelection::Replace) are
    /// excluded at the call site — both delete by design.
    #[error("regenerating {path} would drop committed tags never selected for removal: {}", tags.join(", "))]
    CommittedTagsDropped { path: String, tags: Vec<String> },

    /// Listing the physical repository's tags failed (`--tags-from-registry`).
    ///
    /// Boxed for the same reason as [`Self::Observe`].
    ///
    /// The producer's own type, not a root error. It was `Box<ocx_lib::Error>`,
    /// reached by the hand-written flattening `From<ocx_package::error::Error>`
    /// — and `Publisher::list_tags` is what raises it, so the conversion only
    /// re-spelled a value the caller already held. WP-35 could not keep it
    /// (`scripts/crate_map.toml` does not allow this crate `ocx_lib`, and the
    /// root error has no successor), and dropping it costs nothing: the ladder's
    /// arm is `Self::ListTags { source, .. } => source.classify()`, and
    /// `ocx_package::error::Error` carries its own `downcast_arm!` rung at
    /// `ocx_cli/src/exit/ocx_package.rs`. Verified rather than assumed, on both
    /// axes an extraction can move an exit code on:
    ///
    /// * **Classification.** Every variant the flattening named agrees arm for
    ///   arm — `File`/`InternalFile` both 74, `SerializationFailure` both 65,
    ///   and `OciClient`, `Digest`, `Platform`, `Archive` and the two-hop
    ///   `Index` all delegating to the same inner type either way. The
    ///   fall-through agrees by construction: `ocx_lib::Error::Package(e)`'s arm
    ///   is `e.as_ref().classify()`, back into this very ladder.
    /// * **The `source()` chain**, which is what WP-33 actually broke. Walked
    ///   from `PackageError::Index(IndexError::OciClient(Authentication(io)))`,
    ///   both shapes terminate on the same `io::Error` and neither yields a
    ///   `ClientError` to a `downcast_ref` — every wrapper is
    ///   `#[error(transparent)]`, so the walk starts past it in both.
    #[error("failed to list the tags on {repository}")]
    ListTags {
        repository: String,
        #[source]
        source: Box<ocx_package::error::Error>,
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

    /// The C4 retry re-read the head that won the race — the branch head when
    /// accumulating, the index base when rebuilding a spent or stale branch —
    /// but the package root is absent from it: the winning commit deleted or
    /// never carried it.
    /// Distinct from [`Self::MissingBaseRef`]: the ref resolved fine, the FILE
    /// at it did not.
    #[error("no committed root at {path} on {repo}@{sha} to retry the announce against")]
    MissingHeadRoot { repo: String, path: String, sha: String },

    /// An otherwise-unchanged run found its open pull request unmergeable
    /// (ADR `adr_announce_diverged_branch_rebuild.md` D2). Every run that
    /// commits repoints the branch with [`RefUpdate`](crate::forge::RefUpdate)
    /// `::Reset` and makes the request mergeable by construction, so this is
    /// the one corner announce cannot clear itself: it has no close-request or
    /// delete-ref primitive. Nothing is lost — every tag is already on the base
    /// — only the pull request is stuck.
    #[error(
        "pull request #{number} ({url}) cannot merge into the index base — close it, or delete branch {branch}, so the next announce rebuilds it"
    )]
    PullRequestUnmergeable { number: u64, url: String, branch: String },

    /// Writing a root or CAS object to the `--out` directory failed.
    #[error("failed to write {path}")]
    OutputWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// `Display` body for [`AnnounceError::RootNameMismatch`] — thiserror's format
/// string cannot branch, and a root with no `name` at all would otherwise
/// render as `names , not …`, which reads like a bug in the tool rather than a
/// defect in the file.
fn root_name_mismatch_message(path: &str, committed: &str, expected: &str) -> String {
    if committed.is_empty() {
        return format!("committed root at {path} carries no name; this run announces {expected}");
    }
    format!("committed root at {path} names {committed}, not the {expected} this run announces")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_variant_classifies_via_the_inner_error() {
        let error = AnnounceError::Ssrf {
            namespace: "ocx.sh".to_string(),
            source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                host: "127.0.0.1".to_string(),
                ip: "127.0.0.1".parse().expect("valid ip literal"),
            },
        };
        // ocx#455: the message names the exact config entry, so an operator
        // who keyed the exemption on the physical host learns which key to use.
        assert!(
            error.to_string().contains("[registries.\"ocx.sh\"].trusted_hosts"),
            "got: {error}"
        );
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
            source: Box::new(ocx_package::error::Error::EmptyPushSet),
        };
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
    }

    /// The description fetch reaches the same registry as the observe loop, so
    /// its failures classify the same way — an unreachable registry must not
    /// collapse to exit 1 just because it was the description being fetched.
    #[test]
    fn observe_desc_variant_classifies_via_the_inner_error() {
        let inner = ocx_oci::client::error::ClientError::ManifestNotFound("x".to_string());
        let error = AnnounceError::ObserveDesc {
            repository: "oci://ghcr.io/acme/widget".to_string(),
            source: Box::new(inner),
        };
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
}
