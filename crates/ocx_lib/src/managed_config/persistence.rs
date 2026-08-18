// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Network fetch + pure persistence primitives for the managed-config
//! package (managed-config v2 — config-as-package).
//!
//! | Function | Concerns | Testable |
//! |---|---|---|
//! | [`fetch_managed_config`] | Network: auth, OCI fetch, package-shape validation, capped layer pull | `StubTransport` unit tests |
//! | [`persist_managed_config`] | Pure: TOML parse + `[managed]` strip, atomic snapshot write | Unit-testable with synthetic text |
//!
//! ## Wire shape (v2)
//!
//! The managed config is an **ordinary ocx package** (published by
//! `ocx config push`): an image index whose `any/any` entry points at an
//! image manifest with a tar+gzip layer containing `config.toml`. A flat
//! image manifest (no index) is also accepted. The **top-level digest** (the
//! index digest, or the flat manifest's digest) is the tier's drift identity
//! — the same value [`probe_managed_config_digest`] returns.
//!
//! ## Size caps (CWE-400)
//!
//! Three independent 64 KiB ceilings: the layer's declared size, the streamed
//! blob bytes ([`crate::oci::client::Client::fetch_layer_blob_capped`]), and
//! the decompressed tar stream (`take(cap + 1)`).
//!
//! `fetch_managed_config` runs on a client whose `MirrorMap` derives from the
//! **local-only** mirror view (ADR "Mirror posture" — the managed payload is
//! excluded from its own fetch route, so it can never redirect or hijack its
//! own refresh). Callers (setup phase 1.5, `ocx config update`, the background
//! tick) are responsible for building that client.

use crate::config::managed::ManagedConfigSnapshot;
use crate::oci::{Digest, Identifier};

// ── Fetched payload (intermediate transfer object) ────────────────────────────

/// Result of a successful [`fetch_managed_config`] before persistence.
#[derive(Debug)]
pub struct FetchedManagedConfig {
    /// The top-level manifest digest (image-index digest, or the flat image
    /// manifest's digest) — the tier's drift identity.
    pub manifest_digest: Digest,
    /// The extracted `config.toml` text (digest-verified, size-capped, not
    /// yet TOML-validated — that happens in [`persist_managed_config`]).
    pub config_text: String,
}

// ── Fetch errors ──────────────────────────────────────────────────────────────

/// Errors raised while fetching the managed-config package from the registry.
///
/// Every shape variant means the package at the source is not a consumable
/// managed-config package (→ `DataError` 65); [`Self::FetchFailed`] preserves
/// the network/auth cause and delegates classification to it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedConfigFetchError {
    /// A network error from the OCI client, preserving the full
    /// [`crate::oci::client::error::ClientError`] source chain.
    #[error("failed to fetch managed config from registry")]
    FetchFailed {
        /// The underlying OCI client error.
        #[source]
        source: crate::oci::client::error::ClientError,
    },

    /// The manifest chain had an unexpected shape (e.g. an index entry that
    /// resolves to another index, or a child manifest that vanished).
    #[error("unexpected manifest shape for managed config: {detail}")]
    UnexpectedManifest {
        /// Human-readable detail about the shape mismatch.
        detail: String,
    },

    /// The image index has no platform-agnostic `any/any` entry — the package
    /// was pushed for concrete platforms only and cannot serve as a managed
    /// config (publish with the default `--platform any/any`).
    #[error("managed config package has no any/any platform entry")]
    NoAnyPlatformEntry,

    /// The selected image manifest has no tar+gzip layer.
    #[error("managed config package has no tar+gzip layer")]
    NoGzipLayer,

    /// The package's layer archive contains no `config.toml` entry.
    #[error("managed config package layer contains no config.toml")]
    MissingConfigToml,

    /// The declared layer size exceeded
    /// [`crate::managed_config::MAX_MANAGED_CONFIG_BYTES`].
    #[error("managed config layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    LayerSizeExceeded {
        /// The size declared in the manifest layer descriptor.
        declared: i64,
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The SHA-256 digest of the fetched layer bytes does not match the digest
    /// declared in the manifest.
    #[error("managed config layer digest mismatch: declared '{declared}', computed '{computed}'")]
    LayerDigestMismatch {
        /// The digest declared in the manifest descriptor.
        declared: String,
        /// The digest actually computed from the fetched bytes.
        computed: String,
    },

    /// The `config.toml` entry (or the decompressed archive stream) exceeds
    /// [`crate::managed_config::MAX_MANAGED_CONFIG_BYTES`] — gzip-bomb guard.
    #[error("managed config config.toml entry exceeds the maximum allowed {maximum} bytes")]
    ConfigEntryTooLarge {
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The layer bytes are not a readable gzip'd tar archive (or the
    /// `config.toml` entry is not valid UTF-8).
    #[error("managed config layer is not a readable archive: {detail}")]
    InvalidArchive {
        /// Human-readable detail about the archive failure.
        detail: String,
    },
}

