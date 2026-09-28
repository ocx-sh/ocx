// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publish leg for the managed-config tier — `ocx config push`: one `config.toml`
//! as an ordinary package (`adr_managed_config_tier.md` v2 amendment).

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
    /// Also update the rolling tags derived from the pushed version tag.
    pub cascade: bool,
    /// `ocx config update` consumes only the `any/any` entry, so any other
    /// platform publishes a package it cannot use.
    pub platform: Platform,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors raised while validating or publishing a managed-config payload.
#[derive(Debug, thiserror::Error)]
pub enum ManagedConfigPublishError {
    #[error("failed to read managed config payload '{}'", path.display())]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Over [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`].
    #[error("managed config payload is {actual} bytes, exceeding the maximum allowed {maximum} bytes")]
    PayloadTooLarge {
        /// Both sizes are in bytes.
        actual: u64,
        maximum: u64,
    },

    /// Not UTF-8, not TOML, or not the config schema.
    #[error("managed config payload is not a valid config file")]
    InvalidToml {
        #[source]
        source: toml::de::Error,
    },

    /// A consumer strips a published `[managed]` section anyway.
    #[error("managed config payload must not contain a [managed] section")]
    ContainsManagedSection,

    /// Both `trusted_root` and `trusted_root_json`: which one wins is not predictable from the file.
    #[error("managed config payload declares both trusted_root and trusted_root_json in [trust.sigstore]: keep one")]
    AmbiguousTrustRoot,

    /// Both `extra_ca_certs` and `extra_ca_certs_pem`, for the same reason as [`Self::AmbiguousTrustRoot`].
    #[error("managed config payload declares both extra_ca_certs and extra_ca_certs_pem: keep one")]
    AmbiguousExtraCaCerts,

    /// A `[[trust.policy]]` key signer names a path, which exists only on the operator's disk.
    #[error("managed config payload declares a key signer by path in [[trust.policy]]: inline it as `key_pem` instead")]
    ManagedConfigKeyByPath,

    /// A `[[trust.policy]]` entry does not compile, which would fail closed on every consumer at once.
    #[error("managed config payload declares an unusable [[trust.policy]] entry")]
    InvalidTrustPolicy {
        #[source]
        source: ocx_trust::TrustPolicyError,
    },

    #[error("failed to read trusted root '{}' named by [trust.sigstore] trusted_root", path.display())]
    TrustedRootReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("trusted root '{}' is not a usable Sigstore trusted root: {detail}", path.display())]
    TrustedRootInvalid {
        /// The resolved trusted-root path.
        path: PathBuf,
        detail: String,
    },

    /// Carries [`ExtraRootsSource`], not a path, so a PEM body pasted where a
    /// path belongs is redacted rather than echoed (CWE-532).
    #[error("cannot read {origin} named by the payload")]
    ExtraCaCertsReadFailed {
        origin: ExtraRootsSource,
        #[source]
        source: std::io::Error,
    },

    /// The file named by `extra_ca_certs` is not a usable CA bundle (65: the
    /// bytes are wrong); the inner verdict names the path, so this must not.
    #[error("the extra CA certificate bundle named by the payload is not usable")]
    ExtraCaCertsInvalid {
        #[source]
        source: ocx_config::tls::TlsError,
    },

    /// The inline `extra_ca_certs_pem` is not a usable CA bundle (78: the
    /// payload's own content is wrong); the inner verdict names the key, so this must not.
    #[error("the extra CA certificate bundle inlined in the payload is not usable")]
    ExtraCaCertsPemInvalid {
        #[source]
        source: TlsError,
    },

    /// Strict, never lossy: a lossy decode can grow a bundle that fits the
    /// read cap past the consumer's equal inline cap.
    #[error(
        "{origin} is not UTF-8 and cannot be inlined; strip the non-UTF-8 label lines outside the \
         -----BEGIN/-----END blocks"
    )]
    ExtraCaCertsNotUtf8 {
        origin: ExtraRootsSource,
        #[source]
        source: std::str::Utf8Error,
    },

    #[error("failed to stage managed config payload for publishing")]
    StageFailed {
        #[source]
        source: std::io::Error,
    },

    #[error("failed to bundle managed config payload")]
    BundleFailed {
        #[source]
        source: Box<crate::Error>,
    },

    #[error("failed to list existing tags for '{identifier}'")]
    ListTagsFailed {
        identifier: Box<OciIdentifier>,
        #[source]
        source: Box<crate::Error>,
    },

    #[error("failed to push managed config package")]
    PushFailed {
        #[source]
        source: Box<crate::Error>,
    },
}

