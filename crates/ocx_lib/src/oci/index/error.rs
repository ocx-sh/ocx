// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

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
/// value; both ends carry the same [`ArcError`](crate::error::ArcError)
/// instead, under [`Error::SourceFetchFailed`] — transparent, so neither sees a
/// prefix an uncoalesced fetch would not have produced.
pub fn broadcast_failure<V: Clone>(
    handle: crate::utility::singleflight::Handle<V>,
    error: crate::Error,
) -> crate::Error {
    let shared = crate::error::ArcError::from(error);
    handle.fail(shared.clone());
    Error::SourceFetchFailed(shared).into()
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
/// through [`SharedError`](crate::utility::singleflight::SharedError). Peeling
/// only the leader would make the held-vs-propagated verdict depend on which
/// caller happened to win — and since the coalescing group exists precisely
/// because there *is* a concurrent fan-out, the waiter shape is the common one
/// under load, not the exotic one.
///
/// `SharedError` keeps its payload private but exposes it as its `source()`
/// (deliberately, so `classify_error` can recover the discriminant), so the
/// peel needs no new accessor on the primitive.
///
/// Only [`singleflight::Error::Failed`](crate::utility::singleflight::Error::Failed)
/// is peeled. `Abandoned`, `Timeout` and `CapacityExceeded` are the primitive's
/// own coordination failures, not a source's verdict — they carry no leader
/// error to peel and must keep propagating.
pub fn coalesced_cause(error: &crate::Error) -> &crate::Error {
    use std::error::Error as _;
    match error {
        crate::Error::OciIndex(Error::SourceFetchFailed(arc)) => arc.as_error(),
        crate::Error::OciIndex(Error::SingleflightFailed(crate::utility::singleflight::Error::Failed(shared))) => {
            shared
                .source()
                .and_then(|cause| cause.downcast_ref::<crate::error::ArcError>())
                .map_or(error, crate::error::ArcError::as_error)
        }
        other => other,
    }
}

/// Errors specific to OCI index operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
    SourceWalkFailed(#[source] crate::error::ArcError),

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
    SingleflightFailed(#[source] crate::utility::singleflight::Error),

    /// A coalesced fetch **within one source** failed — the `config.json` or
    /// root-document leader in [`OcxIndex`](super::OcxIndex).
    ///
    /// The same `ArcError` mechanism as [`Self::SourceWalkFailed`], for the
    /// same reason: a singleflight leader hands its error to the broadcast, so
    /// it cannot also return it by value. Transparent rather than prefixed —
    /// unlike a chain walk, there is no second layer here to name, and the
    /// caller must read the same message a direct, uncoalesced fetch produced.
    #[error(transparent)]
    SourceFetchFailed(crate::error::ArcError),

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
        claimed: crate::oci::Digest,
        computed: crate::oci::Digest,
    },

    /// A source answered a digest-addressed chain walk with a DIFFERENT digest
    /// than the one requested. The requested digest is the committed pin (or a
    /// lock's), so accepting the answer would move it — the same trust-boundary
    /// class as [`Self::DispatchObjectDigestMismatch`], one hop further out:
    /// there the bytes disagree with their own claimed digest, here the source's
    /// self-consistent answer disagrees with what was asked for.
    #[error("source answered a request for '{requested}' with '{answered}'")]
    WalkedDigestMismatch {
        requested: crate::oci::Digest,
        answered: crate::oci::Digest,
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
    #[error(transparent)]
    Ssrf(#[from] crate::oci::ssrf::SsrfError),

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
    InvalidImageIndex(#[from] crate::oci::manifest::InvalidImageIndex),

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
    /// variant is [`ExitCode::Unavailable`] (69), unchanged. Exit codes are the
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
        /// [`MirrorConfigError::InvalidEntry`](crate::config::mirror::MirrorConfigError::InvalidEntry).
        #[source]
        source: Option<Box<crate::config::mirror::MirrorConfigError>>,
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

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            // Both mean "nothing here to install" to a wrapper: no tags at all,
            // or no tag that could carry a version. An exit code is a coarse
            // contract — the message is what disambiguates the two.
            // Same verdict, same code: the package is not installable. Keeping
            // 79 here means the `case $?` contract is untouched by ocx#251 —
            // what changed is the message, which now names the index that was
            // consulted instead of leaving a bare "not found".
            Self::RemoteManifestNotFound(_) | Self::NoIndexableTag(_) | Self::NotInIndex { .. } => ExitCode::NotFound,
            Self::NestedImageIndex { .. } => ExitCode::DataError,
            // Delegate to the full chain walker on the wrapped typed error,
            // not just a single-hop `classify()` on the inner `Error`. Mirrors
            // the `PackageErrorKind::Internal(inner)` pattern so nested causes
            // (e.g. a `ClientError::Authentication` inside a `crate::Error`)
            // are resolved via the generic `try_classify` ladder.
            Self::SourceWalkFailed(arc) | Self::SourceFetchFailed(arc) => {
                return Some(crate::cli::classify_error(arc.as_error()));
            }
            // Yield to the chain walker rather than answering here: the variant
            // carries the singleflight error as `#[source]`, and that type
            // classifies itself (a broadcast leader failure defers to the
            // leader's own typed error, a timeout is `TempFail`). Answering
            // `Failure` here would be a terminal `Some` that ends the walk, so
            // every waiter would exit 1 while the leader exits 80/65/69 — the
            // same operation reporting a different code depending on whether it
            // happened to win the singleflight race. Mirrors
            // `PackageManagerError::SetupFailed`, which wraps the same type.
            Self::SingleflightFailed(_) => return None,
            // A deliberate local policy (offline / frozen) refused the
            // resolution — categorically the same as every other policy block.
            Self::PolicyResolutionBlocked { .. } => ExitCode::PolicyBlocked,
            // Malformed / untrusted static-file index input at a trust
            // boundary — the OCI data-error class (65).
            Self::UnsupportedIndexFormat { .. }
            | Self::DispatchObjectDigestMismatch { .. }
            | Self::WalkedDigestMismatch { .. }
            | Self::YankedRefused { .. }
            | Self::MalformedPhysicalRef { .. }
            | Self::RootRepositoryMismatch { .. }
            | Self::MalformedCatalogKey { .. }
            | Self::InvalidImageIndex(_)
            | Self::MalformedIndexDocument { .. } => ExitCode::DataError,
            // A transport-layer failure reaching the static-file index — the
            // resource is unavailable, same class as a registry outage. An
            // absent catalog document is the same class for the same reason:
            // the source is reachable but is not serving what a published index
            // must serve, and every cause of it is external and retryable.
            //
            // Deliberately status-blind: `IndexHttpFailed::status` exists for
            // the retry classifier, not to split this arm. Fanning it out would
            // be a CLI-surface break for a distinction nothing asked for.
            Self::IndexHttpFailed { .. } | Self::CatalogDocumentAbsent { .. } => ExitCode::Unavailable,
            // A misconfigured index-role traffic target — a configuration fault.
            Self::PlainHttpIndexNotAllowed { .. } | Self::InvalidIndexUrl { .. } => ExitCode::ConfigError,
            // An SSRF-refused physical host — the fix is `trusted_hosts` config.
            Self::Ssrf(_) => ExitCode::ConfigError,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An SSRF refusal classifies to `ConfigError` (78): the fix path is adding
    /// the host to `[registries."<ns>"].trusted_hosts`.
    #[test]
    fn ssrf_refusal_classifies_as_config_error() {
        let error = Error::Ssrf(crate::oci::ssrf::SsrfError::ForbiddenTarget {
            host: "127.0.0.1".to_string(),
            ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        });
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
    }

    /// The exit code of a concurrent resolve must not depend on who reached the
    /// key first.
    ///
    /// Both errors are built in the exact shape `walk_chain` produces: the
    /// leader propagates `SourceWalkFailed(ArcError)`, and the waiter receives
    /// that same error broadcast inside
    /// `SingleflightFailed(Failed(SharedError(..)))`. Testing only the leader
    /// cannot see the bug — the leader arm was always correct; it was the
    /// waiter's terminal `Some(Failure)` that ended the walk before the typed
    /// error underneath was ever reached.
    ///
    /// Chain-walked via [`crate::cli::classify_error`] rather than a single-hop
    /// `classify()`, because the whole mechanism is the walk continuing through
    /// `#[source]`.
    #[test]
    fn leader_and_waiter_classify_a_source_walk_failure_identically() {
        use crate::utility::singleflight;

        // One case per exit-code class a source walk realistically produces:
        // a rejected credential, malformed publisher data, an unreachable index.
        let cases: Vec<(crate::Error, ExitCode)> = vec![
            (
                crate::Error::OciClient(crate::oci::client::error::ClientError::Authentication(Box::new(
                    std::io::Error::other("token refused"),
                ))),
                ExitCode::AuthError,
            ),
            (
                crate::Error::OciIndex(Error::YankedRefused {
                    identifier: "ocx.sh/kitware/cmake:3.28".to_string(),
                }),
                ExitCode::DataError,
            ),
            (
                crate::Error::OciIndex(Error::IndexHttpFailed {
                    url: "https://index.ocx.sh/c/index.json".to_string(),
                    status: None,
                    source: Box::new(std::io::Error::other("connection reset")),
                }),
                ExitCode::Unavailable,
            ),
        ];

        for (inner, expected) in cases {
            let arc = crate::error::ArcError::from(inner);
            let leader = Error::SourceWalkFailed(arc.clone());
            let waiter = Error::SingleflightFailed(singleflight::Error::Failed(singleflight::SharedError::for_test(
                Error::SourceWalkFailed(arc),
            )));

            let leader_code = crate::cli::classify_error(&leader);
            let waiter_code = crate::cli::classify_error(&waiter);
            assert_eq!(leader_code, expected, "leader must report the source walk's own code");
            assert_eq!(
                waiter_code, leader_code,
                "waiter must report the same code as the leader for the same failure"
            );
        }
    }

    /// A coordination failure with no leader error to defer to still carries its
    /// own meaning: a singleflight timeout is transient, so it must reach
    /// `TempFail` (75) — the retryable class — rather than collapsing to a
    /// generic failure the way the terminal arm did.
    #[test]
    fn singleflight_timeout_classifies_as_temp_fail() {
        let error = Error::SingleflightFailed(crate::utility::singleflight::Error::Timeout);
        assert_eq!(crate::cli::classify_error(&error), ExitCode::TempFail);
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
}