impl crate::cli::ClassifyExitCode for ManagedConfigFetchError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // Network/auth failures delegate to the inner OCI client error's
            // classification (Unavailable 69 / AuthError 80 / etc.).
            Self::FetchFailed { source } => source.classify(),
            // Shape mismatches are malformed registry data.
            Self::UnexpectedManifest { .. }
            | Self::NoAnyPlatformEntry
            | Self::NoGzipLayer
            | Self::MissingConfigToml
            | Self::LayerSizeExceeded { .. }
            | Self::LayerDigestMismatch { .. }
            | Self::ConfigEntryTooLarge { .. }
            | Self::InvalidArchive { .. } => Some(crate::cli::ExitCode::DataError),
        }
    }
}

// ── Persist errors ────────────────────────────────────────────────────────────

/// Errors raised while persisting a fetched managed-config payload to the
/// two-file snapshot (metadata `snapshot.json` + payload `config.toml`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedConfigPersistError {
    /// The payload text is not valid TOML.
    #[error("managed config payload is not valid TOML")]
    InvalidToml {
        /// The underlying TOML parse failure.
        #[source]
        source: toml::de::Error,
    },

    /// Writing the atomic snapshot file failed.
    #[error("failed to write managed config snapshot")]
    SnapshotWriteFailed {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

impl crate::cli::ClassifyExitCode for ManagedConfigPersistError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            Self::InvalidToml { .. } => Some(crate::cli::ExitCode::DataError),
            Self::SnapshotWriteFailed { .. } => Some(crate::cli::ExitCode::IoError),
        }
    }
}

// ── Combined update error ─────────────────────────────────────────────────────

/// Combined error for a full fetch-then-persist update cycle
/// (`PackageManager::update_managed_config`, `ocx config update`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedConfigUpdateError {
    /// The fetch step failed.
    #[error("failed to fetch managed config")]
    Fetch(#[from] ManagedConfigFetchError),
    /// The persist step failed.
    #[error("failed to persist managed config")]
    Persist(#[from] ManagedConfigPersistError),
    /// The resolved source has no manifest in the registry — the ref is
    /// genuinely absent, not a network or auth fault (`fetch_managed_config`
    /// returns `Ok(None)` for this case). Surfaced as a first-class error so
    /// `ocx config update` exits nonzero instead of a false
    /// `ManagedConfigUpdateResult::NotConfigured` success, and so `ocx self
    /// setup --managed-config` never falls through to its "always resolved"
    /// `unreachable!()` arm.
    #[error("managed config source '{effective_source}' not found in registry")]
    SourceNotFound {
        /// The resolved source that produced no manifest.
        effective_source: crate::oci::Identifier,
    },
    /// A `tag@digest` version pin was specified but the tag resolved to a
    /// different digest (fail-closed immutability assertion — mirrors `ocx
    /// self setup`'s `PinDigestMismatch`). Nothing was persisted.
    #[error("managed config pin digest mismatch: expected '{expected}' but the tag resolved to '{fetched}'")]
    PinDigestMismatch {
        /// The digest pinned in the VERSION argument.
        expected: crate::oci::Digest,
        /// The digest the tag actually resolved to.
        fetched: crate::oci::Digest,
    },
}

impl crate::cli::ClassifyExitCode for ManagedConfigUpdateError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            Self::Fetch(source) => source.classify(),
            Self::Persist(source) => source.classify(),
            Self::SourceNotFound { .. } => Some(crate::cli::ExitCode::NotFound),
            Self::PinDigestMismatch { .. } => Some(crate::cli::ExitCode::DataError),
        }
    }
}

// ── Network primitive ─────────────────────────────────────────────────────────

