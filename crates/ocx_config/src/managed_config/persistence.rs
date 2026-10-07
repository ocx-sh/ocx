// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fetch ([`fetch_managed_config`]) and persistence ([`persist_managed_config`]) of the
//! managed-config package.
//!
//! Three independent 64 KiB caps (CWE-400): the declared layer size, the streamed blob bytes, the
//! decompressed tar stream.

use crate::managed::ManagedConfigSnapshot;
use crate::managed_config::ManagedConfigPaths;
use ocx_oci::client::ReadAddressing;
use ocx_oci::{Digest, OciIdentifier};

// ── Fetched payload (intermediate transfer object) ────────────────────────────

/// Result of a successful [`fetch_managed_config`] before persistence.
#[derive(Debug)]
pub struct FetchedManagedConfig {
    /// The top-level manifest digest, the tier's drift identity.
    pub manifest_digest: Digest,
    /// The `config.toml` text, digest-verified and size-capped but not yet TOML-validated.
    pub config_text: String,
}

// ── Fetch errors ──────────────────────────────────────────────────────────────

/// Errors raised while fetching the managed-config package from the registry.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum ManagedConfigFetchError {
    /// A network or auth error from the OCI client.
    #[error("failed to fetch managed config from registry")]
    #[exit(delegate = source)]
    FetchFailed {
        /// The underlying OCI client error.
        #[source]
        source: ocx_oci::client::error::ClientError,
    },

    /// The manifest chain had an unexpected shape (e.g. an index entry that
    /// resolves to another index, or a child manifest that vanished).
    #[error("unexpected manifest shape for managed config: {detail}")]
    #[exit(
        DataError,
        slug = "managed_config_unexpected_manifest",
        summary = "The managed-config package has an unexpected manifest shape"
    )]
    UnexpectedManifest {
        /// Human-readable detail about the shape mismatch.
        detail: String,
    },

    /// The image index has no `any/any` entry (publish with the default `--platform any/any`).
    #[error("managed config package has no any/any platform entry")]
    #[exit(
        DataError,
        slug = "managed_config_no_any_platform",
        summary = "The managed-config package has no any/any platform entry"
    )]
    NoAnyPlatformEntry,

    /// The selected image manifest has no tar+gzip layer.
    #[error("managed config package has no tar+gzip layer")]
    #[exit(
        DataError,
        slug = "managed_config_no_gzip_layer",
        summary = "The managed-config package has no tar+gzip layer"
    )]
    NoGzipLayer,

    /// The package's layer archive contains no `config.toml` entry.
    #[error("managed config package layer contains no config.toml")]
    #[exit(
        DataError,
        slug = "managed_config_missing_config_toml",
        summary = "The managed-config layer contains no config.toml"
    )]
    MissingConfigToml,

    /// The declared layer size exceeded
    /// [`crate::managed_config::MAX_MANAGED_CONFIG_BYTES`].
    #[error("managed config layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    #[exit(
        DataError,
        slug = "managed_config_layer_size_exceeded",
        summary = "The managed-config layer declares a size above the allowed maximum"
    )]
    LayerSizeExceeded {
        /// The size declared in the manifest layer descriptor.
        declared: i64,
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The SHA-256 digest of the fetched layer bytes does not match the digest
    /// declared in the manifest.
    #[error("managed config layer digest mismatch: declared '{declared}', computed '{computed}'")]
    #[exit(
        DataError,
        slug = "managed_config_layer_digest_mismatch",
        summary = "The managed-config layer does not hash to its declared digest"
    )]
    LayerDigestMismatch {
        /// The digest declared in the manifest descriptor.
        declared: String,
        /// The digest actually computed from the fetched bytes.
        computed: String,
    },

    /// The `config.toml` entry (or the decompressed archive stream) exceeds
    /// [`crate::managed_config::MAX_MANAGED_CONFIG_BYTES`] — gzip-bomb guard.
    #[error("managed config config.toml entry exceeds the maximum allowed {maximum} bytes")]
    #[exit(
        DataError,
        slug = "managed_config_entry_too_large",
        summary = "The managed-config config.toml entry exceeds the allowed size"
    )]
    ConfigEntryTooLarge {
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The layer bytes are not a readable gzip'd tar archive (or the
    /// `config.toml` entry is not valid UTF-8).
    #[error("managed config layer is not a readable archive: {detail}")]
    #[exit(
        DataError,
        slug = "managed_config_invalid_archive",
        summary = "The managed-config layer is not a readable archive"
    )]
    InvalidArchive {
        /// Human-readable detail about the archive failure.
        detail: String,
    },
}

// ── Persist errors ────────────────────────────────────────────────────────────

/// Errors raised while persisting a fetched managed-config payload.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum ManagedConfigPersistError {
    /// The payload text is not valid TOML.
    #[error("managed config payload is not valid TOML")]
    #[exit(
        DataError,
        slug = "managed_config_invalid_toml",
        summary = "The fetched managed-config payload is not valid TOML"
    )]
    InvalidToml {
        /// The underlying TOML parse failure.
        #[source]
        source: toml::de::Error,
    },

    /// Writing the atomic snapshot file failed.
    #[error("failed to write managed config snapshot")]
    #[exit(
        IoError,
        slug = "managed_config_snapshot_write",
        summary = "Writing the managed-config snapshot failed"
    )]
    SnapshotWriteFailed {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The payload's `extra_ca_certs_pem` does not load on this host; refused before the write, so
    /// the previous snapshot stays in force.
    ///
    /// `ocx config push` proves it on the publisher's platform only; persisted, it would fail
    /// `Context::try_init` on every command here, `ocx config update` included.
    #[error("managed config payload carries an extra CA bundle this host cannot load; the previous snapshot is kept")]
    #[exit(delegate = source)]
    ExtraCaCertsInvalid {
        /// What the parser or verifier rejected, naming the block, never the bytes.
        #[source]
        source: crate::tls::TlsError,
    },
}

// ── Combined update error ─────────────────────────────────────────────────────

