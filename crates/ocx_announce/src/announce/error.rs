// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the announce orchestration.
//!
//! Exit classifiers must match these variants explicitly: a `source()` walk skips
//! the transparent `Forge`, and `Observe`'s boxed source never matches
//! `downcast_ref::<ClientError>()`.

/// Failures raised by [`announce`](super::announce).
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
// The catch-all slug the retired `_` arm answered for a future variant; published in the error schema, so kept.
#[exit(reserve(
    Failure,
    slug = "announce_failed",
    summary = "Announcing failed with an unclassified cause"
))]
#[non_exhaustive]
pub enum AnnounceError {
    /// No forge was given; announce reads the committed index root through one in every mode.
    #[error("announce requires a forge to read the committed index root")]
    #[exit(
        defer(Failure),
        slug = "forge_required",
        summary = "No forge was supplied to read the committed index root through"
    )]
    ForgeRequired,

    /// The resolved curated tag set was empty; `reserved_dropped` names the
    /// reserved tags the filter removed on the way.
    #[error("{}", no_curated_tags_message(reserved_dropped))]
    #[exit(
        UsageError,
        slug = "no_curated_tags",
        summary = "No tag is left to announce once reserved tags are dropped"
    )]
    NoCuratedTags { reserved_dropped: Vec<String> },

    /// No committed root exists for the package at `base_ref` — a new package
    /// goes through the human package-claim lane, never announce.
    #[error(
        "unclaimed package: no committed root at {path} on {base_ref} for {package} — new packages go through the human lane"
    )]
    #[exit(
        NotFound,
        slug = "unclaimed_package",
        summary = "The package has no index entry to announce into"
    )]
    UnclaimedPackage {
        package: String,
        path: String,
        base_ref: String,
    },

    /// The committed root bytes are not valid JSON.
    #[error("committed root at {path} is not valid JSON")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "index_root_parse",
            summary = "The committed index root is not valid JSON"
        )
    )]
    RootParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    /// The committed root parsed but is not a JSON object.
    #[error("committed root at {path} is not a JSON object")]
    #[exit(
        defer(Failure),
        slug = "index_root_not_object",
        summary = "The committed index root is not a JSON object"
    )]
    RootNotObject { path: String },

    /// The committed root's `name` disagrees with the identifier the run
    /// announces; an absent `name` is a mismatch with an empty `committed`.
    #[error("{}", root_name_mismatch_message(path, committed, expected))]
    #[exit(
        DataError,
        slug = "root_name_mismatch",
        summary = "The committed index root names a different package"
    )]
    RootNameMismatch {
        path: String,
        committed: String,
        expected: String,
    },

    /// The committed root is missing a field announce needs to proceed.
    #[error("committed root is missing the {field} field")]
    #[exit(
        defer(Failure),
        slug = "index_root_missing_field",
        summary = "The committed index root lacks a required field"
    )]
    RootMissingField { field: &'static str },

    /// The root's `repository` pointer is not a well-formed `oci://host/path`
    /// reference (strict parse).
    #[error("malformed physical repository pointer {value}")]
    #[exit(
        defer(Failure),
        slug = "malformed_physical_repository",
        summary = "The index root names a malformed registry repository"
    )]
    MalformedPhysicalRepository { value: String },

    /// The physical host failed the SSRF pre-flight; `namespace` is the logical
    /// namespace the `trusted_hosts` exemption is keyed on, not the host.
    #[error(
        "the physical host of {namespace}/… was refused; list it (bare host, no port) under [registries.\"{namespace}\"].trusted_hosts"
    )]
    // Delegated explicitly: `Observe`/`ObserveDesc`/`ListTags` box their source (never downcasts) and
    // `Forge` is transparent; `Ssrf` delegates for uniformity.
    #[exit(delegate = source)]
    Ssrf {
        namespace: String,
        #[source]
        source: ocx_oci::ssrf::SsrfError,
    },

    /// A curated tag does not resolve on the physical repository.
    #[error("tag {tag} does not resolve on {repository} — check for a typo")]
    #[exit(
        NotFound,
        slug = "unresolved_tag",
        summary = "A tag to announce does not exist in the registry"
    )]
    UnresolvedTag { tag: String, repository: String },

    /// A tag read as absent, then present on the follow-up probe: a push raced the two reads.
    #[error("tag {tag} on {repository} appeared while it was being observed; retry the announce")]
    // A push landed between the two reads; a rerun observes the tag as present.
    #[exit(
        TempFail,
        slug = "observe_raced",
        summary = "A tag appeared between the two reads of one announce"
    )]
    ObserveRaced { tag: String, repository: String },

    /// Fetching a curated tag's manifest from the physical registry failed.
    ///
    /// Boxed, or `ClientError`'s size trips `clippy::result_large_err`.
    #[error("failed to observe tag {tag} on {repository}")]
    #[exit(delegate = source)]
    Observe {
        tag: String,
        repository: String,
        #[source]
        source: Box<ocx_oci::client::error::ClientError>,
    },

    /// Fetching or decoding the `__ocx.desc` artifact failed.
    #[error("failed to observe the description of {repository}")]
    #[exit(delegate = source)]
    ObserveDesc {
        repository: String,
        #[source]
        source: Box<ocx_oci::client::error::ClientError>,
    },

    /// The committed root records a description the physical repository no
    /// longer serves; retraction is unspecified, so announce never clears `desc`.
    #[error("__ocx.desc disappeared from {repository} (was {digest})")]
    #[exit(
        DataError,
        slug = "desc_disappeared",
        summary = "The package description vanished between two reads"
    )]
    DescDisappeared { repository: String, digest: String },

    /// The regenerated root would delete a committed tag under a selection that
    /// never deletes (`adr_announce_diverged_branch_rebuild.md § The invariant,
    /// restated`).
    ///
    /// Sound only while `GitWorkspace::commit_files` refuses a fast-forward onto
    /// a head the run did not read. Reserved tags and
    /// [`TagSelection::Replace`](crate::announce::TagSelection::Replace) are
    /// excluded at the call site.
    #[error("regenerating {path} would drop committed tags never selected for removal: {}", tags.join(", "))]
    // Never `TempFail`: a rerun reproduces it exactly, so a retrying publisher would loop.
    #[exit(
        DataError,
        slug = "committed_tags_dropped",
        summary = "The announce would drop tags the index already committed"
    )]
    CommittedTagsDropped { path: String, tags: Vec<String> },

    /// Listing the physical repository's tags failed (`--tags-from-registry`).
    #[error("failed to list the tags on {repository}")]
    #[exit(delegate = source)]
    ListTags {
        repository: String,
        #[source]
        source: Box<ocx_package::error::Error>,
    },

    /// A curated tag resolves to a bare OCI image manifest, which `ocx package
    /// push` never publishes.
    #[error("tag {tag} on {repository} resolves to an OCI image manifest; the index records image indices only")]
    // Not `NotFound`: the artifact exists, only its shape is wrong.
    #[exit(
        DataError,
        slug = "tag_not_an_image_index",
        summary = "A tag to announce does not point at an image index"
    )]
    TagIsNotAnImageIndex { tag: String, repository: String },

    /// A tag was named to both `--yank` and `--unyank`.
    #[error("tag(s) {tags:?} given to both yank and unyank")]
    #[exit(
        defer(Failure),
        slug = "yank_unyank_overlap",
        summary = "The same tag is both yanked and unyanked"
    )]
    YankUnyankOverlap { tags: Vec<String> },

    /// A `--yank` named a tag outside the curated set.
    #[error("cannot yank {tag}: not in the curated tag set")]
    #[exit(
        defer(Failure),
        slug = "yank_tag_not_curated",
        summary = "A tag to yank is not a curated tag"
    )]
    YankTagNotCurated { tag: String },

    /// A `--unyank` named a tag outside the curated set.
    #[error("cannot unyank {tag}: not in the curated tag set")]
    #[exit(
        defer(Failure),
        slug = "unyank_tag_not_curated",
        summary = "A tag to unyank is not a curated tag"
    )]
    UnyankTagNotCurated { tag: String },

    /// A forge operation (fork ensure, commit, or pull-request) failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Forge(#[from] crate::forge::ForgeError),

    /// No base ref was found on the fork to commit the announce branch onto.
    #[error("no base ref found on {repo} to commit onto")]
    #[exit(
        defer(Failure),
        slug = "missing_base_ref",
        summary = "The index base branch does not exist"
    )]
    MissingBaseRef { repo: String },

    /// The retry re-read the head that won the race, but the package root is
    /// absent from it.
    #[error("no committed root at {path} on {repo}@{sha} to retry the announce against")]
    #[exit(
        defer(Failure),
        slug = "missing_head_root",
        summary = "A concurrent claim's or announce's head carries no index root"
    )]
    MissingHeadRoot { repo: String, path: String, sha: String },

    /// An otherwise-unchanged run found its open pull request unmergeable
    /// (`adr_announce_diverged_branch_rebuild.md`); announce has no close-request
    /// or delete-ref primitive to clear it.
    #[error(
        "pull request #{number} ({url}) cannot merge into the index base — close it, or delete branch {branch}, so the next announce rebuilds it"
    )]
    // Not `TempFail`: a retry can never succeed; only a human clears the conflict.
    #[exit(
        DataError,
        slug = "pull_request_unmergeable",
        summary = "The open announce pull request conflicts with the index"
    )]
    PullRequestUnmergeable { number: u64, url: String, branch: String },

    /// Writing a root or CAS object to the `--output` directory failed.
    #[error("failed to write {path}")]
    // The generic `io::Error` walker maps only `PermissionDenied`; any other kind would exit 1.
    #[exit(IoError, slug = "output_write", summary = "Writing the output tree failed")]
    OutputWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// `Display` body for [`AnnounceError::RootNameMismatch`]; a root with no
/// `name` would otherwise render as `names , not …`.
fn root_name_mismatch_message(path: &str, committed: &str, expected: &str) -> String {
    if committed.is_empty() {
        return format!("committed root at {path} carries no name; this run announces {expected}");
    }
    format!("committed root at {path} names {committed}, not the {expected} this run announces")
}

/// `Display` body for [`AnnounceError::NoCuratedTags`].
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