/// Fetches the managed-config package for `identifier` from the OCI registry
/// and extracts its `config.toml` payload.
///
/// `client` MUST be built from the **local-only** mirror view (ADR "Mirror
/// posture") — the managed payload's own `[mirrors]`/`[managed]` content must
/// never influence the route used to fetch it (no-cycle, no self-brick).
///
/// Flow (see the module doc for the wire shape):
///
/// 1. fetch the top-level manifest; its digest is the drift identity,
/// 2. image index → select the `any/any` entry and fetch its image manifest
///    (a flat image manifest is accepted directly),
/// 3. select the tar+gzip layer, enforce the declared-size cap, stream the
///    blob with a byte cap, re-verify its digest,
/// 4. decompress (capped) and scan the tar for `config.toml` — extra archive
///    entries are ignored.
///
/// Returns `Ok(None)` when the reference does not exist (ref genuinely
/// absent); returns `Err` on any network/auth/shape failure.
///
/// # Errors
///
/// See [`ManagedConfigFetchError`] variants.
pub async fn fetch_managed_config(
    client: &crate::oci::client::Client,
    identifier: &Identifier,
) -> Result<Option<FetchedManagedConfig>, ManagedConfigFetchError> {
    let maximum = crate::managed_config::MAX_MANAGED_CONFIG_BYTES;

    let Some((_, top_digest, top_manifest)) = client
        .fetch_manifest_raw_bytes(identifier)
        .await
        .map_err(|source| ManagedConfigFetchError::FetchFailed { source })?
    else {
        return Ok(None);
    };

    // Resolve the image manifest carrying the layer: index → any/any entry;
    // flat image manifest accepted as-is (single-platform push fallback).
    let image_manifest = match top_manifest {
        crate::oci::Manifest::Image(image) => image,
        crate::oci::Manifest::ImageIndex(index) => {
            let entry = index
                .manifests
                .iter()
                .find(|entry| match &entry.platform {
                    // No platform on an index entry = platform-agnostic
                    // (matches `Index::fetch_candidates`' convention).
                    None => true,
                    Some(platform) => crate::oci::Platform::try_from(platform.clone()).is_ok_and(|p| p.is_any()),
                })
                .ok_or(ManagedConfigFetchError::NoAnyPlatformEntry)?;
            let child_digest =
                Digest::try_from(entry.digest.as_str()).map_err(|e| ManagedConfigFetchError::UnexpectedManifest {
                    detail: format!("index entry digest '{}' is malformed: {e}", entry.digest),
                })?;
            // Content-addressed child fetch: tag dropped, digest pins.
            let child_identifier = identifier.without_specifiers().clone_with_digest(child_digest);
            let child = client
                .fetch_manifest_raw_bytes(&child_identifier)
                .await
                .map_err(|source| ManagedConfigFetchError::FetchFailed { source })?
                .ok_or_else(|| ManagedConfigFetchError::UnexpectedManifest {
                    detail: format!("index entry '{child_identifier}' vanished before the child fetch"),
                })?;
            match child.2 {
                crate::oci::Manifest::Image(image) => image,
                crate::oci::Manifest::ImageIndex(_) => {
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
        .find(|layer| layer.media_type == crate::media_type::MEDIA_TYPE_TAR_GZ)
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

    // Digest re-verify before touching the bytes (BlobStore's "verified
    // digest == sha256(bytes)" contract, moved into the fetch leg in v2).
    let computed = crate::oci::Algorithm::Sha256.hash(&layer_bytes);
    if computed != layer_digest {
        return Err(ManagedConfigFetchError::LayerDigestMismatch {
            declared: layer_digest.to_string(),
            computed: computed.to_string(),
        });
    }

    // Cap 3: decompressed stream + tar scan (pure, unit-tested).
    let config_text = extract_config_toml(&layer_bytes, maximum)?;

    Ok(Some(FetchedManagedConfig {
        manifest_digest: top_digest,
        config_text,
    }))
}

/// Scans a gzip'd tar archive for the `config.toml` entry and returns its
/// text. Pure function over the compressed bytes.
///
/// The decompressed stream is capped at `maximum + 1` bytes (gzip-bomb guard)
/// — an archive whose scan would decompress more than `maximum` bytes before
/// `config.toml` is found fails with [`ManagedConfigFetchError::ConfigEntryTooLarge`].
/// Extra entries besides `config.toml` are ignored.
// ponytail: tar+gzip only — the one shape `ocx config push` produces. Other
// compressions (xz/zst) would need archive-backend dispatch; add if a real
// operator payload ever needs them.
fn extract_config_toml(compressed: &[u8], maximum: u64) -> Result<String, ManagedConfigFetchError> {
    use std::io::Read as _;

    let invalid = |detail: String| ManagedConfigFetchError::InvalidArchive { detail };

    let decoder = flate2::read::GzDecoder::new(compressed);
    // Budget = content ceiling + one 512-byte tar header block (+1 sentinel).
    // Without the header allowance a config of exactly `maximum` content bytes —
    // admitted by publish-side validation and by the entry-size gate below —
    // truncates: the header eats 512 of the budget and tar reports the premature
    // EOF as a clean Ok(0). Still a decompression-bomb backstop (ceiling is
    // `maximum` + one block, not unbounded); the entry-size gate rejects any
    // entry larger than `maximum` before its content is read.
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
                // A read failure mid-scan is either a corrupt archive or the
                // decompression cap tripping — the cap is the security-relevant
                // reading, so report it when the stream was plausibly truncated.
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
            // The capped reader ran dry mid-entry → the archive tried to
            // decompress past the ceiling.
            std::io::ErrorKind::UnexpectedEof => ManagedConfigFetchError::ConfigEntryTooLarge { maximum },
            _ => invalid(format!("config.toml entry unreadable: {e}")),
        })?;
        return Ok(text);
    }
    Err(ManagedConfigFetchError::MissingConfigToml)
}

/// Probes only the managed-config package's current top-level manifest digest
/// for `identifier`, WITHOUT downloading any manifest body or layer.
///
/// Wraps [`crate::oci::client::Client::probe_manifest_digest`] (transport
/// HEAD) and re-maps its error onto [`ManagedConfigFetchError`]. The digest
/// returned is the same value [`fetch_managed_config`] carries in
/// [`FetchedManagedConfig::manifest_digest`], so it compares directly against
/// a persisted [`ManagedConfigSnapshot::digest`].
///
/// Returns `Ok(None)` when the reference does not exist. Used by the
/// digest-only refresh paths (`notify`/`manual` background tick and
/// `ocx config update --check`) so drift detection never pulls content.
///
/// # Errors
///
/// See [`ManagedConfigFetchError::FetchFailed`].
pub async fn probe_managed_config_digest(
    client: &crate::oci::client::Client,
    identifier: &Identifier,
) -> Result<Option<Digest>, ManagedConfigFetchError> {
    client
        .probe_manifest_digest(identifier)
        .await
        .map_err(|source| ManagedConfigFetchError::FetchFailed { source })
}

// ── Pure persistence primitive ────────────────────────────────────────────────

/// Parses the fetched payload as TOML, strips any `[managed]` section (WARN if
/// present — ADR Decision I, a remote payload can never redirect the tier that
/// fetched it), and writes the resulting [`ManagedConfigSnapshot`] as two
/// sibling files under
/// [`crate::file_structure::StateStore::managed_config_dir`]: the readable
/// `config.toml` payload and the `snapshot.json` metadata (each via its own
/// atomic temp+rename).
///
/// `source` is the effective managed-config source; its canonical `Display`
/// form is carried into [`ManagedConfigSnapshot::source`] (identity gate) and
/// its tag into [`ManagedConfigSnapshot::tag`] (snapshot v2 bookkeeping).
/// Digest verification happens in [`fetch_managed_config`] — by the time a
/// [`FetchedManagedConfig`] exists, its text is digest-verified.
///
/// # Errors
///
/// See [`ManagedConfigPersistError`] variants.
pub async fn persist_managed_config(
    state: &crate::file_structure::StateStore,
    source: &Identifier,
    fetched: FetchedManagedConfig,
) -> Result<ManagedConfigSnapshot, ManagedConfigPersistError> {
    let text = fetched.config_text;

    // Validate the payload matches the Config schema; the strip below operates
    // on the raw TOML text so the persisted string never carries a literal
    // `[managed]` section (ADR Decision I — the loader's own strip is a second,
    // redundant line of defense against a snapshot.json written by another
    // path).
    let parsed: crate::config::Config =
        toml::from_str(&text).map_err(|source| ManagedConfigPersistError::InvalidToml { source })?;

    let config = if parsed.managed.is_some() {
        crate::log::warn!(
            "managed-config payload for '{source}' contained a [managed] section; stripped before persisting \
             (a remote payload can never redirect the tier that fetched it)"
        );
        let mut value: toml::Value =
            toml::from_str(&text).map_err(|source| ManagedConfigPersistError::InvalidToml { source })?;
        if let Some(table) = value.as_table_mut() {
            table.remove("managed");
        }
        toml::to_string(&value).expect("re-serializing a toml::Value with one key removed cannot fail")
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

    write_snapshot_atomic(state, &snapshot)
        .await
        .map_err(|source| ManagedConfigPersistError::SnapshotWriteFailed { source })?;

    Ok(snapshot)
}

/// Writes `snapshot` as two sibling files under
/// [`crate::file_structure::StateStore::managed_config_dir`]: the raw
/// `config.toml` payload and the `snapshot.json` metadata, each via its own
/// atomic temp+rename.
///
/// Write order is payload-first, metadata-last. `snapshot.json` is the commit
/// marker: by the time a reader observes it, the `config.toml` written before
/// it is already in place, so the reader never sees metadata pointing at a
/// missing payload. A crash between the two writes leaves the metadata absent —
/// the whole snapshot then reads as absent (benign-state rule; the next drift
/// sync re-persists it).
async fn write_snapshot_atomic(
    state: &crate::file_structure::StateStore,
    snapshot: &ManagedConfigSnapshot,
) -> std::io::Result<()> {
    let dir = state.managed_config_dir();
    let snapshot_path = state.managed_config_snapshot_file();
    let payload_path = state.managed_config_toml_file();
    let metadata = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let payload = snapshot.config.clone().into_bytes();

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        // ponytail: two atomic renames, not one — each file is torn-write-safe
        // on its own, but the cross-file pairing is not itself atomic, so racing
        // writers can leave metadata from one and payload from another. Both are
        // complete, valid files and drift-sync self-heals on the next tick;
        // tighten to a temp-dir swap only if a real pairing hazard shows up.
        crate::utility::fs::write_bytes_atomic(&payload_path, &payload)?;
        crate::utility::fs::write_bytes_atomic(&snapshot_path, &metadata)?;
        Ok(())
    })
    .await
    .map_err(|join_error| std::io::Error::other(join_error.to_string()))?
}

