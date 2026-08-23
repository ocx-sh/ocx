// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;
use crate::oci::{Digest, Identifier, PinnedIdentifier, native};

/// Errors that can occur during OCI client operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Authentication with the registry failed.
    #[error("registry authentication failed: {0}")]
    Authentication(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Digest mismatch between expected and actual content hash.
    /// Fires for manifest digests and for verified blob digests.
    #[error("digest mismatch: expected '{expected}', got '{actual}'")]
    DigestMismatch { expected: String, actual: String },
    /// The transport delivered fewer bytes than the manifest-declared blob size.
    ///
    /// Distinguishes an incomplete delivery (transport truncation, or an
    /// ocx-side short read) from the registry serving wrong content
    /// ([`ClientError::DigestMismatch`], CWE-345). Both produce a
    /// non-matching hash — a prefix cannot hash to the whole — so without
    /// this variant every incomplete transfer is reported as if the registry
    /// had lied, in both directions.
    #[error("short blob read: got {actual} of {expected} bytes")]
    ShortBlobRead { expected: u64, actual: u64 },
    /// The decompressed output of a layer exceeded the decompression-bomb cap
    /// (CWE-400) before extraction completed. The compressed stream is rejected
    /// rather than allowed to exhaust disk. `cap` is the byte ceiling that was
    /// crossed.
    #[error("decompressed layer exceeded the {cap}-byte cap (possible decompression bomb)")]
    DecompressionCapExceeded { cap: u64 },
    /// Expected an image manifest but got an image index or unknown type.
    #[error("expected an image manifest, got an image index")]
    UnexpectedManifestType,
    /// Manifest structure is invalid (e.g. wrong layer count, missing fields).
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    /// The registry answered a manifest request with something that cannot be
    /// a manifest — an HTML login portal, a proxy error page, a mirror pointed
    /// at a host that no longer serves the registry.
    ///
    /// Distinct from [`Self::DigestMismatch`], which is what this used to be
    /// reported as: the bytes are not corrupted, they are the wrong bytes
    /// entirely, and the source names the content type and the URL they
    /// finally came from. Both exit 65 — rerunning fixes neither.
    #[error("registry did not answer with a manifest")]
    NotAManifest(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A registry served an image index that deserialised but violates the OCI
    /// image spec (wrong `schemaVersion`, unaddressable descriptor). Refused at
    /// registry admission so malformed bytes never reach the index.
    #[error(transparent)]
    InvalidImageIndex(#[from] crate::oci::manifest::InvalidImageIndex),
    /// A single-layer artifact manifest's `artifactType` did not match what
    /// the caller expected. Raised by
    /// [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("unexpected artifact type: expected '{expected}', got {actual:?}")]
    UnexpectedArtifactType { expected: String, actual: Option<String> },
    /// A single-layer artifact manifest had zero or more than one layer.
    /// Raised by [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("expected exactly one layer, got {count}")]
    WrongLayerCount { count: usize },
    /// A single-layer artifact's layer `mediaType` did not match what the
    /// caller expected. Raised by
    /// [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("unexpected layer media type: expected '{expected}', got '{actual}'")]
    UnexpectedLayerMediaType { expected: String, actual: String },
    /// A single-layer artifact's declared layer size exceeded the
    /// caller-supplied ceiling (CWE-400 pre-check, before any bytes are
    /// fetched). Raised by [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    LayerSizeExceeded { declared: i64, maximum: u64 },
    /// A registry-supplied graph exceeded a traversal limit, so the operation
    /// would only ever have completed in part.
    ///
    /// Refusing is the point: a copy that silently dropped a signature past its
    /// referrer limit reports success for an artifact that is no longer signed
    /// at the target. Carries the limit and the value that crossed it so a
    /// caller can tell hostile input from a ceiling that wants raising without
    /// parsing the message.
    #[error("{limit_kind} limit of {limit} exceeded (reached {actual}) while copying {subject}")]
    TraversalLimitExceeded {
        limit_kind: TraversalLimit,
        limit: usize,
        actual: usize,
        subject: String,
    },
    /// The requested manifest does not exist in the registry.
    #[error("manifest not found: {0}")]
    ManifestNotFound(String),
    /// The requested repository does not exist in the registry.
    ///
    /// Distinct from [`ClientError::Registry`] so callers can treat an
    /// authoritative "repository absent" (e.g. first publish to a new
    /// repository) differently from a transient registry failure.
    #[error("repository not found: {0}")]
    RepositoryNotFound(String),
    /// A referenced blob does not exist in the registry.
    ///
    /// The identifier is the canonical OCX `registry/repository[:tag]@digest`
    /// form: the registry + repository of the image the lookup was
    /// issued against, plus the missing blob's digest. The advisory
    /// tag (when present) is the tag of the image that triggered the
    /// blob resolution — not the blob itself.
    #[error("blob not found: {0}")]
    BlobNotFound(PinnedIdentifier),
    /// A registry operation failed.
    #[error("registry operation failed: {0}")]
    Registry(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The registry named a destination the transport refuses to contact.
    ///
    /// Exactly the two typed request-time guards, both of which name a
    /// destination and both of which refuse it: an upload-session `Location`
    /// off the registry's own origin (the credential and the blob body would go
    /// to a host the caller never named — CWE-918) and a plaintext
    /// authentication realm named by an HTTPS registry (the credential would
    /// cross in the clear — CWE-319).
    ///
    /// A redirect the transport did not follow is [`Self::UnfollowedRedirect`],
    /// not this — the two shapes share an exit code and nothing else, and half
    /// of that set refuses nothing.
    ///
    /// Exit 65, alongside [`ClientError::DigestMismatch`] and
    /// [`ClientError::NotAManifest`], for the same reason those two are there:
    /// the registry served something wrong and a rerun reaches the same
    /// answer. 69 would tell a CI retry wrapper to re-issue the push against
    /// the same hostile `Location` until its budget runs out — the one thing
    /// the guard exists to prevent — and would make a registry outage
    /// indistinguishable from a credential-exfiltration attempt. 78 would be
    /// wrong for a different reason: no config change fixes a `Location`
    /// header, so it would send the operator to a file with nothing to edit.
    #[error("registry named a destination the transport refuses: {0}")]
    UnsafeDestination(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The registry answered with a redirect the transport did not follow.
    ///
    /// Four causes, and the transport cannot tell them apart once the error
    /// reaches this layer — two are refusals, two are not:
    ///
    /// - the redirect policy declining an `https` -> `http` hop;
    /// - the upload path declining a mid-session handoff (its session-URL
    ///   requests run on a no-redirect client precisely so the blob body is not
    ///   replayed to a registry-chosen host);
    /// - the hop chain exceeding the fork's limit;
    /// - a redirect no client could act on at all — a missing or unparseable
    ///   `Location`, or a body that cannot be replayed — which `tower-http`
    ///   hands back as the 3xx itself on any client, whatever its policy.
    ///
    /// Split from [`Self::UnsafeDestination`] for the last two: they name no
    /// destination and refuse nothing, so reporting them as a refused unsafe
    /// destination sends an operator hunting for an attacker over a registry
    /// that emitted a malformed `302`.
    ///
    /// Exit 65 for all four, the same as its sibling and for the same reason —
    /// a rerun walks the same chain to the same answer.
    #[error("registry answered with a redirect the transport did not follow: {0}")]
    UnfollowedRedirect(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A registry operation failed for a reason that may not repeat: the
    /// connect never completed, a request timed out, or the registry answered
    /// 429 / 502 / 503 / 504.
    ///
    /// Split from [`ClientError::Registry`] because the two demand opposite
    /// reactions from a caller: this one says "run the same command again",
    /// [`ClientError::Registry`] says "the registry answered, and it will
    /// answer the same way next time".
    #[error("transient registry failure: {0}")]
    RegistryTransient(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// File I/O error with path context.
    #[error("I/O error for '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
    /// Invalid UTF-8 encoding encountered.
    #[error("invalid UTF-8 encoding: {0}")]
    InvalidEncoding(#[source] std::string::FromUtf8Error),
    /// An internal library error (e.g. codesign, archive processing).
    #[error("{0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A read routed through a configured mirror failed.
    ///
    /// Carries the routing alongside the failure: the upstream host the caller
    /// asked for, the mirror host that stood in for it, and the physical
    /// reference actually fetched. Without it a mirror failure names a host the
    /// user never typed and a config entry nothing points back to.
    ///
    /// Adds provenance, never a verdict — [`ClassifyExitCode`] delegates
    /// straight to the wrapped error, so wrapping can never change an exit
    /// code. Not-found sentinels are deliberately never wrapped: callers match
    /// on them to mean "absent", and burying one here would turn a missing tag
    /// into a hard failure.
    #[error("fetching '{physical}' via mirror '{mirror}' configured for '{origin}'")]
    Mirrored {
        origin: String,
        mirror: String,
        physical: String,
        #[source]
        source: Box<ClientError>,
    },

    /// The registry does not implement the OCI Referrers API and has no
    /// fallback-tag referrers index. Distinct from [`Self::Registry`] and
    /// [`Self::RegistryTransient`]: the registry is reachable and answering,
    /// the endpoint simply is not served — a rerun can never change that.
    #[error("registry {registry} does not support the OCI Referrers API")]
    ReferrersUnsupported { registry: String },
}

/// Which bounded traversal ran out of room, for
/// [`ClientError::TraversalLimitExceeded`].
///
/// A closed set rather than a message fragment, so the text cannot drift from
/// the constant that produced it and a caller can match on the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalLimit {
    /// How deep a referrer-of-a-referrer chain is followed.
    ReferrerDepth,
    /// How many referrer manifests one leaf may carry, across the whole chain.
    ReferrersPerLeaf,
    /// How many distinct blobs one manifest may name.
    BlobsPerManifest,
}

impl std::fmt::Display for TraversalLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReferrerDepth => "referrer depth",
            Self::ReferrersPerLeaf => "referrers per leaf",
            Self::BlobsPerManifest => "blobs per manifest",
        })
    }
}

impl ClientError {
    /// Wrap any error as a [`ClientError::Internal`].
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }

    /// Builds a [`ClientError::BlobNotFound`] from the image the lookup
    /// was issued against and the missing blob's digest.
    ///
    /// The image's own digest (if any) is dropped — the stored identifier
    /// carries the *blob* digest, which is what was actually missing.
    /// Falls back to [`ClientError::Registry`] if the image reference
    /// cannot produce a well-formed [`PinnedIdentifier`]. This path is
    /// unreachable after a HEAD succeeded against the registry: the
    /// transport has already used `image` to issue a real HTTP request,
    /// so the reference is known-valid by construction. The debug
    /// assertions fire loudly in dev builds to catch any regression.
    pub fn blob_not_found(image: &native::Reference, blob_digest: &Digest) -> Self {
        let identifier = match Identifier::try_from(image.clone()) {
            Ok(id) => id.clone_with_digest(blob_digest.clone()),
            Err(e) => {
                debug_assert!(false, "unreachable after HEAD succeeded: {e}");
                return Self::Registry(Box::new(e));
            }
        };
        match PinnedIdentifier::try_from(identifier) {
            Ok(pinned) => Self::BlobNotFound(pinned),
            Err(e) => {
                debug_assert!(false, "unreachable after HEAD succeeded: {e}");
                Self::Registry(Box::new(e))
            }
        }
    }
}

// ── Shared artifact-fetch shape classification ──────────────────────────────

/// Intermediate classification of a [`ClientError`] returned by
/// [`crate::oci::client::Client::fetch_single_layer_artifact`], shared by
/// every domain that maps the fetch failure onto its own error taxonomy —
/// `crate::patch::persistence` and `crate::managed_config::persistence` had
/// byte-identical match arms translating `ClientError`'s shape-validation
/// variants onto their own (identically-shaped) domain enum. Domains keep
/// their own error type (one enum per module); this only factors out the
/// `ClientError` classification itself.
#[derive(Debug)]
pub(crate) enum ArtifactFetchError {
    /// The manifest was not a single-image manifest (image index or
    /// otherwise unexpected shape).
    UnexpectedManifest { detail: String },
    /// The artifact type did not match what the caller expected.
    UnexpectedArtifactType { actual: Option<String> },
    /// The manifest had zero or more than one layer.
    WrongLayerCount { count: usize },
    /// The layer's `mediaType` did not match what the caller expected.
    UnexpectedLayerMediaType { expected: String, actual: String },
    /// The declared layer size exceeded the caller-supplied ceiling.
    LayerSizeExceeded { declared: i64, maximum: u64 },
    /// Every other `ClientError` — the caller's own catch-all (network, auth,
    /// registry failures, etc).
    Other(ClientError),
}

impl ArtifactFetchError {
    /// Classifies `error`. `manifest_kind` is spliced into the
    /// `UnexpectedManifestType` detail message (e.g. `"__ocx.patch"`,
    /// `"managed config"`) so each domain's message stays specific.
    pub(crate) fn classify(error: ClientError, manifest_kind: &str) -> Self {
        match error {
            ClientError::UnexpectedManifestType => Self::UnexpectedManifest {
                detail: format!("expected image manifest for {manifest_kind}, got image index"),
            },
            ClientError::UnexpectedArtifactType { actual, .. } => Self::UnexpectedArtifactType { actual },
            ClientError::WrongLayerCount { count } => Self::WrongLayerCount { count },
            ClientError::UnexpectedLayerMediaType { expected, actual } => {
                Self::UnexpectedLayerMediaType { expected, actual }
            }
            ClientError::LayerSizeExceeded { declared, maximum } => Self::LayerSizeExceeded { declared, maximum },
            ClientError::InvalidManifest(detail) => Self::UnexpectedManifest { detail },
            source => Self::Other(source),
        }
    }
}

impl ClassifyExitCode for ClientError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            // Provenance only: the routing does not change what went wrong, so
            // the wrapped error keeps its own code.
            Self::Mirrored { source, .. } => return source.classify(),
            Self::Authentication(_) => ExitCode::AuthError,
            Self::ManifestNotFound(_) | Self::BlobNotFound(_) | Self::RepositoryNotFound(_) => ExitCode::NotFound,
            Self::Io { .. } => ExitCode::IoError,
            // The 75-vs-69 contract: 75 means the same command may succeed if
            // it is run again, 69 means it will not. `Registry` is the second
            // case — the registry answered, just not usefully — so a wrapper
            // that retries on 75 leaves it alone.
            Self::Registry(_) => ExitCode::Unavailable,
            Self::RegistryTransient(_) => ExitCode::TempFail,
            // An incomplete delivery is a transfer fault, not malformed data:
            // the same pull usually succeeds on retry, so it is TempFail rather
            // than DataError.
            Self::ShortBlobRead { .. } => ExitCode::TempFail,
            // A registry that does not serve the Referrers API is answering
            // correctly about a capability it lacks — never transient, and not
            // a data fault either, so it carries its own code.
            Self::ReferrersUnsupported { .. } => ExitCode::ReferrersUnsupported,
            Self::DigestMismatch { .. }
            | Self::UnsafeDestination(_)
            | Self::UnfollowedRedirect(_)
            | Self::DecompressionCapExceeded { .. }
            | Self::UnexpectedManifestType
            | Self::InvalidManifest(_)
            | Self::NotAManifest(_)
            | Self::InvalidImageIndex(_)
            | Self::UnexpectedArtifactType { .. }
            | Self::WrongLayerCount { .. }
            | Self::UnexpectedLayerMediaType { .. }
            | Self::LayerSizeExceeded { .. }
            // A graph too large to traverse completely is registry-supplied
            // input this build refuses, not a fault that a rerun can clear.
            | Self::TraversalLimitExceeded { .. }
            | Self::Serialization(_)
            | Self::InvalidEncoding(_) => ExitCode::DataError,
            Self::Internal(_) => ExitCode::Failure,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An incomplete delivery exits 75 (retry), the registry serving wrong
    /// content exits 65 (terminal). Asserting both pins the *distinction* —
    /// folding `ShortBlobRead` into the `DataError` bucket would make a
    /// transient truncation look like a supply-chain failure to `case $?`.
    /// Mirror routing is provenance, so the exit code must be whatever the
    /// wrapped failure would have exited with on its own. Asserting two
    /// different codes is what pins the delegation — a single case passes just
    /// as well against a hardcoded constant.
    #[test]
    fn a_mirrored_failure_delegates_its_exit_code_to_the_wrapped_one() {
        let mirrored = |source: ClientError| ClientError::Mirrored {
            origin: "ghcr.io".to_string(),
            mirror: "artifactory.example.com".to_string(),
            physical: "artifactory.example.com/ghcr-remote/owner/tool:1.0".to_string(),
            source: Box::new(source),
        };

        assert_eq!(
            mirrored(ClientError::DigestMismatch {
                expected: "sha256:aaa".to_string(),
                actual: "sha256:bbb".to_string(),
            })
            .classify(),
            Some(ExitCode::DataError)
        );
        assert_eq!(
            mirrored(ClientError::RegistryTransient(Box::new(std::io::Error::other(
                "connect timed out"
            ))))
            .classify(),
            Some(ExitCode::TempFail)
        );
    }

    /// The message must name all three: what was fetched, the mirror it went
    /// to, and the upstream that mirror stands in for. Any one alone leaves the
    /// reader unable to connect the failure to the config entry behind it.
    #[test]
    fn a_mirrored_failure_names_the_routing_in_its_message() {
        let rendered = ClientError::Mirrored {
            origin: "ghcr.io".to_string(),
            mirror: "artifactory.example.com".to_string(),
            physical: "artifactory.example.com/ghcr-remote/owner/tool:1.0".to_string(),
            source: Box::new(ClientError::NotAManifest(Box::new(std::io::Error::other(
                "unexpected content type 'text/html'",
            )))),
        }
        .to_string();

        assert!(rendered.contains("ghcr.io"), "must name the upstream, got: {rendered}");
        assert!(
            rendered.contains("artifactory.example.com"),
            "must name the mirror, got: {rendered}"
        );
        assert!(
            rendered.contains("ghcr-remote/owner/tool:1.0"),
            "must name the physical reference, got: {rendered}"
        );
    }

    #[test]
    fn short_blob_read_is_temp_fail_while_digest_mismatch_stays_data_error() {
        assert_eq!(
            ClientError::ShortBlobRead {
                expected: 1024,
                actual: 512,
            }
            .classify(),
            Some(ExitCode::TempFail)
        );
        assert_eq!(
            ClientError::DigestMismatch {
                expected: "sha256:aaa".to_string(),
                actual: "sha256:bbb".to_string(),
            }
            .classify(),
            Some(ExitCode::DataError)
        );
    }
}
