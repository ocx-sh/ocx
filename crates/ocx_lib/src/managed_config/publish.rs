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
//! | [`validate_managed_config_payload`] | Pure: size cap, TOML parse as [`crate::config::Config`], `[managed]` rejection | Unit-testable with synthetic bytes |
//! | [`publish_managed_config`] | I/O + network: stage, bundle, push (cascade-aware) | Acceptance test |

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
    /// The repository does not exist yet — tolerate a failing tag listing
    /// during a cascade push instead of aborting.
    pub new: bool,
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

    /// Listing existing tags for a cascade push failed and `--new` was not
    /// passed.
    #[error("failed to list existing tags for '{identifier}' (pass --new for a first publish)")]
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
            Self::PayloadTooLarge { .. } | Self::InvalidToml { .. } | Self::ContainsManagedSection => {
                Some(crate::cli::ExitCode::ConfigError)
            }
            Self::ReadFailed { source, .. } => Some(match source.kind() {
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
/// 3. carries no `[managed]` section.
///
/// # Errors
///
/// [`ManagedConfigPublishError::PayloadTooLarge`],
/// [`ManagedConfigPublishError::InvalidToml`],
/// [`ManagedConfigPublishError::ContainsManagedSection`].
pub fn validate_managed_config_payload(bytes: &[u8]) -> Result<(), ManagedConfigPublishError> {
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
    Ok(())
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
    validate_managed_config_payload(&bytes)?;

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
        }),
        platform: options.platform,
    };
    let layers = [LayerRef::File {
        path: archive,
        layout: Default::default(),
    }];

    let outcome = if options.cascade {
        let existing_tags = match publisher.list_tags(identifier.clone()).await {
            Ok(tags) => tags,
            Err(source) => {
                if options.new {
                    crate::log::info!("failed to list tags, assuming new managed-config repository: {source}");
                    Vec::new()
                } else {
                    return Err(ManagedConfigPublishError::ListTagsFailed {
                        identifier: Box::new(identifier.clone()),
                        source: Box::new(source),
                    });
                }
            }
        };
        let existing_versions = Publisher::parse_versions(&existing_tags);
        // Canonical tagging (`adr_index_indirection.md` Decision E) is a
        // `ocx package push` CLI contract; managed-config publishing has no
        // `--[no-]canonical-tag` surface of its own, so it opts out to keep
        // today's tag set unchanged.
        publisher
            .push_cascade(vec![info], &layers, existing_versions, None, false)
            .await
            .map_err(|source| ManagedConfigPublishError::PushFailed {
                source: Box::new(source),
            })?
    } else {
        publisher
            .push(vec![info], &layers, None, false)
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
}
