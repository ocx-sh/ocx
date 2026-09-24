// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publish leg for the managed-config tier — `ocx config push`.
//!
//! Managed config is published as an **ordinary ocx package** whose content
//! is a single `config.toml` file (managed-config v2, ADR
//! `adr_managed_config_tier.md` v2 amendment). No custom artifact type, no
//! parallel publish subsystem: the payload is staged as `config.toml`,
//! bundled via [`ocx_package::bundle::BundleBuilder`] (tar+gzip), given a
//! synthesized minimal bundle metadata, and pushed through the existing
//! [`ocx_package::publisher::Publisher`] — so versioning, cascade tags, rollback
//! and variants all reuse the package machinery.
//!
//! | Function | Concerns | Testable |
//! |---|---|---|
//! | [`validate_managed_config_payload`] | Pure: size cap, TOML parse as [`ocx_config::Config`], `[managed]` rejection, `[trust.sigstore]` XOR, `extra_ca_certs`/`extra_ca_certs_pem` XOR | Unit-testable with synthetic bytes |
//! | [`inline_trusted_root`] | Pure: rewrite a path-form `trusted_root` into `trusted_root_json` | Unit-testable with synthetic text |
//! | [`declared_extra_ca_certs`] | Pure: the path-form `extra_ca_certs` a payload declares, if any | Unit-testable with synthetic text |
//! | [`inline_extra_ca_certs`] | Pure: rewrite a path-form `extra_ca_certs` into `extra_ca_certs_pem` | Unit-testable with synthetic text |
//! | [`guard_inlined_payload_size`] | Pure: re-check the post-inline payload against [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`] | Unit-testable with synthetic bytes |
//! | [`read_candidate_payload`] | I/O: the bounded candidate read `config push` and `config test` share | Unit-testable with a FIFO and an oversize file |
//! | [`publish_managed_config`] | I/O + network: read the trust root and the CA bundle, stage, bundle, push (cascade-aware) | Acceptance test |
//!
//! ## Why the trust root is inlined at publish time
//!
//! `[trust.sigstore] trusted_root = "…"` names a path on the **operator's**
//! machine. A fleet adopting the published payload has no such file, and the
//! loader deliberately ignores a path-form `trusted_root` arriving from the
//! managed tier — so publishing one unchanged would ship a silently inert
//! trust root. [`publish_managed_config`] therefore reads the file, proves it
//! parses as a Sigstore trusted root, and republishes it as the self-contained
//! `trusted_root_json` string the fleet can actually consume.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ocx_config::ConfigTier;
use ocx_config::tls::{ExtraRootsSource, TlsError, parse_pem, read_path};
use ocx_oci::layer_ref::LayerRef;
use ocx_oci::{OciIdentifier, Platform};
use ocx_package::info::Info;
use ocx_package::metadata::{Metadata, bundle};
use ocx_package::publisher::{Publisher, PushOutcome};
use ocx_util::fs::path::FileReference;
use ocx_util::fs::{BoundedReadError, read_bounded_async};

// ── Options ───────────────────────────────────────────────────────────────────

