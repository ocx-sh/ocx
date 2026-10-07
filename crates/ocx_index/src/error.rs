// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_exit::{Pick, Row};

/// [`Error::InvalidIndexUrl`] `origin` for the configured base.
pub const INDEX_URL_FROM_REGISTRIES: &str = "[registries.\"<ns>\"] index";

/// [`Error::InvalidIndexUrl`] `origin` for the index-role mirror override keyed by `upstream`.
pub fn index_url_from_mirrors(upstream: &str) -> String {
    format!("[mirrors.\"{upstream}\"] index")
}

/// Hands `error` to a singleflight leader's cohort and returns the leader's own
/// [`Error::SourceFetchFailed`] carrying the same [`ArcError`].
pub fn broadcast_failure<V: Clone>(handle: ocx_util::singleflight::Handle<V>, error: Error) -> Error {
    let shared = ArcError::from(error);
    handle.fail(shared.clone());
    Error::SourceFetchFailed(shared)
}

/// Peels the wrapper a coalesced fetch returns — the inverse of [`broadcast_failure`].
///
/// A structural `matches!` sees only the wrapper, so every structural test of a
/// source error (e.g. `ChainedIndex::is_source_outage`) must peel here first.
pub fn coalesced_cause(error: &Error) -> &Error {
    use std::error::Error as _;
    match error {
        // One hop suffices: `OcxIndex::resolve_root` never has config and root fetches in flight together.
        Error::SourceFetchFailed(arc) => arc.as_error(),
        // Peel a waiter's type-erased leader error too, or the held-vs-propagated verdict depends on who won.
        // Only `Failed` peels: the other singleflight errors carry no leader error and must propagate.
        Error::SingleflightFailed(ocx_util::singleflight::Error::Failed(shared)) => shared
            .source()
            .and_then(|cause| cause.downcast_ref::<ArcError>())
            .map_or(error, ArcError::as_error),
        other => other,
    }
}

/// Wraps an I/O error with its path as [`Error::File`].
pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::File(ocx_util::error::FileError::new(path, error))
}

/// A clonable handle on an index error, for broadcasting one leader's failure
/// to a singleflight cohort.
///
/// Typed rather than [`SharedError`](ocx_util::singleflight::SharedError), so
/// [`coalesced_cause`] stays a structural match.
#[derive(Debug, Clone)]
pub struct ArcError(std::sync::Arc<Error>);

impl ArcError {
    /// Returns a reference to the wrapped error.
    pub fn as_error(&self) -> &Error {
        &self.0
    }
}

impl From<Error> for ArcError {
    fn from(error: Error) -> Self {
        Self(std::sync::Arc::new(error))
    }
}