/// Reads and parses the two-file snapshot from `state`'s well-known paths,
/// treating any I/O or parse failure as absent (benign-state rule — a missing
/// or corrupt snapshot is never a hard error at read time).
pub async fn read_managed_config_snapshot(state: &crate::file_structure::StateStore) -> Option<ManagedConfigSnapshot> {
    read_managed_config_snapshot_at(&state.managed_config_snapshot_file()).await
}

/// Path-based variant of [`read_managed_config_snapshot`], for callers (the
/// config loader) that must not construct a
/// [`crate::file_structure::StateStore`] or [`crate::file_structure::FileStructure`].
///
/// `path` is the metadata `snapshot.json`; the payload is loaded from the
/// sibling `config.toml` derived by
/// [`crate::file_structure::StateStore::managed_config_toml_path_for_snapshot`].
/// A missing or unreadable payload sibling makes the whole snapshot read as
/// absent — the reader never yields a snapshot with an empty `config`.
pub async fn read_managed_config_snapshot_at(path: &std::path::Path) -> Option<ManagedConfigSnapshot> {
    let metadata_bytes = tokio::fs::read(path).await.ok()?;
    let mut snapshot: ManagedConfigSnapshot = serde_json::from_slice(&metadata_bytes).ok()?;
    let payload_path = crate::file_structure::StateStore::managed_config_toml_path_for_snapshot(path);
    snapshot.config = tokio::fs::read_to_string(&payload_path).await.ok()?;
    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_structure::StateStore;
    use crate::managed_config::test_support::{
        gzip_tar, seed_package, seed_package_multi_platform, stub_client_with_package,
    };
    use crate::oci::Algorithm;
    use crate::oci::client::Client;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};

    fn identifier() -> Identifier {
        Identifier::parse("corp.example.com/ocx-config:user").unwrap()
    }

    fn fetched(config_text: &str) -> FetchedManagedConfig {
        FetchedManagedConfig {
            manifest_digest: crate::oci::Digest::Sha256("a".repeat(64)),
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
                "mediaType": crate::media_type::MEDIA_TYPE_TAR_GZ,
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
            crate::media_type::MEDIA_TYPE_TAR_GZ,
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
            crate::media_type::MEDIA_TYPE_TAR_GZ,
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
            crate::media_type::MEDIA_TYPE_TAR_GZ,
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
            crate::media_type::MEDIA_TYPE_TAR_GZ,
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
            crate::media_type::MEDIA_TYPE_TAR_GZ,
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
            crate::media_type::MEDIA_TYPE_TAR_GZ,
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
                "mediaType": crate::media_type::MEDIA_TYPE_TAR_GZ,
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
        let state = StateStore::new(dir.path());
        let toml = "[registry]\ndefault = \"corp.example.com\"\n";

        let snapshot = persist_managed_config(&state, &identifier(), fetched(toml))
            .await
            .expect("valid fetch must persist");

        assert_eq!(snapshot.source, "corp.example.com/ocx-config:user");
        assert_eq!(snapshot.tag.as_deref(), Some("user"), "snapshot v2 carries the tag");
        assert_eq!(snapshot.config, toml);
        assert!(!snapshot.fetched_at.is_empty(), "fetched_at must be populated");

        // Metadata file is JSON WITHOUT the embedded payload (config lives in the sibling).
        let metadata =
            std::fs::read_to_string(state.managed_config_snapshot_file()).expect("snapshot metadata file must exist");
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
        let payload =
            std::fs::read_to_string(state.managed_config_toml_file()).expect("config.toml sibling must exist");
        assert_eq!(payload, toml, "the payload sibling holds the raw config bytes");

        // Full round-trip through the reader repopulates config from the sibling.
        let round_tripped = read_managed_config_snapshot(&state)
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
            StateStore::managed_config_toml_path_for_snapshot(&path),
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
        let state = StateStore::new(dir.path());
        let toml = "[registry]\ndefault = \"corp.example.com\"\n[managed]\nsource = \"hostile.test/other:v1\"\n";

        let snapshot = persist_managed_config(&state, &identifier(), fetched(toml))
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

    /// An invalid-TOML payload fails persist and leaves the existing snapshot
    /// byte-for-byte untouched (never a partial overwrite).
    #[tokio::test]
    async fn persist_managed_config_invalid_toml_leaves_snapshot_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateStore::new(dir.path());

        std::fs::create_dir_all(state.managed_config_dir()).unwrap();
        let existing = format!(
            "{{\"source\":\"corp.example.com/ocx-config:user\",\"digest\":\"sha256:{}\",\"fetched_at\":\"old\",\"config\":\"[registry]\\ndefault = \\\"old\\\"\\n\"}}",
            "a".repeat(64)
        );
        std::fs::write(state.managed_config_snapshot_file(), &existing).unwrap();

        let result = persist_managed_config(&state, &identifier(), fetched("not = [valid")).await;
        assert!(
            matches!(result, Err(ManagedConfigPersistError::InvalidToml { .. })),
            "invalid TOML must fail persist, got {result:?}"
        );

        let on_disk = std::fs::read_to_string(state.managed_config_snapshot_file()).unwrap();
        assert_eq!(
            on_disk, existing,
            "a failed persist must leave the existing snapshot byte-for-byte untouched"
        );
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
        let state = StateStore::new(dir.path());

        let fetched = fetch_managed_config(&client, &id)
            .await
            .expect("archive extraction must succeed even for invalid-TOML content")
            .expect("existing tag must yield Some");
        assert_eq!(
            fetched.config_text, "not = [valid",
            "the raw text survives the fetch leg"
        );

        let result = persist_managed_config(&state, &id, fetched).await;
        assert!(
            matches!(result, Err(ManagedConfigPersistError::InvalidToml { .. })),
            "invalid-TOML content must fail the persist leg, got {result:?}"
        );
        assert!(
            !state.managed_config_snapshot_file().exists(),
            "a persist that fails TOML validation must write no metadata snapshot"
        );
        assert!(
            !state.managed_config_toml_file().exists(),
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
            StateStore::managed_config_toml_path_for_snapshot(&path),
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
        let state = StateStore::new(dir.path());

        let tasks: Vec<_> = (0..10)
            .map(|i| {
                let state = state.clone();
                tokio::spawn(async move {
                    let toml = format!("[registry]\ndefault = \"writer-{i}\"\n");
                    persist_managed_config(&state, &identifier(), fetched(&toml)).await
                })
            })
            .collect();

        for task in tasks {
            task.await
                .expect("task must not panic")
                .expect("every concurrent persist must succeed (each writes its own consistent bytes)");
        }

        // Metadata file: exactly one writer's complete, valid JSON.
        let metadata = std::fs::read_to_string(state.managed_config_snapshot_file()).unwrap();
        let parsed: ManagedConfigSnapshot =
            serde_json::from_str(&metadata).expect("concurrent writers must never leave a torn/corrupt metadata file");
        assert_eq!(parsed.source, "corp.example.com/ocx-config:user");

        // Payload file: exactly one writer's complete config, not a byte mix.
        let payload = std::fs::read_to_string(state.managed_config_toml_file()).unwrap();
        assert!(
            payload.contains("writer-"),
            "the payload must be exactly one writer's content, not a byte mix: {payload:?}"
        );
    }
}