/// Options for [`publish_managed_config`].
#[derive(Debug, Clone)]
pub struct ManagedConfigPublishOptions {
    /// Update rolling variant tags derived from the pushed version tag
    /// (e.g. `user-1.4.2` also updates `user-1.4`, `user-1`, `user`).
    pub cascade: bool,
    /// Platform entry written into the package index. Managed-config fetch
    /// only consumes the platform-agnostic `any/any` entry, so anything else
    /// produces a package `ocx config update` cannot use.
    pub platform: Platform,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors raised while validating or publishing a managed-config payload.
#[derive(Debug, thiserror::Error)]
pub enum ManagedConfigPublishError {
    /// Reading the payload file failed.
    #[error("failed to read managed config payload '{}'", path.display())]
    ReadFailed {
        /// The payload path that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The payload exceeds [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`].
    #[error("managed config payload is {actual} bytes, exceeding the maximum allowed {maximum} bytes")]
    PayloadTooLarge {
        /// Actual payload size in bytes.
        actual: u64,
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The payload is not valid TOML (or not valid UTF-8), or does not match
    /// the config schema.
    #[error("managed config payload is not a valid config file")]
    InvalidToml {
        /// The underlying TOML parse failure.
        #[source]
        source: toml::de::Error,
    },

    /// The payload contains a `[managed]` section. The seed `[managed]` block
    /// lives only in the local `$OCX_HOME/config.toml`; a published payload
    /// carrying one would be stripped on the consumer side anyway (ADR
    /// Decision I), so publishing it is rejected as an operator mistake.
    #[error("managed config payload must not contain a [managed] section")]
    ContainsManagedSection,

    /// The payload's `[trust.sigstore]` declares both `trusted_root` and
    /// `trusted_root_json`. Publishing either silently discards the other, and
    /// which one wins is not predictable from the file.
    #[error("managed config payload declares both trusted_root and trusted_root_json in [trust.sigstore]: keep one")]
    AmbiguousTrustRoot,

    /// The payload declares both `extra_ca_certs` and `extra_ca_certs_pem`
    /// (C-003, S-005). Publishing either silently discards the other, and
    /// which one wins is not predictable from the file — the sibling of
    /// [`Self::AmbiguousTrustRoot`], for the same reason.
    #[error("managed config payload declares both extra_ca_certs and extra_ca_certs_pem: keep one")]
    AmbiguousExtraCaCerts,

    /// A `[[trust.policy]]` signer names its key by path (`key = "etc/acme.pub"`).
    ///
    /// The twin of [`Self::AmbiguousTrustRoot`], for the same reason: a managed
    /// payload is a `config.toml` shipped as a package to a fleet, so a path in
    /// one names the *operator's* disk and means nothing on any consumer's. The
    /// refusal removes an incoherent state rather than adding a guard — inlining
    /// the key with `key_pem` is the form that travels.
    ///
    /// Local tiers (project / operator / user config on the author's own disk)
    /// leave `key` unrestricted; this applies only to a published payload.
    #[error("managed config payload declares a key signer by path in [[trust.policy]]: inline it as `key_pem` instead")]
    ManagedConfigKeyByPath,

    /// A `[[trust.policy]]` entry in the payload does not compile.
    ///
    /// Caught here rather than left to the fleet: a payload is adopted by every
    /// consumer at once, so an empty `signers` array or a malformed `key_pem`
    /// would fail closed on every machine simultaneously, with the diagnostic
    /// arriving at the consumer instead of the operator who wrote it. The path
    /// form is already refused above, so compiling here reads no file.
    #[error("managed config payload declares an unusable [[trust.policy]] entry")]
    InvalidTrustPolicy {
        /// Why the policy could not be compiled.
        #[source]
        source: ocx_trust::TrustPolicyError,
    },

    /// Reading the trusted-root file named by `[trust.sigstore] trusted_root`
    /// failed. The path is resolved relative to the payload's own directory.
    #[error("failed to read trusted root '{}' named by [trust.sigstore] trusted_root", path.display())]
    TrustedRootReadFailed {
        /// The resolved trusted-root path that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The file named by `[trust.sigstore] trusted_root` is not a usable
    /// Sigstore trusted root. Caught here rather than on every machine in the
    /// fleet after adoption.
    #[error("trusted root '{}' is not a usable Sigstore trusted root: {detail}", path.display())]
    TrustedRootInvalid {
        /// The resolved trusted-root path.
        path: PathBuf,
        /// What the trust-root loader rejected.
        detail: String,
    },

    /// Reading the file named by the root-level `extra_ca_certs` failed. The
    /// path is resolved relative to the payload's own directory (D-1, same
    /// grammar and relative rule as `trusted_root` above). Shaped like
    /// [`Self::TrustedRootReadFailed`] (C-003), except that the path travels
    /// as the [`ExtraRootsSource`] the runtime door uses, so a PEM body
    /// pasted where a path belongs is redacted here exactly as it is there
    /// (D-11, CWE-532) rather than echoed whole by `ocx config push`.
    #[error("cannot read {origin} named by the payload")]
    ExtraCaCertsReadFailed {
        /// The resolved path, as the runtime door renders it.
        origin: ExtraRootsSource,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The file named by the root-level `extra_ca_certs` is not a usable CA
    /// bundle (C-004): wrong PEM tag, empty, or malformed — the same
    /// `config::tls::parse_pem` verdict every consumer
    /// would reach, delivered to the operator instead. An over-size bundle
    /// is NOT this variant: `read_bounded` refuses it before a byte is
    /// parsed, so it surfaces as [`Self::ExtraCaCertsReadFailed`] (74).
    /// Classified as a data error (65), not a config error like
    /// [`Self::TrustedRootInvalid`]: the config names the file correctly,
    /// the file's bytes are what is wrong — parity with the loader-side
    /// `TlsError` classification for a file-typed source. The path is named
    /// once, by the inner verdict's origin, never here as well.
    #[error("the extra CA certificate bundle named by the payload is not usable")]
    ExtraCaCertsInvalid {
        /// What the certificate parser rejected, naming the path.
        #[source]
        source: ocx_config::tls::TlsError,
    },

    /// The root-level `extra_ca_certs_pem` the operator authored directly in
    /// the payload is not a usable CA bundle (C-004, inline form). Caught
    /// here, on the publisher's platform, so the diagnostic lands with the
    /// operator; every consumer re-runs the same check with its own verifier
    /// at adoption (`persistence.rs`, `ExtraCaCertsInvalid`) and keeps its
    /// previous snapshot when that one refuses. Config (78), like the
    /// loader's verdict on the same inline text: the payload's own content is
    /// what is wrong, not a file it names. The key is named once, by the
    /// inner verdict's origin, never here as well.
    #[error("the extra CA certificate bundle inlined in the payload is not usable")]
    ExtraCaCertsPemInvalid {
        /// What the certificate parser rejected.
        #[source]
        source: TlsError,
    },

    /// The file named by the root-level `extra_ca_certs` is not UTF-8, so it
    /// cannot be inlined into a TOML string unchanged. Strict, never lossy: a
    /// lossy expansion could push a bundle that fits the 32 KiB read cap past
    /// the same 32 KiB inline cap on every consumer. Data (65), like
    /// [`Self::ExtraCaCertsInvalid`]: the file's bytes are what is wrong.
    #[error(
        "{origin} is not UTF-8 and cannot be inlined; strip the non-UTF-8 label lines outside the \
         -----BEGIN/-----END blocks"
    )]
    ExtraCaCertsNotUtf8 {
        /// The resolved path, as the runtime door renders it.
        origin: ExtraRootsSource,
        /// Where the decode stopped.
        #[source]
        source: std::str::Utf8Error,
    },

    /// Staging the payload into the temporary publish directory failed.
    #[error("failed to stage managed config payload for publishing")]
    StageFailed {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// Bundling the staged payload into a tar+gzip archive failed.
    #[error("failed to bundle managed config payload")]
    BundleFailed {
        /// The underlying bundling failure (boxed: `crate::Error` is large).
        #[source]
        source: Box<crate::Error>,
    },

    /// Listing existing tags for a cascade push failed.
    #[error("failed to list existing tags for '{identifier}'")]
    ListTagsFailed {
        /// The identifier whose tags could not be listed.
        identifier: Box<OciIdentifier>,
        /// The underlying registry failure (boxed: `crate::Error` is large).
        #[source]
        source: Box<crate::Error>,
    },

    /// The push itself failed.
    #[error("failed to push managed config package")]
    PushFailed {
        /// The underlying push failure (boxed: `crate::Error` is large).
        #[source]
        source: Box<crate::Error>,
    },
}

/// Whether a `kind = "key"` signer names its key by **path**.
///
/// The refusal below is about paths, not about `key` being set at all: a KMS
/// reference (`awskms://alias/release`) travels with the payload and means the
/// same thing on every consumer's machine, so "inline it as `key_pem`" is
/// advice no operator can follow for one. Unparseable references fall through
/// to `compile()`, which names what is wrong with them.
fn names_a_path(key: &ocx_trust::KeyMatcher) -> bool {
    key.key
        .as_deref()
        .and_then(|reference| ocx_trust::key_ref::KeyRef::parse(reference).ok())
        .is_some_and(|reference| reference.as_path().is_some())
}

// ── Pure validation ───────────────────────────────────────────────────────────

/// Validates a managed-config payload before publishing.
///
/// Pure function over the raw payload bytes:
///
/// 1. size ≤ [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`] (the same
///    cap the consumer-side fetch enforces — an oversize payload could never
///    be adopted),
/// 2. parses as [`ocx_config::Config`] (unknown **top-level** sections are
///    tolerated for forward compatibility, matching the loader's posture),
/// 3. carries no `[managed]` section,
/// 4. does not declare both `[trust.sigstore]` trust-root spellings at once,
/// 5. names no `[[trust.policy]]` key by path — a fleet payload carries key
///    material inline as `key_pem` or not at all,
/// 6. compiles every `[[trust.policy]]` entry it declares.
///
/// Returns the payload as text so a caller that needs to look at it again
/// ([`crate::managed_config::preview_managed_config`]) reuses this UTF-8
/// decode instead of repeating its error mapping.
///
/// # Errors
///
/// [`ManagedConfigPublishError::PayloadTooLarge`],
/// [`ManagedConfigPublishError::InvalidToml`],
/// [`ManagedConfigPublishError::ContainsManagedSection`],
/// [`ManagedConfigPublishError::AmbiguousTrustRoot`],
/// [`ManagedConfigPublishError::AmbiguousExtraCaCerts`],
/// [`ManagedConfigPublishError::ManagedConfigKeyByPath`],
/// [`ManagedConfigPublishError::InvalidTrustPolicy`].
pub fn validate_managed_config_payload(bytes: &[u8]) -> Result<&str, ManagedConfigPublishError> {
    use serde::de::Error as _;

    let actual = bytes.len() as u64;
    let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
    if actual > maximum {
        return Err(ManagedConfigPublishError::PayloadTooLarge { actual, maximum });
    }

    let text = std::str::from_utf8(bytes).map_err(|utf8_error| ManagedConfigPublishError::InvalidToml {
        source: toml::de::Error::custom(utf8_error),
    })?;
    let parsed: ocx_config::Config =
        toml::from_str(text).map_err(|source| ManagedConfigPublishError::InvalidToml { source })?;

    if parsed.managed.is_some() {
        return Err(ManagedConfigPublishError::ContainsManagedSection);
    }
    if let Some(sigstore) = parsed.trust.as_ref().and_then(|trust| trust.sigstore.as_ref())
        && sigstore.trusted_root.is_some()
        && sigstore.trusted_root_json.is_some()
    {
        return Err(ManagedConfigPublishError::AmbiguousTrustRoot);
    }
    // Sibling of the check above, one table up: a root-level `extra_ca_certs`
    // and `extra_ca_certs_pem` set together is the same unpredictable-winner
    // ambiguity (C-003, S-005) — `ocx config test` refuses it for the same
    // reason it refuses `AmbiguousTrustRoot`.
    if parsed.extra_ca_certs.is_some() && parsed.extra_ca_certs_pem.is_some() {
        return Err(ManagedConfigPublishError::AmbiguousExtraCaCerts);
    }
    // The same rule one table over: key material a fleet receives must travel
    // with the payload, so a signer names its key inline or not at all.
    if let Some(trust) = parsed.trust.as_ref()
        && trust.policy.iter().any(|policy| {
            policy
                .signers
                .iter()
                .any(|signer| matches!(signer, ocx_trust::SignerSpec::Key(key) if names_a_path(key)))
        })
    {
        return Err(ManagedConfigPublishError::ManagedConfigKeyByPath);
    }
    // Only now that every remaining key is inline: compiling reads no file, so
    // this is a pure shape + PEM check the operator gets instead of the fleet.
    for policy in parsed.trust.iter().flat_map(|trust| trust.policy.iter()) {
        policy
            .compile()
            .map_err(|source| ManagedConfigPublishError::InvalidTrustPolicy { source })?;
    }
    Ok(text)
}

/// The path-form trust root a payload declares, if any.
///
/// Split out from [`inline_trusted_root`] so the caller can skip the file read
/// entirely for the overwhelmingly common payload that names no trust root.
#[must_use]
pub fn declared_trusted_root(text: &str) -> Option<PathBuf> {
    let parsed: ocx_config::Config = toml::from_str(text).ok()?;
    parsed.trust?.sigstore?.trusted_root
}

/// The path-form `extra_ca_certs` a payload declares, if any. Sibling of
/// [`declared_trusted_root`] (C-003) — a plain root-level field, so no
/// `[trust.sigstore]` traversal.
#[must_use]
pub fn declared_extra_ca_certs(text: &str) -> Option<PathBuf> {
    let parsed: ocx_config::Config = toml::from_str(text).ok()?;
    parsed.extra_ca_certs
}

/// The inline `extra_ca_certs_pem` a payload authors directly, if any —
/// the form [`publish_managed_config`] proves usable before the fleet adopts
/// it. A `_pem` the path arm inlined is never seen here: that text already
/// passed `parse_pem` on the way in.
#[must_use]
pub fn declared_extra_ca_certs_pem(text: &str) -> Option<String> {
    let parsed: ocx_config::Config = toml::from_str(text).ok()?;
    parsed.extra_ca_certs_pem
}

/// Rewrites a payload's path-form `extra_ca_certs` into the self-contained
/// `extra_ca_certs_pem` string, leaving everything else — key order,
/// comments, spacing — byte-identical. Sibling of [`inline_trusted_root`];
/// unlike that function the key being replaced sits at the document root,
/// not nested under `[trust.sigstore]`.
///
/// # Errors
/// [`ManagedConfigPublishError::InvalidToml`] when the payload does not parse
/// as TOML — which [`validate_managed_config_payload`] has already ruled out
/// for every caller in this module.
pub fn inline_extra_ca_certs(text: &str, pem: &str) -> Result<String, ManagedConfigPublishError> {
    use serde::de::Error as _;

    let mut document =
        text.parse::<toml_edit::DocumentMut>()
            .map_err(|error| ManagedConfigPublishError::InvalidToml {
                source: toml::de::Error::custom(error),
            })?;
    if document.remove("extra_ca_certs").is_none() {
        return Ok(text.to_string());
    }
    document.insert("extra_ca_certs_pem", toml_edit::value(pem));
    Ok(document.to_string())
}

/// Re-checks a payload's size against
/// [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`] after publish-time
/// inlining (`trusted_root_json` and/or `extra_ca_certs_pem`) has potentially
/// grown it past the cap the pre-inline check in
/// [`validate_managed_config_payload`] could not see (C-003). Closes the same
/// gap for the `trusted_root` sibling, which had no re-check before.
///
/// # Errors
/// [`ManagedConfigPublishError::PayloadTooLarge`] when the inlined payload
/// exceeds the cap.
fn guard_inlined_payload_size(bytes: &[u8]) -> Result<(), ManagedConfigPublishError> {
    let actual = bytes.len() as u64;
    let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
    if actual > maximum {
        return Err(ManagedConfigPublishError::PayloadTooLarge { actual, maximum });
    }
    Ok(())
}

/// Rewrites a payload's path-form `[trust.sigstore] trusted_root` into the
/// self-contained `trusted_root_json` string, leaving everything else — key
/// order, comments, spacing — byte-identical.
///
/// Pure: `json` is the already-read, already-validated trusted-root document.
/// A payload with no `[trust.sigstore] trusted_root` is returned unchanged, so
/// this is safe to call unconditionally.
///
/// # Errors
///
/// [`ManagedConfigPublishError::InvalidToml`] when the payload does not parse
/// as TOML — which [`validate_managed_config_payload`] has already ruled out
/// for every caller in this module.
pub fn inline_trusted_root(text: &str, json: &str) -> Result<String, ManagedConfigPublishError> {
    use serde::de::Error as _;

    let mut document =
        text.parse::<toml_edit::DocumentMut>()
            .map_err(|error| ManagedConfigPublishError::InvalidToml {
                source: toml::de::Error::custom(error),
            })?;
    let Some(sigstore) = document
        .get_mut("trust")
        .and_then(|trust| trust.get_mut("sigstore"))
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return Ok(text.to_string());
    };
    if sigstore.remove("trusted_root").is_none() {
        return Ok(text.to_string());
    }
    sigstore.insert("trusted_root_json", toml_edit::value(json));
    Ok(document.to_string())
}

/// Resolves a path a payload declares (`trusted_root`, `extra_ca_certs`)
/// against the payload's own directory, through the same [`FileReference`]
/// grammar the loader applies to a local `config.toml`.
///
/// `to_string_lossy` is exact: the value is deserialized from a TOML string,
/// so it is UTF-8 by construction.
fn anchor_declared_path(declared: &Path, config_path: &Path) -> PathBuf {
    let written = declared.to_string_lossy().into_owned();
    FileReference::parse(&written).anchored_at(config_path.parent().unwrap_or(Path::new(".")))
}

/// Reads and validates the bundle a path-form `extra_ca_certs` names, on the
/// blocking pool — [`read_path`] is sync, and
/// [`parse_pem`] probe-builds a client through the platform
/// verifier, so the two go to the pool as one task rather than growing an
/// async twin of either (the `oci/verify/trust_resolve.rs` precedent). A
/// `JoinError` becomes an `Other` I/O error, never `NotFound`, so a
/// panicking pool task is not reported as a missing file.
///
/// # Errors
///
/// [`ManagedConfigPublishError::ExtraCaCertsReadFailed`] — `NotFound` (79),
/// `PermissionDenied` (77), any other I/O failure, a non-regular file or a
/// bundle over [`ocx_util::tls::MAX_EXTRA_CA_CERTS_BYTES`] (74:
/// `InvalidInput`, the file is there but is not one this process will read
/// as given); [`ManagedConfigPublishError::ExtraCaCertsInvalid`] (65) when
/// the bytes are not a usable CA bundle.
async fn read_extra_ca_certs(path: &Path) -> Result<Vec<u8>, ManagedConfigPublishError> {
    let origin = publish_origin(path);
    let target = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || {
        let (bytes, origin) = read_path(&target, publish_origin(&target)).map_err(|error| match error {
            TlsError::Unreadable { origin, io } => {
                ManagedConfigPublishError::ExtraCaCertsReadFailed { origin, source: io }
            }
            source => ManagedConfigPublishError::ExtraCaCertsInvalid { source },
        })?;
        parse_pem(&bytes, &origin).map_err(|source| ManagedConfigPublishError::ExtraCaCertsInvalid { source })?;
        Ok(bytes)
    })
    .await;
    match read {
        Ok(result) => result,
        Err(join) => Err(ManagedConfigPublishError::ExtraCaCertsReadFailed {
            origin,
            source: std::io::Error::other(format!("extra CA certificate read task panicked: {join}")),
        }),
    }
}

/// The origin a publish-side refusal names for a path-form `extra_ca_certs`:
/// the runtime door's rendering (`extra_ca_certs=<path>`, redacted when the
/// value is not a path) with no tier — the payload is the operator's source
/// file, not one of the consumer's config tiers.
fn publish_origin(path: &Path) -> ExtraRootsSource {
    ExtraRootsSource::ConfigPath {
        path: path.to_path_buf(),
        tier: None,
    }
}

/// Proves an `extra_ca_certs_pem` authored directly in the payload is a
/// usable bundle, on the blocking pool for the same reason as
/// [`read_extra_ca_certs`]: [`parse_pem`]'s probe build loads the
/// platform trust store. The origin is the managed tier — the verdict every
/// consumer's loader would reach, delivered to the operator instead.
///
/// # Errors
///
/// [`ManagedConfigPublishError::ExtraCaCertsPemInvalid`] (78) when the text
/// is not a usable CA bundle; [`ManagedConfigPublishError::StageFailed`] for
/// a panicking pool task.
async fn validate_extra_ca_certs_pem(pem: String) -> Result<(), ManagedConfigPublishError> {
    tokio::task::spawn_blocking(move || {
        parse_pem(pem.as_bytes(), &ExtraRootsSource::ConfigInline(ConfigTier::Managed))
            .map(|_| ())
            .map_err(|source| ManagedConfigPublishError::ExtraCaCertsPemInvalid { source })
    })
    .await
    .map_err(|join| ManagedConfigPublishError::StageFailed {
        source: std::io::Error::other(format!("extra CA certificate validation task panicked: {join}")),
    })?
}

/// Reads the candidate payload `ocx config push` and `ocx config test`
/// share, bounded at [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`] and
/// refusing anything that is not a regular file before `open(2)` — a FIFO
/// named as the candidate would otherwise block forever waiting for a writer,
/// and `/dev/zero` would be read until memory ran out (CWE-400).
///
/// # Errors
///
/// [`ManagedConfigPublishError::PayloadTooLarge`] (78) over the cap — `actual`
/// is the file's length from its metadata, since the bounded read stopped one
/// byte past the cap; [`ManagedConfigPublishError::ReadFailed`] otherwise —
/// `NotFound` (79), `PermissionDenied` (77), any other I/O failure or a
/// non-regular file (74: `InvalidInput`, naming the rule).
pub async fn read_candidate_payload(path: &Path) -> Result<Vec<u8>, ManagedConfigPublishError> {
    let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
    let read_failed = |source| ManagedConfigPublishError::ReadFailed {
        path: path.to_path_buf(),
        source,
    };
    match read_bounded_async(path, maximum).await {
        Ok(bytes) => Ok(bytes),
        Err(BoundedReadError::TooLarge { .. }) => {
            let actual = tokio::fs::metadata(path).await.map_err(read_failed)?.len();
            Err(ManagedConfigPublishError::PayloadTooLarge { actual, maximum })
        }
        Err(refused) => Err(read_failed(refused.into_io_error())),
    }
}

// ── Publish orchestration ─────────────────────────────────────────────────────

/// Publishes `config_path` as a managed-config package under `identifier`.
///
/// Stages the payload as `config.toml` (regardless of the input file name),
/// bundles it into a tar+gzip layer, synthesizes minimal bundle metadata (no
/// metadata JSON file involved), and pushes via [`Publisher::push`] /
/// [`Publisher::push_cascade`]. The caller is responsible for
/// [`Publisher::ensure_auth`].
///
/// A `[trust.sigstore] trusted_root` naming a local file is read (relative to
/// `config_path`'s own directory), proved loadable as a Sigstore trusted root,
/// and inlined as `trusted_root_json` — see the module docs for why. A
/// root-level `extra_ca_certs` is treated the same way: read bounded, proved
/// a usable CA bundle, and inlined as `extra_ca_certs_pem`; an
/// `extra_ca_certs_pem` authored directly gets the same proof without the
/// rewrite. The payload size is checked again after either rewrite, since
/// inlining is what grows it.
///
/// # Errors
///
/// See [`ManagedConfigPublishError`] variants.
pub async fn publish_managed_config(
    publisher: &Publisher,
    identifier: &OciIdentifier,
    config_path: &Path,
    options: ManagedConfigPublishOptions,
) -> Result<PushOutcome, ManagedConfigPublishError> {
    let bytes = read_candidate_payload(config_path).await?;
    let text = validate_managed_config_payload(&bytes)?;
    let text = match declared_trusted_root(text) {
        None => text.to_string(),
        Some(declared) => {
            // Relative to the payload's own directory, exactly as the loader
            // anchors it when reading a local `config.toml` — one grammar and one
            // relative rule, through the same `FileReference` seam
            // `SigstoreTrust::anchor_relative_root` goes through. "Exactly as the
            // loader" is the whole point of this branch, so the two must not drift
            // on Windows, and must not drift on the spelling either: this path
            // never reaches `ConfigLoader::anchor_relative_paths`, so a payload
            // writing `file:///x` would otherwise send the operator's own publish
            // run looking for a file named `file:///x`.
            let path = anchor_declared_path(&declared, config_path);
            // Bounded at the same ceiling verification puts on the same
            // document, so the transport an operator chose does not change
            // how large a trust root may be; a FIFO is refused before the
            // open, and a file over the cap is a read failure (74), not a
            // parse failure (78).
            let json = read_bounded_async(&path, ocx_sign::verify::MAX_TRUSTED_ROOT_BYTES)
                .await
                .map_err(|refused| ManagedConfigPublishError::TrustedRootReadFailed {
                    path: path.clone(),
                    source: refused.into_io_error(),
                })?;
            // Prove it loads before a whole fleet adopts it. `load_trusted_root_json`
            // is the same entry point verification uses, so "publish succeeded"
            // means "every consumer can build a trust root from this".
            ocx_sign::verify::TrustRoot::load_trusted_root_json(&json).map_err(|kind| {
                ManagedConfigPublishError::TrustedRootInvalid {
                    path: path.clone(),
                    detail: kind.to_string(),
                }
            })?;
            let json =
                std::str::from_utf8(&json).map_err(|utf8_error| ManagedConfigPublishError::TrustedRootInvalid {
                    path: path.clone(),
                    detail: utf8_error.to_string(),
                })?;
            inline_trusted_root(text, json)?
        }
    };
    let text = match declared_extra_ca_certs(&text) {
        None => {
            // The inline form the operator wrote by hand gets the same proof
            // the path form gets on its way in — a bad root published here
            // refuses every consumer's every command, `config update`
            // included, until an out-of-band `OCX_EXTRA_CA_CERTS` override.
            if let Some(pem) = declared_extra_ca_certs_pem(&text) {
                validate_extra_ca_certs_pem(pem).await?;
            }
            text
        }
        Some(declared) => {
            // Same grammar and relative rule as the trusted_root branch above
            // (D-1: one CA-path grammar, not two).
            let path = anchor_declared_path(&declared, config_path);
            let pem = read_extra_ca_certs(&path).await?;
            let pem = std::str::from_utf8(&pem).map_err(|source| ManagedConfigPublishError::ExtraCaCertsNotUtf8 {
                origin: publish_origin(&path),
                source,
            })?;
            inline_extra_ca_certs(&text, pem)?
        }
    };
    let bytes = text.into_bytes();
    guard_inlined_payload_size(&bytes)?;

    // Stage as `config.toml` in a temp dir so the archive entry name is
    // canonical no matter what the operator's input file is called.
    let stage = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .map_err(|join_error| ManagedConfigPublishError::StageFailed {
            source: std::io::Error::other(join_error.to_string()),
        })?
        .map_err(|source| ManagedConfigPublishError::StageFailed { source })?;
    let staged = stage.path().join("config.toml");
    tokio::fs::write(&staged, &bytes)
        .await
        .map_err(|source| ManagedConfigPublishError::StageFailed { source })?;

    let archive = stage.path().join("config.tar.gz");
    ocx_package::bundle::BundleBuilder::from_path(&staged)
        .create(&archive)
        .await
        .map_err(|source| ManagedConfigPublishError::BundleFailed {
            source: Box::new(source.into()),
        })?;

    let info = Info {
        metadata: Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env: Default::default(),
            dependencies: Default::default(),
            entrypoints: Default::default(),
            binaries: None,
            integrations: Default::default(),
        }),
        platform: options.platform,
    };
    let layers = [LayerRef::File {
        path: archive,
        layout: Default::default(),
        mount_from: None,
    }];