impl std::fmt::Display for ArcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for ArcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// The index tier's own result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors specific to OCI index operations.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "OciIndexError")]
pub enum Error {
    /// JSON serialization or deserialization failed; unlike [`Self::MalformedIndexDocument`], names no document.
    #[error("JSON serialization error")]
    #[exit(
        DataError,
        slug = "index_serialization",
        summary = "An index document could not be serialized"
    )]
    SerializationFailure(#[from] serde_json::Error),

    /// An OCI client operation failed.
    #[error(transparent)]
    #[exit(delegate)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    /// A digest string could not be parsed.
    #[error(transparent)]
    #[exit(delegate)]
    Digest(#[from] ocx_oci::digest::error::DigestError),

    /// A pinned identifier validation failed.
    #[error(transparent)]
    #[exit(delegate)]
    PinnedIdentifier(#[from] ocx_oci::pinned_package_ref::PinnedIdentifierError),

    /// The store refused an index-tier operation.
    #[error(transparent)]
    #[exit(delegate)]
    Store(#[from] ocx_store::file_structure::error::Error),

    /// A file operation under the index home failed.
    #[error(transparent)]
    #[exit(delegate)]
    File(#[from] ocx_util::error::FileError),

    /// A path under the index home was not usable (e.g. it had no parent).
    #[error("invalid path: {0}")]
    #[exit(Failure, slug = "index_path_invalid", summary = "A local index path is invalid")]
    PathInvalid(std::path::PathBuf),

    /// A remote manifest was expected but not found during index update.
    #[error("remote manifest not found for '{0}' during index update")]
    #[exit(
        NotFound,
        slug = "index_manifest_not_found",
        summary = "The registry has no manifest for a tag the index update walked"
    )]
    RemoteManifestNotFound(String),

    /// A refresh had candidate tags but none could become a version pointer
    /// (no manifest, a bare manifest, or a reserved name).
    #[error(
        "no indexable tag for '{0}' — every candidate tag resolved to no manifest, a bare manifest, or a reserved name"
    )]
    #[exit(
        NotFound,
        slug = "no_indexable_tag",
        summary = "The repository carries no tag the index can record"
    )]
    NoIndexableTag(String),

    /// A chained-index source walk failed; the leader and every singleflight waiter share one [`ArcError`].
    #[error("chained index source walk failed: {0}")]
    #[exit(
        with = source_failure,
        rows((Failure, slug = "index_source_failed", summary = "An index source failed with an unclassified cause"))
    )]
    SourceWalkFailed(#[source] ArcError),

    /// A singleflight coordination primitive failed (capacity exceeded, timeout
    /// or abandoned leader), as opposed to a source-side failure.
    // Names no component: every coalescing group in this crate raises it, so a name would misdirect.
    #[error("index singleflight failed")]
    // `chain` defers the code: a `Some` would end the walk before the leader's typed source, so waiters would exit 1.
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "index_singleflight_failed",
            summary = "A shared index operation failed with an unclassified cause"
        )
    )]
    SingleflightFailed(#[source] ocx_util::singleflight::Error),

    /// A coalesced fetch within one source (the `config.json` or root-document
    /// leader in [`OcxIndex`](super::OcxIndex)) failed.
    // Transparent, or a coalesced fetch reads differently from the same uncoalesced one.
    #[error(transparent)]
    #[exit(
        with = source_failure,
        rows((Failure, slug = "index_source_failed", summary = "An index source failed with an unclassified cause"))
    )]
    SourceFetchFailed(ArcError),

    /// A platform-selected child manifest was itself an image index, which the OCI spec does not describe.
    #[error("nested image index at {digest} is not a supported OCI shape")]
    #[exit(
        DataError,
        slug = "nested_image_index",
        summary = "An image index nests another image index, which OCI does not support"
    )]
    NestedImageIndex { digest: ocx_oci::Digest },

    /// `--offline` or `--frozen` refused to ask a source what the local index
    /// cannot answer; `policy` is the flag label (`"offline"` / `"frozen"`).
    #[error("{}", .block.message(.identifier, .policy))]
    #[exit(
        PolicyBlocked,
        slug = "index_resolution_blocked",
        summary = "A local policy refused resolving through the index"
    )]
    PolicyResolutionBlocked {
        identifier: String,
        policy: &'static str,
        block: PolicyBlock,
    },

    /// An index document declared a `format_version` OCX does not understand.
    /// Fail-closed (`adr_index_indirection.md#f1`): a newer format may change shapes OCX would mis-parse.
    #[error("index format_version {version} is not supported")]
    #[exit(
        DataError,
        slug = "unsupported_index_format",
        summary = "The index format version is not supported"
    )]
    UnsupportedIndexFormat { version: u64 },

    /// A fetched dispatch object's bytes did not hash to the digest the root
    /// pointed at (`adr_index_indirection.md#f1`, CWE-345).
    #[error("dispatch object digest mismatch: root claims {claimed}, bytes hash to {computed}")]
    #[exit(
        DataError,
        slug = "index_dispatch_digest_mismatch",
        summary = "A dispatch object does not hash to the digest its root claims"
    )]
    DispatchObjectDigestMismatch {
        claimed: ocx_oci::Digest,
        computed: ocx_oci::Digest,
    },

    /// A source answered a digest-addressed chain walk with a different digest
    /// than the requested pin; accepting it would move the pin.
    #[error("source answered a request for '{requested}' with '{answered}'")]
    #[exit(
        DataError,
        slug = "walked_digest_mismatch",
        summary = "A source answered a digest request with different content"
    )]
    WalkedDigestMismatch {
        requested: ocx_oci::Digest,
        answered: ocx_oci::Digest,
    },

    /// A tag resolved to a yanked entry and no opt-in was given; a digest-pinned
    /// resolve of the same content still succeeds (`adr_index_indirection.md#f3`).
    #[error("'{identifier}' is yanked; resolve it by digest or set OCX_ALLOW_YANKED=1 to override")]
    #[exit(DataError, slug = "yanked_refused", summary = "The resolved version is yanked")]
    YankedRefused { identifier: String },

    /// The index configured for the identifier's registry holds no entry for it.
    ///
    /// Terminal: a configured index owns its whole registry, and handing the miss
    /// to the plain registry would resolve past the yank gate. An unreachable or
    /// malformed index raises its own error instead, never this.
    #[error(
        "'{identifier}' is not in the index at {base_url}, which is authoritative for every name in \
         registry '{namespace}'; announce it there with `ocx package announce`, or take the namespace \
         off the index with `[registries.\"{namespace}\"] index = \"\"`"
    )]
    #[exit(NotFound, slug = "not_in_index", summary = "The package is not listed in the index")]
    NotInIndex {
        identifier: String,
        namespace: String,
        base_url: String,
    },

    /// A root's `repository` pointer was not a well-formed `oci://` reference
    /// (`adr_index_indirection.md#c3`); a missing scheme is never a host guess.
    #[error("malformed physical repository reference '{value}' in index root")]
    #[exit(
        DataError,
        slug = "malformed_physical_ref",
        summary = "An index root carries a malformed physical repository reference"
    )]
    MalformedPhysicalRef { value: String },

    /// The SSRF guard refused a root's `repository` host (a private, loopback,
    /// link-local or metadata address not in `[registries."<ns>"].trusted_hosts`).
    #[error(transparent)]
    #[exit(delegate = source)]
    Ssrf {
        #[from]
        source: ocx_oci::ssrf::PhysicalDialRefused,
    },

    /// An existing derived root names a different `repository` than the identifier
    /// implies; refused rather than overwritten (`adr_index_indirection.md#f1`).
    #[error("derived root for '{repository}' points at '{found}', expected '{expected}'")]
    #[exit(
        DataError,
        slug = "root_repository_mismatch",
        summary = "A derived index root points at another repository"
    )]
    RootRepositoryMismatch {
        repository: String,
        expected: String,
        found: String,
    },

    /// A dispatch object parsed as an OCI image index but violates the image
    /// spec (a wrong `schemaVersion`, or a descriptor that cannot address its child).
    #[error(transparent)]
    #[exit(
        DataError,
        slug = "invalid_image_index",
        summary = "An image index document is invalid"
    )]
    InvalidImageIndex(#[from] ocx_oci::manifest::InvalidImageIndex),

    /// A static-file index document (root, dispatch object, or catalog) could
    /// not be parsed as the expected frozen wire shape.
    #[error("malformed index document at {url}")]
    #[exit(
        DataError,
        slug = "malformed_index_document",
        summary = "An index document is not valid JSON of the expected shape"
    )]
    MalformedIndexDocument {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    /// A static-file index request failed: connection, TLS, an unexpected HTTPS
    /// status, or a `file://` refusal.
    ///
    /// `status` is the HTTP status when one was received, read by the retry
    /// classifier (`adr_index_sync_performance.md#d-010`). Exits 75 when
    /// [`Error::is_transient_transport`], else 69.
    #[error("index request to {url} failed")]
    #[exit(
        with = http_failure,
        rows(
            (TempFail, slug = "index_http_transient", summary = "An index request failed in a way a retry may clear"),
            (Unavailable, slug = "index_http_failed", summary = "An index request failed"),
        )
    )]
    IndexHttpFailed {
        url: String,
        status: Option<u16>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// An index-role target uses `http://` on a host not allowed plain HTTP
    /// (CWE-319): the root document is the trust anchor, so plaintext would hand
    /// every downstream resolution to an on-path attacker.
    #[error(
        "index traffic to '{host}' for registry '{namespace}' uses http:// but that host is not allowed plain HTTP; \
         set insecure = true under [registries.\"{host}\"] or add the host to OCX_INSECURE_REGISTRIES"
    )]
    #[exit(
        ConfigError,
        slug = "plain_http_index_not_allowed",
        summary = "An index URL uses plain HTTP for a host not allowed to"
    )]
    PlainHttpIndexNotAllowed { namespace: String, host: String },

    /// An index-role target is unparseable or uses a scheme outside `https`, gated
    /// `http`, or a `file://` configured base (`adr_servable_index_snapshot.md`).
    #[error("invalid index url '{url}' for registry '{namespace}' (from {origin})")]
    #[exit(
        ConfigError,
        slug = "invalid_index_url",
        summary = "A configured index URL is invalid"
    )]
    InvalidIndexUrl {
        namespace: String,
        url: String,
        /// The setting `url` came from: [`INDEX_URL_FROM_REGISTRIES`] or [`index_url_from_mirrors`].
        origin: String,
        /// Absent for a refused scheme, which has no underlying parse failure.
        #[source]
        source: Option<Box<ocx_config::mirror::MirrorConfigError>>,
    },

    /// A published catalog carried a key that is not a well-formed repository
    /// path (CWE-22). The registry's enumeration is refused whole, never filtered,
    /// which would snapshot a tampered catalog (`adr_index_indirection.md#f2`).
    #[error("index source '{index_source}' served a malformed catalog key '{key}': {reason}")]
    #[exit(
        DataError,
        slug = "malformed_catalog_key",
        summary = "An index source served a malformed catalog key"
    )]
    MalformedCatalogKey {
        index_source: String,
        key: String,
        reason: String,
    },

    /// A published index source serves no `c/index.json` at all — distinct from an
    /// empty catalog, so `index sync` fails instead of exiting 0 having refreshed nothing.
    #[error("index source '{index_source}' serves no catalog document at {url}")]
    #[exit(
        Unavailable,
        slug = "catalog_document_absent",
        summary = "The index source serves no catalog document"
    )]
    CatalogDocumentAbsent { index_source: String, url: String },
}

