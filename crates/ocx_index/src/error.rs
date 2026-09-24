// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// [`Error::InvalidIndexUrl`] `origin` for the configured base.
pub const INDEX_URL_FROM_REGISTRIES: &str = "[registries.\"<ns>\"] index";

/// [`Error::InvalidIndexUrl`] `origin` for the index-role mirror override
/// keyed by `upstream` — the `[mirrors]` table entry, or the `OCX_MIRRORS`
/// environment entry that fed it.
///
/// Interpolates the real key rather than a placeholder: the operator has to
/// find the entry, and the upstream host is what it is keyed by.
pub fn index_url_from_mirrors(upstream: &str) -> String {
    format!("[mirrors.\"{upstream}\"] index")
}

/// Hands `error` to a singleflight leader's in-flight cohort and returns the
/// leader's own form of it.
///
/// The leader gives its error to the broadcast, so it cannot also return it by
/// value; both ends carry the same [`ArcError`] instead, under
/// [`Error::SourceFetchFailed`] — transparent, so neither sees a prefix an
/// uncoalesced fetch would not have produced.
pub fn broadcast_failure<V: Clone>(handle: ocx_util::singleflight::Handle<V>, error: Error) -> Error {
    let shared = ArcError::from(error);
    handle.fail(shared.clone());
    Error::SourceFetchFailed(shared)
}

/// Peels the wrapper a coalesced fetch's leader returns — the inverse of
/// [`broadcast_failure`].
///
/// `Display` and exit-code classification already read *through*
/// [`Error::SourceFetchFailed`]: it is `#[error(transparent)]` and its
/// `classify` arm delegates to the wrapped error. Anything matching on the
/// error's **structure** does not — a `matches!` over the typed variants sees
/// the wrapper, not the variant underneath — so every structural test of a
/// source error goes through here first. `ChainedIndex::is_source_outage`,
/// which decides whether an index outage may be answered from the committed
/// local root, is the load-bearing one.
///
/// One hop, deliberately, because one is all a leader can add:
/// `OcxIndex::resolve_root` runs its `check_format_version()?` *before* it
/// acquires the root handle, so the config and root fetches are never both in
/// flight on one call. Were a second layer ever introduced, a single peel
/// leaves the outer wrapper in place and a structural test says "no" — the
/// caller then propagates, which is the safe direction for every consumer here
/// (holding an error is what would be unsafe).
///
/// Both halves of a coalesced call are peeled, because both are the same
/// leader's error wearing a different wrapper. The **leader** returns
/// [`Error::SourceFetchFailed`]; a **waiter** that lost the race receives
/// [`Error::SingleflightFailed`] carrying the leader's error type-erased
/// through [`SharedError`](ocx_util::singleflight::SharedError). Peeling
/// only the leader would make the held-vs-propagated verdict depend on which
/// caller happened to win — and since the coalescing group exists precisely
/// because there *is* a concurrent fan-out, the waiter shape is the common one
/// under load, not the exotic one.
///
/// `SharedError` keeps its payload private but exposes it as its `source()`
/// (deliberately, so `classify_error` can recover the discriminant), so the
/// peel needs no new accessor on the primitive.
///
/// Only [`singleflight::Error::Failed`](ocx_util::singleflight::Error::Failed)
/// is peeled. `Abandoned`, `Timeout` and `CapacityExceeded` are the primitive's
/// own coordination failures, not a source's verdict — they carry no leader
/// error to peel and must keep propagating.
pub fn coalesced_cause(error: &Error) -> &Error {
    use std::error::Error as _;
    match error {
        Error::SourceFetchFailed(arc) => arc.as_error(),
        Error::SingleflightFailed(ocx_util::singleflight::Error::Failed(shared)) => shared
            .source()
            .and_then(|cause| cause.downcast_ref::<ArcError>())
            .map_or(error, ArcError::as_error),
        other => other,
    }
}

