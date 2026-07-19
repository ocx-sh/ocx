// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// Errors specific to OCI index operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A remote manifest was expected but not found during index update.
    #[error("remote manifest not found for '{0}' during index update")]
    RemoteManifestNotFound(String),

    /// A chained-index source walk failed. Carries the original typed error
    /// inside an [`ArcError`] so it can be cloned for singleflight broadcast
    /// to waiters while preserving the full error chain. The leader and
    /// every waiter see the same underlying `crate::Error`.
    #[error("chained index source walk failed: {0}")]
    SourceWalkFailed(#[source] crate::error::ArcError),

    /// A singleflight coordination primitive failed (capacity exceeded,
    /// timeout, or abandoned leader) while walking the chain. Distinct
    /// from [`Self::SourceWalkFailed`], which reports a source-side failure.
    #[error("chained index singleflight failed")]
    SingleflightFailed(#[source] crate::utility::singleflight::Error),

    /// A platform-selected child manifest turned out to be another image
    /// index. The OCI spec does not describe an image index nested inside
    /// another image index, so `PackageManager::resolve` refuses it as an
    /// unsupported shape rather than treating it as a leaf.
    #[error("nested image index at {digest} is not a supported OCI shape")]
    NestedImageIndex { digest: crate::oci::Digest },

    /// A no-resolve routing policy (`--offline` or `--frozen`) refused to
    /// resolve an unpinned (tag-only) reference from a source. The local
    /// index did not have the tag and the active policy forbids walking the
    /// chain to fetch + commit an unknown version. `policy` is the lowercase
    /// flag label (`"offline"` / `"frozen"`); `identifier` is the reference
    /// that could not be resolved. Populate the local index (e.g.
    /// `ocx index update`) or loosen the flag.
    #[error(
        "{policy} mode refused to resolve unpinned reference '{identifier}'; run `ocx index update` or pin a digest"
    )]
    PolicyResolutionBlocked { identifier: String, policy: &'static str },

    /// A static-file index (`index.ocx.sh`) served a `config.json` whose
    /// `format_version` OCX does not understand. Fail-closed
    /// (`adr_index_indirection.md` F1): a newer wire format may change shapes
    /// OCX would otherwise mis-parse.
    #[error("index.ocx.sh config format_version {version} is not supported")]
    UnsupportedIndexFormat { version: u64 },

    /// A fetched observation object's bytes did not hash to the digest the
    /// root pointed at. This is the one place OCX re-derives a digest it did
    /// not mint, so a mismatch is the index path's trust-boundary failure
    /// (`adr_index_indirection.md` F1, CWE-345) — never a silent load.
    #[error("observation object digest mismatch: root claims {claimed}, bytes hash to {computed}")]
    ObservationDigestMismatch {
        claimed: crate::oci::Digest,
        computed: crate::oci::Digest,
    },

    /// A tag resolved to a yanked entry (per-tag `yanked` marker or root
    /// `status: yanked`) and no explicit opt-in was given. A yank is a
    /// publisher signal, not a delete — a digest-pinned resolve of the same
    /// content still succeeds (`adr_index_indirection.md` F3).
    #[error("'{identifier}' is yanked; resolve it by digest or set OCX_ALLOW_YANKED=1 to override")]
    YankedRefused { identifier: String },

    /// A root's `repository` pointer was not a well-formed physical reference.
    /// The index-side `oci://` scheme is a strict wire contract
    /// (`adr_index_indirection.md` C3): a missing or unknown scheme is a hard
    /// parse error, never a silent host guess.
    #[error("malformed physical repository reference '{value}' in index root")]
    MalformedPhysicalRef { value: String },

    /// An existing OCX-authored derived root document names a different physical
    /// `repository` than the identifier being committed implies. Overwriting it
    /// would corrupt the authored root, so a cross-check failure is a hard
    /// `DataError` (`adr_index_indirection.md` F1), never a silent overwrite.
    #[error("derived root for '{repository}' points at '{found}', expected '{expected}'")]
    RootRepositoryMismatch {
        repository: String,
        expected: String,
        found: String,
    },

    /// A static-file index document (root, observation object, or catalog)
    /// could not be parsed as the expected frozen wire shape.
    #[error("malformed index document at {url}")]
    MalformedIndexDocument {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    /// An HTTP request to a static-file index endpoint failed at the transport
    /// layer (connection, TLS, unexpected status). The source is boxed so the
    /// index error type stays free of a `reqwest` dependency edge.
    #[error("index HTTP request to {url} failed")]
    IndexHttpFailed {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// An index-role traffic target (the `[registries."<namespace>"] index`
    /// base, or its `[mirrors."<host>"] index` role override) uses plain
    /// `http://` but the target host is not in `OCX_INSECURE_REGISTRIES`. The
    /// root document is the index path's trust anchor (nothing pins it from
    /// above), so an on-path attacker on a plaintext index owns every
    /// downstream resolution — refuse loud rather than silently downgrade
    /// (CWE-319, same doctrine as the registry role).
    #[error(
        "index traffic to '{host}' for registry '{namespace}' uses http:// but is not in OCX_INSECURE_REGISTRIES; \
         add it to allow plain-HTTP transport"
    )]
    PlainHttpIndexNotAllowed { namespace: String, host: String },

    /// The `[registries."<namespace>"] index` base URL could not be parsed
    /// into a scheme + host.
    #[error("invalid index url configured for registry '{namespace}'")]
    InvalidIndexUrl {
        namespace: String,
        #[source]
        source: crate::config::mirror::MirrorConfigError,
    },

    /// A published index's `c/index.json` catalog carried a key that is not a
    /// well-formed OCI repository path (CWE-22). Catalog keys are
    /// attacker-controlled for a mirrored or compromised index; each key
    /// becomes the `repository` component of an identifier and then a
    /// filesystem path, so a key like `../../victim` would write outside the
    /// index home. The whole sync is refused fail-closed
    /// (`adr_index_indirection.md` F2 "surfaces, never silently acts").
    #[error("index source '{index_source}' served a malformed catalog key '{key}': {reason}")]
    MalformedCatalogKey {
        index_source: String,
        key: String,
        reason: String,
    },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::RemoteManifestNotFound(_) => ExitCode::NotFound,
            Self::NestedImageIndex { .. } => ExitCode::DataError,
            // Delegate to the full chain walker on the wrapped typed error,
            // not just a single-hop `classify()` on the inner `Error`. Mirrors
            // the `PackageErrorKind::Internal(inner)` pattern so nested causes
            // (e.g. a `ClientError::Authentication` inside a `crate::Error`)
            // are resolved via the generic `try_classify` ladder.
            Self::SourceWalkFailed(arc) => return Some(crate::cli::classify_error(arc.as_error())),
            Self::SingleflightFailed(_) => ExitCode::Failure,
            // A deliberate local policy (offline / frozen) refused the
            // resolution — categorically the same as every other policy block.
            Self::PolicyResolutionBlocked { .. } => ExitCode::PolicyBlocked,
            // Malformed / untrusted static-file index input at a trust
            // boundary — the OCI data-error class (65).
            Self::UnsupportedIndexFormat { .. }
            | Self::ObservationDigestMismatch { .. }
            | Self::YankedRefused { .. }
            | Self::MalformedPhysicalRef { .. }
            | Self::RootRepositoryMismatch { .. }
            | Self::MalformedCatalogKey { .. }
            | Self::MalformedIndexDocument { .. } => ExitCode::DataError,
            // A transport-layer failure reaching the static-file index — the
            // resource is unavailable, same class as a registry outage.
            Self::IndexHttpFailed { .. } => ExitCode::Unavailable,
            // A misconfigured index-role traffic target — a configuration fault.
            Self::PlainHttpIndexNotAllowed { .. } | Self::InvalidIndexUrl { .. } => ExitCode::ConfigError,
        })
    }
}