/// Combined error for a full fetch-then-persist update cycle
/// (`PackageManager::update_managed_config`, `ocx config update`).
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum ManagedConfigUpdateError {
    /// The fetch step failed.
    #[error("failed to fetch managed config")]
    #[exit(delegate)]
    Fetch(#[from] ManagedConfigFetchError),
    /// The persist step failed.
    #[error("failed to persist managed config")]
    #[exit(delegate)]
    Persist(#[from] ManagedConfigPersistError),
    /// The resolved source has no manifest in the registry.
    ///
    /// An error, not a `NotConfigured` success, or `ocx self setup --managed-config` reaches its
    /// `unreachable!()` arm.
    #[error("managed config source '{effective_source}' not found in registry")]
    #[exit(
        NotFound,
        slug = "managed_config_source_not_found",
        summary = "The registry has no managed-config package at the configured source"
    )]
    SourceNotFound {
        /// The resolved source that produced no manifest.
        effective_source: ocx_oci::OciIdentifier,
    },
    /// A `tag@digest` pin's tag resolved to a different digest; nothing was persisted.
    #[error("managed config pin digest mismatch: expected '{expected}' but the tag resolved to '{fetched}'")]
    #[exit(
        DataError,
        slug = "pin_digest_mismatch",
        summary = "The registry resolved the pinned tag to a different digest"
    )]
    PinDigestMismatch {
        /// The digest pinned in the VERSION argument.
        expected: ocx_oci::Digest,
        /// The digest the tag actually resolved to.
        fetched: ocx_oci::Digest,
    },
}

// ── Network primitive ─────────────────────────────────────────────────────────

/// Fetches the managed-config package for `identifier` and extracts its `config.toml`;
/// `Ok(None)` when the reference does not exist.
///
/// `client` must be built from the local-only mirror view, or a payload could redirect or brick
/// its own refresh.
///
/// # Errors
///
/// See [`ManagedConfigFetchError`] variants.
pub async fn fetch_managed_config(
    client: &ocx_oci::client::Client,
    identifier: &OciIdentifier,
) -> Result<Option<FetchedManagedConfig>, ManagedConfigFetchError> {
    let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;

    let Some((_, top_digest, top_manifest)) = client
        .fetch_manifest_raw_bytes_addressed(identifier, ReadAddressing::Mirrored)
        .await
        .map_err(|source| ManagedConfigFetchError::FetchFailed { source })?
    else {
        return Ok(None);
    };

    let image_manifest = match top_manifest {
        ocx_oci::Manifest::Image(image) => image,
        ocx_oci::Manifest::ImageIndex(index) => {
            let entry = index
                .manifests
                .iter()
                .find(|entry| match &entry.platform {
                    // No platform = platform-agnostic, as in `Index::fetch_candidates`.
                    None => true,
                    Some(platform) => ocx_oci::Platform::try_from(platform.clone()).is_ok_and(|p| p.is_any()),
                })
                .ok_or(ManagedConfigFetchError::NoAnyPlatformEntry)?;
            let child_digest =
                Digest::try_from(entry.digest.as_str()).map_err(|e| ManagedConfigFetchError::UnexpectedManifest {
                    detail: format!("index entry digest '{}' is malformed: {e}", entry.digest),
                })?;
            // Content-addressed child fetch: tag dropped, digest pins.
            let child_identifier = identifier.without_specifiers().clone_with_digest(child_digest);
            let child = client
                .fetch_manifest_raw_bytes_addressed(&child_identifier, ReadAddressing::Mirrored)
                .await
                .map_err(|source| ManagedConfigFetchError::FetchFailed { source })?
                .ok_or_else(|| ManagedConfigFetchError::UnexpectedManifest {
                    detail: format!("index entry '{child_identifier}' vanished before the child fetch"),
                })?;
            match child.2 {
                ocx_oci::Manifest::Image(image) => image,
                ocx_oci::Manifest::ImageIndex(_) => {
                    return Err(ManagedConfigFetchError::UnexpectedManifest {
                        detail: "index entry resolves to another index".to_string(),
                    });
                }
            }
        }
    };

    let layer_descriptor = image_manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == ocx_oci::media_type::MEDIA_TYPE_TAR_GZ)
        .ok_or(ManagedConfigFetchError::NoGzipLayer)?;

    // Cap 1: declared layer size.
    let declared = layer_descriptor.size;
    match u64::try_from(declared) {
        Ok(size) if size <= maximum => {}
        _ => return Err(ManagedConfigFetchError::LayerSizeExceeded { declared, maximum }),
    }

    let layer_digest = Digest::try_from(layer_descriptor.digest.as_str()).map_err(|e| {
        ManagedConfigFetchError::UnexpectedManifest {
            detail: format!("layer digest '{}' is malformed: {e}", layer_descriptor.digest),
        }
    })?;

    // Cap 2: streamed blob bytes.
    let layer_bytes = client
        .fetch_layer_blob_capped(identifier, &layer_digest, maximum)
        .await
        .map_err(|source| ManagedConfigFetchError::FetchFailed { source })?;

    // Re-verify the digest before touching the bytes.
    let computed = ocx_oci::Algorithm::Sha256.hash(&layer_bytes);
    if computed != layer_digest {
        return Err(ManagedConfigFetchError::LayerDigestMismatch {
            declared: layer_digest.to_string(),
            computed: computed.to_string(),
        });
    }

    // Cap 3: decompressed stream + tar scan.
    let config_text = extract_config_toml(&layer_bytes, maximum)?;

    Ok(Some(FetchedManagedConfig {
        manifest_digest: top_digest,
        config_text,
    }))
}