/// The index tier's own `ocx_lib::error::file_error`: the same two fields into
/// the same shape, so it reconstructs as `Error::InternalFile(path, cause)`.
pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::File(ocx_util::error::FileError::new(path, error))
}

/// A clonable handle on an index error, for broadcasting one leader's failure
/// to a singleflight cohort.
///
/// [`Error`] is not `Clone` — [`Self::File`] holds an `io::Error` — so the
/// leader cannot both hand its error to the broadcast and return it by value.
///
/// Deliberately `Arc<Error>` rather than the type-erased
/// [`SharedError`](ocx_util::singleflight::SharedError): erasure existed to
/// escape a *crate-wide* error this tier may no longer name, and a tier that
/// owns its root has nothing to escape. Keeping it typed is what lets
/// [`coalesced_cause`] stay a structural match rather than a downcast, which
/// `ChainedIndex::is_source_outage` depends on.
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
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// JSON serialization or deserialization failed.
    ///
    /// Reconstructs as `ocx_lib::Error::SerializationFailure`. Distinct from
    /// [`Self::MalformedIndexDocument`], which names the document whose bytes
    /// failed and is the index's own wire-shape refusal; this is the generic
    /// serde failure the crate-wide error carried before the tier had a root.
    #[error("JSON serialization error")]
    SerializationFailure(#[from] serde_json::Error),

    /// An OCI client operation failed. Reconstructs as
    /// `ocx_lib::Error::OciClient`.
    #[error(transparent)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    /// A digest string could not be parsed. Reconstructs as
    /// `ocx_lib::Error::Digest`.
    #[error(transparent)]
    Digest(#[from] ocx_oci::digest::error::DigestError),

    /// A pinned identifier validation failed. Reconstructs as
    /// `ocx_lib::Error::PinnedIdentifier`.
    #[error(transparent)]
    PinnedIdentifier(#[from] ocx_oci::pinned_package_ref::PinnedIdentifierError),

    /// The index tier reached the store and the store refused.
    ///
    /// Reconstructed as `ocx_lib::Error::FileStructure` by the `From` impl at
    /// the CLI boundary, so the value a caller classifies is byte-for-byte the
    /// one it was before this tier owned its own root (DEC-23).
    #[error(transparent)]
    Store(#[from] ocx_store::file_structure::error::Error),

    /// A file operation under the index home failed.
    ///
    /// Carries [`FileError`](ocx_util::error::FileError)'s two fields and no
    /// `source()` (ocx#286), so it reconstructs into
    /// `ocx_lib::Error::InternalFile(path, cause)` exactly.
    #[error(transparent)]
    File(#[from] ocx_util::error::FileError),

    /// A path under the index home had no parent, or was otherwise not a
    /// usable filesystem path. Reconstructs into
    /// `ocx_lib::Error::InternalPathInvalid`.
    #[error("invalid path: {0}")]
    PathInvalid(std::path::PathBuf),

    /// A remote manifest was expected but not found during index update.
    #[error("remote manifest not found for '{0}' during index update")]
    RemoteManifestNotFound(String),

    /// A refresh had candidate tags but none could become a version pointer:
    /// each resolved to no manifest, to a bare (single-platform) image manifest,
    /// or carried a reserved name. Distinct from [`Self::RemoteManifestNotFound`],
    /// which means the source listed no tags at all — the same "nothing to
    /// install" verdict, so the same exit code, but a different cause and so a
    /// different message.
    #[error(
        "no indexable tag for '{0}' — every candidate tag resolved to no manifest, a bare manifest, or a reserved name"
    )]
    NoIndexableTag(String),

    /// A chained-index source walk failed. Carries the original typed error
    /// inside an [`ArcError`] so it can be cloned for singleflight broadcast
    /// to waiters while preserving the full error chain. The leader and
    /// every waiter see the same underlying `crate::Error`.
    #[error("chained index source walk failed: {0}")]
    SourceWalkFailed(#[source] ArcError),

    /// A singleflight coordination primitive failed (capacity exceeded,
    /// timeout, or abandoned leader). Distinct from [`Self::SourceWalkFailed`],
    /// which reports a source-side failure.
    ///
    /// Raised by every coalescing group under `oci/index`, not just the chain
    /// walk: `ChainedIndex`'s resolve group, `OcxIndex`'s `config_group` /
    /// `root_group`, and `OciIndex`'s tag groups. The message names no
    /// component for that reason — it said "chained index" while
    /// `ChainedIndex` was the only raiser, and a capacity refusal inside
    /// `OcxIndex` then pointed the operator at a subsystem that was not
    /// involved. Which group it was is a `source()` hop away.
    #[error("index singleflight failed")]
    SingleflightFailed(#[source] ocx_util::singleflight::Error),

    /// A coalesced fetch **within one source** failed — the `config.json` or
    /// root-document leader in [`OcxIndex`](super::OcxIndex).
    ///
    /// The same `ArcError` mechanism as [`Self::SourceWalkFailed`], for the
    /// same reason: a singleflight leader hands its error to the broadcast, so
    /// it cannot also return it by value. Transparent rather than prefixed —
    /// unlike a chain walk, there is no second layer here to name, and the
    /// caller must read the same message a direct, uncoalesced fetch produced.
    #[error(transparent)]
    SourceFetchFailed(ArcError),

    /// A platform-selected child manifest turned out to be another image
    /// index. The OCI spec does not describe an image index nested inside
    /// another image index, so `PackageManager::resolve` refuses it as an
    /// unsupported shape rather than treating it as a leaf.
    #[error("nested image index at {digest} is not a supported OCI shape")]
    NestedImageIndex { digest: ocx_oci::Digest },

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

    /// An index document carrying the format's version pin — `config.json` or
    /// the `c/index.json` envelope, read off the wire or off a local copy —
    /// declared a `format_version` OCX does not understand. Fail-closed
    /// (`adr_index_indirection.md` F1): a newer wire format may change shapes
    /// OCX would otherwise mis-parse. One pin, one policy, one error, whichever
    /// document carries it.
    #[error("index format_version {version} is not supported")]
    UnsupportedIndexFormat { version: u64 },

    /// A fetched dispatch object's bytes did not hash to the digest the root
    /// pointed at. This is the one place OCX re-derives a digest it did not
    /// mint, so a mismatch is the index path's trust-boundary failure
    /// (`adr_index_indirection.md` F1, CWE-345) — never a silent load.
    #[error("dispatch object digest mismatch: root claims {claimed}, bytes hash to {computed}")]
    DispatchObjectDigestMismatch {
        claimed: ocx_oci::Digest,
        computed: ocx_oci::Digest,
    },

    /// A source answered a digest-addressed chain walk with a DIFFERENT digest
    /// than the one requested. The requested digest is the committed pin (or a
    /// lock's), so accepting the answer would move it — the same trust-boundary
    /// class as [`Self::DispatchObjectDigestMismatch`], one hop further out:
    /// there the bytes disagree with their own claimed digest, here the source's
    /// self-consistent answer disagrees with what was asked for.
    #[error("source answered a request for '{requested}' with '{answered}'")]
    WalkedDigestMismatch {
        requested: ocx_oci::Digest,
        answered: ocx_oci::Digest,
    },

    /// A tag resolved to a yanked entry (per-tag `yanked` marker or root
    /// `status: yanked`) and no explicit opt-in was given. A yank is a
    /// publisher signal, not a delete — a digest-pinned resolve of the same
    /// content still succeeds (`adr_index_indirection.md` F3).
    #[error("'{identifier}' is yanked; resolve it by digest or set OCX_ALLOW_YANKED=1 to override")]
    YankedRefused { identifier: String },

    /// A resolve reached the index configured for the identifier's registry and
    /// that index holds no entry for it. Terminal by construction (ocx#251): a
    /// configured index is authoritative for its whole registry, so its miss is
    /// never handed off to the plain OCI registry underneath — the hand-off is
    /// what let a name resolve past the index and past its yank gate.
    ///
    /// Distinct from every failure arm around it. Reaching this variant means
    /// the index answered and answered "no": an unreachable, malformed or
    /// version-unsupported index raises its own error from
    /// [`OcxIndex::resolve_root`](super::OcxIndex) before a miss can be
    /// observed, so an outage can never present as an absent package.
    #[error(
        "'{identifier}' is not in the index at {base_url}, which is authoritative for every name in \
         registry '{namespace}'; announce it there with `ocx package announce`, or take the namespace \
         off the index with `[registries.\"{namespace}\"] index = \"\"`"
    )]
    NotInIndex {
        identifier: String,
        namespace: String,
        base_url: String,
    },

    /// A root's `repository` pointer was not a well-formed physical reference.
    /// The index-side `oci://` scheme is a strict wire contract
    /// (`adr_index_indirection.md` C3): a missing or unknown scheme is a hard
    /// parse error, never a silent host guess.
    #[error("malformed physical repository reference '{value}' in index root")]
    MalformedPhysicalRef { value: String },

    /// A root's `repository` host was refused by the default-on SSRF guard
    /// (ocx#218): it resolved to a private / loopback / link-local / metadata
    /// address and was not listed in the namespace's `trusted_hosts`. The host
    /// arrives in remote-controlled index data, so it is validated before the
    /// first physical registry request. The fix path is configuration
    /// (`[registries."<ns>"].trusted_hosts`), hence `ConfigError`.
    ///
    /// `namespace` is the logical registry the entry is keyed on — the one the
    /// package identifier carries, not the physical host that was refused. An
    /// operator who keys the entry on the host instead sees only the inner
    /// refusal and cannot tell which entry it is reading (ocx#455).
    #[error(transparent)]
    Ssrf {
        #[from]
        source: ocx_oci::ssrf::PhysicalDialRefused,
    },

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

    /// A dispatch object deserialised as an OCI image index but violates an
    /// invariant of the image spec — a wrong `schemaVersion`, or a descriptor
    /// that cannot address its child. Raised on the read side at both index
    /// boundaries (the live `index.ocx.sh` fetch and the local read-back), so
    /// malformed index data is refused rather than reported as an ordinary
    /// empty selection. Distinct from [`Self::MalformedIndexDocument`], which
    /// means the bytes did not deserialise at all.
    #[error(transparent)]
    InvalidImageIndex(#[from] ocx_oci::manifest::InvalidImageIndex),

    /// A static-file index document (root, dispatch object, or catalog) could
    /// not be parsed as the expected frozen wire shape.
    #[error("malformed index document at {url}")]
    MalformedIndexDocument {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    /// A request to a static-file index endpoint failed at the transport layer
    /// — connection, TLS or an unexpected status over HTTPS, and equally a
    /// permission, path-containment or file-type refusal from the `file://`
    /// transport, which raises this same variant. The source is boxed so the
    /// index error type stays free of a `reqwest` dependency edge.
    ///
    /// `status` carries the HTTP status **structurally** when one was received
    /// (`None` for a pre-response transport failure, and for every `file://`
    /// refusal, which has no status). Formatting it into the boxed source, as
    /// this variant used to, leaves the retry classifier unable to read it back
    /// out of a `Box<dyn Error>` message
    /// (`adr_index_sync_performance.md` D-010a). It is `u16` rather than
    /// `reqwest::StatusCode` to keep that dependency edge out of this type.
    ///
    /// It does **not** affect exit-code classification: every arm of this
    /// variant is [`ExitCode::Unavailable`](ocx_exit::ExitCode::Unavailable) (69),
    /// unchanged. Exit codes are the
    /// CLI surface other tools branch on, and nothing about a retry decision
    /// needs them split.
    #[error("index request to {url} failed")]
    IndexHttpFailed {
        url: String,
        status: Option<u16>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// An index-role traffic target (the `[registries."<namespace>"] index`
    /// base, or its `[mirrors."<host>"] index` role override) uses plain
    /// `http://` but the target host is in neither half of the insecure-host union
    /// (`[registries."<host>"] insecure`, `OCX_INSECURE_REGISTRIES`). The
    /// root document is the index path's trust anchor (nothing pins it from
    /// above), so an on-path attacker on a plaintext index owns every
    /// downstream resolution — refuse loud rather than silently downgrade
    /// (CWE-319, same doctrine as the registry role).
    #[error(
        "index traffic to '{host}' for registry '{namespace}' uses http:// but that host is not allowed plain HTTP; \
         set insecure = true under [registries.\"{host}\"] or add the host to OCX_INSECURE_REGISTRIES"
    )]
    PlainHttpIndexNotAllowed { namespace: String, host: String },

    /// An index-role traffic target is unusable: unparseable, or carrying a
    /// scheme outside the closed set (`adr_servable_index_snapshot.md` C-018 —
    /// `https`, gated `http`, or a `file://` configured base with an empty
    /// authority and an absolute path).
    ///
    /// `origin` names *which* setting the operator must fix, because the
    /// refused value is not always the configured base: a
    /// `[mirrors."<host>"] index` override replaces the scheme after the base
    /// was checked, and a `file://` override is refused there (C-020).
    #[error("invalid index url '{url}' for registry '{namespace}' (from {origin})")]
    InvalidIndexUrl {
        namespace: String,
        url: String,
        /// The setting `url` came from, named as the operator will find it —
        /// [`INDEX_URL_FROM_REGISTRIES`], or
        /// [`index_url_from_mirrors`] carrying the upstream key that selected
        /// the override.
        origin: String,
        /// Absent for a refused scheme, which is a policy decision with no
        /// underlying parse failure. Boxed to keep this variant off
        /// `clippy::result_large_err`'s threshold, matching
        /// [`MirrorConfigError::InvalidEntry`](ocx_config::mirror::MirrorConfigError::InvalidEntry).
        #[source]
        source: Option<Box<ocx_config::mirror::MirrorConfigError>>,
    },

    /// A published index's `c/index.json` catalog carried a key that is not a
    /// well-formed OCI repository path (CWE-22). Catalog keys are
    /// attacker-controlled for a mirrored or compromised index; each key
    /// becomes the `repository` component of an identifier and then a
    /// filesystem path, so a key like `../../victim` would write outside the
    /// index home.
    ///
    /// That registry's enumeration is refused fail-closed
    /// (`adr_index_indirection.md` F2 "surfaces, never silently acts") — never a
    /// filtered key list, which would snapshot a tampered catalog minus the part
    /// that gave it away. Under `ocx index sync` the batch rule then applies:
    /// the other named registries still enumerate and snapshot, and the command
    /// still fails afterwards.
    #[error("index source '{index_source}' served a malformed catalog key '{key}': {reason}")]
    MalformedCatalogKey {
        index_source: String,
        key: String,
        reason: String,
    },

    /// A published index source serves no `c/index.json` at all.
    ///
    /// Distinct from a catalog that lists **zero packages**, and the distinction
    /// is the whole point: a served empty catalog is a source saying "I have
    /// nothing", while an absent document is a source that cannot answer the
    /// question. Collapsing the two let `index sync` exit 0
    /// having refreshed nothing and printed nothing, which is C-013's
    /// authoritative-stop rule inverted ("no fall-through, no empty-set
    /// success"). Reachable from a base URL with a wrong path component, a tree
    /// deployed before `c/` was published, or a CDN 404 on the catalog path.
    #[error("index source '{index_source}' serves no catalog document at {url}")]
    CatalogDocumentAbsent { index_source: String, url: String },
}

#[cfg(test)]
mod tests {
    use super::*;

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