impl Error {
    /// Whether this is an [`Error::IndexHttpFailed`] a rerun may clear: a transient status, or a
    /// refused or timed-out connect.
    ///
    /// The source is downcast directly, never walked: a refused certificate arrives wrapped in
    /// `UntrustedCertificateHint`, and a `file://` refusal or the size cap carries no `reqwest` error.
    pub fn is_transient_transport(&self) -> bool {
        let Self::IndexHttpFailed { status, source, .. } = self else {
            return false;
        };
        status.is_some_and(ocx_oci::transport_policy::is_transient_status)
            || source
                .downcast_ref::<reqwest::Error>()
                .is_some_and(ocx_oci::transport_policy::is_transient_transport_error)
    }
}

/// Starts the cause walk at the shared inner error: [`ArcError::source`] skips it, so a chain over the field would lose its verdict.
fn source_failure(error: &Error, [row]: [Row; 1]) -> Pick<'_> {
    match error {
        Error::SourceWalkFailed(arc) | Error::SourceFetchFailed(arc) => Pick::chain(arc.as_error(), row),
        _ => Pick::row(row),
    }
}

/// A retryable transport failure exits 75, any other failed index request 69.
fn http_failure(error: &Error, [transient, failed]: [Row; 2]) -> Pick<'_> {
    Pick::row(if error.is_transient_transport() {
        transient
    } else {
        failed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picker is wired to two arms only; the fallback arm answers the declared row instead of
    /// chaining back into the error it was handed, which would classify it again.
    #[test]
    fn source_failure_fallback_answers_the_declared_row() {
        static ENTRY: ocx_exit::DetailEntry = ocx_exit::DetailEntry {
            slug: "index_source_failed",
            exit_code: ocx_exit::ExitCode::Failure,
            family: "Error",
            summary: "An index source failed with an unclassified cause",
        };
        let error = Error::NestedImageIndex {
            digest: ocx_oci::Digest::Sha256("0".repeat(64)),
        };

        let pick = source_failure(&error, [Row::__declared(&ENTRY, false)]);

        assert_eq!(pick.code(), Some(ocx_exit::ExitCode::Failure));
        assert!(matches!(pick.detail(), ocx_exit::Detail::Fixed(entry) if entry.slug == "index_source_failed"));
    }

    /// An SSRF refusal classifies to `ConfigError` (78): the fix path is adding
    /// the host to `[registries."<ns>"].trusted_hosts`.
    #[test]
    fn ssrf_refusal_classifies_as_config_error() {
        let error = Error::Ssrf {
            source: ocx_oci::ssrf::PhysicalDialRefused {
                namespace: "ocx.sh".to_string(),
                source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                    host: "127.0.0.1".to_string(),
                    ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                },
            },
        };
        // ocx#455: the message names the exact entry, keyed on the logical
        // namespace, so an operator who keyed it on the physical host learns
        // which key to use.
        assert!(
            error.to_string().contains("[registries.\"ocx.sh\"].trusted_hosts"),
            "got: {error}"
        );
    }

    /// The refusal has to name the config key, quoted with its port, or the
    /// operator is told only half the fix. Every other test of this variant
    /// matches on its shape, which the old env-var-only wording satisfied too.
    #[test]
    fn the_plain_http_index_refusal_names_both_ways_to_allow_it() {
        let rendered = Error::PlainHttpIndexNotAllowed {
            namespace: "ocx.sh".to_string(),
            host: "index.corp:8080".to_string(),
        }
        .to_string();

        assert!(rendered.contains("index.corp:8080"), "{rendered}");
        assert!(
            rendered.contains("set insecure = true under [registries.\"index.corp:8080\"]"),
            "the exact TOML key, port included, is what an operator can paste: {rendered}"
        );
        assert!(rendered.contains("OCX_INSECURE_REGISTRIES"), "{rendered}");
    }

    /// The wrapper adds nothing: `Ssrf` is `#[error(transparent)]`, so a chain
    /// walker that stringifies the index error reads the byte string
    /// `ocx_oci::ssrf::PhysicalDialRefused` renders.
    ///
    /// Asserted here rather than beside that type: the wording is `ocx_oci`'s
    /// and the wrapper is `ocx_index`'s, and `ocx_oci` may not name `ocx_index`
    /// (ADR 1.9). The literal is the one `ssrf.rs`'s own test pins, restated so
    /// a transparency that quietly became a prefix reds on this side.
    #[test]
    fn the_ssrf_variant_renders_its_source_verbatim() {
        let refused = ocx_oci::ssrf::PhysicalDialRefused {
            namespace: "ocx.sh".to_string(),
            source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                host: "127.0.0.1".to_string(),
                ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            },
        };
        let rendered = refused.to_string();
        assert_eq!(
            rendered,
            "the physical host of ocx.sh/\u{2026} was refused; list it (bare host, no port) under \
             [registries.\"ocx.sh\"].trusted_hosts"
        );
        assert_eq!(Error::from(refused).to_string(), rendered);
    }
}

/// What a no-resolve policy could not look up — the half of
/// [`Error::PolicyResolutionBlocked`] that decides what the refusal tells the
/// user to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyBlock {
    /// An unpinned tag the local index does not hold.
    UnpinnedTag,
    /// A name in a registry an index owns, whose location no locally
    /// committed root records — only the index can say where it lives.
    UnrecordedLocation,
}

impl PolicyBlock {
    /// The refusal's rendered text, shared by every error that carries one.
    pub fn message(self, identifier: &str, policy: &str) -> String {
        match self {
            Self::UnpinnedTag => format!(
                "{policy} mode refused to resolve unpinned reference '{identifier}'; run `ocx index update` or pin a digest"
            ),
            Self::UnrecordedLocation => format!(
                "'{identifier}' is served by an index and has no locally recorded location, which {policy} mode cannot look up; run `ocx index update {identifier}` once online"
            ),
        }
    }
}