/// Scans a gzip'd tar archive for its `config.toml` entry, under a decompression cap.
// ponytail: tar+gzip only, the one shape `ocx config push` produces; add archive-backend dispatch
// if a real operator payload ever needs xz/zst.
fn extract_config_toml(compressed: &[u8], maximum: u64) -> Result<String, ManagedConfigFetchError> {
    use std::io::Read as _;

    let invalid = |detail: String| ManagedConfigFetchError::InvalidArchive { detail };

    let decoder = flate2::read::GzDecoder::new(compressed);
    // Plus one tar header block, or a config of exactly `maximum` bytes truncates as a clean EOF.
    const TAR_HEADER_BLOCK: u64 = 512;
    let capped = decoder.take(maximum.saturating_add(TAR_HEADER_BLOCK).saturating_add(1));
    let mut archive = tar::Archive::new(capped);
    let entries = archive
        .entries()
        .map_err(|e| invalid(format!("not a tar stream: {e}")))?;

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Err(invalid(format!("tar entry unreadable: {e}")));
            }
        };
        let is_config = entry
            .path()
            .map(|path| path.file_name() == Some(std::ffi::OsStr::new("config.toml")) && path.components().count() <= 2)
            .unwrap_or(false);
        if !is_config {
            continue; // extras ignored
        }
        if entry.size() > maximum {
            return Err(ManagedConfigFetchError::ConfigEntryTooLarge { maximum });
        }
        let mut text = String::new();
        entry.read_to_string(&mut text).map_err(|e| match e.kind() {
            // The capped reader ran dry mid-entry: the archive decompressed past the ceiling.
            std::io::ErrorKind::UnexpectedEof => ManagedConfigFetchError::ConfigEntryTooLarge { maximum },
            _ => invalid(format!("config.toml entry unreadable: {e}")),
        })?;
        return Ok(text);
    }
    Err(ManagedConfigFetchError::MissingConfigToml)
}

/// Probes only the top-level manifest digest, comparable against a persisted
/// [`ManagedConfigSnapshot::digest`]; `Ok(None)` when the reference does not exist.
///
/// # Errors
///
/// See [`ManagedConfigFetchError::FetchFailed`].
pub async fn probe_managed_config_digest(
    client: &ocx_oci::client::Client,
    identifier: &OciIdentifier,
) -> Result<Option<Digest>, ManagedConfigFetchError> {
    client
        .probe_manifest_digest_addressed(identifier, ReadAddressing::Mirrored)
        .await
        .map_err(|source| ManagedConfigFetchError::FetchFailed { source })
}

// ── Pure persistence primitive ────────────────────────────────────────────────

/// Parses the payload, strips any `[update]` and `[managed]` section (a payload never redirects
/// the tier that fetched it), proves its `extra_ca_certs_pem` loads on this host, and writes the
/// snapshot.
///
/// # Errors
///
/// See [`ManagedConfigPersistError`] variants.
pub async fn persist_managed_config(
    paths: &ManagedConfigPaths,
    source: &OciIdentifier,
    fetched: FetchedManagedConfig,
) -> Result<ManagedConfigSnapshot, ManagedConfigPersistError> {
    let text = fetched.config_text;

    let mut table: toml::Table =
        toml::from_str(&text).map_err(|source| ManagedConfigPersistError::InvalidToml { source })?;
    // Before the typed parse: `[update]` is personal, and a shape this ocx cannot read must not
    // refuse the rest of a payload written for a newer one.
    let stripped_update = table.remove("update").is_some();
    if stripped_update {
        log::debug!(
            "managed-config payload for '{source}' contained an [update] section; dropped before persisting \
             ([update] is read from local config.toml only)"
        );
    }
    let stripped_managed = table.remove("managed").is_some();
    if stripped_managed {
        log::warn!(
            "managed-config payload for '{source}' contained a [managed] section; stripped before persisting \
             (a remote payload can never redirect the tier that fetched it)"
        );
    }

    let stripped = stripped_update || stripped_managed;
    // The text parse when nothing was stripped, so its error keeps the line and column.
    let parsed: crate::Config = if stripped {
        toml::Value::Table(table.clone()).try_into()
    } else {
        toml::from_str(&text)
    }
    .map_err(|source| ManagedConfigPersistError::InvalidToml { source })?;

    // Before the write, or every later `try_init` fails closed on this snapshot.
    if let Some(pem) = parsed.extra_ca_certs_pem.clone() {
        tokio::task::spawn_blocking(move || {
            crate::tls::parse_pem(
                pem.as_bytes(),
                &crate::tls::ExtraRootsSource::ConfigInline(crate::ConfigTier::Managed),
            )
        })
        .await
        .map_err(|join| ManagedConfigPersistError::SnapshotWriteFailed {
            source: std::io::Error::other(format!("extra CA roots validation task panicked: {join}")),
        })?
        .map_err(|source| ManagedConfigPersistError::ExtraCaCertsInvalid { source })?;
    }

    let config = if stripped {
        toml::to_string(&table).map_err(|error| ManagedConfigPersistError::SnapshotWriteFailed {
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })?
    } else {
        text
    };

    let snapshot = ManagedConfigSnapshot {
        source: source.to_string(),
        tag: source.tag().map(str::to_string),
        digest: fetched.manifest_digest,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        config,
    };

    write_snapshot_atomic(paths, &snapshot)
        .await
        .map_err(|source| ManagedConfigPersistError::SnapshotWriteFailed { source })?;

    Ok(snapshot)
}

/// Writes the payload `config.toml` first, then `snapshot.json` as the commit marker, or a
/// reader could see metadata whose payload is not yet in place.
async fn write_snapshot_atomic(paths: &ManagedConfigPaths, snapshot: &ManagedConfigSnapshot) -> std::io::Result<()> {
    let dir = paths.dir();
    let snapshot_path = paths.snapshot_file();
    let payload_path = paths.toml_file();
    let metadata = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let payload = snapshot.config.clone().into_bytes();

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        // ponytail: two atomic renames, so racing writers can pair one's metadata with another's
        // payload; drift-sync self-heals it, tighten to a temp-dir swap if that ever bites.
        ocx_util::fs::write_bytes_atomic(&payload_path, &payload)?;
        ocx_util::fs::write_bytes_atomic(&snapshot_path, &metadata)?;
        Ok(())
    })
    .await
    .map_err(|join_error| std::io::Error::other(join_error.to_string()))?
}

/// Reads the two-file snapshot; any I/O or parse failure reads as absent.
pub async fn read_managed_config_snapshot(paths: &ManagedConfigPaths) -> Option<ManagedConfigSnapshot> {
    read_managed_config_snapshot_at(&paths.snapshot_file()).await
}