/// Whether a key signer names its key by path; a KMS reference travels with the
/// payload and must not be refused, and an unparseable one is left to `compile()`.
fn names_a_path(key: &ocx_trust::KeyMatcher) -> bool {
    key.key
        .as_deref()
        .and_then(|reference| ocx_trust::key_ref::KeyRef::parse(reference).ok())
        .is_some_and(|reference| reference.as_path().is_some())
}

// ── Pure validation ───────────────────────────────────────────────────────────

/// Validates a managed-config payload purely over its bytes and returns it as text.
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
    if parsed.extra_ca_certs.is_some() && parsed.extra_ca_certs_pem.is_some() {
        return Err(ManagedConfigPublishError::AmbiguousExtraCaCerts);
    }
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
    // After the key-by-path refusal, or compiling would read a file from the operator's disk.
    for policy in parsed.trust.iter().flat_map(|trust| trust.policy.iter()) {
        policy
            .compile()
            .map_err(|source| ManagedConfigPublishError::InvalidTrustPolicy { source })?;
    }
    Ok(text)
}

/// The path-form trust root a payload declares, if any.
#[must_use]
pub fn declared_trusted_root(text: &str) -> Option<PathBuf> {
    let parsed: ocx_config::Config = toml::from_str(text).ok()?;
    parsed.trust?.sigstore?.trusted_root
}

/// The path-form `extra_ca_certs` a payload declares, if any.
#[must_use]
pub fn declared_extra_ca_certs(text: &str) -> Option<PathBuf> {
    let parsed: ocx_config::Config = toml::from_str(text).ok()?;
    parsed.extra_ca_certs
}

/// The inline `extra_ca_certs_pem` a payload authors directly, if any.
#[must_use]
pub fn declared_extra_ca_certs_pem(text: &str) -> Option<String> {
    let parsed: ocx_config::Config = toml::from_str(text).ok()?;
    parsed.extra_ca_certs_pem
}

/// Replaces a path-form `extra_ca_certs` with `extra_ca_certs_pem`, leaving the
/// rest of the document byte-identical; a payload without one is returned unchanged.
///
/// # Errors
/// [`ManagedConfigPublishError::InvalidToml`] when the payload is not TOML.
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

/// Re-checks the size cap after inlining, which can grow the payload past it.
///
/// # Errors
/// [`ManagedConfigPublishError::PayloadTooLarge`] over the cap.
fn guard_inlined_payload_size(bytes: &[u8]) -> Result<(), ManagedConfigPublishError> {
    let actual = bytes.len() as u64;
    let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
    if actual > maximum {
        return Err(ManagedConfigPublishError::PayloadTooLarge { actual, maximum });
    }
    Ok(())
}

/// Replaces a path-form `[trust.sigstore] trusted_root` with `trusted_root_json`,
/// leaving the rest of the document byte-identical; a payload without one is returned unchanged.
///
/// # Errors
///
/// [`ManagedConfigPublishError::InvalidToml`] when the payload is not TOML.
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

/// Resolves a declared path against the payload's directory with the loader's
/// [`FileReference`] grammar; `to_string_lossy` is exact because TOML strings are UTF-8.
fn anchor_declared_path(declared: &Path, config_path: &Path) -> PathBuf {
    let written = declared.to_string_lossy().into_owned();
    FileReference::parse(&written).anchored_at(config_path.parent().unwrap_or(Path::new(".")))
}

/// Reads and validates the bundle a path-form `extra_ca_certs` names, on the
/// blocking pool because [`read_path`] and [`parse_pem`] both block.
///
/// # Errors
///
/// [`ManagedConfigPublishError::ExtraCaCertsReadFailed`] for any read refusal,
/// including an over-cap or non-regular file (74: `InvalidInput`);
/// [`ManagedConfigPublishError::ExtraCaCertsInvalid`] (65) for unusable bytes.
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

/// The refusal origin for a path-form `extra_ca_certs`, with no tier because the
/// payload is not one of the consumer's config tiers.
fn publish_origin(path: &Path) -> ExtraRootsSource {
    ExtraRootsSource::ConfigPath {
        path: path.to_path_buf(),
        tier: None,
    }
}

