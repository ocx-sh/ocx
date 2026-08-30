// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publish leg for the managed-config tier — `ocx config push`.
//!
//! Managed config is published as an **ordinary ocx package** whose content
//! is a single `config.toml` file (managed-config v2, ADR
//! `adr_managed_config_tier.md` v2 amendment). No custom artifact type, no
//! parallel publish subsystem: the payload is staged as `config.toml`,
//! bundled via [`crate::package::bundle::BundleBuilder`] (tar+gzip), given a
//! synthesized minimal bundle metadata, and pushed through the existing
//! [`crate::publisher::Publisher`] — so versioning, cascade tags, rollback
//! and variants all reuse the package machinery.
//!
//! | Function | Concerns | Testable |
//! |---|---|---|
//! | [`validate_managed_config_payload`] | Pure: size cap, TOML parse as [`crate::config::Config`], `[managed]` rejection, `[trust.sigstore]` XOR | Unit-testable with synthetic bytes |
//! | [`inline_trusted_root`] | Pure: rewrite a path-form `trusted_root` into `trusted_root_json` | Unit-testable with synthetic text |
//! | [`publish_managed_config`] | I/O + network: read the trust root, stage, bundle, push (cascade-aware) | Acceptance test |
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

use crate::oci::{Identifier, Platform};
use crate::package::info::Info;
use crate::package::metadata::{Metadata, bundle};
use crate::publisher::{LayerRef, Publisher, PushOutcome};

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
#[non_exhaustive]
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

    /// The payload exceeds [`crate::managed_config::MAX_MANAGED_CONFIG_BYTES`].
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
        source: crate::trust::TrustPolicyError,
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
        identifier: Box<Identifier>,
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

impl crate::cli::ClassifyExitCode for ManagedConfigPublishError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // Payload rejections are operator config mistakes.
            Self::PayloadTooLarge { .. }
            | Self::InvalidToml { .. }
            | Self::ContainsManagedSection
            | Self::AmbiguousTrustRoot
            | Self::ManagedConfigKeyByPath
            | Self::TrustedRootInvalid { .. } => Some(crate::cli::ExitCode::ConfigError),
            // The third door onto one refusal: a payload naming a recognised
            // but unimplemented backend is "upgrade ocx", not "your config is
            // malformed", and `--key` plus the local config tiers both already
            // answer 85 for the identical value.
            Self::InvalidTrustPolicy { source } if source.names_unsupported_backend() => {
                Some(crate::cli::ExitCode::UnsupportedKeyBackend)
            }
            Self::InvalidTrustPolicy { .. } => Some(crate::cli::ExitCode::ConfigError),
            Self::ReadFailed { source, .. } | Self::TrustedRootReadFailed { source, .. } => Some(match source.kind() {
                std::io::ErrorKind::NotFound => crate::cli::ExitCode::NotFound,
                std::io::ErrorKind::PermissionDenied => crate::cli::ExitCode::PermissionDenied,
                _ => crate::cli::ExitCode::IoError,
            }),
            Self::StageFailed { .. } => Some(crate::cli::ExitCode::IoError),
            // Registry/bundling failures delegate to the inner cause's own
            // classification (Unavailable 69 / AuthError 80 / …). Explicit
            // delegation, not `None`: the boxed source's `TypeId` is
            // `Box<crate::Error>`, which the chain walker's downcast ladder
            // would never match.
            Self::BundleFailed { source } | Self::ListTagsFailed { source, .. } | Self::PushFailed { source } => {
                source.classify()
            }
        }
    }
}

/// Whether a `kind = "key"` signer names its key by **path**.
///
/// The refusal below is about paths, not about `key` being set at all: a KMS
/// reference (`awskms://alias/release`) travels with the payload and means the
/// same thing on every consumer's machine, so "inline it as `key_pem`" is
/// advice no operator can follow for one. Unparseable references fall through
/// to `compile()`, which names what is wrong with them.
fn names_a_path(key: &crate::trust::KeyMatcher) -> bool {
    key.key
        .as_deref()
        .and_then(|reference| crate::oci::sign::KeyRef::parse(reference).ok())
        .is_some_and(|reference| reference.as_path().is_some())
}