/// [`read_managed_config_snapshot`] from the `snapshot.json` path; a missing payload sibling
/// reads as absent, never as an empty `config`.
pub async fn read_managed_config_snapshot_at(path: &std::path::Path) -> Option<ManagedConfigSnapshot> {
    let metadata_bytes = tokio::fs::read(path).await.ok()?;
    let mut snapshot: ManagedConfigSnapshot = serde_json::from_slice(&metadata_bytes).ok()?;
    let payload_path = ManagedConfigPaths::toml_beside_snapshot(path);
    snapshot.config = tokio::fs::read_to_string(&payload_path).await.ok()?;
    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_config::test_support::{
        gzip_tar, seed_package, seed_package_multi_platform, stub_client_with_package,
    };
    use ocx_oci::Algorithm;
    use ocx_oci::client::Client;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

    fn identifier() -> OciIdentifier {
        OciIdentifier::parse_target("corp.example.com/ocx-config:user", ocx_oci::DEFAULT_REGISTRY).unwrap()
    }

    fn fetched(config_text: &str) -> FetchedManagedConfig {
        FetchedManagedConfig {
            manifest_digest: ocx_oci::Digest::Sha256("a".repeat(64)),
            config_text: config_text.to_string(),
        }
    }

    // ── fetch_managed_config (package wire shape) ────────────────────────────

    #[tokio::test]
    async fn fetch_selects_any_platform_entry_and_extracts_config() {
        let id = identifier();
        let (client, index_digest) = stub_client_with_package(&id, "[registry]\ndefault = \"corp\"\n");

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("well-formed package must fetch")
            .expect("existing tag must yield Some");
        assert_eq!(fetched.manifest_digest.to_string(), index_digest);
        assert_eq!(fetched.config_text, "[registry]\ndefault = \"corp\"\n");
    }

    /// A flat image manifest (no index) is accepted — single-platform push
    /// fallback (`--platform` dead-knob mitigation).
    #[tokio::test]
    async fn fetch_accepts_flat_image_manifest() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"flat\"\n")]);
        let layer_digest = Algorithm::Sha256.hash(&layer).to_string();
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": format!("sha256:{}", "0".repeat(64)),
                "size": 2
            },
            "layers": [{
                "mediaType": ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
                "digest": layer_digest,
                "size": layer.len(),
            }],
        })
        .to_string();
        let manifest_bytes = manifest_json.into_bytes();
        let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes).to_string();
        {
            let mut inner = stub_data.write();
            inner
                .manifests
                .insert(id.to_string(), (manifest_bytes, manifest_digest.clone()));
            inner.blobs.insert(layer_digest, layer);
        }
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("flat manifest must fetch")
            .expect("existing tag must yield Some");
        assert_eq!(fetched.manifest_digest.to_string(), manifest_digest);
        assert_eq!(fetched.config_text, "[registry]\ndefault = \"flat\"\n");
    }

    #[tokio::test]
    async fn fetch_absent_tag_returns_none() {
        let id = identifier();
        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let result = fetch_managed_config(&client, &id).await.expect("absent must not error");
        assert!(result.is_none());
    }

    /// A package pushed for concrete platforms only (no `any/any` entry) is
    /// rejected — never silently pick a platform-specific config.
    #[tokio::test]
    async fn fetch_concrete_platform_only_rejected() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"x = 1\n")]);
        seed_package(
            &stub_data,
            &id,
            layer,
            ("linux", "amd64"),
            None,
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let result = fetch_managed_config(&client, &id).await;
        assert!(
            matches!(result, Err(ManagedConfigFetchError::NoAnyPlatformEntry)),
            "concrete-platform-only package must be rejected, got {result:?}"
        );
    }

    /// An index carrying BOTH a concrete-platform entry (linux/amd64, listed
    /// first) and an `any/any` entry must resolve to the `any/any` child — the
    /// managed-config fetch is platform-agnostic and never picks a
    /// platform-specific config just because it appears earlier in the index.
    #[tokio::test]
    async fn fetch_selects_any_entry_over_preceding_concrete_platform() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let concrete_layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"amd64-specific\"\n")]);
        let any_layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"platform-agnostic\"\n")]);
        // linux/amd64 deliberately FIRST so a naive `.first()` would pick it.
        seed_package_multi_platform(
            &stub_data,
            &id,
            &[(("linux", "amd64"), concrete_layer), (("any", "any"), any_layer)],
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("multi-entry package must fetch")
            .expect("existing tag must yield Some");
        assert_eq!(
            fetched.config_text, "[registry]\ndefault = \"platform-agnostic\"\n",
            "the any/any entry must win even when a concrete platform precedes it"
        );
    }

    #[tokio::test]
    async fn fetch_oversize_declared_layer_rejected() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"x = 1\n")]);
        seed_package(
            &stub_data,
            &id,
            layer,
            ("any", "any"),
            Some((crate::managed_config::MAX_MANAGED_CONFIG_BYTES + 1) as i64),
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let result = fetch_managed_config(&client, &id).await;
        assert!(
            matches!(result, Err(ManagedConfigFetchError::LayerSizeExceeded { .. })),
            "oversize declared layer must be rejected, got {result:?}"
        );
    }

    /// S1 boundary (declared layer size): a layer whose manifest descriptor
    /// declares EXACTLY `MAX_MANAGED_CONFIG_BYTES` passes the `size <= maximum`
    /// gate (the real gzip bytes are tiny) and the package fetches cleanly.
    /// Its MAX+1 twin is `fetch_oversize_declared_layer_rejected` above.
    #[tokio::test]
    async fn fetch_accepts_declared_layer_size_at_capacity() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"at-cap\"\n")]);
        seed_package(
            &stub_data,
            &id,
            layer,
            ("any", "any"),
            Some(crate::managed_config::MAX_MANAGED_CONFIG_BYTES as i64),
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("a declared size exactly at the cap must be accepted")
            .expect("existing tag must yield Some");
        assert_eq!(fetched.config_text, "[registry]\ndefault = \"at-cap\"\n");
    }

    /// A registry serving different bytes than the declared layer digest fails
    /// digest re-verification (fetch-side in v2).
    #[tokio::test]
    async fn fetch_layer_digest_mismatch_rejected() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"x = 1\n")]);
        let layer_digest = Algorithm::Sha256.hash(&layer).to_string();
        seed_package(
            &stub_data,
            &id,
            layer,
            ("any", "any"),
            None,
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        // Tamper: swap the stored blob bytes under the declared digest.
        stub_data
            .write()
            .blobs
            .insert(layer_digest, gzip_tar(&[("config.toml", b"tampered = true\n")]));
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let result = fetch_managed_config(&client, &id).await;
        assert!(
            matches!(result, Err(ManagedConfigFetchError::LayerDigestMismatch { .. })),
            "tampered layer bytes must fail digest re-verification, got {result:?}"
        );
    }

    /// A package whose layer is not tar+gzip (e.g. tar+xz) is rejected.
    #[tokio::test]
    async fn fetch_non_gzip_layer_rejected() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"x = 1\n")]);
        seed_package(
            &stub_data,
            &id,
            layer,
            ("any", "any"),
            None,
            "application/vnd.oci.image.layer.v1.tar+xz",
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let result = fetch_managed_config(&client, &id).await;
        assert!(
            matches!(result, Err(ManagedConfigFetchError::NoGzipLayer)),
            "non-gzip layer must be rejected, got {result:?}"
        );
    }

    /// A package without a `config.toml` entry in its layer is rejected.
    #[tokio::test]
    async fn fetch_missing_config_toml_rejected() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("README.md", b"not a config\n")]);
        seed_package(
            &stub_data,
            &id,
            layer,
            ("any", "any"),
            None,
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let result = fetch_managed_config(&client, &id).await;
        assert!(
            matches!(result, Err(ManagedConfigFetchError::MissingConfigToml)),
            "missing config.toml must be rejected, got {result:?}"
        );
    }

    /// Extra entries in the layer archive are ignored — only `config.toml`
    /// is consumed.
    #[tokio::test]
    async fn fetch_ignores_extra_archive_entries() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[
            ("README.md", b"docs\n".as_slice()),
            ("config.toml", b"[registry]\ndefault = \"extras\"\n".as_slice()),
        ]);
        seed_package(
            &stub_data,
            &id,
            layer,
            ("any", "any"),
            None,
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("extras must not fail the fetch")
            .expect("existing tag must yield Some");
        assert_eq!(fetched.config_text, "[registry]\ndefault = \"extras\"\n");
    }

    /// Drift-identity invariant: the digest `fetch_managed_config` persists
    /// and the digest `probe_managed_config_digest` probes are the same value
    /// (the top-level/index digest) — the tick/`--check` drift comparison can
    /// never mismatch on digest source.
    #[tokio::test]
    async fn fetch_snapshot_digest_equals_probe_digest() {
        let id = identifier();
        let (client, _) = stub_client_with_package(&id, "[registry]\ndefault = \"corp\"\n");

        let fetched = fetch_managed_config(&client, &id).await.unwrap().unwrap();
        let probed = probe_managed_config_digest(&client, &id)
            .await
            .expect("probe must succeed")
            .expect("existing tag must yield Some");
        assert_eq!(
            fetched.manifest_digest, probed,
            "fetch and probe must agree on the drift-identity digest"
        );
    }

    /// S4: the drift-identity coherence invariant also holds for the flat
    /// (non-index) image-manifest shape — the digest `fetch_managed_config`
    /// carries equals the `probe_managed_config_digest` HEAD result, so a
    /// single-platform-push package never mismatches on the tick/`--check`
    /// drift comparison. Twin of `fetch_snapshot_digest_equals_probe_digest`
    /// for the flat wire shape.
    #[tokio::test]
    async fn fetch_flat_manifest_digest_equals_probe_digest() {
        let id = identifier();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"flat\"\n")]);
        let layer_digest = Algorithm::Sha256.hash(&layer).to_string();
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": format!("sha256:{}", "0".repeat(64)),
                "size": 2
            },
            "layers": [{
                "mediaType": ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
                "digest": layer_digest,
                "size": layer.len(),
            }],
        })
        .to_string();
        let manifest_bytes = manifest_json.into_bytes();
        let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes).to_string();
        {
            let mut inner = stub_data.write();
            inner
                .manifests
                .insert(id.to_string(), (manifest_bytes, manifest_digest.clone()));
            inner.blobs.insert(layer_digest, layer);
        }
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let fetched = fetch_managed_config(&client, &id).await.unwrap().unwrap();
        let probed = probe_managed_config_digest(&client, &id)
            .await
            .expect("probe must succeed")
            .expect("existing tag must yield Some");
        assert_eq!(fetched.manifest_digest.to_string(), manifest_digest);
        assert_eq!(
            fetched.manifest_digest, probed,
            "flat-manifest fetch and probe must agree on the drift-identity digest"
        );
    }

    // ── extract_config_toml (pure) ────────────────────────────────────────────

    #[test]
    fn extract_rejects_gzip_bomb() {
        // 1 MiB of zeros compresses to ~1 KiB — the declared/streamed caps
        // pass, the decompression cap must trip.
        let bomb_payload = vec![0u8; 1024 * 1024];
        let layer = gzip_tar(&[("config.toml", bomb_payload.as_slice())]);
        assert!(
            (layer.len() as u64) < crate::managed_config::MAX_MANAGED_CONFIG_BYTES,
            "the bomb must be small compressed for the test to be meaningful"
        );
        let result = extract_config_toml(&layer, crate::managed_config::MAX_MANAGED_CONFIG_BYTES);
        assert!(
            matches!(result, Err(ManagedConfigFetchError::ConfigEntryTooLarge { .. })),
            "a gzip bomb must trip the decompression cap, got {result:?}"
        );
    }

    #[test]
    fn extract_rejects_garbage_bytes() {
        let result = extract_config_toml(b"not gzip at all", 1024);
        assert!(
            matches!(
                result,
                Err(ManagedConfigFetchError::InvalidArchive { .. }) | Err(ManagedConfigFetchError::MissingConfigToml)
            ),
            "garbage bytes must not extract, got {result:?}"
        );
    }

    /// S2: a gzip stream truncated mid-body is not a readable archive — the tar
    /// scan surfaces [`ManagedConfigFetchError::InvalidArchive`] (a corrupt
    /// registry response), never a silent empty config.
    #[test]
    fn extract_rejects_truncated_gzip() {
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"corp\"\n")]);
        // Cut the gzip stream in half so decompression fails mid-scan.
        let truncated = &layer[..layer.len() / 2];
        let result = extract_config_toml(truncated, crate::managed_config::MAX_MANAGED_CONFIG_BYTES);
        assert!(
            matches!(result, Err(ManagedConfigFetchError::InvalidArchive { .. })),
            "a truncated gzip stream must fail as an invalid archive, got {result:?}"
        );
    }

    /// S3: a `config.toml` nested more than two path components deep
    /// (`deep/nested/config.toml`, 3 components) does NOT satisfy the
    /// `components().count() <= 2` gate — it is skipped like any other extra
    /// entry, so a package that buries its config that deep is treated as
    /// having none ([`ManagedConfigFetchError::MissingConfigToml`]).
    #[test]
    fn extract_skips_deeply_nested_config_toml() {
        let layer = gzip_tar(&[("deep/nested/config.toml", b"[registry]\ndefault = \"buried\"\n")]);
        let result = extract_config_toml(&layer, crate::managed_config::MAX_MANAGED_CONFIG_BYTES);
        assert!(
            matches!(result, Err(ManagedConfigFetchError::MissingConfigToml)),
            "a config.toml three components deep must be skipped, got {result:?}"
        );
    }

    /// A `config.toml` one directory deep (`subdir/config.toml`, 2 components)
    /// is still within the `<= 2` gate and IS consumed — pins the inclusive
    /// boundary the skip test above sits just past.
    #[test]
    fn extract_accepts_two_component_config_toml() {
        let layer = gzip_tar(&[("subdir/config.toml", b"[registry]\ndefault = \"nested-ok\"\n")]);
        let text = extract_config_toml(&layer, crate::managed_config::MAX_MANAGED_CONFIG_BYTES)
            .expect("a two-component config.toml is within the depth gate");
        assert_eq!(text, "[registry]\ndefault = \"nested-ok\"\n");
    }

    /// S1 (decompressed cap): a `config.toml` comfortably under the ceiling
    /// (accounting for the 512-byte tar header the whole-stream `take(cap + 1)`
    /// guard also counts) extracts cleanly.
    #[test]
    fn extract_accepts_config_toml_under_decompressed_cap() {
        let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;
        let body = "#".repeat((maximum - 1024) as usize);
        let layer = gzip_tar(&[("config.toml", body.as_bytes())]);
        let text = extract_config_toml(&layer, maximum).expect("a payload under the decompressed cap must extract");
        assert_eq!(text.len() as u64, maximum - 1024);
    }

    /// S1 boundary (decompressed cap): a `config.toml` of EXACTLY
    /// `MAX_MANAGED_CONFIG_BYTES` content bytes — the ceiling publish-side
    /// validation admits — round-trips byte-identical. The decompressed budget
    /// now accounts for the 512-byte tar header (`take(maximum + 512 + 1)`), so
    /// the header no longer eats into the content allowance and truncates it.
    #[test]
    fn extract_config_toml_at_maximum_bytes_round_trips_in_full() {
        let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;
        let body = "#".repeat(maximum as usize);
        let layer = gzip_tar(&[("config.toml", body.as_bytes())]);
        let text = extract_config_toml(&layer, maximum).expect("an exactly-maximum entry must extract in full");
        assert_eq!(
            text.len() as u64,
            maximum,
            "an exactly-maximum config must round-trip byte-identical, never truncate"
        );
    }

    /// One content byte past `maximum` is rejected cleanly by the entry-size
    /// gate (`entry.size() > maximum` → `ConfigEntryTooLarge`) BEFORE any
    /// content is read — never truncated, never silently accepted.
    #[test]
    fn extract_config_toml_over_maximum_bytes_is_rejected() {
        let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;
        let body = "#".repeat((maximum + 1) as usize);
        let layer = gzip_tar(&[("config.toml", body.as_bytes())]);
        let result = extract_config_toml(&layer, maximum);
        assert!(
            matches!(result, Err(ManagedConfigFetchError::ConfigEntryTooLarge { .. })),
            "a config one byte over the ceiling must be rejected, got {result:?}"
        );
    }

    // ── persist_managed_config ────────────────────────────────────────────────

    #[tokio::test]
    async fn persist_managed_config_round_trip_writes_atomic_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ManagedConfigPaths::new(dir.path());
        let toml = "[registry]\ndefault = \"corp.example.com\"\n";

        let snapshot = persist_managed_config(&paths, &identifier(), fetched(toml))
            .await
            .expect("valid fetch must persist");

        assert_eq!(snapshot.source, "corp.example.com/ocx-config:user");
        assert_eq!(snapshot.tag.as_deref(), Some("user"), "snapshot v2 carries the tag");
        assert_eq!(snapshot.config, toml);
        assert!(!snapshot.fetched_at.is_empty(), "fetched_at must be populated");

        // Metadata file is JSON WITHOUT the embedded payload (config lives in the sibling).
        let metadata = std::fs::read_to_string(paths.snapshot_file()).expect("snapshot metadata file must exist");
        assert!(
            !metadata.contains("[registry]"),
            "the metadata snapshot must not embed the config payload: {metadata}"
        );
        let parsed: ManagedConfigSnapshot = serde_json::from_str(&metadata).expect("metadata file must be valid JSON");
        assert_eq!(parsed.source, snapshot.source);
        assert_eq!(parsed.tag, snapshot.tag);
        assert_eq!(
            parsed.config, "",
            "config is #[serde(skip)] — absent from the metadata file"
        );

        // Payload lives in the readable sibling config.toml, byte-identical.
        let payload = std::fs::read_to_string(paths.toml_file()).expect("config.toml sibling must exist");
        assert_eq!(payload, toml, "the payload sibling holds the raw config bytes");

        // Full round-trip through the reader repopulates config from the sibling.
        let round_tripped = read_managed_config_snapshot(&paths)
            .await
            .expect("reader must load both files");
        assert_eq!(round_tripped.config, toml);
        assert_eq!(round_tripped.source, snapshot.source);
    }

    /// A metadata snapshot without a `tag` field stays readable — `tag` defaults
    /// to `None`; refreshed on the next drift sync, no migration. The payload
    /// sibling is present, isolating the missing-`tag` case from a missing payload.
    #[tokio::test]
    async fn read_snapshot_without_tag_field_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let metadata = format!(
            "{{\"source\":\"corp.example.com/ocx-config:user\",\"digest\":\"sha256:{}\",\"fetched_at\":\"old\"}}",
            "a".repeat(64)
        );
        std::fs::write(&path, &metadata).unwrap();
        std::fs::write(
            ManagedConfigPaths::toml_beside_snapshot(&path),
            "[registry]\ndefault = \"x\"\n",
        )
        .unwrap();

        let snapshot = read_managed_config_snapshot_at(&path)
            .await
            .expect("a snapshot without a tag field must stay readable");
        assert_eq!(snapshot.tag, None);
        assert_eq!(
            snapshot.config, "[registry]\ndefault = \"x\"\n",
            "config comes from the sibling"
        );
    }

    /// ADR Decision I (one-hop): a remote payload's `[managed]` section is
    /// stripped before persisting so it can never redirect the tier that
    /// fetched it.
    #[tokio::test]
    async fn persist_managed_config_strips_managed_section_from_payload() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ManagedConfigPaths::new(dir.path());
        let toml = "[registry]\ndefault = \"corp.example.com\"\n[managed]\nsource = \"hostile.test/other:v1\"\n";

        let snapshot = persist_managed_config(&paths, &identifier(), fetched(toml))
            .await
            .expect("valid fetch must persist");

        assert!(
            !snapshot.config.contains("[managed]"),
            "the [managed] section must be stripped from the persisted payload (ADR Decision I)"
        );
        assert!(
            snapshot.config.contains("corp.example.com"),
            "other sections survive the strip"
        );
    }

    /// A payload's `[update]` is dropped before the typed parse, whatever its shape, so a payload
    /// written for a newer ocx still persists on an older host.
    #[tokio::test]
    async fn persist_managed_config_drops_a_malformed_update_section() {
        // A bare key must precede the first table header, or TOML files it under that table.
        for toml in [
            "update = 1\n[registry]\ndefault = \"corp.example.com\"\n",
            "[registry]\ndefault = \"corp.example.com\"\n[update]\ninterval = []\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths = ManagedConfigPaths::new(dir.path());

            let snapshot = persist_managed_config(&paths, &identifier(), fetched(toml))
                .await
                .unwrap_or_else(|error| panic!("{toml:?}: {error}"));
            assert!(!snapshot.config.contains("update"), "{}", snapshot.config);
            assert!(snapshot.config.contains("corp.example.com"), "{}", snapshot.config);
        }
    }

    /// An invalid-TOML payload fails persist and leaves the existing snapshot
    /// byte-for-byte untouched (never a partial overwrite).
    #[tokio::test]
    async fn persist_managed_config_invalid_toml_leaves_snapshot_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ManagedConfigPaths::new(dir.path());

        std::fs::create_dir_all(paths.dir()).unwrap();
        let existing = format!(
            "{{\"source\":\"corp.example.com/ocx-config:user\",\"digest\":\"sha256:{}\",\"fetched_at\":\"old\",\"config\":\"[registry]\\ndefault = \\\"old\\\"\\n\"}}",
            "a".repeat(64)
        );
        std::fs::write(paths.snapshot_file(), &existing).unwrap();

        let result = persist_managed_config(&paths, &identifier(), fetched("not = [valid")).await;
        assert!(
            matches!(result, Err(ManagedConfigPersistError::InvalidToml { .. })),
            "invalid TOML must fail persist, got {result:?}"
        );

        let on_disk = std::fs::read_to_string(paths.snapshot_file()).unwrap();
        assert_eq!(
            on_disk, existing,
            "a failed persist must leave the existing snapshot byte-for-byte untouched"
        );
    }

    /// H1 (review r2): a payload whose `extra_ca_certs_pem` this host cannot
    /// load is refused at adoption — the previous snapshot stays byte-for-byte
    /// in force, the verdict names the managed tier and the block (never the
    /// bytes) and classifies 78 — while the same payload with a loadable
    /// bundle persists.
    ///
    /// Mutation: delete the `parse_pem` call in `persist_managed_config` —
    /// the red half persists the unusable bundle and this reds.
    #[tokio::test]
    async fn persist_managed_config_refuses_an_extra_ca_bundle_this_host_cannot_load() {
        use ocx_test_support::pki::{TestPki, pem_block};

        let dir = tempfile::tempdir().unwrap();
        let paths = ManagedConfigPaths::new(dir.path());
        std::fs::create_dir_all(paths.dir()).unwrap();
        let existing = format!(
            "{{\"source\":\"corp.example.com/ocx-config:user\",\"digest\":\"sha256:{}\",\"fetched_at\":\"old\"}}",
            "a".repeat(64)
        );
        std::fs::write(paths.snapshot_file(), &existing).unwrap();

        let unusable = format!(
            "extra_ca_certs_pem = '''\n{}'''\n",
            pem_block("CERTIFICATE", b"this is not DER at all")
        );
        let result = persist_managed_config(&paths, &identifier(), fetched(&unusable)).await;
        let Err(error @ ManagedConfigPersistError::ExtraCaCertsInvalid { .. }) = result else {
            panic!("an unusable extra_ca_certs_pem must be refused at adoption, got {result:?}");
        };
        let chain = ocx_test_support::pki::error_chain(&error);
        assert!(
            chain.contains("extra_ca_certs_pem (managed config)") && chain.contains("block 1 does not parse"),
            "the verdict names the tier and the block: {chain}"
        );
        assert!(!chain.contains("this is not DER"), "never the bytes (D-11): {chain}");
        assert_eq!(
            std::fs::read_to_string(paths.snapshot_file()).unwrap(),
            existing,
            "the previous snapshot stays in force"
        );

        let usable = format!("extra_ca_certs_pem = '''\n{}'''\n", TestPki::mint().root_pem());
        let snapshot = persist_managed_config(&paths, &identifier(), fetched(&usable))
            .await
            .expect("positive control: a loadable bundle persists");
        assert_eq!(snapshot.config, usable);
    }

    /// S2: the full fetch-then-persist path over a package whose `config.toml`
    /// is well-formed as an archive entry (extract succeeds) but is invalid
    /// TOML — persistence rejects it with
    /// [`ManagedConfigPersistError::InvalidToml`], and no snapshot is written.
    /// Fetch (archive extraction) and persist (TOML validation) are distinct
    /// legs: the payload survives the fetch but fails the persist.
    #[tokio::test]
    async fn fetch_then_persist_invalid_toml_config_rejected() {
        let id = identifier();
        let (client, _) = stub_client_with_package(&id, "not = [valid");
        let dir = tempfile::tempdir().unwrap();
        let paths = ManagedConfigPaths::new(dir.path());

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("archive extraction must succeed even for invalid-TOML content")
            .expect("existing tag must yield Some");
        assert_eq!(
            fetched.config_text, "not = [valid",
            "the raw text survives the fetch leg"
        );

        let result = persist_managed_config(&paths, &id, fetched).await;
        assert!(
            matches!(result, Err(ManagedConfigPersistError::InvalidToml { .. })),
            "invalid-TOML content must fail the persist leg, got {result:?}"
        );
        assert!(
            !paths.snapshot_file().exists(),
            "a persist that fails TOML validation must write no metadata snapshot"
        );
        assert!(
            !paths.toml_file().exists(),
            "a persist that fails TOML validation must write no payload sibling"
        );
    }

    /// Finding #6: a metadata file that is syntactically valid JSON but is
    /// missing a required field (here `digest`) must be treated as absent
    /// (`None`), exactly like syntactic corruption. This holds because
    /// [`ManagedConfigSnapshot`]'s required fields carry no `#[serde(default)]`
    /// (only the optional v2 `tag` — and the `#[serde(skip)]` payload `config`,
    /// loaded separately — do), so a missing required field fails
    /// deserialization and `read_managed_config_snapshot_at`'s `.ok()`
    /// collapses it to `None`. A payload sibling is present so this asserts the
    /// metadata-parse path, not the missing-payload path.
    #[tokio::test]
    async fn read_snapshot_valid_json_missing_required_field_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");

        // Valid JSON, but the required `digest` field is absent.
        let missing_field = "{\"source\":\"corp.example.com/ocx-config:user\",\"fetched_at\":\"2026-07-05T00:00:00Z\"}";
        std::fs::write(&path, missing_field).unwrap();
        std::fs::write(
            ManagedConfigPaths::toml_beside_snapshot(&path),
            "[registry]\ndefault = \"x\"\n",
        )
        .unwrap();

        let snapshot = read_managed_config_snapshot_at(&path).await;
        assert!(
            snapshot.is_none(),
            "a valid-JSON-but-missing-required-field metadata snapshot must be treated as absent, got {snapshot:?}"
        );
    }

    /// Split-file contract: valid metadata JSON but NO sibling `config.toml`
    /// (a torn/partial write, or a hand-deleted payload) reads as absent — the
    /// reader never folds an empty config.
    #[tokio::test]
    async fn read_snapshot_missing_payload_sibling_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let metadata = format!(
            "{{\"source\":\"corp.example.com/ocx-config:user\",\"digest\":\"sha256:{}\",\"fetched_at\":\"now\"}}",
            "a".repeat(64)
        );
        std::fs::write(&path, &metadata).unwrap();
        // No config.toml sibling written.

        assert!(
            read_managed_config_snapshot_at(&path).await.is_none(),
            "metadata without its payload sibling must read as absent"
        );
    }

    /// Syntactic-JSON corruption is also treated as absent — pins the existing
    /// contract alongside the missing-field case above.
    #[tokio::test]
    async fn read_snapshot_syntactic_corruption_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(&path, b"{not valid json").unwrap();

        assert!(
            read_managed_config_snapshot_at(&path).await.is_none(),
            "syntactically corrupt JSON must be treated as absent"
        );
    }

    /// Criterion 21: concurrent double-apply race — each file is written via its
    /// own temp+rename, so a racing writer can never leave a byte-interleaved
    /// (torn) metadata or payload file; every file on disk is exactly one
    /// writer's complete content. Mirrors `StateStore::touch_atomic_concurrent_safety`.
    /// (The cross-file pairing is not itself atomic — see `write_snapshot_atomic`'s
    /// note — so this asserts each file individually, not that the two came from
    /// the same writer.)
    #[tokio::test(flavor = "multi_thread")]
    async fn persist_managed_config_concurrent_writers_leave_one_consistent_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ManagedConfigPaths::new(dir.path());

        let tasks: Vec<_> = (0..10)
            .map(|i| {
                let paths = paths.clone();
                tokio::spawn(async move {
                    let toml = format!("[registry]\ndefault = \"writer-{i}\"\n");
                    persist_managed_config(&paths, &identifier(), fetched(&toml)).await
                })
            })
            .collect();

        for task in tasks {
            task.await
                .expect("task must not panic")
                .expect("every concurrent persist must succeed (each writes its own consistent bytes)");
        }

        // Metadata file: exactly one writer's complete, valid JSON.
        let metadata = std::fs::read_to_string(paths.snapshot_file()).unwrap();
        let parsed: ManagedConfigSnapshot =
            serde_json::from_str(&metadata).expect("concurrent writers must never leave a torn/corrupt metadata file");
        assert_eq!(parsed.source, "corp.example.com/ocx-config:user");

        // Payload file: exactly one writer's complete config, not a byte mix.
        let payload = std::fs::read_to_string(paths.toml_file()).unwrap();
        assert!(
            payload.contains("writer-"),
            "the payload must be exactly one writer's content, not a byte mix: {payload:?}"
        );
    }
}