/// Proves an authored `extra_ca_certs_pem` is a usable bundle, on the blocking
/// pool because [`parse_pem`] loads the platform trust store.
///
/// # Errors
///
/// [`ManagedConfigPublishError::ExtraCaCertsPemInvalid`] (78) for an unusable
/// bundle; [`ManagedConfigPublishError::StageFailed`] for a panicking pool task.
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

/// Reads the candidate payload, bounded at
/// [`ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES`] and refusing a
/// non-regular file before `open(2)`, or a FIFO hangs and `/dev/zero` exhausts memory.
///
/// # Errors
///
/// [`ManagedConfigPublishError::PayloadTooLarge`] (78) over the cap;
/// [`ManagedConfigPublishError::ReadFailed`] for any other refusal.
pub async fn read_candidate_payload(path: &Path) -> Result<Vec<u8>, ManagedConfigPublishError> {
    let maximum = ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES;
    let read_failed = |source| ManagedConfigPublishError::ReadFailed {
        path: path.to_path_buf(),
        source,
    };
    match read_bounded_async(path, maximum).await {
        Ok(bytes) => Ok(bytes),
        Err(BoundedReadError::TooLarge { .. }) => {
            // The bounded read stopped one byte past the cap, so the real size comes from metadata.
            let actual = tokio::fs::metadata(path).await.map_err(read_failed)?.len();
            Err(ManagedConfigPublishError::PayloadTooLarge { actual, maximum })
        }
        Err(refused) => Err(read_failed(refused.into_io_error())),
    }
}

// ── Publish orchestration ─────────────────────────────────────────────────────

/// Publishes `config_path` as a managed-config package under `identifier`; the
/// caller owns [`Publisher::ensure_auth`]. Path-form `trusted_root` and
/// `extra_ca_certs` are proved usable and inlined, since the fleet cannot read
/// the operator's files and would otherwise get a silently inert trust root.
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
            // Through the loader's `FileReference` grammar, or `file:///x` is read as a file of that name.
            let path = anchor_declared_path(&declared, config_path);
            // Verification's own size cap, so a root publish accepts is never too large for a consumer.
            let json = read_bounded_async(&path, ocx_sign::verify::MAX_TRUSTED_ROOT_BYTES)
                .await
                .map_err(|refused| ManagedConfigPublishError::TrustedRootReadFailed {
                    path: path.clone(),
                    source: refused.into_io_error(),
                })?;
            // Verification's own loader, so a published root is one every consumer can build.
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
            // A bad root published here breaks every consumer command, `config update` included.
            if let Some(pem) = declared_extra_ca_certs_pem(&text) {
                validate_extra_ca_certs_pem(pem).await?;
            }
            text
        }
        Some(declared) => {
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

    // The archive entry must be `config.toml` whatever the input file is called.
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
        // Keep tags (`adr_index_indirection.md` Decision E), annotations and `--default`
        // have no `ocx config push` surface, so all three stay off.
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

    // Held past the push, or the archive is deleted before it is read.
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

    /// Boundary: a payload of EXACTLY `MAX_MANAGED_CONFIG_BYTES` validates —
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

    // ── `extra_ca_certs` → `extra_ca_certs_pem` inlining ─────

    /// A real self-signed CA (the test stack's Fulcio root): the positive
    /// cases need material the certificate parser accepts, not a placeholder.
    const EXTRA_CA_CERTS_FIXTURE_PEM: &str = include_str!("../../../../test/sigstore/keys/fulcio-ca.crt.pem");

    /// The per-file cap every `extra_ca_certs` read goes through.
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

    /// Both spellings in one payload is the same
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

    /// The read is skipped for the overwhelmingly common payload that
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

    /// The path is replaced by the bundle it named, as
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

    /// Nothing to inline, nothing rewritten — byte-identical.
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

    /// A payload naming a bundle that does not exist exits 79 and the
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

    /// Redaction at the publish door: a PEM body pasted under
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

    /// An unreadable bundle is 77, not a generic I/O failure — the fix
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

    /// The read goes through `read_bounded`, which refuses a non-regular
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

    /// A bundle over the 32 KiB cap is `read_bounded`'s `TooLarge`
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

    /// A file whose PEM block is not a `CERTIFICATE` —
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

    /// A bundle the certificate parser accepts (the `pem` crate skips
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

    /// Inline form: an `extra_ca_certs_pem` authored directly in the
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

    /// The ambiguity is refused by the validator BEFORE any file is
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

    /// The size gate runs again AFTER inlining. The payload and the
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
