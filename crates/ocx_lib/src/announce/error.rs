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
    #[error("no curated tags given")]
    NoCuratedTags,

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

    /// A curated tag resolves to a single-platform image manifest; announce v1
    /// observes multi-platform image indexes only (ocx packages always publish
    /// an image index).
    #[error(
        "tag {tag} on {repository} resolves to a single-platform image manifest, expected a multi-platform image index"
    )]
    SinglePlatformManifest { tag: String, repository: String },

    /// A curated tag's image index carries no platform entries to observe.
    #[error("tag {tag} on {repository} resolves to an image index with no platform entries")]
    EmptyObservation { tag: String, repository: String },

    /// A curated tag's image index carries a malformed platform-manifest digest.
    #[error("tag {tag} on {repository} carries a malformed platform digest {digest}")]
    MalformedPlatformDigest {
        tag: String,
        repository: String,
        digest: String,
    },

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

impl crate::cli::ClassifyExitCode for AnnounceError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // Delegated explicitly — see the module doc for why the generic
            // source-chain walker cannot reach any of these on its own.
            Self::Ssrf(inner) => inner.classify(),
            Self::Forge(inner) => inner.classify(),
            Self::Observe { source, .. } => source.classify(),
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

    #[test]
    fn unclassified_variant_defers_to_the_chain_walker() {
        assert_eq!(AnnounceError::NoCuratedTags.classify(), None);
    }
}