// ── Pure validation ───────────────────────────────────────────────────────────

/// Validates a managed-config payload before publishing.
///
/// Pure function over the raw payload bytes:
///
/// 1. size ≤ [`crate::managed_config::MAX_MANAGED_CONFIG_BYTES`] (the same
///    cap the consumer-side fetch enforces — an oversize payload could never
///    be adopted),
/// 2. parses as [`crate::config::Config`] (unknown **top-level** sections are
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
/// [`ManagedConfigPublishError::ManagedConfigKeyByPath`],
/// [`ManagedConfigPublishError::InvalidTrustPolicy`].
pub fn validate_managed_config_payload(bytes: &[u8]) -> Result<&str, ManagedConfigPublishError> {
    use serde::de::Error as _;

    let actual = bytes.len() as u64;
    let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;
    if actual > maximum {
        return Err(ManagedConfigPublishError::PayloadTooLarge { actual, maximum });
    }

    let text = std::str::from_utf8(bytes).map_err(|utf8_error| ManagedConfigPublishError::InvalidToml {
        source: toml::de::Error::custom(utf8_error),
    })?;
    let parsed: crate::config::Config =
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
    // The same rule one table over: key material a fleet receives must travel
    // with the payload, so a signer names its key inline or not at all.
    if let Some(trust) = parsed.trust.as_ref()
        && trust.policy.iter().any(|policy| {
            policy
                .signers
                .iter()
                .any(|signer| matches!(signer, crate::trust::SignerSpec::Key(key) if names_a_path(key)))
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
    let parsed: crate::config::Config = toml::from_str(text).ok()?;
    parsed.trust?.sigstore?.trusted_root
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
/// and inlined as `trusted_root_json` — see the module docs for why.
///
/// # Errors
///
/// See [`ManagedConfigPublishError`] variants.
pub async fn publish_managed_config(
    publisher: &Publisher,
    identifier: &Identifier,
    config_path: &Path,
    options: ManagedConfigPublishOptions,
) -> Result<PushOutcome, ManagedConfigPublishError> {
    let bytes = tokio::fs::read(config_path)
        .await
        .map_err(|source| ManagedConfigPublishError::ReadFailed {
            path: config_path.to_path_buf(),
            source,
        })?;
    let text = validate_managed_config_payload(&bytes)?;
    let bytes = match declared_trusted_root(text) {
        None => bytes.clone(),
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
            //
            // `to_string_lossy` is exact: the value is deserialized from a TOML
            // string, so it is UTF-8 by construction.
            let written = declared.to_string_lossy().into_owned();
            let path = crate::utility::fs::path::FileReference::parse(&written)
                .anchored_at(config_path.parent().unwrap_or(Path::new(".")));
            let json =
                tokio::fs::read(&path)
                    .await
                    .map_err(|source| ManagedConfigPublishError::TrustedRootReadFailed {
                        path: path.clone(),
                        source,
                    })?;
            // Prove it loads before a whole fleet adopts it. `load_trusted_root_json`
            // is the same entry point verification uses, so "publish succeeded"
            // means "every consumer can build a trust root from this".
            crate::oci::verify::TrustRoot::load_trusted_root_json(&json).map_err(|kind| {
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
            inline_trusted_root(text, json)?.into_bytes()
        }
    };

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
    crate::package::bundle::BundleBuilder::from_path(&staged)
        .create(&archive)
        .await
        .map_err(|source| ManagedConfigPublishError::BundleFailed {
            source: Box::new(source),
        })?;

    let info = Info {
        identifier: identifier.clone(),
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
                source: Box::new(source),
            }
        })?;
        let existing_versions = Publisher::parse_versions(&existing_tags);
        // Keep tagging (`adr_index_indirection.md` Decision E) is a
        // `ocx package push` CLI contract; managed-config publishing has no
        // `--[no-]keep-tag` surface of its own, so it opts out to keep
        // today's tag set unchanged. Index annotations are likewise a
        // `ocx package push --annotation` contract with no `ocx config push`
        // surface, so none are written.
        publisher
            .push_cascade(vec![info], &layers, existing_versions, None, false, &BTreeMap::new())
            .await
            .map_err(|source| ManagedConfigPublishError::PushFailed {
                source: Box::new(source),
            })?
    } else {
        publisher
            .push(vec![info], &layers, None, false, &BTreeMap::new())
            .await
            .map_err(|source| ManagedConfigPublishError::PushFailed {
                source: Box::new(source),
            })?
    };

    // `stage` (TempDir) lives until here so the archive exists for the push.
    drop(stage);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};

    #[test]
    fn validate_accepts_plain_config() {
        let toml = b"[registry]\ndefault = \"corp.example.com\"\n";
        validate_managed_config_payload(toml).expect("plain config must validate");
    }

    /// Fleet forward-compat: a payload published by a newer ocx may carry
    /// top-level sections this binary does not know — accepted, matching the
    /// loader's no-`deny_unknown_fields` posture on [`crate::config::Config`].
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
        assert_eq!(err.classify(), Some(ExitCode::ConfigError));
    }

    #[test]
    fn validate_rejects_invalid_toml() {
        let err = validate_managed_config_payload(b"not = [valid").expect_err("invalid TOML must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::InvalidToml { .. }));
        assert_eq!(err.classify(), Some(ExitCode::ConfigError));
    }

    #[test]
    fn validate_rejects_non_utf8_payload() {
        let err = validate_managed_config_payload(&[0xff, 0xfe, 0x00]).expect_err("non-UTF-8 must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::InvalidToml { .. }));
    }

    #[test]
    fn validate_rejects_oversize_payload() {
        let oversize = "# padding\n".repeat(7_000); // ~70 KiB > 64 KiB cap
        assert!(oversize.len() as u64 > crate::managed_config::MAX_MANAGED_CONFIG_BYTES);
        let err = validate_managed_config_payload(oversize.as_bytes()).expect_err("oversize must be rejected");
        assert!(matches!(err, ManagedConfigPublishError::PayloadTooLarge { .. }));
        assert_eq!(err.classify(), Some(ExitCode::ConfigError));
    }

    /// S1 boundary: a payload of EXACTLY `MAX_MANAGED_CONFIG_BYTES` validates —
    /// the size gate is `> maximum` (strict), so the ceiling itself is
    /// admitted. Its MAX+1 twin is `validate_rejects_oversize_payload` above.
    /// (Padded as a single TOML comment line so the whole file is valid TOML.)
    #[test]
    fn validate_accepts_payload_of_exactly_maximum_bytes() {
        let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;
        let payload = format!("# {}", "x".repeat((maximum - 2) as usize));
        assert_eq!(payload.len() as u64, maximum, "the payload must be exactly at the cap");
        validate_managed_config_payload(payload.as_bytes()).expect("a payload exactly at the cap must validate");
    }

    /// Registry-side failures delegate classification to the inner cause
    /// (here `OfflineMode` → PolicyBlocked 81), both directly and through the
    /// `classify_error` chain walker.
    #[test]
    fn push_failures_defer_to_inner_classification() {
        let err = ManagedConfigPublishError::PushFailed {
            source: Box::new(crate::Error::OfflineMode),
        };
        assert_eq!(err.classify(), Some(ExitCode::PolicyBlocked));
        assert_eq!(
            crate::cli::classify_error(&err as &(dyn std::error::Error + 'static)),
            ExitCode::PolicyBlocked
        );
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
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
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
        let sigstore = toml::from_str::<crate::config::Config>(parsed)
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
                    source: crate::trust::TrustPolicyError::NoSigners { .. }
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
                    source: crate::trust::TrustPolicyError::KeyMalformed { .. }
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
                    source: crate::trust::TrustPolicyError::KeyReferenceInvalid {
                        source: crate::oci::sign::KeyRefError::FileColonPrefix { .. },
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
        assert_eq!(
            error.classify(),
            Some(ExitCode::UnsupportedKeyBackend),
            "the same 85 the `--key` and local-config doors answer; got {error:?}"
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
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
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

        let local: crate::config::Config =
            toml::from_str(&payload_with_key_reference(&reference)).expect("the same text is ordinary config");
        local.trust_policies()[0]
            .compile()
            .expect("a local tier resolves the very same reference");
    }
}