    let outcome = if options.cascade {
        let existing_tags = publisher.list_tags(identifier.clone()).await.map_err(|source| {
            ManagedConfigPublishError::ListTagsFailed {
                identifier: Box::new(identifier.clone()),
                source: Box::new(source.into()),
            }
        })?;
        let existing_versions = Publisher::parse_versions(&existing_tags);
        // Keep tagging (`adr_index_indirection.md` Decision E) is a
        // `ocx package push` CLI contract; managed-config publishing has no
        // `--[no-]keep-tag` surface of its own, so it opts out to keep
        // today's tag set unchanged. Index annotations are likewise a
        // `ocx package push --annotation` contract with no `ocx config push`
        // surface, so none are written. `--default` is a third such
        // contract: a managed config publishes no variants.
        publisher
            .push_cascade(
                identifier,
                vec![info],
                &layers,
                existing_versions,
                None,
                false,
                false,
                &BTreeMap::new(),
            )
            .await
            .map_err(|source| ManagedConfigPublishError::PushFailed {
                source: Box::new(source.into()),
            })?
    } else {
        publisher
            .push(identifier, vec![info], &layers, None, false, false, &BTreeMap::new())
            .await
            .map_err(|source| ManagedConfigPublishError::PushFailed {
                source: Box::new(source.into()),
            })?
    };

    // `stage` (TempDir) lives until here so the archive exists for the push.
    drop(stage);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `catch_unwind` on the FIFO rows, which only exist where `mkfifo` does.
    #[cfg(unix)]
    use futures::FutureExt as _;

    #[test]
    fn validate_accepts_plain_config() {
        let toml = b"[registry]\ndefault = \"corp.example.com\"\n";
        validate_managed_config_payload(toml).expect("plain config must validate");
    }

    /// Fleet forward-compat: a payload published by a newer ocx may carry
    /// top-level sections this binary does not know — accepted, matching the
    /// loader's no-`deny_unknown_fields` posture on [`ocx_config::Config`].
    #[test]
    fn validate_accepts_unknown_top_level_sections() {
        let toml = b"[registry]\ndefault = \"corp.example.com\"\n[future_section]\nkey = \"value\"\n";
        validate_managed_config_payload(toml).expect("unknown top-level sections must be tolerated");
    }

    #[test]
    fn validate_rejects_managed_section() {
        let toml = b"[managed]\nsource = \"corp.example.com/ocx-config:user\"\n";
        let err = validate_managed_config_payload(toml).expect_err("[managed] must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::ContainsManagedSection));
    }

    #[test]
    fn validate_rejects_invalid_toml() {
        let err = validate_managed_config_payload(b"not = [valid").expect_err("invalid TOML must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::InvalidToml { .. }));
    }

    #[test]
    fn validate_rejects_non_utf8_payload() {
        let err = validate_managed_config_payload(&[0xff, 0xfe, 0x00]).expect_err("non-UTF-8 must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::InvalidToml { .. }));
    }

    #[test]
    fn validate_rejects_oversize_payload() {
        let oversize = "# padding\n".repeat(7_000); // ~70 KiB > 64 KiB cap
        assert!(oversize.len() as u64 > ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES);
        let err = validate_managed_config_payload(oversize.as_bytes()).expect_err("oversize must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::PayloadTooLarge { .. }));
    }

    /// S1 boundary: a payload of EXACTLY `MAX_MANAGED_CONFIG_BYTES` validates —
    /// the size gate is `> maximum` (strict), so the ceiling itself is
    /// admitted. Its MAX+1 twin is `validate_rejects_oversize_payload` above.
    /// (Padded as a single TOML comment line so the whole file is valid TOML.)
    #[test]
    fn validate_accepts_payload_of_exactly_maximum_bytes() {
        let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
        let payload = format!("# {}", "x".repeat((maximum - 2) as usize));
        assert_eq!(payload.len() as u64, maximum, "the payload must be exactly at the cap");
        validate_managed_config_payload(payload.as_bytes()).expect("a payload exactly at the cap must validate");
    }

    #[test]
    fn validate_rejects_both_trust_root_spellings() {
        let toml = r#"
[trust.sigstore]
trusted_root = "sigstore/trusted-root.json"
trusted_root_json = "{}"
"#;
        let error = validate_managed_config_payload(toml.as_bytes()).expect_err("XOR is enforced");
        assert!(matches!(error, ManagedConfigPublishError::AmbiguousTrustRoot));
    }

    #[test]
    fn validate_accepts_either_trust_root_spelling_alone() {
        for toml in [
            "[trust.sigstore]\ntrusted_root = \"sigstore/trusted-root.json\"\n",
            "[trust.sigstore]\ntrusted_root_json = \"{}\"\n",
        ] {
            validate_managed_config_payload(toml.as_bytes()).expect("one spelling alone is fine");
        }
    }

    #[test]
    fn declared_trusted_root_finds_the_path_form_only() {
        assert_eq!(
            declared_trusted_root("[trust.sigstore]\ntrusted_root = \"sigstore/trusted-root.json\"\n"),
            Some(PathBuf::from("sigstore/trusted-root.json"))
        );
        assert_eq!(
            declared_trusted_root("[trust.sigstore]\ntrusted_root_json = \"{}\"\n"),
            None,
            "an already-inline payload needs no read"
        );
        assert_eq!(declared_trusted_root("[registry]\ndefault = \"ghcr.io\"\n"), None);
    }

    #[test]
    fn inline_trusted_root_swaps_the_path_for_the_document() {
        let payload = "[trust.sigstore]\ntrusted_root = \"sigstore/trusted-root.json\"\nrekor_url = \"https://rekor.corp.example\"\n";
        let rewritten = inline_trusted_root(payload, "{\"mediaType\":\"x\"}").expect("rewrite");

        assert!(
            !rewritten.contains("trusted_root ="),
            "the operator path must not survive: {rewritten}"
        );
        assert!(
            rewritten.contains("trusted_root_json ="),
            "the document replaces it: {rewritten}"
        );
        assert!(
            rewritten.contains("rekor_url = \"https://rekor.corp.example\""),
            "every untouched key is preserved: {rewritten}"
        );

        // And what comes out still validates — the rewrite cannot mint the
        // very ambiguity `validate_managed_config_payload` refuses.
        let parsed = validate_managed_config_payload(rewritten.as_bytes()).expect("rewritten payload is publishable");
        let sigstore = toml::from_str::<ocx_config::Config>(parsed)
            .expect("parses")
            .trust
            .expect("trust")
            .sigstore
            .expect("sigstore");
        assert_eq!(sigstore.trusted_root, None);
        assert_eq!(sigstore.trusted_root_json.as_deref(), Some("{\"mediaType\":\"x\"}"));
    }

    #[test]
    fn inline_trusted_root_preserves_operator_comments() {
        // The reason this goes through `toml_edit` rather than a serde
        // round-trip: an operator's `config.toml` is hand-authored, and a
        // publish step that silently ate their comments would be noticed once,
        // in the worst way.
        let payload = "# corporate trust root, rotated quarterly\n[trust.sigstore]\ntrusted_root = \"root.json\"\n";
        let rewritten = inline_trusted_root(payload, "{}").expect("rewrite");
        assert!(
            rewritten.contains("# corporate trust root, rotated quarterly"),
            "the comment survives: {rewritten}"
        );
    }

    #[test]
    fn inline_trusted_root_leaves_a_payload_with_no_path_form_untouched() {
        for payload in [
            "[registry]\ndefault = \"ghcr.io\"\n",
            "[trust.sigstore]\ntrusted_root_json = \"{}\"\n",
        ] {
            assert_eq!(
                inline_trusted_root(payload, "{}").expect("rewrite"),
                payload,
                "byte-identical when there is nothing to inline"
            );
        }
    }

    /// The public half of the golden cosign pair — the only thing a `key_pem`
    /// entry ever carries.
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../test/tests/fixtures/golden/keys/cosign.pub");

    /// The reference is quoted by the TOML serializer rather than by the format
    /// string: one caller builds it from a tempdir, and a Windows tempdir is
    /// `C:\Users\…` — where `\U` in a basic string is a unicode escape, so an
    /// interpolated `"{reference}"` makes the payload unparseable on exactly one
    /// platform.
    fn payload_with_key_reference(reference: &str) -> String {
        let reference = toml::Value::from(reference);
        format!("[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nsigners = [{{ kind = \"key\", key = {reference} }}]\n")
    }

    /// A payload is adopted fleet-wide at once, so a policy that cannot compile
    /// fails closed on every consumer simultaneously — with the diagnostic
    /// landing on the wrong person. Compiling here moves it to the operator who
    /// wrote it. Every remaining key is inline by this point, so no file is read.
    #[test]
    fn validate_rejects_a_policy_that_does_not_compile() {
        let empty_signers = "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nsigners = []\n";
        let error = validate_managed_config_payload(empty_signers.as_bytes())
            .expect_err("an empty signers array accepts nobody and must be refused");
        assert!(
            matches!(
                error,
                ManagedConfigPublishError::InvalidTrustPolicy {
                    source: ocx_trust::TrustPolicyError::NoSigners { .. }
                }
            ),
            "got {error:?}"
        );

        let malformed_pem =
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nsigners = [{ kind = \"key\", key_pem = \"not a pem\" }]\n";
        let error = validate_managed_config_payload(malformed_pem.as_bytes())
            .expect_err("key material that no consumer can parse must be refused here");
        assert!(
            matches!(
                error,
                ManagedConfigPublishError::InvalidTrustPolicy {
                    source: ocx_trust::TrustPolicyError::KeyMalformed { .. }
                }
            ),
            "got {error:?}"
        );
    }

    /// The other direction: a payload whose policies *do* compile is accepted,
    /// or the refusal above would be indistinguishable from "managed payloads
    /// may carry no `[[trust.policy]]` at all".
    #[test]
    fn validate_accepts_a_policy_that_compiles() {
        let keyless = "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n\
                       signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n";
        validate_managed_config_payload(keyless.as_bytes()).expect("a compilable policy is publishable");
    }

    /// **A managed payload takes `key_pem` only.** It is a `config.toml` shipped
    /// as a package to a fleet, so a path in one names the *operator's* disk and
    /// means nothing on any consumer's. The refusal removes an incoherent state
    /// rather than adding a guard — the same convention `trusted_root` /
    /// `trusted_root_json` already follows.
    #[test]
    fn validate_rejects_a_key_signer_declared_by_path() {
        // Both spellings of the path form: relative and absolute name the
        // operator's disk alike, and a scan that caught only one would ship the
        // other as a payload that resolves to nothing on every consumer.
        for reference in ["etc/acme-release.pub", "/srv/keys/acme.pub"] {
            let error = validate_managed_config_payload(payload_with_key_reference(reference).as_bytes())
                .err()
                .unwrap_or_else(|| panic!("`{reference}` names a path and must be refused"));
            assert!(
                matches!(error, ManagedConfigPublishError::ManagedConfigKeyByPath),
                "`{reference}` got {error:?}"
            );
        }

        // The removed `file:` spelling is refused too — one door later, and by
        // the grammar rather than by this rule. `names_a_path` reads it through
        // `KeyRef::parse`, which no longer yields a path for it, so the payload
        // falls through to the `compile()` pass. Both halves asserted: still
        // refused, and *not* as `ManagedConfigKeyByPath`, whose `key_pem`
        // remedy is not the fix for a value that is simply misspelled.
        let removed =
            validate_managed_config_payload(payload_with_key_reference("file:etc/acme-release.pub").as_bytes())
                .expect_err("the removed spelling is not publishable either");
        assert!(
            matches!(
                &removed,
                ManagedConfigPublishError::InvalidTrustPolicy {
                    source: ocx_trust::TrustPolicyError::KeyReferenceInvalid {
                        source: ocx_trust::key_ref::KeyRefError::FileColonPrefix { .. },
                        ..
                    }
                }
            ),
            "the grammar names it, not the path rule; got {removed:?}"
        );
    }

    /// A KMS reference is not a path, and the path refusal must not eat it.
    ///
    /// `awskms://alias/release` travels with the payload and means the same
    /// thing on every consumer's machine, so the `key_pem` remedy the path
    /// refusal names is advice no operator can follow for one — a KMS key has
    /// no PEM to inline. It is refused, but as the third door onto 85
    /// `unsupported_key_backend`: the same code `--key awskms://…` and a local
    /// `config.toml` signer already answer for the identical value.
    ///
    /// Both halves, because either alone passes on a validator that answers the
    /// same way for everything: the KMS form must not be `ManagedConfigKeyByPath`
    /// **and** the path form must still be.
    #[test]
    fn a_kms_reference_is_85_not_the_path_refusal() {
        let error = validate_managed_config_payload(payload_with_key_reference("awskms://alias/release").as_bytes())
            .expect_err("an unimplemented backend cannot be published either");
        assert!(
            !matches!(error, ManagedConfigPublishError::ManagedConfigKeyByPath),
            "a KMS reference names no path, and `key_pem` is not a remedy for it; got {error:?}"
        );

        let by_path = validate_managed_config_payload(payload_with_key_reference("etc/acme.pub").as_bytes())
            .expect_err("a path form is refused in a managed payload");
        assert!(
            matches!(by_path, ManagedConfigPublishError::ManagedConfigKeyByPath),
            "narrowing the refusal to paths must not stop it refusing paths; got {by_path:?}"
        );
    }

    /// The refusal names `key_pem` as the fix — an operator who reads only the
    /// error message has to know what to write instead.
    #[test]
    fn the_key_by_path_refusal_names_key_pem_as_the_fix() {
        let error = validate_managed_config_payload(payload_with_key_reference("etc/acme.pub").as_bytes())
            .expect_err("a path form is refused in a managed payload");
        assert!(
            matches!(error, ManagedConfigPublishError::ManagedConfigKeyByPath),
            "got {error:?}"
        );
        assert!(
            error.to_string().contains("key_pem"),
            "the refusal must name the fix; got: {error}"
        );
    }

    /// The inline form is what travels, so it must be accepted — otherwise the
    /// refusal above would leave a fleet with no way to pin a key at all.
    #[test]
    fn validate_accepts_a_key_signer_declared_inline() {
        let toml = format!(
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nsigners = [{{ kind = \"key\", key_pem = \"\"\"\n{}\"\"\" }}]\n",
            GOLDEN_PUBLIC_KEY_PEM
        );
        validate_managed_config_payload(toml.as_bytes()).expect("an inline key travels with the payload");
    }

    /// The refusal is scoped to key signers. A keyless policy names no file at
    /// all, so it must publish unchanged — a broader scan would break every
    /// payload that already ships one.
    #[test]
    fn validate_accepts_a_keyless_signer_in_a_managed_payload() {
        let toml = "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n\
                    signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n";
        validate_managed_config_payload(toml.as_bytes()).expect("a keyless signer names no operator path");
    }

    /// **The local tier is unrestricted**, and this is the half that proves the
    /// rule is about *publishing*, not about the value. The identical `key`
    /// string that the managed payload refuses compiles fine when it is read as
    /// an ordinary config on the author's own disk.
    #[test]
    fn the_same_key_reference_is_accepted_in_a_local_tier() {
        let directory = tempfile::tempdir().expect("tempdir");
        let key_path = directory.path().join("acme-release.pub");
        std::fs::write(&key_path, GOLDEN_PUBLIC_KEY_PEM).expect("write the key");
        let reference = key_path.display().to_string();

        validate_managed_config_payload(payload_with_key_reference(&reference).as_bytes())
            .expect_err("refused as a published payload");

        let local: ocx_config::Config =
            toml::from_str(&payload_with_key_reference(&reference)).expect("the same text is ordinary config");
        local.trust_policies()[0]
            .compile()
            .expect("a local tier resolves the very same reference");
    }

    // ── `extra_ca_certs` → `extra_ca_certs_pem` inlining — C-003, S-003 ─────

    /// A real self-signed CA (the test stack's Fulcio root): the positive
    /// cases need material the certificate parser accepts, not a placeholder.
    const EXTRA_CA_CERTS_FIXTURE_PEM: &str = include_str!("../../../../test/sigstore/keys/fulcio-ca.crt.pem");

    /// D-10: the per-file cap every `extra_ca_certs` read goes through.
    const EXTRA_CA_CERTS_CAP_BYTES: usize = ocx_util::tls::MAX_EXTRA_CA_CERTS_BYTES;

    /// The identifier and publisher are inert: nearly every case below fails
    /// before the first registry write, so an empty in-memory stub transport
    /// is enough — and the one positive control that reaches the push
    /// completes against it, proving the validation passed rather than that
    /// a registry answered.
    fn stub_publisher() -> Publisher {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            StubTransportData::new(),
        ))))
    }

    /// Stages `payload` as the operator's config file in a fresh tempdir and
    /// publishes it; the tempdir is returned so the caller can plant the
    /// bundle the payload names next to it.
    async fn publish_from_tempdir(
        directory: &tempfile::TempDir,
        payload: &str,
    ) -> Result<PushOutcome, ManagedConfigPublishError> {
        let config_path = directory.path().join("corp-config.toml");
        std::fs::write(&config_path, payload).expect("write the payload");
        publish_path(&config_path).await
    }

    /// Publishes whatever `config_path` names — a FIFO, an oversize file —
    /// against the stub publisher, bounded at five seconds: the reads on
    /// this path used to be unbounded, and a red here is "it hung", which
    /// the timeout turns into a failure instead.
    async fn publish_path(config_path: &Path) -> Result<PushOutcome, ManagedConfigPublishError> {
        let identifier = OciIdentifier::parse_target("registry.test/acme/config:v1", ocx_oci::DEFAULT_REGISTRY)
            .expect("identifier parses");
        let publisher = stub_publisher();
        let publish = publish_managed_config(
            &publisher,
            &identifier,
            config_path,
            ManagedConfigPublishOptions {
                cascade: false,
                platform: Platform::Any,
            },
        );
        match tokio::time::timeout(std::time::Duration::from_secs(5), publish).await {
            Ok(outcome) => outcome,
            Err(_elapsed) => panic!("publish of {} hung instead of refusing", config_path.display()),
        }
    }

    /// The candidate itself is a FIFO: refused as 74 before the open, not a
    /// hang on `open(2)` waiting for a writer that never comes.
    ///
    /// Mutation: restore `tokio::fs::read` in `read_candidate_payload` — the
    /// timeout fires. The FIFO is released afterwards so the pool thread the
    /// mutation left in `open(2)` lets the runtime shut down, and the red is
    /// a panic rather than a hang.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_refuses_a_fifo_candidate_without_blocking() {
        let directory = tempfile::tempdir().expect("tempdir");
        let fifo = directory.path().join("corp-config.fifo");
        ocx_test_support::fifo::mkfifo(&fifo);

        let outcome = std::panic::AssertUnwindSafe(publish_path(&fifo)).catch_unwind().await;
        ocx_test_support::fifo::release_blocked_reader(&fifo);
        let error = outcome
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            .expect_err("a FIFO is not a payload");
        assert!(
            matches!(&error, ManagedConfigPublishError::ReadFailed { path, source }
                if *path == fifo && source.kind() == std::io::ErrorKind::InvalidInput),
            "got {error:?}"
        );
        assert!(
            error.to_string().contains("corp-config.fifo"),
            "the refusal names the path: {error}"
        );
    }

    /// A candidate over the 64 KiB cap is 78 with the file's real length —
    /// from its metadata, not from a buffer the bounded read stopped filling
    /// one byte past the cap.
    ///
    /// What this row pins is the code and the reported length, **not** the
    /// boundedness of the read: an unbounded `tokio::fs::read` of this
    /// fixture returns the same 78 with the same length (the size check ran
    /// on the whole payload before the fold too), so restoring it leaves
    /// this row green. The read's boundedness is
    /// [`publish_refuses_a_fifo_candidate_without_blocking`]'s guard.
    ///
    /// Mutation: report `cap + 1` as `actual` — the length assertion reds.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_oversize_candidate_is_78() {
        let directory = tempfile::tempdir().expect("tempdir");
        let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
        let payload = "# padding\n".repeat(maximum as usize / 10 + 1);
        assert!(
            payload.len() as u64 > maximum + 1,
            "the fixture must exceed the cap by more than one byte"
        );
        let candidate = directory.path().join("huge-config.toml");
        std::fs::write(&candidate, &payload).expect("write the payload");

        let error = publish_path(&candidate)
            .await
            .expect_err("an oversize candidate is refused");
        assert!(
            matches!(&error, ManagedConfigPublishError::PayloadTooLarge { actual, maximum: reported }
                if *actual == payload.len() as u64 && *reported == maximum),
            "the refusal carries the file's own length; got {error:?}"
        );
    }

    /// A `trusted_root` file over the 1 MiB read ceiling is a read failure
    /// (74), the same door a FIFO or `/dev/zero` takes — never read whole and
    /// handed to the parser for a 78.
    ///
    /// Mutation: restore `tokio::fs::read` for the trusted root — the whole
    /// file is read and refused by the parser as `TrustedRootInvalid` (78).
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_trusted_root_over_the_read_ceiling_is_74() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cap = ocx_sign::verify::MAX_TRUSTED_ROOT_BYTES as usize;
        std::fs::write(directory.path().join("root.json"), vec![b' '; cap + 1]).expect("write the root");

        let error = publish_from_tempdir(&directory, "[trust.sigstore]\ntrusted_root = \"root.json\"\n")
            .await
            .expect_err("an over-cap trust root is refused");
        assert!(
            matches!(&error, ManagedConfigPublishError::TrustedRootReadFailed { source, .. }
                if source.kind() == std::io::ErrorKind::InvalidInput),
            "size is a read failure, not a parse failure; got {error:?}"
        );
    }

    /// A `trusted_root` naming a FIFO is refused promptly (74), not a hang.
    ///
    /// Mutation: restore `tokio::fs::read` for the trusted root — the
    /// timeout fires; the FIFO is released so the red is a panic, not a hang.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_trusted_root_fifo_is_refused_promptly() {
        let directory = tempfile::tempdir().expect("tempdir");
        let fifo = directory.path().join("root.fifo");
        ocx_test_support::fifo::mkfifo(&fifo);

        let publish = publish_from_tempdir(&directory, "[trust.sigstore]\ntrusted_root = \"root.fifo\"\n");
        let outcome = std::panic::AssertUnwindSafe(publish).catch_unwind().await;
        ocx_test_support::fifo::release_blocked_reader(&fifo);
        let error = outcome
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            .expect_err("a FIFO is not a trust root");
        assert!(
            matches!(&error, ManagedConfigPublishError::TrustedRootReadFailed { path, source }
                if *path == fifo && source.kind() == std::io::ErrorKind::InvalidInput),
            "got {error:?}"
        );
    }

    /// `extra_ca_certs = "<name>"` relative to the payload's own directory.
    fn payload_naming_extra_ca_certs(name: &str) -> String {
        format!("extra_ca_certs = {}\n", toml::Value::from(name))
    }

    /// C-003, S-003: both spellings in one payload is the same
    /// unpredictable-winner ambiguity as `trusted_root` / `trusted_root_json`,
    /// refused by the shared validator (78) so `ocx config test` refuses it too.
    #[test]
    fn validate_rejects_both_extra_ca_certs_keys() {
        let toml =
            format!("extra_ca_certs = \"corp-ca.pem\"\nextra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n");
        let error = validate_managed_config_payload(toml.as_bytes()).expect_err("XOR is enforced");
        assert!(
            matches!(error, ManagedConfigPublishError::AmbiguousExtraCaCerts),
            "got {error:?}"
        );
    }

    /// The premise for the refusal above: either spelling alone validates.
    #[test]
    fn validate_accepts_either_extra_ca_certs_key_alone() {
        for toml in [
            "extra_ca_certs = \"corp-ca.pem\"\n".to_string(),
            format!("extra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n"),
        ] {
            validate_managed_config_payload(toml.as_bytes()).expect("one spelling alone is fine");
        }
    }

    /// C-003: the read is skipped for the overwhelmingly common payload that
    /// names no bundle — only the path form declares a file to read.
    #[test]
    fn declared_extra_ca_certs_finds_the_path_form_only() {
        assert_eq!(
            declared_extra_ca_certs("extra_ca_certs = \"certs/corp-ca.pem\"\n"),
            Some(PathBuf::from("certs/corp-ca.pem"))
        );
        assert_eq!(
            declared_extra_ca_certs(&format!("extra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n")),
            None,
            "an already-inline payload needs no read"
        );
        assert_eq!(declared_extra_ca_certs("[registry]\ndefault = \"ghcr.io\"\n"), None);
    }

    /// C-003: the path is replaced by the bundle it named, as
    /// `extra_ca_certs_pem` at the document root — asserted through the
    /// parser, so a rewrite that lands the key under some table instead of
    /// at the root reads back as `None` here. Every untouched key survives,
    /// and the result validates: the rewrite cannot mint the ambiguity the
    /// validator refuses.
    #[test]
    fn inline_extra_ca_certs_swaps_the_path_for_the_pem_at_the_document_root() {
        let payload = "extra_ca_certs = \"corp-ca.pem\"\n\n[registry]\ndefault = \"corp.example\"\n";
        let rewritten = inline_extra_ca_certs(payload, EXTRA_CA_CERTS_FIXTURE_PEM).expect("rewrite");

        let parsed = validate_managed_config_payload(rewritten.as_bytes()).expect("rewritten payload is publishable");
        let config: ocx_config::Config = toml::from_str(parsed).expect("parses");
        assert_eq!(
            config.extra_ca_certs, None,
            "the operator path must not survive: {rewritten}"
        );
        assert_eq!(
            config.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM),
            "the bundle replaces it, byte-for-byte: {rewritten}"
        );
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("corp.example"),
            "every untouched key is preserved: {rewritten}"
        );
    }

    /// C-003: nothing to inline, nothing rewritten — byte-identical.
    #[test]
    fn inline_extra_ca_certs_leaves_a_payload_with_no_path_form_untouched() {
        for payload in [
            "[registry]\ndefault = \"ghcr.io\"\n".to_string(),
            format!("extra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n"),
        ] {
            assert_eq!(
                inline_extra_ca_certs(&payload, EXTRA_CA_CERTS_FIXTURE_PEM).expect("rewrite"),
                payload,
                "byte-identical when there is nothing to inline"
            );
        }
    }

    /// S-003: a payload naming a bundle that does not exist exits 79 and the
    /// error names the resolved path — the operator wrote the wrong name, and
    /// the message has to say which file was looked for.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_missing_path_is_79() {
        let directory = tempfile::tempdir().expect("tempdir");
        let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs("absent-ca.pem"))
            .await
            .expect_err("a missing bundle cannot be inlined");
        let expected = directory.path().join("absent-ca.pem");
        assert!(
            matches!(
                &error,
                ManagedConfigPublishError::ExtraCaCertsReadFailed {
                    origin: ExtraRootsSource::ConfigPath { path, tier: None },
                    ..
                } if *path == expected
            ),
            "the refusal names the resolved path; got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains(&format!("extra_ca_certs={}", expected.display())),
            "the message names the key and the path: {error}"
        );
    }

    /// D-11 at the publish door (review r1): a PEM body pasted under
    /// `extra_ca_certs` in the source config is a "path" over 256 bytes with
    /// newlines. The runtime door redacts it; `ocx config push` must too —
    /// the whole `{:#}` chain, since that is what the CLI prints.
    ///
    /// Mutation: render `path.display()` in `ExtraCaCertsReadFailed` again —
    /// the chain carries `-----BEGIN` and this reds.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_pathological_path_is_redacted_through_the_chain() {
        let directory = tempfile::tempdir().expect("tempdir");
        let body_probe = EXTRA_CA_CERTS_FIXTURE_PEM
            .lines()
            .find(|line| !line.starts_with("-----"))
            .expect("a PEM block has a body line")
            .to_owned();
        let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs(EXTRA_CA_CERTS_FIXTURE_PEM))
            .await
            .expect_err("PEM text is not a readable path");

        let chain = ocx_util::error::render_chain(&error);
        assert!(
            matches!(error, ManagedConfigPublishError::ExtraCaCertsReadFailed { .. }),
            "got {error:?}"
        );
        assert!(!chain.contains("-----BEGIN"), "the chain echoes PEM: {chain}");
        assert!(!chain.contains(&body_probe), "the chain echoes a body: {chain}");
        assert!(
            chain.contains("bytes, not a readable path"),
            "the origin is redacted: {chain}"
        );
    }

    /// C-003: an unreadable bundle is 77, not a generic I/O failure — the fix
    /// is a chmod, not a different path. Observed-condition skip: under root
    /// the mode bits do not bite, so the test reads the file itself first.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_permission_denied_is_77() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let bundle = directory.path().join("locked-ca.pem");
        std::fs::write(&bundle, EXTRA_CA_CERTS_FIXTURE_PEM).expect("write the bundle");
        std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
        if std::fs::read(&bundle).is_ok() {
            eprintln!("skipped: this process bypasses mode bits (root), so PermissionDenied is unreachable");
            return;
        }

        let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs("locked-ca.pem"))
            .await
            .expect_err("an unreadable bundle cannot be inlined");
        assert!(
            matches!(error, ManagedConfigPublishError::ExtraCaCertsReadFailed { .. }),
            "got {error:?}"
        );
    }

    /// D-10: the read goes through `read_bounded`, which refuses a non-regular
    /// file before reading a byte — `/dev/zero` would otherwise be an infinite
    /// bundle (the repo's own CWE-400 fix for `--key file:/dev/zero`). 74.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_non_regular_file_is_74() {
        let directory = tempfile::tempdir().expect("tempdir");
        let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs("/dev/zero"))
            .await
            .expect_err("a character device is not a bundle");
        assert!(
            matches!(error, ManagedConfigPublishError::ExtraCaCertsReadFailed { .. }),
            "got {error:?}"
        );
    }

    /// D-10, DX-8: a bundle over the 32 KiB cap is `read_bounded`'s `TooLarge`
    /// and surfaces as 74 (parity with `[trust.sigstore] trusted_root`), never
    /// as content error 65 — the file is well-formed PEM throughout, so the
    /// code can only come from the size gate.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_over_cap_file_is_74() {
        let directory = tempfile::tempdir().expect("tempdir");
        let copies = EXTRA_CA_CERTS_CAP_BYTES / EXTRA_CA_CERTS_FIXTURE_PEM.len() + 1;
        let bundle = EXTRA_CA_CERTS_FIXTURE_PEM.repeat(copies);
        assert!(
            bundle.len() > EXTRA_CA_CERTS_CAP_BYTES,
            "the fixture must exceed the cap"
        );
        std::fs::write(directory.path().join("huge-ca.pem"), &bundle).expect("write the bundle");

        let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs("huge-ca.pem"))
            .await
            .expect_err("an over-cap bundle cannot be inlined");
        assert!(
            matches!(error, ManagedConfigPublishError::ExtraCaCertsReadFailed { .. }),
            "size is a read failure, not a content failure; got {error:?}"
        );
    }

    /// C-003, C-004, D-10: a file whose PEM block is not a `CERTIFICATE` —
    /// a public key here, the pasted-private-key case in the wild — is refused
    /// as data (65), never silently skipped the way `reqwest`'s bundle parser
    /// would. Garbage that is no PEM at all takes the same door.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_content_that_is_not_a_certificate_is_65() {
        for (name, content) in [("key.pem", GOLDEN_PUBLIC_KEY_PEM), ("garbage.pem", "not a pem\n")] {
            let directory = tempfile::tempdir().expect("tempdir");
            std::fs::write(directory.path().join(name), content).expect("write the file");

            let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs(name))
                .await
                .err()
                .unwrap_or_else(|| panic!("`{name}` is not a CA bundle and must be refused"));
            assert!(
                matches!(error, ManagedConfigPublishError::ExtraCaCertsInvalid { .. }),
                "`{name}` got {error:?}"
            );
            // Named once, by the inner verdict's origin — the wrapper adds no
            // second copy of it.
            let chain = ocx_util::error::render_chain(&error);
            let path = directory.path().join(name).display().to_string();
            assert_eq!(
                chain.matches(&path).count(),
                1,
                "`{name}`: the path is named once: {chain}"
            );
        }
    }

    /// C-003: a bundle the certificate parser accepts (the `pem` crate skips
    /// leading label text as bytes) but that is not UTF-8 cannot be inlined
    /// into a TOML string unchanged — refused as data (65), never expanded
    /// lossily.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_non_utf8_bundle_is_65() {
        let directory = tempfile::tempdir().expect("tempdir");
        let bundle = [b"subject=caf\xe9\n".as_slice(), EXTRA_CA_CERTS_FIXTURE_PEM.as_bytes()].concat();
        std::fs::write(directory.path().join("latin1-ca.pem"), &bundle).expect("write the bundle");

        let error = publish_from_tempdir(&directory, &payload_naming_extra_ca_certs("latin1-ca.pem"))
            .await
            .expect_err("a non-UTF-8 bundle cannot be inlined");
        assert!(
            matches!(error, ManagedConfigPublishError::ExtraCaCertsNotUtf8 { .. }),
            "got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("strip the non-UTF-8 label lines outside the -----BEGIN/-----END blocks"),
            "the refusal names the remedy: {error}"
        );
    }

    /// C-003 (inline form): an `extra_ca_certs_pem` authored directly in the
    /// payload is proved usable at push — garbage is refused as config (78,
    /// the loader's verdict on the same text) and nothing is pushed — while
    /// a valid one passes through to the push itself. The validator stays
    /// pure (`ocx config test` catches only the ambiguity), so the proof
    /// lives on the publish path.
    ///
    /// Mutation: drop the `validate_extra_ca_certs_pem` call — the garbage
    /// payload reaches the stub publisher and this reds on the variant.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_pem_authored_inline_is_validated_at_push() {
        let directory = tempfile::tempdir().expect("tempdir");

        let error = publish_from_tempdir(&directory, "extra_ca_certs_pem = \"not a pem\"\n")
            .await
            .expect_err("garbage inline text is refused before the push");
        assert!(
            matches!(error, ManagedConfigPublishError::ExtraCaCertsPemInvalid { .. }),
            "got {error:?}"
        );
        // The key is named once, by the inner verdict's origin.
        let chain = ocx_util::error::render_chain(&error);
        assert_eq!(
            chain.matches("extra_ca_certs_pem").count(),
            1,
            "the key is named once: {chain}"
        );

        // Positive control: a usable bundle passes validation and publishes.
        let valid = format!("extra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n");
        publish_from_tempdir(&directory, &valid)
            .await
            .expect("a usable inline bundle publishes");
    }

    /// C-003: the ambiguity is refused by the validator BEFORE any file is
    /// read — the path named here does not exist, so a publish that read
    /// first would answer 79 instead of 78.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_refuses_both_extra_ca_certs_keys_before_reading_anything() {
        let directory = tempfile::tempdir().expect("tempdir");
        let payload =
            format!("extra_ca_certs = \"absent-ca.pem\"\nextra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n");
        let error = publish_from_tempdir(&directory, &payload)
            .await
            .expect_err("both spellings are refused");
        assert!(
            matches!(error, ManagedConfigPublishError::AmbiguousExtraCaCerts),
            "got {error:?}"
        );
    }

    /// C-003: the size gate runs again AFTER inlining. The payload and the
    /// bundle each sit under their own cap, so only their sum can trip it —
    /// a publish that checked size once, before the rewrite, would ship a
    /// payload no consumer can fetch.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_extra_ca_certs_inlined_payload_over_cap_is_refused() {
        let directory = tempfile::tempdir().expect("tempdir");
        let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES as usize;

        // ~27 KiB of well-formed certificates: under the 32 KiB bundle cap.
        let bundle = EXTRA_CA_CERTS_FIXTURE_PEM.repeat(40);
        assert!(
            bundle.len() < EXTRA_CA_CERTS_CAP_BYTES,
            "the bundle alone must pass the read cap"
        );
        std::fs::write(directory.path().join("bundle.pem"), &bundle).expect("write the bundle");

        // Padded to sit under the payload cap on its own, over it once inlined.
        let padding_bytes = maximum - bundle.len() / 2;
        let payload = format!(
            "{}# {}\n",
            payload_naming_extra_ca_certs("bundle.pem"),
            "x".repeat(padding_bytes - 3)
        );
        assert!(
            payload.len() <= maximum,
            "the payload alone must pass the pre-inline cap"
        );
        assert!(
            payload.len() + bundle.len() > maximum,
            "payload plus bundle must exceed the cap, or the re-check is untested"
        );

        let error = publish_from_tempdir(&directory, &payload)
            .await
            .expect_err("the inlined payload exceeds the cap");
        assert!(
            matches!(error, ManagedConfigPublishError::PayloadTooLarge { .. }),
            "got {error:?}"
        );
    }
}
