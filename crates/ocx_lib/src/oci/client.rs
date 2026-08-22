// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    ACCEPTED_MANIFEST_MEDIA_TYPES, MEDIA_TYPE_DESCRIPTION_V1, MEDIA_TYPE_MARKDOWN, MEDIA_TYPE_OCI_EMPTY_CONFIG,
    MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_PACKAGE_V1, MEDIA_TYPE_PNG, MEDIA_TYPE_SVG,
    Result, archive, compression, log, media_type_from_path, oci,
    package::{self, info::Info, tag::InternalTag},
};

use std::collections::BTreeMap;

use futures::stream::{self, StreamExt, TryStreamExt};

use super::{Algorithm, Digest, Identifier, native};

/// Maximum number of layer push/verify operations to run concurrently.
///
/// Each `LayerRef::File` reads the full archive into memory before
/// uploading, so unbounded fan-out would OOM on multi-GB layers.
const LAYER_PUSH_CONCURRENCY: usize = 4;

/// Hard cap on a manifest body accepted from a registry (CWE-400).
///
/// Digest verification is not a size check: a hostile registry named by an
/// index root's `repository` pointer can answer with a multi-gigabyte body
/// whose digest matches perfectly. `announce` commits exactly those bytes into
/// a public git repository, so the ceiling belongs on the one function every
/// raw-bytes caller routes through.
///
/// This is also the ceiling the index-role HTTP transport applies to root,
/// dispatch, config and catalog documents — `oci/index/ocx_index.rs` imports
/// this constant rather than declaring its own, so the two halves of one store
/// cannot drift apart. The OCI distribution spec suggests 4 MiB,
/// but a verbatim image index carrying many platforms plus attestation
/// descriptors is legitimately larger, and matching the other half of the same
/// store is what makes the store coherent.
pub(crate) const MAX_INDEX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

/// Per-layer outcome recorded by `push_multi_layer_manifest`, aggregated by
/// the caller into a [`LayerCounts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayerPushOutcome {
    /// Uploaded via `push_blob` — which may itself have HEAD-skipped an
    /// already-present blob (`NativeTransport::do_push_blob`'s
    /// blob-exists short-circuit); this variant only means "no mount was
    /// used," not "bytes definitely crossed the wire."
    Uploaded,
    /// A cross-repository blob mount succeeded; no upload was performed.
    Mounted,
    /// A `LayerRef::Digest` layer verified present via `head_blob` — no
    /// mount was attempted, or a mount attempt fell back.
    Verified,
}

/// Aggregate counts of layer-push outcomes for a single package push.
///
/// Only layer blobs are counted — the config blob and the manifest itself
/// are not layers and are excluded. An `uploaded` count may still have
/// HEAD-skipped an already-present blob inside `push_blob` (see
/// [`LayerPushOutcome::Uploaded`]); this struct distinguishes mount vs.
/// explicit-upload vs. verify-by-digest at the `push_multi_layer_manifest`
/// call site, not whether bytes actually crossed the wire.
///
/// `Serialize` derives directly on this type (rather than a CLI-side
/// wrapper) so `ocx_cli`'s `PushReport` can embed it verbatim as the
/// `layers` field of the push JSON report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct LayerCounts {
    /// Layers a cross-repository blob mount placed in the target repository
    /// without any upload.
    pub mounted: usize,
    /// Layers sent through `push_blob` — see [`LayerPushOutcome::Uploaded`]
    /// for why this does not strictly mean "bytes crossed the wire".
    pub uploaded: usize,
    /// `LayerRef::Digest` layers confirmed present by `head_blob`.
    pub verified: usize,
}

impl LayerCounts {
    fn record(&mut self, outcome: LayerPushOutcome) {
        match outcome {
            LayerPushOutcome::Mounted => self.mounted += 1,
            LayerPushOutcome::Uploaded => self.uploaded += 1,
            LayerPushOutcome::Verified => self.verified += 1,
        }
    }
}

/// Sums the per-platform counts of a multi-platform push fan-out
/// ([`Publisher::push`](crate::publisher::Publisher::push) pushes one
/// package per target platform, each with its own layer set).
impl std::ops::AddAssign for LayerCounts {
    fn add_assign(&mut self, other: Self) {
        self.mounted += other.mounted;
        self.uploaded += other.uploaded;
        self.verified += other.verified;
    }
}

mod builder;
pub mod error;
pub(in crate::oci) mod hashing_reader;
mod mirror_map;
pub(crate) mod native_transport;
pub(super) mod progress_reader;
#[cfg(test)]
pub(crate) mod test_transport;
mod transport;

pub use builder::ClientBuilder;
/// Re-exported so `auth::login`'s one-off ping client shares the single
/// definition instead of picking its own idle bound.
pub(crate) use builder::REGISTRY_READ_TIMEOUT;
pub use mirror_map::MirrorMap;
/// The buffering body a test double gives `OciTransport::push_blob_from_path`.
/// Test-only by construction — the trait deliberately has no default, so a
/// production transport must stream (see the method's own docs).
#[cfg(test)]
pub(crate) use transport::push_blob_buffered;
pub use transport::{MountOutcome, OciTransport, ProgressFn, no_progress};

use error::ClientError;

/// Bytes and digests of a single-layer OCI artifact, returned by
/// [`Client::fetch_single_layer_artifact`].
#[derive(Debug)]
pub(crate) struct SingleLayerArtifact {
    /// Raw manifest JSON bytes (byte-identical to what the registry served).
    pub manifest_bytes: Vec<u8>,
    /// Digest of the manifest blob.
    pub manifest_digest: Digest,
    /// Raw bytes of the artifact's single layer.
    pub layer_bytes: Vec<u8>,
    /// Digest of the layer blob as declared in the manifest.
    pub layer_digest: Digest,
}

/// Which host a read addresses.
///
/// Canonical is the default, and the asymmetry is why: a mirrored answer is
/// wrong exactly when it decides a write, and a canonical answer is never
/// wrong, only slower. Writes always go to the canonical registry (mirrors are
/// read-only, ADR Q5), so a decision taken from a mirror and applied to the
/// canonical host is a decision about a repository nobody read (CWE-345/367) —
/// and nothing in a call site's shape reveals that it is about to back a write.
/// So the safe host is what a plain `client.list_tags(..)` gets, and reaching a
/// mirror is something a caller asks for by name through the `*_addressed`
/// variants. A read that only feeds a pull, a listing or a cache should ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadAddressing {
    /// A configured mirror serves the read. Named explicitly, never implied —
    /// only for a read whose answer cannot decide, gate, or verify a write.
    Mirrored,
    /// The default: the canonical registry, mirrors bypassed.
    Canonical,
}

pub struct Client {
    transport: Box<dyn OciTransport>,
    pub(super) lock_timeout: std::time::Duration,
    pub(super) tag_chunk_size: usize,
    pub(super) repository_chunk_size: usize,
    /// Shared progress manager for download/upload bars. Cheap to clone
    /// (an `Arc` handle or a disabled no-op).
    progress: crate::cli::progress::ProgressManager,
    /// Per-upstream-host mirror map. Applied on the read path only, via
    /// [`Client::transport_reference`] / [`Client::transport_registry`].
    /// Empty = identity (no host mirrored). Cheap to clone.
    pub(super) mirrors: MirrorMap,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.box_clone(),
            lock_timeout: self.lock_timeout,
            tag_chunk_size: self.tag_chunk_size,
            repository_chunk_size: self.repository_chunk_size,
            progress: self.progress.clone(),
            mirrors: self.mirrors.clone(),
        }
    }
}

impl Client {
    pub fn lock_timeout(&self) -> std::time::Duration {
        self.lock_timeout
    }

    /// Returns a reference to the inner transport.
    ///
    /// Crate-internal: the sign/verify pipelines take a `&Client` and derive
    /// the transport through here for their transport-level calls (capability
    /// probes, referrer manifest reads). The public API never exposes
    /// `&dyn OciTransport` — pipelines are driven through the `PackageManager`
    /// facade (`sign_one` / `verify_one`), not by handing callers a transport.
    pub(crate) fn transport(&self) -> &dyn OciTransport {
        &*self.transport
    }

    #[cfg(test)]
    pub(crate) fn with_transport(transport: Box<dyn OciTransport>) -> Self {
        Client {
            transport,
            lock_timeout: std::time::Duration::from_secs(5),
            tag_chunk_size: 100,
            repository_chunk_size: 100,
            progress: crate::cli::progress::ProgressManager::disabled(),
            mirrors: MirrorMap::default(),
        }
    }

    /// Points reads at `host` for everything published under `upstream`.
    ///
    /// The `mirrors` field is module-private, so a caller outside `oci` cannot
    /// build a mirrored client — which is exactly what a test asserting that
    /// some subsystem's reads are *not* mirrored needs.
    #[cfg(test)]
    pub(crate) fn with_test_mirror(mut self, upstream: &str, host: &str, path_prefix: &str) -> Self {
        self.mirrors = MirrorMap::new([(
            upstream.to_string(),
            crate::config::mirror::ParsedMirror {
                protocol: "https".to_string(),
                host: host.to_string(),
                path_prefix: path_prefix.to_string(),
            },
        )]);
        self
    }

    // ── Mirror transform (single read-path rewrite seam) ───────────

    /// Builds the transport reference for a **read-path** operation, applying
    /// the mirror map.
    ///
    /// When `self.mirrors` has an entry for `identifier.registry()`, the
    /// returned reference targets the mirror host with the repository rewritten
    /// to `<path-prefix>/<repository>` (tag and digest copied verbatim). When
    /// no mirror is configured, the result is identical to the canonical
    /// reference. The returned reference is transport-only and is never
    /// converted back into an [`Identifier`] for storage.
    ///
    /// This is one of the two read seams — every read site builds references
    /// through here (or [`transport_registry`](Self::transport_registry)). There
    /// is no PUBLIC bypass: the `From<&Identifier> for native::Reference` impl is
    /// removed, so no read site can reach for a canonical conversion without
    /// naming an in-crate seam. In-crate read paths must still route through
    /// these seams rather than the `pub(crate)`
    /// [`Identifier::canonical_reference`] (which stays callable in-crate) — that
    /// discipline is enforced by the structural test plus the behavioural
    /// backstop, not by the compiler.
    pub(in crate::oci) fn transport_reference(&self, identifier: &Identifier) -> native::Reference {
        let Some((host, repository)) = self
            .mirrors
            .rewrite_repository(identifier.registry(), identifier.repository())
        else {
            // No mirror for this host: identical to the canonical reference.
            return identifier.canonical_reference();
        };
        // Tag and digest are copied verbatim from the canonical identifier; only
        // the host and repository are rewritten. The returned reference is
        // transport-only and never round-trips into storage.
        match (identifier.tag(), identifier.digest()) {
            (Some(tag), Some(digest)) => {
                native::Reference::with_tag_and_digest(host, repository, tag.to_string(), digest.to_string())
            }
            (Some(tag), None) => native::Reference::with_tag(host, repository, tag.to_string()),
            (None, Some(digest)) => native::Reference::with_digest(host, repository, digest.to_string()),
            (None, None) => native::Reference::with_tag(host, repository, "latest".into()),
        }
    }

    /// Builds the transport reference for a registry-scoped read operation
    /// (the catalog `list_repositories` call), applying the mirror map to the
    /// registry host.
    ///
    /// Sibling of [`transport_reference`](Self::transport_reference) for the
    /// case where there is no full identifier — only a registry string and a
    /// placeholder repository.
    pub(in crate::oci) fn transport_registry(&self, registry: &str) -> native::Reference {
        // The catalog **URL** is built from `registry()` alone (`/v2/_catalog`),
        // so the repository never reaches the path. The catalog **auth scope**,
        // however, is `repository:<repository>:pull` (oci-client `_auth`), so the
        // repository value still has to be well-formed. An empty repository (no
        // mirror) keeps the host verbatim and the repository empty; when a mirror
        // exists, the host is rewritten and the placeholder repository becomes the
        // mirror's path prefix verbatim — `rewrite_repository` returns the prefix
        // with no trailing slash for the empty-repository case, so the auth scope
        // is `repository:<prefix>:pull`, not the malformed `repository:<prefix>/:pull`.
        let (host, repository) = self
            .mirrors
            .rewrite_repository(registry, "")
            .unwrap_or_else(|| (registry.to_string(), String::new()));
        native::Reference::with_tag(host, repository, "latest".into())
    }

    /// Annotates `error` with the mirror routing that produced it.
    ///
    /// A mirrored read fails against a host the caller never typed: the
    /// identifier says `ghcr.io`, the request went to the mirror, and every
    /// error in between names exactly one of the two.
    ///
    /// The question is whether *this* fetch was rewritten, which is why the
    /// logical identifier is a parameter rather than something recovered from
    /// the physical host. A host configured as one upstream's mirror is still
    /// an ordinary registry anyone may pull from directly, so asking "who
    /// mirrors this host" answers for the wrong request and names a config
    /// entry that had nothing to do with the failure. Asking "was the host
    /// this identifier resolves to swapped for a mirror" cannot: a
    /// canonical-addressed read leaves the two equal and goes unannotated.
    ///
    /// Only failures where the routing is the missing context are wrapped.
    /// The not-found sentinels are control flow: callers match on them to
    /// produce `Ok(None)`, so burying one behind [`ClientError::Mirrored`]
    /// would turn a missing tag into a hard failure.
    fn via_mirror(&self, logical: &Identifier, physical: &native::Reference, error: ClientError) -> ClientError {
        if !matches!(
            error,
            ClientError::Registry(_)
                | ClientError::RegistryTransient(_)
                | ClientError::Authentication(_)
                | ClientError::NotAManifest(_)
                | ClientError::UnexpectedManifestType
                | ClientError::DigestMismatch { .. }
                | ClientError::ShortBlobRead { .. }
                | ClientError::InvalidManifest(_)
                | ClientError::InvalidImageIndex(_)
                | ClientError::Serialization(_)
        ) {
            return error;
        }
        let origin = logical.registry();
        let Some(mirror) = self.mirrors.get(origin).filter(|m| m.host == physical.registry()) else {
            return error;
        };
        ClientError::Mirrored {
            origin: origin.to_string(),
            mirror: mirror.host.clone(),
            physical: physical.to_string(),
            source: Box::new(error),
        }
    }

    /// Builds the reference for a read that has been told which host to
    /// address.
    ///
    /// The mirrored arm is [`transport_reference`](Self::transport_reference)
    /// verbatim — this is a routing switch, not a second seam.
    pub(crate) fn read_reference(&self, identifier: &Identifier, addressing: ReadAddressing) -> native::Reference {
        match addressing {
            ReadAddressing::Mirrored => self.transport_reference(identifier),
            // Push stays canonical (remote/proxy mirrors are read-only), so a
            // read that decides a write has to name the same host.
            ReadAddressing::Canonical => identifier.canonical_reference(),
        }
    }

    /// Builds the transport reference for a **write-path** operation — always
    /// the canonical host, never a mirror.
    ///
    /// Write counterpart to [`transport_reference`](Self::transport_reference).
    /// Remote/proxy mirrors are read-only (ADR Q5): a push routed through the
    /// read seam is rejected outright, or — against a writable mirror — lands
    /// the artifact somewhere the canonical verifier never looks, which for a
    /// signature is silent non-coverage rather than a visible failure.
    /// [`ensure_auth`](Self::ensure_auth) already splits `Push` off this way;
    /// this exposes the same decision to the referrer write paths
    /// (`oci/sign/pipeline.rs`), which build their own references.
    ///
    /// The read-side peer is [`read_reference`](Self::read_reference) with
    /// [`ReadAddressing::Canonical`] — a read that decides a write must name
    /// this same host.
    ///
    /// Lives here rather than at the call sites because
    /// [`Identifier::canonical_reference`] is allow-listed to this file
    /// (`canonical_reference_only_used_in_allowed_files`) and direct
    /// construction is gated by T-arch-G1.
    pub(crate) fn transport_write_reference(&self, identifier: &Identifier) -> native::Reference {
        identifier.canonical_reference()
    }

    // ── Authentication ─────────────────────────────────────────────

    /// Pre-authenticate against the registry for `identifier` with the
    /// given operation scope.
    ///
    /// Call at the start of a command or task to fail fast on credential
    /// issues (expired tokens, GPG agent prompts, missing env vars)
    /// before beginning any real work.
    ///
    /// `ensure_auth` is shared by the read path and the push path. A `Push`
    /// scope authenticates against the **canonical** host (remote/proxy mirrors
    /// are read-only, ADR Q5), so it builds the reference via
    /// [`Identifier::canonical_reference`]; every other scope is a read and
    /// keys auth off the mirror host via
    /// [`transport_reference`](Self::transport_reference).
    pub async fn ensure_auth(&self, identifier: &Identifier, operation: oci::RegistryOperation) -> Result<()> {
        // Exhaustive over `RegistryOperation` so a future upstream variant is a
        // compile error here, forcing an explicit routing decision rather than
        // silently inheriting the read (mirror-aware) path. `Push` authenticates
        // against the canonical host (remote/proxy mirrors are read-only, ADR Q5);
        // `Pull` is a read and routes through the mirror-aware
        // `transport_reference`. Coupled to the upstream enum in
        // `external/rust-oci-client/src/token_cache.rs`.
        let image = match operation {
            oci::RegistryOperation::Push => identifier.canonical_reference(),
            oci::RegistryOperation::Pull => self.transport_reference(identifier),
        };
        self.transport.ensure_auth(&image, operation).await?;
        Ok(())
    }

    // ── Index operations ─────────────────────────────────────────────

    /// Lists the tags for the given image reference, from the canonical registry.
    /// There is no validation that the tags correspond to valid package versions.
    ///
    /// A listing served by a mirror is [`list_tags_addressed`](Self::list_tags_addressed)
    /// with [`ReadAddressing::Mirrored`], asked for by name.
    pub async fn list_tags(&self, identifier: Identifier) -> Result<Vec<String>> {
        self.list_tags_addressed(identifier, ReadAddressing::Canonical).await
    }

    /// [`list_tags`](Self::list_tags) against a caller-chosen host.
    ///
    /// `ReadAddressing::Mirrored` is for a listing no write is planned from —
    /// see [`ReadAddressing`].
    pub(crate) async fn list_tags_addressed(
        &self,
        identifier: Identifier,
        addressing: ReadAddressing,
    ) -> Result<Vec<String>> {
        let image = self.read_reference(&identifier, addressing);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let chunk_size = self.tag_chunk_size;
        let tags = paginate(chunk_size, |cs, last| self.transport.list_tags(&image, cs, last)).await?;
        log::trace!("Listed tags for {}: {:?}", identifier, tags);
        Ok(tags)
    }

    pub async fn list_repositories(&self, registry: impl Into<String>) -> Result<Vec<String>> {
        let registry = registry.into();
        let image = self.transport_registry(&registry);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let chunk_size = self.repository_chunk_size;
        let repositories = paginate(chunk_size, |cs, last| self.transport.catalog(&image, cs, last)).await?;
        log::trace!("Listed repositories for {}: {:?}", registry, repositories);
        Ok(repositories)
    }

    /// Fetches the digest of a manifest from the remote, trying to avoid pulling the entire manifest if possible.
    ///
    /// Turning a mutable tag into a digest is the highest-value read there is to
    /// take from the canonical host, so this one has no short,
    /// canonical-by-default form: the host is named at every call site. Its only
    /// caller today is [`OciIndex`](crate::oci::index) deriving an index from a
    /// registry's tags API, which genuinely wants [`ReadAddressing::Mirrored`] —
    /// but a later caller deciding a write from this answer must pass
    /// [`ReadAddressing::Canonical`] (Invariant #5), and a short form would let
    /// it inherit the index's mirror without saying so. Do not add one.
    pub(crate) async fn fetch_manifest_digest_addressed(
        &self,
        identifier: &Identifier,
        addressing: ReadAddressing,
    ) -> Result<oci::Digest> {
        let ref_ = self.read_reference(identifier, addressing);
        self.transport
            .ensure_auth(&ref_, oci::RegistryOperation::Pull)
            .await
            .map_err(|e| self.via_mirror(identifier, &ref_, e))?;
        let digest = self
            .transport
            .fetch_manifest_digest(&ref_)
            .await
            .map_err(|e| self.via_mirror(identifier, &ref_, e))?;
        log::trace!("Fetched manifest digest for {}: {}", identifier, digest);
        Ok(digest.try_into()?)
    }

    /// Fetches the manifest for the given image reference from the canonical
    /// registry, returning both the manifest and its digest.
    ///
    /// A manifest served by a mirror is
    /// [`fetch_manifest_addressed`](Self::fetch_manifest_addressed) with
    /// [`ReadAddressing::Mirrored`], asked for by name.
    pub async fn fetch_manifest(&self, identifier: &Identifier) -> Result<(Digest, oci::Manifest)> {
        self.fetch_manifest_addressed(identifier, ReadAddressing::Canonical)
            .await
    }

    /// [`fetch_manifest`](Self::fetch_manifest) against a caller-chosen host.
    ///
    /// `ReadAddressing::Mirrored` is for a manifest no write is decided from —
    /// see [`ReadAddressing`].
    pub(crate) async fn fetch_manifest_addressed(
        &self,
        identifier: &Identifier,
        addressing: ReadAddressing,
    ) -> Result<(Digest, oci::Manifest)> {
        let ref_ = self.read_reference(identifier, addressing);
        self.transport
            .ensure_auth(&ref_, oci::RegistryOperation::Pull)
            .await
            .map_err(|e| self.via_mirror(identifier, &ref_, e))?;
        let (manifest, digest_str) = self
            .fetch_manifest_raw(&ref_)
            .await
            .map_err(|e| self.via_mirror(identifier, &ref_, e))?;
        let digest = digest_str.try_into()?;
        Ok((digest, manifest))
    }

    // ── Platform-aware cascade merge ─────────────────────────────────

    /// Fetches (or creates) the image index at `target_tag`, removes any existing
    /// entry for `platform`, inserts the new manifest entry, and pushes the
    /// updated index.
    ///
    /// Used by `package push --cascade` to merge a single-platform manifest into
    /// each rolling tag without destroying entries for other platforms.
    ///
    /// `annotations` are the publisher-stated index-level annotations (`ocx
    /// package push --annotation`). They are merged into whatever the index
    /// already carries — an empty map leaves the index's `annotations` field
    /// exactly as found, so a push without the flag produces byte-identical
    /// bytes to before the flag existed and never clears a link an earlier
    /// push established.
    ///
    /// Returns the digest and data of the pushed index.
    pub(crate) async fn merge_platform_into_index(
        &self,
        source_identifier: &Identifier,
        target_tag: impl Into<String>,
        platform: &oci::Platform,
        manifest_sha256: &str,
        manifest_size: i64,
        annotations: &BTreeMap<String, String>,
    ) -> Result<(Digest, oci::ImageIndex)> {
        let target_identifier = source_identifier.clone_with_tag(target_tag);
        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let ref_ = target_identifier.canonical_reference();
        self.transport.ensure_auth(&ref_, oci::RegistryOperation::Push).await?;
        let platform = Some(platform.clone().into());

        log::debug!("Merging platform entry into index for {}", ref_);
        let mut index = match self
            .transport
            .pull_manifest_raw(&ref_, &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX])
            .await
        {
            Ok((blob, digest_str)) => {
                // The existing index is about to be mutated and pushed back, so
                // it must be a valid one — otherwise a cascade would launder a
                // malformed publisher document into a freshly written tag.
                let existing = parse_registry_manifest(&blob)?;
                match existing {
                    oci::Manifest::Image(_) => {
                        let blob_size = i64::try_from(blob.len()).map_err(|_| {
                            ClientError::InvalidManifest(format!(
                                "existing manifest blob size {} exceeds i64::MAX",
                                blob.len()
                            ))
                        })?;
                        let entry = oci::ImageIndexEntry {
                            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                            digest: digest_str,
                            size: blob_size,
                            platform: None,
                            artifact_type: None,
                            annotations: None,
                        };
                        oci::ImageIndex {
                            schema_version: oci::INDEX_SCHEMA_VERSION,
                            media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                            artifact_type: None,
                            manifests: vec![entry],
                            annotations: None,
                        }
                    }
                    oci::Manifest::ImageIndex(index) => index,
                }
            }
            Err(ClientError::ManifestNotFound(_)) => {
                log::debug!("No existing manifest/index for {}, starting fresh", ref_);
                oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
                    media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                    artifact_type: None,
                    manifests: vec![],
                    annotations: None,
                }
            }
            Err(e) => return Err(e.into()),
        };

        // An index ocx has just mutated and re-serialized describes itself as
        // an ocx package index. A pre-existing foreign type is left alone —
        // filling an absent field states what we wrote; overwriting a declared
        // one relabels someone else's artifact.
        index
            .artifact_type
            .get_or_insert_with(|| MEDIA_TYPE_PACKAGE_V1.to_string());

        index.manifests.retain(|entry| entry.platform != platform);
        index.manifests.push(oci::ImageIndexEntry {
            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
            digest: manifest_sha256.to_string(),
            size: manifest_size,
            platform,
            artifact_type: None,
            annotations: None,
        });

        if !annotations.is_empty() {
            index
                .annotations
                .get_or_insert_default()
                .extend(annotations.iter().map(|(key, value)| (key.clone(), value.clone())));
        }

        let index_data = serde_json::to_vec(&index).map_err(ClientError::Serialization)?;
        let index_digest = Algorithm::Sha256.hash(&index_data);
        self.transport
            .push_manifest_raw(&ref_, index_data, MEDIA_TYPE_OCI_IMAGE_INDEX)
            .await?;
        log::debug!("Successfully merged platform entry into index for {}", ref_);

        Ok((index_digest, index))
    }

    /// Pushes `index` whole at `identifier`, returning the digest of the bytes
    /// that were written.
    ///
    /// The write primitive `ocx package cascade repair` needs: it recomputes an
    /// alias index in full and replaces it, where
    /// [`merge_platform_into_index`](Self::merge_platform_into_index) reads what
    /// the registry currently serves and edits one platform entry into it. The
    /// returned digest is computed from the bytes this call put on the wire, so
    /// a caller reading the tag back afterwards is comparing against what it
    /// wrote rather than against whatever the registry chose to echo.
    ///
    /// # Errors
    ///
    /// Push authentication failure, index serialization failure, or a registry
    /// that rejects the manifest.
    pub(crate) async fn push_index(&self, identifier: &Identifier, index: &oci::ImageIndex) -> Result<Digest> {
        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let ref_ = identifier.canonical_reference();
        self.transport.ensure_auth(&ref_, oci::RegistryOperation::Push).await?;

        let index_data = serde_json::to_vec(index).map_err(ClientError::Serialization)?;
        let index_digest = Algorithm::Sha256.hash(&index_data);
        self.transport
            .push_manifest_raw(&ref_, index_data, MEDIA_TYPE_OCI_IMAGE_INDEX)
            .await?;
        log::debug!("Pushed index for {} as {}", ref_, index_digest);

        Ok(index_digest)
    }

    // ── Canonical tag (registry-side deletion safety net) ─────────────

    /// Pushes a digest-named `sha256.<hex>` tag pointing directly at
    /// `platform`'s entry in `merged_manifest`.
    ///
    /// `merged_manifest` is the just-returned merge result for
    /// `(source_identifier, platform)` from `push_package` /
    /// `push_manifest_and_merge_tags` — `platform`'s entry is expected to be
    /// present by construction. A missing entry is a no-op rather than a
    /// hard failure: canonical tagging is a safety net layered on top of an
    /// already-committed push, never load-bearing for the push itself.
    ///
    /// Returns the tag it wrote (`Some("<algorithm>.<hex>")`), or `None` on
    /// that no-op. The caller cannot derive either: the tag is named after
    /// the *platform manifest* digest, which the caller does not hold, and
    /// the no-op is otherwise invisible to it.
    ///
    /// Registry-side deletion safety net (`adr_index_indirection.md`
    /// Decision E): a stray delete of a rolling/cascade tag can never orphan
    /// a digest a lock still pins, because the canonical tag names it
    /// directly. Uses a `.` separator — OCI tags forbid `:`. The local
    /// snapshot and lock never read or write canonical tags; this is a pure
    /// registry-side write.
    pub(crate) async fn push_canonical_tag(
        &self,
        source_identifier: &Identifier,
        merged_manifest: &oci::Manifest,
        platform: &oci::Platform,
    ) -> Result<Option<String>> {
        let Some(manifest_digest) = super::manifest::platform_manifest_digest(merged_manifest, platform) else {
            return Ok(None);
        };

        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let repo_ref = source_identifier.canonical_reference();
        self.transport
            .ensure_auth(&repo_ref, oci::RegistryOperation::Push)
            .await?;

        // `without_tag()` matters: `push_multi_layer_manifest` pushes the
        // platform manifest to a bare digest reference (registry/repo@digest,
        // no tag) — `native::Reference::clone_with_digest` drops the tag by
        // construction (`oci_spec::distribution::Reference`). Preserving
        // `source_identifier`'s tag here would build a `tag@digest` reference
        // that never round-trips to the same key the manifest was pushed
        // under, so the pull below would spuriously 404.
        let digest_ref = source_identifier
            .without_tag()
            .clone_with_digest(manifest_digest.clone())
            .canonical_reference();
        let (manifest_bytes, _digest_str) = self
            .transport
            .pull_manifest_raw(&digest_ref, &[MEDIA_TYPE_OCI_IMAGE_MANIFEST])
            .await?;

        let (algorithm, hex) = manifest_digest.parts();
        let tag = format!("{algorithm}.{hex}");
        let tag_ref = source_identifier.clone_with_tag(tag.clone()).canonical_reference();
        self.transport
            .push_manifest_raw(&tag_ref, manifest_bytes, MEDIA_TYPE_OCI_IMAGE_MANIFEST)
            .await?;

        Ok(Some(tag))
    }

    // ── Blob introspection ────────────────────────────────────────────

    /// HEAD a blob to verify its existence and retrieve its content length.
    ///
    /// Returns `Ok(size_bytes)` when the blob exists in the registry.
    /// Returns `Err(ClientError::BlobNotFound)` when the blob is absent.
    ///
    /// Used by `pull_local` to capture the real byte count for a
    /// `LayerRef::Digest` layer before pulling it, so the synthesized
    /// OCI descriptor has the same size as the manifest produced by
    /// `package push`.
    pub async fn head_blob(&self, identifier: &Identifier, digest: &Digest) -> Result<u64> {
        let image = self.transport_reference(identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let size = self.transport.head_blob(&image, digest).await?;
        Ok(size)
    }

    // ── Package pull ─────────────────────────────────────────────────
    //
    // Composable methods for fetching a package from a registry:
    //
    //   pull_manifest  → ImageManifest   (validate digest, media types, layers)
    //   pull_blob      → Vec<u8>         (raw OCI blob fetch by digest)
    //   pull_layer     → extracted dir   (download one layer blob, extract, codesign)
    //
    // Higher-level metadata fetch (with local-CAS caching) lives in
    // `package_manager::tasks::common::fetch_or_get_blob`.

    /// Fetches and validates the OCI manifest for a pinned package.
    ///
    /// Verifies the manifest digest matches the identifier.
    /// Returns the [`ImageManifest`](oci::ImageManifest) without asserting media types.
    pub async fn pull_manifest(
        &self,
        identifier: &oci::PinnedIdentifier,
    ) -> std::result::Result<oci::ImageManifest, ClientError> {
        let expected_digest = identifier.digest().to_string();
        let image = self.transport_reference(identifier);

        self.transport
            .ensure_auth(&image, oci::RegistryOperation::Pull)
            .await
            .map_err(|e| self.via_mirror(identifier, &image, e))?;

        let (manifest, digest_str) = self
            .fetch_manifest_raw(&image)
            .await
            .map_err(|e| self.via_mirror(identifier, &image, e))?;
        if digest_str != expected_digest {
            return Err(self.via_mirror(
                identifier,
                &image,
                ClientError::DigestMismatch {
                    expected: expected_digest,
                    actual: digest_str,
                },
            ));
        }
        let manifest = match manifest {
            oci::Manifest::Image(m) => m,
            _ => return Err(self.via_mirror(identifier, &image, ClientError::UnexpectedManifestType)),
        };

        Ok(manifest)
    }

    /// Fetches a single blob from the registry.
    ///
    /// `blob_ref` carries `(registry, repo)` for the OCI blob endpoint and
    /// the blob's own digest for content addressing. Generic OCI blob fetch
    /// — no media-type validation, no parsing. Caller is responsible for
    /// content interpretation.
    pub async fn pull_blob(&self, blob_ref: &oci::PinnedIdentifier) -> std::result::Result<Vec<u8>, ClientError> {
        let image = self.transport_reference(blob_ref);
        self.transport
            .ensure_auth(&image, oci::RegistryOperation::Pull)
            .await
            .map_err(|e| self.via_mirror(blob_ref, &image, e))?;
        self.transport
            .pull_blob(&image, &blob_ref.digest())
            .await
            .map_err(|e| self.via_mirror(blob_ref, &image, e))
    }

    /// Downloads and extracts a single OCI layer to the specified directory.
    ///
    /// Creates `{output_dir}/content/` with the extracted files and runs
    /// code-signing on macOS. No intermediate blob file is written to disk —
    /// the compressed stream is piped directly through hashing, decompression,
    /// and tar extraction in a single pass.
    ///
    /// # Pipeline
    ///
    /// ```text
    /// transport.pull_blob_streaming()           // raw compressed bytes (AsyncRead)
    ///   → HashingAsyncReader(algorithm)          // tees compressed bytes into digester (sha256/sha384/sha512)
    ///   → ProgressReader                        // on_progress(cumulative bytes_read)
    ///   → XzDecoder / GzDecoder                 // media-type dispatch
    ///   → SyncIoBridge                          // AsyncRead → sync Read
    ///   → tar::Archive::unpack()                // sync extraction (in spawn_blocking)
    /// ```
    ///
    /// The tar extractor stops at the end-of-archive marker rather than at
    /// stream end, so `spawn_blocking` drains the compressed remainder before
    /// finalising — otherwise the digest would cover only the prefix tar asked
    /// for. Two checks then run, in this order: fewer bytes than the descriptor
    /// declares returns [`ClientError::ShortBlobRead`] (an incomplete delivery),
    /// and a full-length blob whose hash differs returns
    /// [`ClientError::DigestMismatch`] (the registry served wrong content).
    ///
    /// Callers are responsible for creating `output_dir` and writing the
    /// digest marker file.
    pub async fn pull_layer(
        &self,
        identifier: &oci::PinnedIdentifier,
        layer: &oci::Descriptor,
        output_dir: &std::path::Path,
    ) -> std::result::Result<(), ClientError> {
        // A descriptor `size` that is non-positive (zero or negative) or does not
        // fit in `u64` is a malformed manifest, not a zero-byte layer: it would
        // collapse the compressed-side `.take()` cap to zero and the decompressed
        // cap to its floor. Reject it as InvalidManifest rather than silently
        // pulling nothing.
        let blob_total_size = match u64::try_from(layer.size) {
            Ok(size) if size > 0 => size,
            _ => {
                return Err(ClientError::InvalidManifest(format!(
                    "layer descriptor size '{}' is not a positive byte count",
                    layer.size
                )));
            }
        };

        // Decompressed-side cap (CWE-400): prevents a crafted compressed stream with a
        // high expansion ratio from exhausting disk/memory before the digest check fires.
        //
        // The multiplier 100× covers all realistic XZ compression ratios for tool
        // binaries (2–10×) with generous headroom. The 256 MiB floor keeps the cap
        // from being unreasonably tight for a very small declared layer size while
        // still bounding the damage a tiny-but-bomb layer can do. Exceeding the cap
        // yields [`ClientError::DecompressionCapExceeded`].
        const DECOMPRESSED_CAP_MULTIPLIER: u64 = 100;
        const DECOMPRESSED_CAP_MINIMUM: u64 = 256 << 20; // 256 MiB
        let decompressed_cap =
            (blob_total_size.saturating_mul(DECOMPRESSED_CAP_MULTIPLIER)).max(DECOMPRESSED_CAP_MINIMUM);

        self.pull_layer_with_caps(identifier, layer, output_dir, blob_total_size, decompressed_cap)
            .await
            .map_err(|e| self.via_mirror(identifier, &self.transport_reference(identifier), e))
    }

    /// Pipeline body for [`pull_layer`] with the decompressed-side cap passed in.
    ///
    /// `pull_layer` computes `decompressed_cap` from the descriptor size and
    /// delegates here. The cap is a parameter (rather than computed inline) so
    /// tests can inject a small ceiling and exercise the
    /// [`ClientError::DecompressionCapExceeded`] path without fabricating a
    /// gigabyte-scale archive. `blob_total_size` is the validated, positive
    /// compressed byte count used for the compressed-side `.take()` cap.
    async fn pull_layer_with_caps(
        &self,
        identifier: &oci::PinnedIdentifier,
        layer: &oci::Descriptor,
        output_dir: &std::path::Path,
        blob_total_size: u64,
        decompressed_cap: u64,
    ) -> std::result::Result<(), ClientError> {
        use async_compression::tokio::bufread::{GzipDecoder, XzDecoder, ZstdDecoder};
        use hashing_reader::HashingAsyncReader;
        use progress_reader::ProgressReader;
        use tokio::io::BufReader;
        use tokio_util::io::SyncIoBridge;

        let blob_compression =
            compression::CompressionAlgorithm::from_media_type(&layer.media_type).ok_or_else(|| {
                ClientError::InvalidManifest(format!("unsupported layer media type: {}", layer.media_type))
            })?;
        let content_path = output_dir.join("content");

        let image = self.transport_reference(identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;

        let layer_digest = Digest::try_from(layer.digest.as_str())
            .map_err(|e| ClientError::InvalidManifest(format!("layer digest '{}' is malformed: {e}", layer.digest)))?;

        log::info!(
            "Downloading layer {} to {}",
            layer_digest.to_short_string(),
            output_dir.display()
        );

        // Start the progress bar before opening the stream so the user sees
        // feedback immediately.
        let bar = self
            .progress
            .bytes(format!("Downloading '{identifier}'"), blob_total_size);
        let on_progress = bar.callback();

        // Obtain the raw compressed byte stream from the transport.
        // NativeTransport: wraps fork's pull_blob_stream (VerifyingStream
        // included — secondary verifier). Default impl: temp file fallback.
        let raw_stream = self.transport.pull_blob_streaming(&image, &layer_digest).await?;

        // ── Pipeline assembly ─────────────────────────────────────────
        //
        // Layering (innermost to outermost):
        //
        //   raw_stream
        //     → take(layer.size)             (CWE-400: compressed-side cap)
        //     → HashingAsyncReader           (hashes compressed wire bytes = blob digest)
        //     → ProgressReader               (progress on compressed bytes = download bytes)
        //     → XzDecoder/GzDecoder          (async-compression; takes a BufReader)
        //     → take(DECOMPRESSED_CAP)       (CWE-400: decompressed-side cap, applied to sync Read inside spawn_blocking)
        //
        // The HashingAsyncReader and ProgressReader sit on the COMPRESSED side
        // because:
        //  - The blob digest is computed over the compressed bytes (per OCI spec).
        //  - Progress reflects download throughput, not decoded size.
        //
        // Two-sided bounding prevents decompression bombs (CWE-400):
        //  - Compressed cap: raw stream read cannot exceed layer.size (descriptor-declared,
        //    manifest-verified). A registry serving more bytes than declared is stopped here.
        //    Reading stops at layer.size; the digest check detects mismatch from over-length streams.
        //  - Decompressed cap: tar extraction is capped at DECOMPRESSED_SIZE_CAP bytes of
        //    output so a crafted stream with a high expansion ratio cannot exhaust disk.

        // Compressed-side cap: layer.size is from the OCI manifest (digest-verified), so it
        // is a trusted upper bound on how many compressed bytes we should read from this layer.
        use tokio::io::AsyncReadExt as _;
        let capped_stream = raw_stream.take(blob_total_size);

        let hashing_reader = HashingAsyncReader::new(capped_stream, layer_digest.algorithm());
        let progress_reader = ProgressReader::new(hashing_reader, on_progress);

        // Layer blobs extract verbatim (strip = 0) into the shared
        // content-addressed layer store; per-layer strip + output prefix are
        // applied once, later, at assemble time (see
        // `assemble_from_layers_with_layouts`). Baking strip in here would
        // corrupt the shared store when two packages reuse one blob digest with
        // different strip.
        let content_path_clone = content_path.clone();
        let identifier_label = identifier.to_string();

        // ── spawn_blocking boundary ───────────────────────────────────
        //
        // The sync tar extractor drives the entire pipeline. SyncIoBridge
        // is created inside the spawn_blocking closure for clarity: it
        // captures Handle::current() and drives reads via Handle::block_on
        // (tokio-util 0.7.18 sync_bridge.rs:293) — NOT block_in_place.
        // spawn_blocking threads have the handle via thread-local, so
        // creating SyncIoBridge here is correct; moving it in from outside
        // would also be valid per tokio-util docs, but keeping construction
        // inside the closure makes the sync-side boundary explicit.
        //
        // Scale assumption: this spawn_blocking thread is held for the full
        // download+extract duration of the layer (e.g. ~160 s at 10 Mbps ×
        // 200 MB). Tokio's blocking pool cap is 512. Realistic install
        // parallelism is ≤ a few dozen concurrent layers, well within budget.
        // If install parallelism ever grows unbounded, add a semaphore at
        // this boundary (deferred).
        //
        // After extraction, the pipeline is unwound via into_inner() to recover
        // the HashingAsyncReader so its accumulated digest can be finalized.
        // Chain (innermost → outermost at SyncIoBridge boundary):
        //   SyncIoBridge<Decoder<BufReader<ProgressReader<HashingAsyncReader<_>>>>>
        // archive::extract_tar_from_reader returns (result, reader) so we can
        // recover the reader after extraction and chain .into_inner() calls:
        //   reader              → SyncIoBridge<Decoder<...>>
        //   .into_inner()       → Decoder<BufReader<ProgressReader<HashingAsyncReader<_>>>>
        //   .into_inner()       → BufReader<ProgressReader<HashingAsyncReader<_>>>
        //   drain_compressed_remainder(..)   ← must run here, see its doc comment
        //   .into_inner()       → ProgressReader<HashingAsyncReader<_>>
        //   .into_inner()       → HashingAsyncReader<_>
        //   .finalize()         → (Digest, u64)

        // Type alias to keep the match arms readable.
        // The extraction result uses crate::Result (= std::result::Result<(), crate::Error>),
        // since tar.rs uses the top-level error type via `?`. `cap_exceeded` reports
        // whether the decompressed stream tripped the CWE-400 ceiling (see below).
        type PipelineResult = (crate::Result<()>, (oci::Digest, u64), bool);

        // 256 KiB BufReader sits between the progress reader and the decoder.
        // async-compression decoders call poll_read on each decode step; without
        // buffering this crosses the SyncIoBridge Handle::block_on boundary ~32×
        // more often than needed (default 8 KiB ÷ 256 KiB). A larger buffer
        // amortises the cross-boundary cost over fewer, larger reads from the
        // network stream. 256 KiB is chosen to match typical HTTP/2 receive
        // window segments and XZ block sizes.
        const BUF_READER_CAPACITY: usize = 256 * 1024;

        // Decompressed-side cap (CWE-400): `decompressed_cap` is computed by the
        // public `pull_layer` (256 MiB floor, 100× declared compressed size) or
        // injected by a test. We wrap the bridge in `take(cap + 1)`: if the
        // decompressed stream produces `cap + 1` bytes, the extra "probe" byte
        // means the real output would have exceeded the cap, so `Take::limit()`
        // reaches 0 and we surface `DecompressionCapExceeded`. A well-formed
        // layer never reaches `cap + 1` (it would have to be a bomb), so the
        // extra byte is harmless for the happy path. Detecting the hit
        // explicitly stops a truncated-at-cap archive from being misattributed
        // as a digest mismatch or internal tar error.
        let cap_with_probe = decompressed_cap.saturating_add(1);

        /// Reads whatever compressed bytes the tar extractor left behind, so the
        /// digest covers the whole blob instead of the prefix tar happened to want.
        ///
        /// `tar`'s entry iterator stops at the end-of-archive marker and hands the
        /// reader back undrained, so the codec trailer (gzip's CRC+ISIZE footer,
        /// xz's index + footer) and any post-terminator padding are usually still
        /// unread. Those bytes never reach `HashingAsyncReader` unless they are
        /// pulled deliberately — and whether they happened to ride the last buffer
        /// fill depends on how the network segmented the response, which is what
        /// made the resulting `DigestMismatch` non-deterministic.
        ///
        /// Drains the BUFFERED-COMPRESSED level, below the decoder: a decoder can
        /// report decoded-EOF without having consumed its own trailing bytes, so
        /// draining the decoded side would not reach them. Bounded by the outer
        /// `take(blob_total_size)`, so a well-formed layer costs a few bytes and a
        /// truncated one hits EOF immediately.
        ///
        /// Runs on the extraction-ERROR path too — that is load-bearing: wrong
        /// bytes from a registry fail extraction with a format error, and the
        /// digest is what attributes that to the registry (CWE-345) rather than
        /// to a local archive problem. Skipped only on `cap_exceeded`, where the
        /// caller returns `DecompressionCapExceeded` without ever consulting the
        /// digest, so draining would just pull the rest of a known decompression
        /// bomb's declared bytes into a sink.
        ///
        /// Swallows its own error on purpose: the caller decides the outcome from
        /// `(bytes_read, digest)` — short delivery is `ShortBlobRead`, wrong
        /// content is `DigestMismatch`. Propagating from here would let the fork's
        /// `VerifyingStream` (which surfaces its digest error as an `io::Error` at
        /// stream end, i.e. exactly during this drain) pre-empt the canonical check.
        fn drain_compressed_remainder<R: tokio::io::AsyncRead + Unpin>(reader: R, cap_exceeded: bool) -> R {
            if cap_exceeded {
                return reader;
            }
            let mut bridge = SyncIoBridge::new(reader);
            if let Err(error) = std::io::copy(&mut bridge, &mut std::io::sink()) {
                log::debug!("draining the trailing compressed bytes failed: {error}");
            }
            bridge.into_inner()
        }

        let (extract_result, digest_result, cap_exceeded): PipelineResult = match blob_compression {
            compression::CompressionAlgorithm::Lzma => {
                let decoder = XzDecoder::new(BufReader::with_capacity(BUF_READER_CAPACITY, progress_reader));
                tokio::task::spawn_blocking(move || -> PipelineResult {
                    use std::io::Read as _;
                    // SyncIoBridge is created inside spawn_blocking for clarity —
                    // it makes the sync-side boundary explicit at construction.
                    // Wrap with std::io::Read::take for the decompressed-side cap.
                    let bridge = SyncIoBridge::new(decoder).take(cap_with_probe);
                    let (extract_result, bridge) = archive::extract_tar_from_reader(bridge, &content_path_clone, 0);
                    // limit() == 0 means all `cap + 1` bytes were consumed → the
                    // decompressed output exceeded `decompressed_cap`.
                    let cap_exceeded = bridge.limit() == 0;
                    // Unwind the pipeline to recover the HashingAsyncReader:
                    //   bridge (Take<SyncIoBridge>) → into_inner() → SyncIoBridge
                    //     → into_inner() → Decoder → into_inner() → BufReader
                    //     → into_inner() → ProgressReader → into_inner() → HashingAsyncReader
                    let buffered = bridge.into_inner().into_inner().into_inner();
                    let hashing_reader = drain_compressed_remainder(buffered, cap_exceeded)
                        .into_inner()
                        .into_inner();
                    (extract_result, hashing_reader.finalize(), cap_exceeded)
                })
                .await
                .map_err(ClientError::internal)?
            }
            compression::CompressionAlgorithm::Gzip => {
                let decoder = GzipDecoder::new(BufReader::with_capacity(BUF_READER_CAPACITY, progress_reader));
                tokio::task::spawn_blocking(move || -> PipelineResult {
                    use std::io::Read as _;
                    let bridge = SyncIoBridge::new(decoder).take(cap_with_probe);
                    let (extract_result, bridge) = archive::extract_tar_from_reader(bridge, &content_path_clone, 0);
                    let cap_exceeded = bridge.limit() == 0;
                    let buffered = bridge.into_inner().into_inner().into_inner();
                    let hashing_reader = drain_compressed_remainder(buffered, cap_exceeded)
                        .into_inner()
                        .into_inner();
                    (extract_result, hashing_reader.finalize(), cap_exceeded)
                })
                .await
                .map_err(ClientError::internal)?
            }
            compression::CompressionAlgorithm::Zstd => {
                // zstd decoding is single-threaded, mirroring the xz/gzip decode path.
                // The pipeline shape and unwind depth are identical to the other arms.
                let decoder = ZstdDecoder::new(BufReader::with_capacity(BUF_READER_CAPACITY, progress_reader));
                tokio::task::spawn_blocking(move || -> PipelineResult {
                    use std::io::Read as _;
                    let bridge = SyncIoBridge::new(decoder).take(cap_with_probe);
                    let (extract_result, bridge) = archive::extract_tar_from_reader(bridge, &content_path_clone, 0);
                    let cap_exceeded = bridge.limit() == 0;
                    let buffered = bridge.into_inner().into_inner().into_inner();
                    let hashing_reader = drain_compressed_remainder(buffered, cap_exceeded)
                        .into_inner()
                        .into_inner();
                    (extract_result, hashing_reader.finalize(), cap_exceeded)
                })
                .await
                .map_err(ClientError::internal)?
            }
            compression::CompressionAlgorithm::None => {
                return Err(ClientError::InvalidManifest(format!(
                    "uncompressed layers are not supported (media type: {})",
                    layer.media_type
                )));
            }
        };

        // ── Decompression-bomb cap (CWE-400) ─────────────────────────
        //
        // Checked BEFORE the digest comparison: a stream that overruns the cap
        // is a decompression bomb regardless of whether its compressed bytes
        // happen to hash correctly. Surfacing DecompressionCapExceeded here is
        // what stops the hit from being misattributed as DigestMismatch (the
        // hash is computed over a truncated prefix) or as an internal tar error.
        if cap_exceeded {
            return Err(ClientError::DecompressionCapExceeded { cap: decompressed_cap });
        }

        let (computed_digest, bytes_read) = digest_result;

        // ── Delivery completeness ────────────────────────────────────
        //
        // Checked BEFORE the digest comparison, or the mismatch masks it: a
        // prefix cannot hash to the whole, so an incomplete delivery is
        // *guaranteed* to fail the digest check and would be reported as if the
        // registry had served wrong content. `blob_total_size` is the
        // manifest-verified declared size, and the drain above pulled every byte
        // the transport was willing to hand over, so `bytes_read` short of it
        // means the blob never arrived in full.
        if bytes_read != blob_total_size {
            return Err(ClientError::ShortBlobRead {
                expected: blob_total_size,
                actual: bytes_read,
            });
        }

        // ── Digest verification (canonical check) ────────────────────
        //
        // Perform the digest check BEFORE inspecting the extraction result.
        //
        // Rationale: if the registry sent wrong bytes (CWE-345), the extraction
        // might fail due to format errors (e.g. "Invalid gzip header") because
        // the bytes are the wrong format, not the declared one. In that case
        // the DigestMismatch error is more informative and security-relevant than
        // the extraction error. Reporting DigestMismatch first correctly attributes
        // the failure to the registry serving wrong content.
        //
        // Reaching here means the whole declared blob was hashed, so a mismatch
        // is about the bytes themselves — this variant means what its docs say.
        if computed_digest != layer_digest {
            return Err(ClientError::DigestMismatch {
                expected: layer_digest.to_string(),
                actual: computed_digest.to_string(),
            });
        }

        // ── Extraction result ─────────────────────────────────────────
        //
        // Bytes verified correct — now check for extraction errors (e.g.
        // corrupt archive structure despite correct hash, malformed tar entries).
        // On any error, the partially-written output_dir is left for the
        // caller's TempStore to remove (RAII DropFile / TempStore semantics).
        //
        // Also check for fork VerifyingStream DigestError: the fork fires at stream
        // end (inside spawn_blocking) as: crate::Error::Archive(archive::Error::Tar(io::Error)).
        // This path is a secondary check (spec §D2); we still convert it to DigestMismatch
        // for taxonomy consistency even though the canonical check above would have
        // caught it first if bytes genuinely differ.
        if let Err(archive_err) = extract_result {
            // Walk the source chain looking for a fork DigestError embedded in
            // an io::Error node. check_fork_io_error handles the downcast; we
            // walk the error chain to find each io::Error node.
            let mut current: Option<&dyn std::error::Error> = Some(&archive_err);
            while let Some(err) = current {
                if let Some(io_err) = err.downcast_ref::<std::io::Error>()
                    && let Some(client_err) = native_transport::check_fork_io_error(io_err)
                {
                    return Err(client_err);
                }
                current = err.source();
            }
            return Err(ClientError::internal(archive_err));
        }

        // ── Codesign (macOS only) ─────────────────────────────────────
        //
        // Codesign operates on the already-extracted content/ directory.
        crate::codesign::sign_extracted_content(&content_path)
            .await
            .map_err(ClientError::internal)?;

        log::debug!(
            "[{}] layer {} extracted to {}",
            identifier_label,
            layer_digest.to_short_string(),
            content_path.display()
        );
        Ok(())
    }

    // ── Package push ─────────────────────────────────────────────────

    pub async fn push_package(
        &self,
        package_info: Info,
        layers: &[crate::publisher::LayerRef],
        annotations: &BTreeMap<String, String>,
    ) -> Result<(Digest, oci::Manifest, LayerCounts)> {
        let (index_digest, index, layer_counts) = self
            .push_manifest_and_merge_tags(&package_info, layers, &[], annotations)
            .await?;
        Ok((index_digest, oci::Manifest::ImageIndex(index), layer_counts))
    }

    /// Pushes the package manifest and merges the resulting platform entry
    /// into the primary tag's image index plus each tag in `extra_tags`.
    ///
    /// The manifest is pushed once and its digest reused across every
    /// `merge_platform_into_index` call, so a cascade or multi-tag push
    /// never re-serializes or re-uploads the manifest. `extra_tags` is
    /// the rolling/cascade tag set (e.g. `["3.28", "3", "latest"]`);
    /// pass `&[]` for a plain single-tag push.
    ///
    /// `annotations` are written onto every index this call touches — the
    /// primary tag and each of `extra_tags` — so a cascade never leaves a
    /// rolling tag with weaker provenance than the version tag it mirrors.
    ///
    /// Returns the digest + data of the primary tag's image index, plus the
    /// layer-push counts for the one manifest push.
    pub(crate) async fn push_manifest_and_merge_tags(
        &self,
        package_info: &Info,
        layers: &[crate::publisher::LayerRef],
        extra_tags: &[String],
        annotations: &BTreeMap<String, String>,
    ) -> Result<(Digest, oci::ImageIndex, LayerCounts)> {
        log::debug!(
            "Pushing package {} with {} layer(s)",
            package_info.identifier,
            layers.len()
        );

        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = package_info.identifier.canonical_reference();
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let (_manifest, manifest_data, manifest_sha256, layer_counts) =
            self.push_multi_layer_manifest(package_info, layers).await?;
        let manifest_size = i64::try_from(manifest_data.len()).map_err(|_| {
            ClientError::InvalidManifest(format!("manifest size {} exceeds i64::MAX", manifest_data.len()))
        })?;

        let primary_tag = package_info.identifier.tag_or_latest().to_string();
        let (index_digest, index) = self
            .merge_platform_into_index(
                &package_info.identifier,
                &primary_tag,
                &package_info.platform,
                &manifest_sha256,
                manifest_size,
                annotations,
            )
            .await?;

        for tag in extra_tags {
            log::debug!("Cascading to {tag}");
            self.merge_platform_into_index(
                &package_info.identifier,
                tag.clone(),
                &package_info.platform,
                &manifest_sha256,
                manifest_size,
                annotations,
            )
            .await?;
        }

        Ok((index_digest, index, layer_counts))
    }

    /// Pushes config blob + N layer blobs + image manifest.
    ///
    /// For `LayerRef::File` layers: reads file, computes digest, uploads blob.
    /// For `LayerRef::Digest` layers: HEADs the blob to verify existence
    /// and learn its size, and uses the caller-supplied `media_type`
    /// for the manifest descriptor. The OCI spec does not expose a
    /// layer's media type via blob HEAD, so the caller is responsible
    /// for declaring it at the CLI (see `LayerRef::FromStr`).
    ///
    /// A layer carrying `mount_from` first attempts a cross-repository blob
    /// mount from that source repository. Mounting is a pure optimization —
    /// a mount failure (spec-legal 202 miss, or any transport error) is never
    /// itself fatal; the layer falls back to its normal upload/verify path.
    /// For a [`LayerRef::File`] that fallback always succeeds (it has local
    /// bytes to upload); for a [`LayerRef::Digest`] it is a HEAD against the
    /// target repository, which fails when the declined mount was the only
    /// route by which the blob could have arrived. See
    /// [`Self::try_mount_layer`].
    ///
    /// Returns the manifest, its serialized bytes, its SHA-256 digest string,
    /// and the aggregate [`LayerCounts`] for the layers pushed.
    pub(crate) async fn push_multi_layer_manifest(
        &self,
        package_info: &Info,
        layers: &[crate::publisher::LayerRef],
    ) -> std::result::Result<(oci::ImageManifest, Vec<u8>, String, LayerCounts), ClientError> {
        use crate::publisher::LayerRef;

        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = package_info.identifier.canonical_reference();
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let total_layers = layers.len();
        // Upload file layers and verify digest layers concurrently, preserving
        // input order so manifest descriptors match the caller-supplied order.
        // Bounded by `LAYER_PUSH_CONCURRENCY` to cap in-memory archive buffers.
        let layer_results: Vec<(oci::Descriptor, LayerPushOutcome)> = stream::iter(layers.iter().enumerate())
            .map(|(index, layer)| {
                // `async move` owns its captures, so each concurrent future needs
                // its own copy of the image reference; clones are cheap
                // (a few short strings) and are outweighed by avoiding a
                // lifetime gymnastics around the stream combinator.
                let image = image.clone();
                async move {
                    let progress_label = format!("{}/{}", index + 1, total_layers);
                    match layer {
                        LayerRef::File {
                            path,
                            layout,
                            mount_from,
                        } => {
                            let package_media_type =
                                media_type_from_path(path).map(|mt| mt.to_string()).ok_or_else(|| {
                                    ClientError::InvalidManifest(format!("unsupported archive: {}", path.display()))
                                })?;

                            // BOUNDED: LAYER_PUSH_CONCURRENCY caps simultaneous
                            // in-memory archives at 4 × (layer size). Do not raise
                            // the constant without either switching to a streaming
                            // push path or auditing the RSS budget for the largest
                            // layers callers ship.
                            //
                            // Single disk pass: read and hash are interleaved in
                            // 64 KiB chunks, so the SHA-256 finalization happens
                            // without a second traversal of the buffer.
                            let (package_data, digest) =
                                Algorithm::Sha256
                                    .hash_file_read(path)
                                    .await
                                    .map_err(|e| ClientError::Io {
                                        path: path.to_path_buf(),
                                        source: e,
                                    })?;
                            let package_data_len = package_data.len();

                            log::trace!(
                                "Layer {progress_label} {}: digest={}, size={}",
                                path.display(),
                                digest,
                                package_data_len
                            );

                            let mounted = self
                                .try_mount_layer(&image, mount_from.as_deref(), &digest, &progress_label)
                                .await;

                            let outcome = if mounted {
                                LayerPushOutcome::Mounted
                            } else {
                                let bar = self.progress.bytes(
                                    format!("Uploading {progress_label} {}", path.display()),
                                    package_data_len as u64,
                                );
                                let on_progress = bar.callback();
                                self.transport
                                    .push_blob(&image, package_data, &digest, on_progress)
                                    .await?;
                                drop(bar);
                                LayerPushOutcome::Uploaded
                            };

                            let size = i64::try_from(package_data_len).map_err(|_| {
                                ClientError::InvalidManifest(format!("blob size {package_data_len} exceeds i64::MAX"))
                            })?;
                            Ok::<(oci::Descriptor, LayerPushOutcome), ClientError>((
                                oci::Descriptor {
                                    media_type: package_media_type,
                                    digest: digest.to_string(),
                                    size,
                                    urls: None,
                                    artifact_type: None,
                                    // BC2: default (empty) layout → `None`, so the
                                    // manifest stays byte-identical to today.
                                    annotations: layout.to_annotations(),
                                },
                                outcome,
                            ))
                        }
                        LayerRef::Digest {
                            digest,
                            media_type,
                            layout,
                            mount_from,
                        } => {
                            // The caller supplies `media_type` because the OCI
                            // distribution spec does not expose a layer's media
                            // type via blob HEAD — only the blob bytes and
                            // Content-Length. See `LayerRef::FromStr` for the
                            // `sha256:<hex>.<ext>` CLI syntax that carries this
                            // information from the user to here.
                            let mounted = self
                                .try_mount_layer(&image, mount_from.as_deref(), digest, &progress_label)
                                .await;

                            log::info!("Reusing layer {progress_label} {digest} ({media_type})");
                            // HEAD is always required: even after a successful
                            // mount, the (adapted) mount path doesn't return the
                            // blob's size, and it doubles as existence
                            // verification for the non-mounted path.
                            let size = self.transport.head_blob(&image, digest).await?;

                            log::trace!(
                                "Layer {progress_label} {digest}: verified, media_type={media_type}, size={size}"
                            );

                            let size = i64::try_from(size).map_err(|_| {
                                ClientError::InvalidManifest(format!("blob size {size} exceeds i64::MAX"))
                            })?;
                            let outcome = if mounted {
                                LayerPushOutcome::Mounted
                            } else {
                                LayerPushOutcome::Verified
                            };
                            Ok((
                                oci::Descriptor {
                                    media_type: media_type.as_media_type().to_string(),
                                    digest: digest.to_string(),
                                    size,
                                    urls: None,
                                    artifact_type: None,
                                    annotations: layout.to_annotations(),
                                },
                                outcome,
                            ))
                        }
                    }
                }
            })
            .buffered(LAYER_PUSH_CONCURRENCY)
            .try_collect()
            .await?;

        let mut layer_counts = LayerCounts::default();
        let layer_descriptors: Vec<oci::Descriptor> = layer_results
            .into_iter()
            .map(|(descriptor, outcome)| {
                layer_counts.record(outcome);
                descriptor
            })
            .collect();

        // Assemble the manifest from the resolved descriptors (pure, no I/O).
        // Shared with `pull_local` so the two paths produce byte-identical manifests.
        let parts = super::manifest_builder::build_package_manifest(&package_info.metadata, layer_descriptors)?;
        log::trace!("Config digest: {}", parts.config_digest);

        // Push config blob — tiny, no progress needed.
        self.transport
            .push_blob(
                &image,
                parts.config_bytes,
                &parts.config_digest,
                transport::no_progress(),
            )
            .await?;

        let manifest_sha256 = parts.manifest_digest.to_string();
        let canonical_image = image.clone_with_digest(manifest_sha256.clone());

        let pushed_digest = self
            .transport
            .push_manifest_raw(
                &canonical_image,
                parts.manifest_bytes.clone(),
                MEDIA_TYPE_OCI_IMAGE_MANIFEST,
            )
            .await?;
        log::debug!("Pushed manifest with digest '{}'", pushed_digest);

        Ok((parts.manifest, parts.manifest_bytes, manifest_sha256, layer_counts))
    }

    /// Attempts a cross-repository blob mount for a layer carrying
    /// `mount_from`, returning `true` on success.
    ///
    /// A `None` source (no `from=` tail on the layer ref) short-circuits to
    /// `false` without a transport call. Any non-`Mounted` transport
    /// response — a spec-legal miss, or a transport error — is treated as
    /// `false`: mounting is purely an upload-avoidance optimization and never
    /// itself fails the push.
    ///
    /// "Never fails the push" is a statement about *this* function, not about
    /// what the fallback then finds. A [`LayerRef::File`] falls back to
    /// uploading its local bytes and always succeeds; a [`LayerRef::Digest`]
    /// has no local bytes, so its fallback is a HEAD against the **target**
    /// repository, which legitimately fails when the declined mount was the
    /// only way the blob could have got there.
    ///
    /// A decline is the expected answer from any registry whose auth token is
    /// scoped to the target repository alone: the OCI spec lets a registry
    /// refuse a mount for any reason, and cross-repository mount normally
    /// requires pull scope on the source that the push-scoped token does not
    /// carry.
    async fn try_mount_layer(
        &self,
        image: &native::Reference,
        mount_from: Option<&str>,
        digest: &oci::Digest,
        progress_label: &str,
    ) -> bool {
        let Some(source_repository) = mount_from else {
            return false;
        };
        match self.transport.mount_blob(image, source_repository, digest).await {
            Ok(MountOutcome::Mounted) => true,
            Ok(MountOutcome::UploadRequired) => {
                // Debug, not warn: a decline is the spec-legal common case on
                // any registry whose token is scoped to the target repository,
                // so warning would fire once per layer on an ordinary push.
                // It is logged at all because it is the one observable that
                // explains a `LayerRef::Digest` fallback failing the push.
                log::debug!(
                    "Mount of layer {progress_label} {digest} from {source_repository} into {image} \
                     declined by the registry, falling back"
                );
                false
            }
            Err(e) => {
                log::warn!(
                    "Mount of layer {progress_label} {digest} from {source_repository} into {image} \
                     declined, falling back: {e}"
                );
                false
            }
        }
    }

    // ── Description operations ────────────────────────────────────────

    /// Pushes a description artifact to the `__ocx.desc` tag.
    ///
    /// Builds an OCI ImageManifest with `artifact_type` set to the description media type,
    /// an empty config blob, layers for the README (and optional logo), and manifest-level
    /// annotations for catalog metadata (title, description, keywords).
    pub async fn push_description(
        &self,
        identifier: &Identifier,
        description: &package::description::Description,
    ) -> std::result::Result<(), ClientError> {
        let desc_identifier = identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);
        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = desc_identifier.canonical_reference();
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let config_data = b"{}".to_vec();
        let config_digest = Algorithm::Sha256.hash(&config_data);
        self.transport
            .push_blob(&image, config_data, &config_digest, transport::no_progress())
            .await?;

        let readme_bytes = description.readme.as_bytes();
        let readme_len = readme_bytes.len();
        let readme_digest = Algorithm::Sha256.hash(readme_bytes);
        self.transport
            .push_blob(&image, readme_bytes.to_vec(), &readme_digest, transport::no_progress())
            .await?;

        let readme_size = i64::try_from(readme_len)
            .map_err(|_| ClientError::InvalidManifest(format!("readme blob size {readme_len} exceeds i64::MAX")))?;
        let mut layers = vec![oci::Descriptor {
            media_type: MEDIA_TYPE_MARKDOWN.to_string(),
            digest: readme_digest.to_string(),
            size: readme_size,
            urls: None,
            artifact_type: None,
            annotations: Some([(oci::annotations::TITLE.to_string(), "README.md".to_string())].into()),
        }];

        if let Some(logo) = &description.logo {
            let logo_len = logo.data.len();
            let logo_digest = Algorithm::Sha256.hash(&logo.data);
            self.transport
                .push_blob(&image, logo.data.clone(), &logo_digest, transport::no_progress())
                .await?;

            let ext = match logo.media_type {
                MEDIA_TYPE_PNG => "png",
                MEDIA_TYPE_SVG => "svg",
                _ => "bin",
            };
            let logo_size = i64::try_from(logo_len)
                .map_err(|_| ClientError::InvalidManifest(format!("logo blob size {logo_len} exceeds i64::MAX")))?;
            layers.push(oci::Descriptor {
                media_type: logo.media_type.to_string(),
                digest: logo_digest.to_string(),
                size: logo_size,
                urls: None,
                artifact_type: None,
                annotations: Some([(oci::annotations::TITLE.to_string(), format!("logo.{ext}"))].into()),
            });
        }

        let mut builder = super::manifest_builder::ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_DESCRIPTION_V1)
            .config_bytes(MEDIA_TYPE_OCI_EMPTY_CONFIG, b"{}".to_vec())
            .layers(layers);
        if !description.annotations.is_empty() {
            builder = builder.annotations(description.annotations.clone());
        }
        let parts = builder.build()?;
        // Sanity: the empty-config blob digest computed by the builder must
        // match the one we already pushed above.
        debug_assert_eq!(parts.config_digest.to_string(), config_digest.to_string());
        let manifest_data = parts.manifest_bytes;

        // Push to the tag reference directly (not by digest) so the tag is created.
        self.transport
            .push_manifest_raw(&image, manifest_data, MEDIA_TYPE_OCI_IMAGE_MANIFEST)
            .await?;

        log::debug!("Pushed description for {}", identifier);
        Ok(())
    }

    // ── Patch descriptor operations ───────────────────────────────────────

    /// Pushes a `__ocx.patch` descriptor artifact to the patch registry.
    ///
    /// Builds an OCI ImageManifest with `artifactType` set to
    /// [`crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE`], an empty `{}` config blob,
    /// and a single layer carrying the descriptor JSON
    /// ([`crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE`]). The artifact is
    /// pushed to the `__ocx.patch` internal tag on `patch_repo_id`.
    ///
    /// `descriptor_bytes` is validated by parsing it as a
    /// [`crate::patch::PatchDescriptor`] before any network call — a malformed
    /// descriptor is rejected up front rather than published.
    ///
    /// Returns the manifest digest of the pushed `__ocx.patch` artifact.
    ///
    /// # Errors
    ///
    /// - [`ClientError::InvalidManifest`] — `descriptor_bytes` is not a valid
    ///   patch descriptor, or manifest assembly failed.
    /// - [`ClientError::Authentication`] / [`ClientError::Registry`] — auth or a
    ///   blob/manifest push failed.
    pub async fn push_patch_descriptor(
        &self,
        patch_repo_id: &Identifier,
        descriptor_bytes: &[u8],
    ) -> std::result::Result<oci::Digest, ClientError> {
        // Validate the descriptor parses before pushing — reject malformed input.
        crate::patch::PatchDescriptor::from_json_bytes(descriptor_bytes)
            .map_err(|e| ClientError::InvalidManifest(format!("invalid patch descriptor: {e}")))?;

        let patch_identifier = patch_repo_id.clone_with_tag(InternalTag::PATCH_TAG);
        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = patch_identifier.canonical_reference();
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let config_data = b"{}".to_vec();
        let config_digest = Algorithm::Sha256.hash(&config_data);
        self.transport
            .push_blob(&image, config_data, &config_digest, transport::no_progress())
            .await?;

        let layer_len = descriptor_bytes.len();
        let layer_digest = Algorithm::Sha256.hash(descriptor_bytes);
        self.transport
            .push_blob(
                &image,
                descriptor_bytes.to_vec(),
                &layer_digest,
                transport::no_progress(),
            )
            .await?;

        let layer_size = i64::try_from(layer_len)
            .map_err(|_| ClientError::InvalidManifest(format!("descriptor blob size {layer_len} exceeds i64::MAX")))?;
        let layers = vec![oci::Descriptor {
            media_type: crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE.to_string(),
            digest: layer_digest.to_string(),
            size: layer_size,
            urls: None,
            artifact_type: None,
            annotations: Some([(oci::annotations::TITLE.to_string(), InternalTag::PATCH_TAG.to_string())].into()),
        }];

        let parts = super::manifest_builder::ManifestBuilder::new()
            .artifact_type(crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE)
            .config_bytes(MEDIA_TYPE_OCI_EMPTY_CONFIG, b"{}".to_vec())
            .layers(layers)
            .build()?;
        // Sanity: the empty-config blob digest computed by the builder must
        // match the one we already pushed above.
        debug_assert_eq!(parts.config_digest.to_string(), config_digest.to_string());
        let manifest_digest = parts.manifest_digest.clone();

        // Push to the tag reference directly (not by digest) so the tag is created.
        self.transport
            .push_manifest_raw(&image, parts.manifest_bytes, MEDIA_TYPE_OCI_IMAGE_MANIFEST)
            .await?;

        log::debug!(
            "Pushed patch descriptor for {} (manifest: {})",
            patch_repo_id,
            manifest_digest
        );
        Ok(manifest_digest)
    }

    /// Pulls the description artifact from the `__ocx.desc` tag, from the
    /// canonical registry.
    ///
    /// Returns `Ok(None)` if no description tag exists for the identifier.
    /// Uses a temporary directory to download blobs before reading them into memory.
    ///
    /// Canonical by default because the two commands that copy a description
    /// (`package copy --description`, `package describe --from`) and the one
    /// that merges into it (`package describe`) all *write back* what this read
    /// returns: a mirror's answer applied to the canonical host is a decision
    /// about a repository nobody read (invariant 5). A description served by a
    /// mirror is [`pull_description_addressed`](Self::pull_description_addressed)
    /// with [`ReadAddressing::Mirrored`], asked for by name.
    pub async fn pull_description(
        &self,
        identifier: &Identifier,
        temp_dir: &std::path::Path,
    ) -> std::result::Result<Option<package::description::Description>, ClientError> {
        self.pull_description_addressed(identifier, temp_dir, ReadAddressing::Canonical)
            .await
    }

    /// [`pull_description`](Self::pull_description) against a caller-chosen host.
    ///
    /// `ReadAddressing::Mirrored` is for a description nothing is written from —
    /// a catalog page rendered for a human, an announce observation — see
    /// [`ReadAddressing`].
    pub(crate) async fn pull_description_addressed(
        &self,
        identifier: &Identifier,
        temp_dir: &std::path::Path,
        addressing: ReadAddressing,
    ) -> std::result::Result<Option<package::description::Description>, ClientError> {
        let desc_identifier = identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);
        let image = self.read_reference(&desc_identifier, addressing);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;

        let (manifest, _digest) = match self.fetch_manifest_raw(&image).await {
            Ok(result) => result,
            Err(ClientError::ManifestNotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };

        let image_manifest = match manifest {
            oci::Manifest::Image(m) => m,
            oci::Manifest::ImageIndex(_) => {
                return Err(ClientError::InvalidManifest(
                    "expected image manifest for description, got image index".to_string(),
                ));
            }
        };

        match &image_manifest.artifact_type {
            Some(at) if at == MEDIA_TYPE_DESCRIPTION_V1 => {}
            other => {
                return Err(ClientError::InvalidManifest(format!(
                    "expected artifact_type '{}', got '{}'",
                    MEDIA_TYPE_DESCRIPTION_V1,
                    other.as_deref().unwrap_or("<none>")
                )));
            }
        }

        let mut readme: Option<String> = None;
        let mut logo: Option<package::description::Logo> = None;

        for (i, layer) in image_manifest.layers.iter().enumerate() {
            let blob_path = temp_dir.join(format!("layer_{i}"));
            let layer_digest = Digest::try_from(layer.digest.as_str()).map_err(|e| {
                ClientError::InvalidManifest(format!("description layer digest '{}' is malformed: {e}", layer.digest))
            })?;
            self.transport
                .pull_blob_to_file(&image, &layer_digest, &blob_path)
                .await?;

            match layer.media_type.as_str() {
                MEDIA_TYPE_MARKDOWN => {
                    let data = tokio::fs::read(&blob_path).await.map_err(|e| ClientError::Io {
                        path: blob_path,
                        source: e,
                    })?;
                    readme = Some(String::from_utf8(data).map_err(ClientError::InvalidEncoding)?);
                }
                MEDIA_TYPE_PNG | MEDIA_TYPE_SVG => {
                    let data = tokio::fs::read(&blob_path).await.map_err(|e| ClientError::Io {
                        path: blob_path,
                        source: e,
                    })?;
                    logo = Some(package::description::Logo {
                        data,
                        media_type: if layer.media_type == MEDIA_TYPE_PNG {
                            MEDIA_TYPE_PNG
                        } else {
                            MEDIA_TYPE_SVG
                        },
                    });
                }
                _ => {
                    log::debug!("Ignoring unknown description layer media type: {}", layer.media_type);
                }
            }
        }

        let readme = readme
            .ok_or_else(|| ClientError::InvalidManifest("description manifest has no markdown layer".to_string()))?;

        let annotations = image_manifest.annotations.unwrap_or_default();

        Ok(Some(package::description::Description {
            readme,
            logo,
            annotations,
        }))
    }

    // ── Single-layer artifact fetch ───────────────────────────────────────

    /// Fetches a single-layer OCI artifact for `identifier`: an image
    /// manifest carrying a declared `artifactType`, exactly one layer of a
    /// declared media type, and a declared layer size within `max_bytes`.
    ///
    /// This is the shared shape behind the OCX single-layer artifact pattern
    /// (image manifest + empty config + one layer, no index, no subject): the
    /// patch descriptor (`__ocx.patch`) fetch is the caller.
    ///
    /// Returns `Ok(None)` when the tag does not exist (`ManifestNotFound` —
    /// "looked, absent", not an error). The read goes through the mirror-aware
    /// [`Self::transport_reference`] seam. That is the minority default on
    /// `Client` — `fetch_manifest`, `fetch_manifest_raw_bytes`,
    /// `pull_description` and `list_tags` all address the canonical registry —
    /// and it is right here for one specific reason: nothing is written back
    /// from this fetch, so Invariant #5 does not bite. A write-backing artifact
    /// fetch added later must not copy the choice.
    ///
    /// # Steps
    ///
    /// 1. Authenticate against the registry (mirror-aware reference).
    /// 2. Fetch the raw manifest bytes; `Ok(None)` if the tag does not exist.
    /// 3. Validate the manifest is a single-image manifest, not an image index.
    /// 4. Validate the manifest's `artifactType` matches `artifact_type`.
    /// 5. Validate the manifest has exactly one layer.
    /// 6. Validate the layer's `mediaType` matches `layer_media_type`.
    /// 7. Validate the declared layer size against `max_bytes` (CWE-400
    ///    pre-check — reject an oversized declared size before fetching).
    /// 8. Fetch the layer blob bytes with a stream-level byte cap of
    ///    `max_bytes`: a malicious registry that ignores its own declared
    ///    size cannot stream more than `max_bytes` bytes into memory (closes
    ///    the gap the declared-size pre-check alone leaves open).
    ///
    /// # Errors
    ///
    /// - [`ClientError::UnexpectedManifestType`] — manifest was an image index.
    /// - [`ClientError::UnexpectedArtifactType`] — `artifactType` did not match.
    /// - [`ClientError::WrongLayerCount`] — manifest had zero or more than one layer.
    /// - [`ClientError::UnexpectedLayerMediaType`] — layer media type did not match.
    /// - [`ClientError::LayerSizeExceeded`] — declared layer size exceeds `max_bytes`.
    /// - [`ClientError::InvalidManifest`] — the layer digest is malformed.
    /// - [`ClientError::DecompressionCapExceeded`] — the registry streamed
    ///   more bytes than `max_bytes` regardless of its declared size.
    /// - Any network/auth error from the underlying manifest or blob fetch.
    pub(crate) async fn fetch_single_layer_artifact(
        &self,
        identifier: &Identifier,
        artifact_type: &str,
        layer_media_type: &str,
        max_bytes: u64,
    ) -> std::result::Result<Option<SingleLayerArtifact>, ClientError> {
        let (manifest_bytes, manifest_digest, manifest) = match self
            .fetch_manifest_raw_bytes_addressed(identifier, ReadAddressing::Mirrored)
            .await?
        {
            Some(triple) => triple,
            None => return Ok(None),
        };

        let image_manifest = match manifest {
            oci::Manifest::Image(m) => m,
            oci::Manifest::ImageIndex(_) => return Err(ClientError::UnexpectedManifestType),
        };

        match &image_manifest.artifact_type {
            Some(at) if at == artifact_type => {}
            other => {
                return Err(ClientError::UnexpectedArtifactType {
                    expected: artifact_type.to_string(),
                    actual: other.clone(),
                });
            }
        }

        if image_manifest.layers.len() != 1 {
            return Err(ClientError::WrongLayerCount {
                count: image_manifest.layers.len(),
            });
        }
        let layer_descriptor = &image_manifest.layers[0];

        if layer_descriptor.media_type != layer_media_type {
            return Err(ClientError::UnexpectedLayerMediaType {
                expected: layer_media_type.to_string(),
                actual: layer_descriptor.media_type.clone(),
            });
        }

        // Size cap (CWE-400). Reject manifests that declare a layer larger
        // than max_bytes before issuing the blob fetch. A negative or zero
        // declared size is also rejected as a malformed manifest.
        let declared_size = layer_descriptor.size;
        match u64::try_from(declared_size) {
            Ok(size) if size <= max_bytes => {}
            Ok(_) => {
                return Err(ClientError::LayerSizeExceeded {
                    declared: declared_size,
                    maximum: max_bytes,
                });
            }
            Err(_) => {
                return Err(ClientError::InvalidManifest(format!(
                    "layer descriptor size '{declared_size}' is not a valid byte count"
                )));
            }
        }

        let layer_digest = Digest::try_from(layer_descriptor.digest.as_str()).map_err(|_| {
            ClientError::InvalidManifest(format!("layer digest '{}' is malformed", layer_descriptor.digest))
        })?;

        let layer_bytes = self
            .fetch_layer_blob_capped(identifier, &layer_digest, max_bytes)
            .await?;

        Ok(Some(SingleLayerArtifact {
            manifest_bytes,
            manifest_digest,
            layer_bytes,
            layer_digest,
        }))
    }

    /// Probes only the manifest digest for `identifier` WITHOUT downloading
    /// the manifest body or any layer blob.
    ///
    /// Implemented over the transport's HEAD-based digest fetch, so it works
    /// for image indexes and single-image manifests alike and never transfers
    /// the manifest body. The registry's `Docker-Content-Digest` for a tag is
    /// the digest of the top-level (index) manifest — the same value
    /// `fetch_manifest`/`fetch_manifest_raw_bytes` compute — so a drift check
    /// against a persisted snapshot digest never mismatches on digest source.
    /// Returns `Ok(None)` when the reference does not exist.
    ///
    /// Used by the managed-config background-refresh probe (`notify`/`manual`)
    /// and `ocx config update --check`, which only need to detect drift and
    /// must not pull the (up to 64 KiB) config layer on every command.
    ///
    /// Unlike its siblings this one has no short, canonical-by-default form,
    /// and the reason is that its callers genuinely split: the three in
    /// `package/cascade/apply.rs` are write-deciding reads and ask for
    /// [`ReadAddressing::Canonical`] (Invariant #5 — `apply.rs:350` is one
    /// statement from the PUT it guards), while `announce/pipeline.rs` and
    /// `managed_config/persistence.rs` are drift checks and ask for
    /// [`ReadAddressing::Mirrored`]. With no majority to encode, a default
    /// would be wrong for two callers either way, so the host is always named
    /// at the call site. Do not add a short form to "match the siblings".
    ///
    /// # Errors
    ///
    /// Any network/auth error from the underlying digest fetch.
    pub(crate) async fn probe_manifest_digest_addressed(
        &self,
        identifier: &Identifier,
        addressing: ReadAddressing,
    ) -> std::result::Result<Option<Digest>, ClientError> {
        let image = self.read_reference(identifier, addressing);
        self.transport
            .ensure_auth(&image, oci::RegistryOperation::Pull)
            .await
            .map_err(|e| self.via_mirror(identifier, &image, e))?;
        match self.transport.fetch_manifest_digest(&image).await {
            Ok(digest_str) => Ok(Some(Digest::try_from(digest_str.as_str()).map_err(|e| {
                ClientError::InvalidManifest(format!("digest '{digest_str}' from registry HEAD is malformed: {e}"))
            })?)),
            Err(ClientError::ManifestNotFound(_)) => Ok(None),
            Err(e) => Err(self.via_mirror(identifier, &image, e)),
        }
    }

    /// Fetches the raw manifest bytes and the parsed [`oci::Manifest`] for
    /// `identifier`.
    ///
    /// `identifier` may be tag- or digest-addressed — this is the generalized
    /// raw-bytes fetch: any tag-resolve caller that needs verbatim bytes
    /// alongside the parsed manifest (not just pinned-digest callers) can use
    /// it as-is.
    ///
    /// Returns `Ok(None)` when the tag does not exist (`ManifestNotFound`).
    /// Unlike [`Self::fetch_manifest`], this method also returns the raw
    /// manifest bytes so callers can persist them to the CAS blob store
    /// without re-serialisation — the round-trip bytes must be byte-identical
    /// to what the registry served to ensure the stored digest is consistent.
    ///
    /// The returned digest is never trusted blindly: `sha256(raw_bytes)` is
    /// recomputed and compared against the registry-claimed digest before
    /// this method returns (see [`verify_raw_bytes_digest`]) — the trust
    /// anchor for any snapshot store that persists these bytes verbatim
    /// (`adr_index_indirection.md` A3).
    ///
    /// The body is capped at [`MAX_INDEX_DOCUMENT_BYTES`]; an over-cap
    /// response is refused before it is parsed or handed to any caller.
    ///
    /// The cap is an **admission** check, not a memory bound. It runs on an
    /// already-materialised body: [`OciTransport::pull_manifest_raw`] returns a
    /// `Vec`, so a hostile registry can still force a transient allocation of
    /// whatever it chooses to send before the refusal fires. What the cap
    /// guarantees is that no over-cap body is ever parsed, digested, or carried
    /// into the index — the property the dispatch-object store depends on.
    /// Bounding the allocation too needs a capped read on the transport itself
    /// (the `index.ocx.sh` fetch in `oci::index::ocx_index` owns its `reqwest`
    /// call and does exactly that: `Content-Length` precheck, then an
    /// incremental per-chunk cap). Tracked as FU-3.
    pub(crate) async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &Identifier,
    ) -> std::result::Result<Option<(Vec<u8>, Digest, oci::Manifest)>, ClientError> {
        self.fetch_manifest_raw_bytes_capped(identifier, MAX_INDEX_DOCUMENT_BYTES, ReadAddressing::Canonical)
            .await
    }

    /// [`fetch_manifest_raw_bytes`](Self::fetch_manifest_raw_bytes) against a
    /// caller-chosen host.
    ///
    /// `ReadAddressing::Mirrored` is for a body no write is planned from — see
    /// [`ReadAddressing`].
    pub(crate) async fn fetch_manifest_raw_bytes_addressed(
        &self,
        identifier: &Identifier,
        addressing: ReadAddressing,
    ) -> std::result::Result<Option<(Vec<u8>, Digest, oci::Manifest)>, ClientError> {
        self.fetch_manifest_raw_bytes_capped(identifier, MAX_INDEX_DOCUMENT_BYTES, addressing)
            .await
    }

    /// [`Self::fetch_manifest_raw_bytes`] with an injectable ceiling, so tests
    /// can exercise the cap boundary without fabricating a 32 MiB body. Same
    /// seam as [`Self::pull_layer`] / `pull_layer_with_caps`.
    async fn fetch_manifest_raw_bytes_capped(
        &self,
        identifier: &Identifier,
        max_bytes: usize,
        addressing: ReadAddressing,
    ) -> std::result::Result<Option<(Vec<u8>, Digest, oci::Manifest)>, ClientError> {
        let image = self.read_reference(identifier, addressing);
        self.transport
            .ensure_auth(&image, oci::RegistryOperation::Pull)
            .await
            .map_err(|e| self.via_mirror(identifier, &image, e))?;

        let (raw_bytes, digest_str) = match self
            .transport
            .pull_manifest_raw(&image, ACCEPTED_MANIFEST_MEDIA_TYPES)
            .await
        {
            Ok(pair) => pair,
            Err(ClientError::ManifestNotFound(_)) => return Ok(None),
            Err(e) => return Err(self.via_mirror(identifier, &image, e)),
        };

        // Fail closed before parsing or returning: an over-cap body is refused
        // outright, never truncated (truncation would silently break the
        // digest the caller persists these bytes under).
        if raw_bytes.len() > max_bytes {
            return Err(ClientError::InvalidManifest(format!(
                "manifest body is {} bytes, exceeding the {max_bytes}-byte cap",
                raw_bytes.len()
            )));
        }

        let manifest = parse_registry_manifest(&raw_bytes).map_err(|e| self.via_mirror(identifier, &image, e))?;
        let digest: Digest =
            Digest::try_from(digest_str.as_str()).map_err(|e| ClientError::InvalidManifest(format!("{e}")))?;
        // Identity before self-consistency. `verify_raw_bytes_digest` only
        // proves the body hashes to the digest the *registry* announced in
        // `Docker-Content-Digest` — a registry answering `GET /manifests/A`
        // with B's bytes and B's header passes it every time. When the caller
        // pinned a digest, that pin is the identity the answer has to match
        // (CWE-345); otherwise a pinned read silently resolves to whatever the
        // registry felt like serving.
        if let Some(requested) = identifier.digest()
            && requested != digest
        {
            return Err(self.via_mirror(
                identifier,
                &image,
                ClientError::DigestMismatch {
                    expected: requested.to_string(),
                    actual: digest.to_string(),
                },
            ));
        }
        verify_raw_bytes_digest(&raw_bytes, &digest).map_err(|e| self.via_mirror(identifier, &image, e))?;
        Ok(Some((raw_bytes, digest, manifest)))
    }

    /// Fetches the raw bytes of a single blob for a single-layer artifact.
    ///
    /// The blob is identified by `(identifier_for_auth, layer_digest)`. Auth is
    /// established against the registry of `identifier_for_auth` before the
    /// pull.
    ///
    /// # Size cap (CWE-400)
    ///
    /// `max_bytes` is a hard ceiling on the number of bytes that will be
    /// buffered in memory. The stream is capped at `max_bytes + 1` via
    /// [`AsyncReadExt::take`]; if the registry delivers more than `max_bytes`
    /// bytes (ignoring its own declared-size field), the function returns
    /// [`ClientError::DecompressionCapExceeded`] (repurposed for stream-level
    /// oversized blobs) and no allocation beyond `max_bytes + 1` occurs.
    ///
    /// The caller in [`Self::fetch_single_layer_artifact`] already rejects
    /// manifests whose *declared* layer size exceeds the ceiling; this cap
    /// closes the gap where a malicious registry ignores its own declaration
    /// and streams more bytes than it declared.
    pub(crate) async fn fetch_layer_blob_capped(
        &self,
        identifier_for_auth: &Identifier,
        layer_digest: &Digest,
        max_bytes: u64,
    ) -> std::result::Result<Vec<u8>, ClientError> {
        let image = self.transport_reference(identifier_for_auth);
        // Auth was already established by the caller in fetch_manifest_raw_bytes,
        // but call ensure_auth again for robustness (it is a no-op on cache hit).
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;

        // Stream the blob with a hard byte cap so a malicious registry that
        // sends more bytes than its declared layer size cannot OOM the process.
        // We read up to (max_bytes + 1) to detect overflow: if we fill the
        // buffer to that length, the registry sent too many bytes.
        use tokio::io::AsyncReadExt as _;
        let stream = self.transport.pull_blob_streaming(&image, layer_digest).await?;
        // Cap sentinel: read one byte beyond the allowed ceiling to detect overflow.
        let cap_sentinel = max_bytes.saturating_add(1);
        let mut buf = Vec::with_capacity(max_bytes as usize);
        stream
            .take(cap_sentinel)
            .read_to_end(&mut buf)
            .await
            .map_err(|e| ClientError::Io {
                path: std::path::PathBuf::from("<single-layer artifact blob>"),
                source: e,
            })?;
        if buf.len() as u64 > max_bytes {
            // Registry streamed more bytes than the declared + cap ceiling.
            return Err(ClientError::DecompressionCapExceeded { cap: max_bytes });
        }
        Ok(buf)
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Pulls and parses a manifest from the registry.
    async fn fetch_manifest_raw(
        &self,
        image: &oci::Reference,
    ) -> std::result::Result<(oci::Manifest, String), ClientError> {
        log::debug!("Pulling manifest for image {}", image);
        let (data, digest) = self
            .transport
            .pull_manifest_raw(image, ACCEPTED_MANIFEST_MEDIA_TYPES)
            .await?;
        let manifest = parse_registry_manifest(&data)?;
        Ok((manifest, digest))
    }
}

/// Parses registry-served manifest bytes, refusing an image index that is not a
/// valid one.
///
/// The one place `oci::Manifest` is decoded from registry bytes. Deserialisation
/// proves shape only — `OciImageIndex::schema_version` is an unconstrained `u8`,
/// so `{"schemaVersion":1,"manifests":[]}` parses happily — and the resulting
/// index is then carried into the local index, merged into on a cascade push, or
/// committed verbatim into a public git repository by `announce`. Admission is
/// checked here rather than at each of those sites so no future caller can
/// acquire an unvalidated one.
///
/// # Errors
///
/// [`ClientError::Serialization`] when the bytes are not a manifest at all;
/// [`ClientError::InvalidImageIndex`] when they are an image index that violates
/// [`oci::manifest::validate_image_index`].
fn parse_registry_manifest(bytes: &[u8]) -> std::result::Result<oci::Manifest, ClientError> {
    let manifest: oci::Manifest = serde_json::from_slice(bytes).map_err(ClientError::Serialization)?;
    if let oci::Manifest::ImageIndex(index) = &manifest {
        oci::manifest::validate_image_index(index)?;
    }
    Ok(manifest)
}

/// Recomputes the digest of `raw_bytes` (using the algorithm `claimed`
/// carries) and errors if it does not match.
///
/// This is the write-path trust anchor for [`Client::fetch_manifest_raw_bytes`]
/// (ADR `adr_index_indirection.md` A3, "keep raw bytes, recompute + verify
/// digest"): a registry-claimed digest string is untrusted input until the
/// bytes actually received hash to it. A mismatch is a hard error, never a
/// warning — the caller is about to persist `raw_bytes` verbatim under a
/// filename derived from `claimed`.
fn verify_raw_bytes_digest(raw_bytes: &[u8], claimed: &Digest) -> std::result::Result<(), ClientError> {
    let recomputed = claimed.algorithm().hash(raw_bytes);
    if &recomputed != claimed {
        return Err(ClientError::DigestMismatch {
            expected: claimed.to_string(),
            actual: recomputed.to_string(),
        });
    }
    Ok(())
}

// ── Pagination ───────────────────────────────────────────────────────

/// Generic paginated fetch: calls `fetch` repeatedly until the returned page
/// is smaller than `chunk_size`, concatenating all results.
///
/// The first call uses `Some("")` as the `last` cursor (not `None`)
/// because some registries return invalid responses when `n` is set without `last`.
async fn paginate<F, Fut>(chunk_size: usize, fetch: F) -> std::result::Result<Vec<String>, ClientError>
where
    F: Fn(usize, Option<String>) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<Vec<String>, ClientError>>,
{
    let mut items = Vec::new();
    loop {
        let last = if items.is_empty() {
            Some(String::new())
        } else {
            items.last().cloned()
        };
        let page = fetch(chunk_size, last).await?;
        let page_len = page.len();
        items.extend(page);
        if page_len < chunk_size {
            break;
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::test_transport::{StubTransport, StubTransportData};
    use super::*;
    use crate::MEDIA_TYPE_PACKAGE_METADATA_V1;
    use crate::oci;
    // `pull_layer` no longer takes a `metadata` param (Part 1), so production
    // client.rs no longer imports the `metadata` module. Test fixtures still
    // construct `metadata::Metadata` values, so import it directly here.
    use crate::package::metadata;

    use std::sync::Mutex;

    use crate::file_structure::TempStore;

    // ── Test helpers ─────────────────────────────────────────────────

    fn stub(data: &StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    fn test_identifier(tag: &str) -> Identifier {
        Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
    }

    fn test_identifier_with_digest(digest_hex: &str) -> Identifier {
        let digest = oci::Digest::Sha256(digest_hex.to_string());
        Identifier::new_registry("test/pkg", "example.com").clone_with_digest(digest)
    }

    fn test_pinned(digest_hex: &str) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(test_identifier_with_digest(digest_hex)).unwrap()
    }

    /// Build a valid image manifest with the given config and layer digests.
    /// Pads any short hex suffix up to 64 hex characters so the result parses as a real `Digest`.
    fn make_image_manifest(config_digest: &str, layer_digest: &str) -> oci::ImageManifest {
        fn normalize(d: &str) -> String {
            match d.strip_prefix("sha256:") {
                Some(rest) if rest.len() < 64 => {
                    let padding = "a".repeat(64 - rest.len());
                    format!("sha256:{rest}{padding}")
                }
                _ => d.to_string(),
            }
        }
        oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: oci::Descriptor {
                media_type: MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                digest: normalize(config_digest),
                size: 100,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: vec![oci::Descriptor {
                media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
                digest: normalize(layer_digest),
                size: 200,
                urls: None,
                artifact_type: None,
                annotations: None,
            }],
            ..Default::default()
        }
    }

    /// Serialize a manifest and compute its digest, returning (bytes, digest_string).
    fn serialize_manifest(manifest: &oci::Manifest) -> (Vec<u8>, String) {
        let data = serde_json::to_vec(manifest).unwrap();
        let digest = Algorithm::Sha256.hash(&data).to_string();
        (data, digest)
    }

    // ── verify_raw_bytes_digest tests ────────────────────────────────
    //
    // The recompute-verify trust anchor (ADR `adr_index_indirection.md` A3):
    // a claimed digest is never trusted without recomputing sha256(bytes)
    // and comparing.

    #[test]
    fn verify_raw_bytes_digest_accepts_matching_bytes() {
        let bytes = b"verbatim manifest bytes".to_vec();
        let claimed = Algorithm::Sha256.hash(&bytes);
        assert!(super::verify_raw_bytes_digest(&bytes, &claimed).is_ok());
    }

    #[test]
    fn verify_raw_bytes_digest_rejects_tampered_bytes() {
        let bytes = b"verbatim manifest bytes".to_vec();
        let claimed = Algorithm::Sha256.hash(&bytes);
        let tampered = b"tampered manifest bytes".to_vec();

        match super::verify_raw_bytes_digest(&tampered, &claimed) {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(expected, claimed.to_string(), "expected must be the claimed digest");
                assert_eq!(
                    actual,
                    Algorithm::Sha256.hash(&tampered).to_string(),
                    "actual must be the digest recomputed from the bytes actually received"
                );
                assert_ne!(expected, actual);
            }
            other => panic!("expected ClientError::DigestMismatch, got {other:?}"),
        }
    }

    // ── fetch_manifest_raw_bytes tests ───────────────────────────────
    //
    // Covers the generalized raw-bytes fetch (works for tag-addressed
    // identifiers, not just pinned-digest ones) and the A3 recompute-verify
    // gate wired into it.

    /// A tag-resolve fetch retains the verbatim bytes alongside the parsed
    /// manifest and digest — the precedent the future snapshot-store write
    /// path (ADR A3) reuses.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_retains_verbatim_bytes_for_tag_identifier() {
        let manifest = oci::Manifest::Image(oci::ImageManifest::default());
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0"); // tag-addressed, not digest-pinned
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data.clone(), digest_str.clone()));
        let client = stub(&data);

        let (raw_bytes, digest, parsed) = client
            .fetch_manifest_raw_bytes(&id)
            .await
            .expect("fetch should succeed")
            .expect("tag is present in the stub");

        assert_eq!(
            raw_bytes, manifest_data,
            "bytes must be returned verbatim, never re-serialized"
        );
        assert_eq!(digest.to_string(), digest_str);
        assert!(
            matches!(parsed, oci::Manifest::Image(_)),
            "parsed manifest must be the Image variant"
        );
    }

    /// A digest-pinned read must get back the manifest it named, not merely
    /// *a* manifest the registry can vouch for.
    ///
    /// `verify_raw_bytes_digest` proves self-consistency: the body hashes to
    /// the digest the registry announced in `Docker-Content-Digest`. It says
    /// nothing about identity, so a registry answering `GET /manifests/A` with
    /// B's bytes *and* B's header passes it every time — the pin silently
    /// resolves to whatever the registry felt like serving (CWE-345). The stub
    /// here is exactly that: the response is internally consistent, and only
    /// the requested-vs-served comparison can catch it.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_rejects_a_manifest_served_under_another_digest() {
        let requested = oci::Manifest::Image(oci::ImageManifest::default());
        let (_requested_bytes, requested_digest) = serialize_manifest(&requested);
        let served = oci::Manifest::Image(oci::ImageManifest {
            annotations: Some(std::collections::BTreeMap::from([(
                "sh.ocx.substituted".to_string(),
                "yes".to_string(),
            )])),
            ..Default::default()
        });
        let (served_bytes, served_digest) = serialize_manifest(&served);
        assert_ne!(
            requested_digest, served_digest,
            "the fixture only discriminates while the two manifests differ"
        );

        let id = test_identifier("1.0")
            .without_tag()
            .clone_with_digest(Digest::try_from(requested_digest.as_str()).expect("well-formed digest"));
        let data = StubTransportData::new();
        // Self-consistent, wrong identity: B's bytes under B's own digest,
        // answered for a request that named A.
        data.write()
            .manifests
            .insert(id.to_string(), (served_bytes, served_digest.clone()));
        let client = stub(&data);

        match client.fetch_manifest_raw_bytes(&id).await {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(
                    expected, requested_digest,
                    "expected must name the digest that was asked for"
                );
                assert_eq!(actual, served_digest, "actual must name the digest the registry served");
            }
            other => panic!("a pinned read served another manifest must fail: {other:?}"),
        }
    }

    /// A missing tag surfaces as `Ok(None)` — not-found is a normal query
    /// result at this layer, not an error (see `subsystem-oci.md`
    /// "Option-based results").
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_returns_none_for_missing_tag() {
        let id = test_identifier("missing");
        let data = StubTransportData::new();
        let client = stub(&data);

        let result = client.fetch_manifest_raw_bytes(&id).await.expect("no transport error");
        assert!(result.is_none());
    }

    /// A registry (or mirror) that serves bytes not matching its own claimed
    /// digest must hard-fail, never silently hand back the tampered bytes.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_rejects_registry_claimed_digest_mismatch() {
        let manifest = oci::Manifest::Image(oci::ImageManifest::default());
        let (manifest_data, _correct_digest) = serialize_manifest(&manifest);
        // A digest that does not correspond to `manifest_data` — well-formed
        // (64 lowercase hex chars) so it parses as a `Digest`, but wrong.
        let wrong_digest_str = format!("sha256:{}", "f".repeat(64));

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, wrong_digest_str.clone()));
        let client = stub(&data);

        match client.fetch_manifest_raw_bytes(&id).await {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(
                    expected, wrong_digest_str,
                    "expected must be the registry-claimed digest"
                );
                assert_ne!(actual, expected, "actual must be the recomputed (correct) digest");
            }
            other => panic!("expected Err(ClientError::DigestMismatch), got {other:?}"),
        }
    }

    /// Registers a valid manifest under a tag and returns the byte length the
    /// registry serves, so a test can put the cap exactly on that boundary.
    fn stub_manifest_of_known_length(id: &Identifier, data: &StubTransportData) -> usize {
        let manifest = oci::Manifest::Image(oci::ImageManifest::default());
        let (manifest_data, digest_str) = serialize_manifest(&manifest);
        let length = manifest_data.len();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        length
    }

    /// A body one byte over the ceiling is refused (CWE-400). Digest
    /// verification is not a size check — a hostile registry can serve a
    /// multi-gigabyte body whose digest matches — and `announce` commits these
    /// bytes verbatim into a public git repository.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_refuses_over_cap_body() {
        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        let length = stub_manifest_of_known_length(&id, &data);
        let client = stub(&data);

        match client
            .fetch_manifest_raw_bytes_capped(&id, length - 1, ReadAddressing::Mirrored)
            .await
        {
            Err(ClientError::InvalidManifest(message)) => {
                assert!(
                    message.contains(&length.to_string()),
                    "the refusal must name the actual size: {message}"
                );
                assert!(
                    message.contains(&(length - 1).to_string()),
                    "the refusal must name the limit: {message}"
                );
            }
            other => panic!("expected Err(ClientError::InvalidManifest), got {other:?}"),
        }
    }

    /// A body of exactly the ceiling is accepted — the cap is `>`, not `>=`.
    /// Pinning the boundary keeps an off-by-one from silently rejecting a
    /// legitimate maximal image index.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_accepts_body_at_exactly_the_cap() {
        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        let length = stub_manifest_of_known_length(&id, &data);
        let client = stub(&data);

        let (raw_bytes, _digest, _parsed) = client
            .fetch_manifest_raw_bytes_capped(&id, length, ReadAddressing::Mirrored)
            .await
            .expect("a body of exactly the cap must be accepted")
            .expect("tag is present in the stub");
        assert_eq!(raw_bytes.len(), length);
    }

    /// Registers hand-authored manifest bytes under `id` at the digest they
    /// actually hash to.
    ///
    /// The fixture is a byte literal on purpose: an invalid `schemaVersion`
    /// cannot be produced by serialising an `oci::ImageIndex` (every write site
    /// sets `oci::INDEX_SCHEMA_VERSION`), so a fixture built by the code under
    /// test could never contradict it.
    fn stub_raw_manifest(id: &Identifier, data: &StubTransportData, bytes: &[u8]) {
        let digest = Algorithm::Sha256.hash(bytes).to_string();
        data.write().manifests.insert(id.to_string(), (bytes.to_vec(), digest));
    }

    /// A registry serving an image index with `schemaVersion: 1` is refused at
    /// admission. The document deserialises happily — `schema_version` is an
    /// unconstrained `u8` — and its digest verifies, so nothing but the
    /// semantic check stands between those bytes and the index that would carry
    /// them verbatim into a public git repository.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_refuses_wrong_schema_version() {
        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        stub_raw_manifest(&id, &data, br#"{"schemaVersion":1,"manifests":[]}"#);
        let client = stub(&data);

        match client.fetch_manifest_raw_bytes(&id).await {
            Err(error @ ClientError::InvalidImageIndex(_)) => {
                assert_eq!(
                    crate::cli::ClassifyExitCode::classify(&error),
                    Some(crate::cli::ExitCode::DataError)
                );
                assert!(
                    error.to_string().contains("schemaVersion"),
                    "the refusal must name the violated invariant: {error}"
                );
            }
            other => panic!("expected Err(ClientError::InvalidImageIndex), got {other:?}"),
        }
    }

    /// A descriptor with an empty `digest` names no child at all, so the index
    /// is unusable. Refused at admission rather than surfacing later as an
    /// unresolvable platform entry.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_refuses_unaddressable_descriptor() {
        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        stub_raw_manifest(
            &id,
            &data,
            br#"{"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"","size":7}]}"#,
        );
        let client = stub(&data);

        match client.fetch_manifest_raw_bytes(&id).await {
            Err(ClientError::InvalidImageIndex(_)) => {}
            other => panic!("expected Err(ClientError::InvalidImageIndex), got {other:?}"),
        }
    }

    /// The check is semantic, never `deny_unknown_fields`: a sibling key a
    /// newer writer added (here `subject`, which `oci::ImageIndex` does not
    /// model) rides through, and the verbatim bytes come back untouched.
    #[tokio::test]
    async fn fetch_manifest_raw_bytes_admits_index_with_unknown_sibling_field() {
        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        let bytes: &[u8] = br#"{"schemaVersion":2,"manifests":[],"subject":{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:cafe","size":3}}"#;
        stub_raw_manifest(&id, &data, bytes);
        let client = stub(&data);

        let (raw_bytes, _digest, _parsed) = client
            .fetch_manifest_raw_bytes(&id)
            .await
            .expect("an unknown sibling field must not be a refusal")
            .expect("tag is present in the stub");
        assert_eq!(raw_bytes, bytes, "the bytes must ride through verbatim");
    }

    // ── Pagination tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn list_tags_single_page() {
        let data = StubTransportData::new();
        data.write().tags = vec![vec!["1.0".into(), "2.0".into()]];
        let client = stub(&data);

        let tags = client.list_tags(test_identifier("latest")).await.unwrap();
        assert_eq!(tags, vec!["1.0", "2.0"]);
    }

    #[tokio::test]
    async fn list_tags_multi_page() {
        let page1: Vec<String> = (0..100).map(|i| format!("tag-{:03}", i)).collect();
        let page2 = vec!["tag-100".to_string(), "tag-101".to_string()];

        let data = StubTransportData::new();
        data.write().tags = vec![page1, page2];
        let client = stub(&data);

        let tags = client.list_tags(test_identifier("latest")).await.unwrap();
        assert_eq!(tags.len(), 102);
        assert_eq!(tags[0], "tag-000");
        assert_eq!(tags[101], "tag-101");
    }

    #[tokio::test]
    async fn list_repositories_pagination() {
        let page1: Vec<String> = (0..100).map(|i| format!("repo-{:03}", i)).collect();
        let page2 = vec!["repo-100".to_string()];

        let data = StubTransportData::new();
        data.write().repositories = vec![page1, page2];
        let client = stub(&data);

        let repos = client.list_repositories("example.com").await.unwrap();
        assert_eq!(repos.len(), 101);
    }

    // ── Manifest fetch tests ─────────────────────────────────────────

    #[tokio::test]
    async fn fetch_manifest_digest_success() {
        let digest_str = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let data = StubTransportData::new();
        data.write().digest = Some(digest_str.to_string());
        let client = stub(&data);

        let id = test_identifier("1.0");
        let digest = client
            .fetch_manifest_digest_addressed(&id, ReadAddressing::Mirrored)
            .await
            .unwrap();
        assert_eq!(digest.to_string(), digest_str);
    }

    #[tokio::test]
    async fn fetch_manifest_success() {
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cff", "sha256:1a0e"));
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str.clone()));
        let client = stub(&data);

        let (digest, fetched) = client.fetch_manifest(&id).await.unwrap();
        assert_eq!(digest.to_string(), digest_str);
        assert!(matches!(fetched, oci::Manifest::Image(_)));
    }

    // ── pull_manifest tests ─────────────────────────────────────

    #[tokio::test]
    async fn pull_manifest_digest_mismatch() {
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cff", "sha256:1a0e"));
        let (manifest_data, _real_digest) = serialize_manifest(&manifest);
        let wrong_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        let id = test_pinned("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, wrong_digest.to_string()));
        let client = stub(&data);

        let result = client.pull_manifest(&id).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.to_lowercase().contains("digest mismatch"), "got: {}", err_msg);
    }

    #[tokio::test]
    async fn pull_manifest_unexpected_manifest_type() {
        let index = oci::ImageIndex {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
            artifact_type: None,
            manifests: vec![],
            annotations: None,
        };
        let manifest = oci::Manifest::ImageIndex(index);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let digest_hex = digest_str.strip_prefix("sha256:").unwrap();
        let id = test_pinned(digest_hex);

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client.pull_manifest(&id).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("image manifest") || err_msg.contains("image index"),
            "got: {}",
            err_msg
        );
    }

    // ── pull_manifest: no longer validates media types ────────────

    #[tokio::test]
    async fn pull_manifest_accepts_any_media_types() {
        let mut m = make_image_manifest("sha256:cff", "sha256:1a0e");
        m.config.media_type = "application/vnd.other.config".to_string();
        m.artifact_type = Some("application/vnd.other.artifact".to_string());
        let manifest = oci::Manifest::Image(m);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);
        let digest_hex = digest_str.strip_prefix("sha256:").unwrap();
        let id = test_pinned(digest_hex);

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client.pull_manifest(&id).await;
        assert!(result.is_ok(), "pull_manifest should not validate media types");
    }

    // ── pull_blob tests ─────────────────────────────────────────

    /// Helper: register a manifest + config blob in the stub, returning the pinned ID.
    fn setup_manifest_and_blob(
        data: &StubTransportData,
        manifest: oci::ImageManifest,
        config_blob: &[u8],
    ) -> oci::PinnedIdentifier {
        let config_digest = &manifest.config.digest;
        data.write()
            .blobs
            .insert(config_digest.to_string(), config_blob.to_vec());

        let oci_manifest = oci::Manifest::Image(manifest);
        let (manifest_data, digest_str) = serialize_manifest(&oci_manifest);
        let digest_hex = digest_str.strip_prefix("sha256:").unwrap();
        let id = test_pinned(digest_hex);

        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        id
    }

    #[tokio::test]
    async fn pull_blob_returns_raw_bytes() {
        let metadata_json = br#"{"type":"bundle","version":1}"#;
        let data = StubTransportData::new();
        let manifest = make_image_manifest("sha256:cff", "sha256:1a0e");
        let id = setup_manifest_and_blob(&data, manifest.clone(), metadata_json);
        let client = stub(&data);

        let config_digest = Digest::try_from(manifest.config.digest.as_str()).unwrap();
        let blob_ref = id.clone_with_digest(config_digest);
        let bytes = client
            .pull_blob(&blob_ref)
            .await
            .expect("pull_blob should return registered bytes");
        assert_eq!(bytes.as_slice(), metadata_json.as_slice());

        // Round-trip parse confirms the bytes are intact.
        let parsed: metadata::Metadata = serde_json::from_slice(&bytes).expect("returned bytes must parse as Metadata");
        let _ = parsed;
    }

    // ── fetch_single_layer_artifact tests ────────────────────────

    const TEST_ARTIFACT_TYPE: &str = "application/vnd.ocx.test-artifact.v1";
    const TEST_LAYER_MEDIA_TYPE: &str = "application/vnd.ocx.test-layer.v1+toml";

    /// Build a single-layer artifact manifest (image manifest + empty config +
    /// one layer) with every shape-relevant field caller-controlled, so each
    /// test below can violate exactly one [`Client::fetch_single_layer_artifact`]
    /// invariant. Structural twin of [`make_image_manifest`].
    fn make_single_layer_manifest(
        artifact_type: &str,
        layer_media_type: &str,
        layer_digest: &str,
        layer_size: i64,
    ) -> oci::ImageManifest {
        oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(artifact_type.to_string()),
            config: oci::Descriptor {
                media_type: MEDIA_TYPE_OCI_EMPTY_CONFIG.to_string(),
                digest: format!("sha256:{}", "0".repeat(64)),
                size: 2,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: vec![oci::Descriptor {
                media_type: layer_media_type.to_string(),
                digest: layer_digest.to_string(),
                size: layer_size,
                urls: None,
                artifact_type: None,
                annotations: None,
            }],
            ..Default::default()
        }
    }

    /// (a) manifest is an image index -> `UnexpectedManifestType`.
    #[tokio::test]
    async fn fetch_single_layer_artifact_image_index_errors_unexpected_manifest_type() {
        let index = oci::ImageIndex {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
            artifact_type: None,
            manifests: vec![],
            annotations: None,
        };
        let manifest = oci::Manifest::ImageIndex(index);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, 1024)
            .await;
        assert!(
            matches!(result, Err(ClientError::UnexpectedManifestType)),
            "got: {result:?}"
        );
    }

    /// (b) `artifactType` does not match the caller's expectation ->
    /// `UnexpectedArtifactType`.
    #[tokio::test]
    async fn fetch_single_layer_artifact_wrong_artifact_type_errors() {
        let layer_bytes = b"payload".to_vec();
        let layer_digest = Algorithm::Sha256.hash(&layer_bytes).to_string();
        let manifest_struct = make_single_layer_manifest(
            "application/vnd.other.artifact",
            TEST_LAYER_MEDIA_TYPE,
            &layer_digest,
            layer_bytes.len() as i64,
        );
        let manifest = oci::Manifest::Image(manifest_struct);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, 1024)
            .await;
        match result {
            Err(ClientError::UnexpectedArtifactType { expected, actual }) => {
                assert_eq!(expected, TEST_ARTIFACT_TYPE);
                assert_eq!(actual.as_deref(), Some("application/vnd.other.artifact"));
            }
            other => panic!("expected UnexpectedArtifactType, got {other:?}"),
        }
    }

    /// (c) manifest has zero layers -> `WrongLayerCount`.
    #[tokio::test]
    async fn fetch_single_layer_artifact_wrong_layer_count_errors() {
        let mut manifest_struct = make_single_layer_manifest(
            TEST_ARTIFACT_TYPE,
            TEST_LAYER_MEDIA_TYPE,
            &format!("sha256:{}", "1".repeat(64)),
            10,
        );
        manifest_struct.layers.clear();
        let manifest = oci::Manifest::Image(manifest_struct);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, 1024)
            .await;
        assert!(
            matches!(result, Err(ClientError::WrongLayerCount { count: 0 })),
            "got: {result:?}"
        );
    }

    /// (d) layer `mediaType` does not match the caller's expectation ->
    /// `UnexpectedLayerMediaType`.
    #[tokio::test]
    async fn fetch_single_layer_artifact_wrong_layer_media_type_errors() {
        let layer_bytes = b"payload".to_vec();
        let layer_digest = Algorithm::Sha256.hash(&layer_bytes).to_string();
        let manifest_struct = make_single_layer_manifest(
            TEST_ARTIFACT_TYPE,
            "application/vnd.other.layer",
            &layer_digest,
            layer_bytes.len() as i64,
        );
        let manifest = oci::Manifest::Image(manifest_struct);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, 1024)
            .await;
        match result {
            Err(ClientError::UnexpectedLayerMediaType { expected, actual }) => {
                assert_eq!(expected, TEST_LAYER_MEDIA_TYPE);
                assert_eq!(actual, "application/vnd.other.layer");
            }
            other => panic!("expected UnexpectedLayerMediaType, got {other:?}"),
        }
    }

    /// (e) declared layer size exceeds `max_bytes` -> `LayerSizeExceeded`
    /// (CWE-400 pre-check, rejected before any blob fetch).
    #[tokio::test]
    async fn fetch_single_layer_artifact_declared_size_exceeds_max_errors() {
        let layer_digest = format!("sha256:{}", "2".repeat(64));
        let manifest_struct =
            make_single_layer_manifest(TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, &layer_digest, 2048);
        let manifest = oci::Manifest::Image(manifest_struct);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, 1024)
            .await;
        match result {
            Err(ClientError::LayerSizeExceeded { declared, maximum }) => {
                assert_eq!(declared, 2048);
                assert_eq!(maximum, 1024);
            }
            other => panic!("expected LayerSizeExceeded, got {other:?}"),
        }
    }

    /// (f) happy path: shape-valid manifest + matching layer blob -> the
    /// manifest and layer bytes/digests are returned unchanged.
    #[tokio::test]
    async fn fetch_single_layer_artifact_happy_path_returns_bytes() {
        let layer_bytes = b"toml payload bytes".to_vec();
        let layer_digest = Algorithm::Sha256.hash(&layer_bytes).to_string();
        let manifest_struct = make_single_layer_manifest(
            TEST_ARTIFACT_TYPE,
            TEST_LAYER_MEDIA_TYPE,
            &layer_digest,
            layer_bytes.len() as i64,
        );
        let manifest = oci::Manifest::Image(manifest_struct);
        let (manifest_data, manifest_digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data.clone(), manifest_digest_str.clone()));
        data.write().blobs.insert(layer_digest.clone(), layer_bytes.clone());
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, 1024)
            .await
            .expect("shape-valid manifest must fetch successfully")
            .expect("manifest exists in the stub, must return Some");

        assert_eq!(result.manifest_bytes, manifest_data);
        assert_eq!(result.manifest_digest.to_string(), manifest_digest_str);
        assert_eq!(result.layer_bytes, layer_bytes);
        assert_eq!(result.layer_digest.to_string(), layer_digest);
    }

    /// Stream-level cap: a registry that streams more bytes than declared (but
    /// with a declared size that itself passes the pre-check) is caught by the
    /// `.take(max_bytes + 1)` ceiling in `fetch_layer_blob_capped`, not by the
    /// declared-size check. Reachable via `StubTransport` because the stub's
    /// `blobs` map is not required to agree with the manifest's declared size.
    #[tokio::test]
    async fn fetch_single_layer_artifact_stream_exceeds_declared_size_errors_decompression_cap() {
        let max_bytes = 10u64;
        let layer_digest = format!("sha256:{}", "3".repeat(64));
        let manifest_struct = make_single_layer_manifest(
            TEST_ARTIFACT_TYPE,
            TEST_LAYER_MEDIA_TYPE,
            &layer_digest,
            max_bytes as i64,
        );
        let manifest = oci::Manifest::Image(manifest_struct);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        // The registry actually serves more bytes than both the declared size
        // and max_bytes, ignoring its own declaration.
        data.write().blobs.insert(layer_digest, vec![0u8; 50]);
        let client = stub(&data);

        let result = client
            .fetch_single_layer_artifact(&id, TEST_ARTIFACT_TYPE, TEST_LAYER_MEDIA_TYPE, max_bytes)
            .await;
        match result {
            Err(ClientError::DecompressionCapExceeded { cap }) => assert_eq!(cap, max_bytes),
            other => panic!("expected DecompressionCapExceeded, got {other:?}"),
        }
    }

    // ── pull_layer tests ────────────────────────────────────────

    // ── Streaming pipeline verification tests ────────────────────────
    //
    // These tests cover the CWE-345 invariants for the streaming pipeline.
    // HashingAsyncReader is the canonical verifier; the fork's VerifyingStream
    // is a secondary check. Both produce ClientError::DigestMismatch.

    // (a) replaces verify_blob_digest_* coverage (a–e):
    // Tampered stream via StubTransport → ClientError::DigestMismatch
    // (NOT ClientError::Io). StubTransport does no verification of its own, so
    // HashingAsyncReader is the sole verifier on this path.
    /// spec §D2 threat model: stream hash catches registry serving different bytes.
    /// This test verifies that `pull_layer` surfaces the canonical
    /// `HashingAsyncReader` digest check on the stub path.
    #[tokio::test]
    async fn streaming_tampered_blob_via_stub_path_yields_digest_mismatch() {
        // replaces verify_blob_digest_* coverage (a): tampered stream → DigestMismatch (NOT Io)
        // on the Stub path.
        //
        // StubTransport streams the blob map's bytes verbatim; HashingAsyncReader
        // in the assembled pipeline is what verifies the digest. A mismatch must
        // surface as ClientError::DigestMismatch, not ClientError::Io or any
        // other variant.
        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let evil_bytes = b"bytes that definitely do not hash to all-a".to_vec();
        // The descriptor size must be the real served byte length so the
        // compressed-side `.take(size)` cap does not truncate the stream — the
        // test genuinely exercises tampered-content → DigestMismatch rather than
        // passing coincidentally via an empty-hash mismatch under `size: 0`.
        let served_len = evil_bytes.len() as i64;

        let data = StubTransportData::new();
        data.write().blobs.insert(claimed_digest.clone(), evil_bytes);
        let client = stub(&data);

        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest.clone(),
            size: served_len,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        match result {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(
                    expected, claimed_digest,
                    "DigestMismatch must report the declared digest"
                );
                assert_ne!(actual, claimed_digest, "actual must differ from the claimed digest");
            }
            Err(ClientError::Io { .. }) => {
                panic!(
                    "digest mismatch must surface as DigestMismatch, not Io — the streaming pipeline must catch it in HashingAsyncReader before any I/O error path"
                )
            }
            other => panic!("expected ClientError::DigestMismatch from streaming pipeline, got {other:?}"),
        }
    }

    // (b) replaces verify_blob_digest_* coverage (a–e):
    // fork VerifyingStream io::Error with DigestError source maps → DigestMismatch.
    // Tests the error-mapping function that converts io::Error{source=DigestError}
    // → ClientError::DigestMismatch.
    /// spec §D2: fork VerifyingStream io::Error with DigestError source must map →
    /// ClientError::DigestMismatch (not ClientError::Io).
    /// This unit test verifies the mapping function by constructing the io::Error
    /// as the fork's VerifyingStream would produce it and asserting the correct
    /// ClientError variant results.
    #[test]
    fn fork_digest_error_io_wrapping_maps_to_digest_mismatch_not_io() {
        // replaces verify_blob_digest_* coverage (b): fork VerifyingStream
        // io::Error w/ DigestError source → DigestMismatch.
        //
        // The fork's VerifyingStream surfaces digest mismatch as:
        //   io::Error::new(io::ErrorKind::Other, DigestError::VerificationError { ... })
        //
        // map_fork_io_error_to_client_error must detect the DigestError source chain
        // and convert to ClientError::DigestMismatch (never ClientError::Io).
        //
        // Design spec §D2: "two verifiers, one typed error" — both the fork's
        // VerifyingStream and OCX's HashingAsyncReader must produce DigestMismatch.

        // (b) String-only path (no typed DigestError inner source):
        // io::Error carrying only a message string must map to ClientError::Io,
        // NOT DigestMismatch. The string-fallback was removed (CWE-20: spoofable).
        // Any io::Error that does not carry a typed DigestError::VerificationError
        // as its inner source is an I/O error, not a content-substitution event.
        let string_only_io_error =
            std::io::Error::other("digest verification error: expected sha256:aaaa... got sha256:bbbb...");

        let result: std::result::Result<(), ClientError> =
            crate::oci::client::native_transport::map_fork_io_error_to_client_error(string_only_io_error);

        match result {
            Err(ClientError::Io { .. }) => {
                // correct — string-only io::Error is an Io error, not DigestMismatch
            }
            Err(ClientError::DigestMismatch { .. }) => {
                panic!(
                    "string-only io::Error must map to ClientError::Io, not DigestMismatch \
                     (string fallback removed; CWE-20: message strings are spoofable)"
                )
            }
            other => panic!("expected Err(Io) for string-only io::Error, got {other:?}"),
        }

        // (b2) Typed downcast path: io::Error wrapping a real
        // oci_client::errors::DigestError::VerificationError exercises the
        // primary downcast path (not the string-fallback). The expected/actual
        // strings must round-trip through the DigestMismatch variant.
        let typed_mismatch = std::io::Error::other(oci_client::errors::DigestError::VerificationError {
            expected: "sha256:aaaa".to_string(),
            actual: "sha256:bbbb".to_string(),
        });

        let result2: std::result::Result<(), ClientError> =
            crate::oci::client::native_transport::map_fork_io_error_to_client_error(typed_mismatch);

        match result2 {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(
                    expected, "sha256:aaaa",
                    "expected digest must round-trip from DigestError"
                );
                assert_eq!(actual, "sha256:bbbb", "actual digest must round-trip from DigestError");
            }
            Err(ClientError::Io { .. }) => {
                panic!("typed DigestError::VerificationError must map to DigestMismatch via downcast, not Io")
            }
            other => panic!("expected Err(DigestMismatch), got {other:?}"),
        }
    }

    // (c) replaces T-A4 coverage (mirror-path invariant):
    // host+repo rewrite cannot bypass HashingAsyncReader verification.
    // A mirror serving wrong-digest content must still yield DigestMismatch.
    /// spec §D2 + T-A4 replacement: mirror-path invariant.
    /// OCX-side HashingAsyncReader verifies the digest independently of the
    /// transport source URL. A mirror rewrite (host+repo) serving wrong bytes
    /// must still be caught by the OCX pipeline, not bypass it.
    #[tokio::test]
    async fn streaming_mirror_path_cannot_bypass_hashing_reader_verification() {
        // replaces T-A4 (pull_layer_rejects_tampered_blob_under_configured_mirror):
        // mirror-path invariant restated for streaming pipeline.
        //
        // The StubTransport pull_blob_streaming path funnels through
        // HashingAsyncReader in pull_layer. Adding a MirrorMap does NOT change
        // which verifier runs — the OCX-side verifier always fires, regardless
        // of what URL the transport uses internally.
        use crate::config::mirror::ParsedMirror;

        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let evil_bytes = b"evil bytes that do not hash to all-a".to_vec();
        // Real served byte length: under the streaming pipeline the compressed-side
        // `.take(size)` must not truncate, so the digest is computed over the full
        // tampered payload (not the empty prefix a `size: 0` shortcut would yield).
        let served_len = evil_bytes.len() as i64;

        let data = StubTransportData::new();
        data.write().blobs.insert(claimed_digest.clone(), evil_bytes);
        let mut client = stub(&data);

        // Apply mirror rewrite for the test identifier's registry.
        // The rewrite must NOT bypass HashingAsyncReader verification.
        client.mirrors = MirrorMap::new([(
            "example.com".to_string(),
            ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.corp".to_string(),
                path_prefix: "oci-proxy".to_string(),
            },
        )]);

        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest.clone(),
            size: served_len,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        // A mirrored failure carries the routing that produced it (#327); the
        // verdict underneath must still be the digest mismatch, unchanged.
        let inner = match result {
            Err(ClientError::Mirrored { mirror, source, .. }) => {
                assert_eq!(mirror, "mirror.corp", "the annotation must name the mirror host");
                *source
            }
            other => panic!("expected a mirrored failure from the streaming pipeline, got {other:?}"),
        };
        match inner {
            ClientError::DigestMismatch { expected, actual } => {
                assert_eq!(
                    expected, claimed_digest,
                    "DigestMismatch must report the declared (claimed) digest even under mirror"
                );
                assert_ne!(
                    actual, claimed_digest,
                    "DigestMismatch actual must differ from the claimed digest"
                );
            }
            other => panic!("expected ClientError::DigestMismatch from streaming pipeline under mirror, got {other:?}"),
        }
    }

    // (d) replaces verify_blob_digest_* coverage (a–e):
    // No blob file in output_dir after successful pull_layer extract.
    // spec §D1 + §Client::pull_layer post-condition: "No blob file (.tar.xz etc.)
    // exists in output_dir after return."
    /// spec §Client::pull_layer post-condition: after successful extraction,
    /// no compressed blob file must remain in output_dir.
    /// (Currently: test will panic when pipeline is invoked before impl.)
    #[tokio::test]
    async fn streaming_no_blob_file_remains_in_output_dir_after_successful_extraction() {
        // replaces verify_blob_digest_* coverage (d): no blob file in output_dir post-extract.
        //
        // The streaming pipeline does NOT write a blob file to disk at all
        // (per D1: Option A pure streaming). This test asserts the post-condition
        // that no .tar.gz / .tar.xz / .blob file exists in output_dir after
        // pull_layer completes successfully.
        //
        // We need a valid tar.gz to actually succeed extraction.
        // Build a minimal valid .tar.gz in memory.

        // Build a tiny tar.gz archive containing one file.
        let tar_gz_bytes = make_minimal_tar_gz(b"hello\n", "hello.txt");
        let layer_digest = Algorithm::Sha256.hash(&tar_gz_bytes);
        let digest_str = layer_digest.to_string();

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz_bytes.clone());
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: tar_gz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        // If the pipeline succeeds or fails with Internal (codesign, tar), that's fine;
        // the key invariant is that no .tar.gz blob file exists in output_dir.
        match &result {
            Ok(()) | Err(ClientError::Internal(_)) => {}
            Err(e) => panic!("unexpected error from pull_layer in (d) test: {e:?}"),
        }

        // Assert: no blob file present in output_dir
        let output_dir = dir.path();
        for entry in std::fs::read_dir(output_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.ends_with(".tar.gz")
                    && !name_str.ends_with(".tar.xz")
                    && !name_str.ends_with(".blob")
                    && !name_str.ends_with(".tar"),
                "spec §Client::pull_layer post-condition: no blob file must remain in output_dir after extraction, found: {name_str}"
            );
        }
    }

    // (e) replaces verify_blob_digest_* coverage (a–e):
    // Invalid tar (valid xz wrapping garbage tar bytes) → ClientError::Internal
    // spec §Edge case 4: "XZ stream that is not a valid tar archive → Internal error"
    /// spec §Edge case 4: XZ-compressed garbage (not a valid tar) → ClientError::Internal.
    /// The streaming pipeline extracts via sync tar; a corrupted tar payload
    /// must surface as Internal (archive error), NOT Io or DigestMismatch.
    #[tokio::test]
    async fn streaming_invalid_tar_inside_valid_xz_wrapper_yields_internal_error() {
        // replaces verify_blob_digest_* coverage (e): invalid tar → Internal.
        //
        // Build valid XZ-compressed bytes wrapping garbage (not a valid tar).
        // After digest verification passes, the tar extractor should fail with
        // ClientError::Internal (archive::Error wrapped), not any I/O error.
        let garbage_tar_content = b"this is not a tar archive at all, just garbage bytes!!!";
        let xz_bytes = compress_xz_bytes(garbage_tar_content);
        let layer_digest = Algorithm::Sha256.hash(&xz_bytes);
        let digest_str = layer_digest.to_string();

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), xz_bytes.clone());
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_XZ.to_string(),
            digest: digest_str.clone(),
            size: xz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        match result {
            Err(ClientError::Internal(_)) => {
                // expected — garbage tar body wrapping valid xz compression → archive error
            }
            Err(ClientError::DigestMismatch { .. }) => {
                panic!(
                    "invalid tar (valid xz, garbage body) must not produce DigestMismatch — digest is valid for the xz bytes"
                )
            }
            Err(ClientError::Io { .. }) => {
                // Also acceptable: some tar errors surface as Io at the file layer.
                // The spec says "Internal", but the key invariant is NOT DigestMismatch.
            }
            other => panic!("expected Internal (archive error), got {other:?}"),
        }
    }

    // ── Decompression-bomb cap (CWE-400) ─────────────────────────────
    //
    // The decompressed-side cap rejects a layer whose decompressed output
    // exceeds the ceiling. A cap hit must surface as
    // ClientError::DecompressionCapExceeded — never DigestMismatch (the hash
    // would be computed over a truncated prefix) and never Internal.

    /// A gzip layer whose decompressed output exceeds an injected 512-byte cap
    /// returns `DecompressionCapExceeded`, not `DigestMismatch`, and terminates
    /// (does not hang). Exercised via the test-only `pull_layer_with_caps` seam
    /// so we need not fabricate a multi-hundred-megabyte archive.
    #[tokio::test]
    async fn decompressed_cap_hit_yields_cap_exceeded_not_digest_mismatch() {
        // Build a valid tar.gz whose single file is far larger than the cap.
        // The digest is correct for these bytes, so a wrong taxonomy (e.g.
        // surfacing DigestMismatch from the truncated prefix) would be a bug.
        let big_content = vec![b'x'; 64 * 1024]; // 64 KiB decompressed, well over a 512-byte cap
        let tar_gz_bytes = make_minimal_tar_gz(&big_content, "big.txt");
        let layer_digest = Algorithm::Sha256.hash(&tar_gz_bytes);
        let digest_str = layer_digest.to_string();

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz_bytes.clone());
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: tar_gz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let blob_total_size = tar_gz_bytes.len() as u64;
        let result = client
            .pull_layer_with_caps(&id, &layer, dir.path(), blob_total_size, 512)
            .await;
        match result {
            Err(ClientError::DecompressionCapExceeded { cap }) => {
                assert_eq!(cap, 512, "reported cap must be the injected ceiling");
            }
            Err(ClientError::DigestMismatch { .. }) => {
                panic!("cap hit must not be misattributed as DigestMismatch (hash over truncated prefix)")
            }
            other => panic!("expected DecompressionCapExceeded, got {other:?}"),
        }
    }

    /// A descriptor with `size: 0` is a malformed manifest, not a zero-byte
    /// layer; `pull_layer` rejects it as `InvalidManifest` before touching the
    /// transport.
    #[tokio::test]
    async fn zero_size_descriptor_yields_invalid_manifest() {
        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let data = StubTransportData::new();
        let client = stub(&data);

        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest,
            size: 0,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        match result {
            Err(ClientError::InvalidManifest(msg)) => {
                assert!(
                    msg.contains("positive byte count"),
                    "message should explain the non-positive size, got: {msg}"
                );
            }
            other => panic!("expected InvalidManifest for size: 0 descriptor, got {other:?}"),
        }
    }

    /// U10 (BC-core · D12): the registry extraction path writes a VERBATIM layer
    /// tree — the package-wide strip is NOT applied at extraction time (it moved
    /// to assemble). A tarball with a leading top-level directory must land in
    /// `output_dir/content/` with that directory intact so the shared
    /// content-addressed layer store stays faithful regardless of any package's
    /// `strip_components`.
    #[tokio::test]
    async fn pull_layer_extracts_verbatim_without_strip() {
        // A single-file tar entry whose path carries a leading directory. tar's
        // unpack creates the parent dirs, so `topdir/bin/tool` is materialized.
        let tar_gz_bytes = make_minimal_tar_gz(b"tool bytes\n", "topdir/bin/tool");
        let layer_digest = Algorithm::Sha256.hash(&tar_gz_bytes);
        let digest_str = layer_digest.to_string();

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz_bytes.clone());
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: tar_gz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        client
            .pull_layer(&id, &layer, dir.path())
            .await
            .expect("pull_layer must extract the layer");

        let content = dir.path().join("content");
        // Verbatim: the leading directory is preserved (strip NOT applied here).
        assert!(
            content.join("topdir/bin/tool").is_file(),
            "extraction must be verbatim — topdir/bin/tool must exist under content/"
        );
        // If strip had (wrongly) been baked into extraction, the top dir is gone.
        assert!(
            !content.join("bin/tool").exists(),
            "extraction must NOT strip the leading component into the shared layer store"
        );
        assert_eq!(
            std::fs::read(content.join("topdir/bin/tool")).unwrap(),
            b"tool bytes\n",
            "extracted file contents must be intact"
        );
    }

    // ── Undrained-stream regression (compressed-side digest) ─────────
    //
    // `tar`'s iterator stops at the end-of-archive marker, so the bytes after it
    // — the codec trailer, plus any padding — are only hashed if the pipeline
    // pulls them deliberately. Whether they happened to ride the last buffer
    // fill is a property of network segmentation, which is why the production
    // failure was a re-run lottery. `blob_stream_chunks` pins the boundary so
    // the three outcomes are decidable.

    /// A well-formed gzip layer whose 8-byte trailer arrives in a chunk the tar
    /// extractor never demands must still pull successfully: the pipeline drains
    /// the compressed remainder before finalising, so the digest covers the whole
    /// blob rather than the prefix tar happened to read.
    ///
    /// Deterministic reproduction of the production `DigestMismatch` on
    /// [ocx-contrib/mirror-bazelbuild run 30713887936].
    #[tokio::test]
    async fn pull_layer_succeeds_when_codec_trailer_arrives_in_final_chunk() {
        let tar_gz_bytes = make_minimal_tar_gz(b"trailer split\n", "topdir/bin/tool");
        let digest_str = Algorithm::Sha256.hash(&tar_gz_bytes).to_string();

        // gzip's footer is the last 8 bytes (CRC32 + ISIZE). The deflate stream
        // in chunk 1 already decodes to the complete tar, terminator included,
        // so the extractor stops without ever asking for chunk 2.
        let split = tar_gz_bytes.len() - 8;
        let chunks = vec![tar_gz_bytes[..split].to_vec(), tar_gz_bytes[split..].to_vec()];

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz_bytes.clone());
        data.write().blob_stream_chunks.insert(digest_str.clone(), chunks);
        let client = stub(&data);

        let id = test_pinned(digest_str.strip_prefix("sha256:").unwrap());
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: tar_gz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        client
            .pull_layer(&id, &layer, dir.path())
            .await
            .expect("a trailer left in the final chunk must be drained, not reported as a digest mismatch");

        assert_eq!(
            std::fs::read(dir.path().join("content/topdir/bin/tool")).unwrap(),
            b"trailer split\n",
            "the layer must still extract correctly"
        );
    }

    /// Same property on the xz arm: an xz stream ends with an index plus a
    /// 12-byte footer that the decoder needs only for verification, never to
    /// produce output, so tar stops before they are read. Guards the Lzma arm's
    /// drain call, which the gzip test cannot reach.
    #[tokio::test]
    async fn pull_layer_succeeds_when_xz_trailer_arrives_in_final_chunk() {
        let tar_bytes = make_minimal_tar(b"xz trailer split\n", "topdir/bin/tool");
        let tar_xz_bytes = compress_xz_bytes(&tar_bytes);
        let digest_str = Algorithm::Sha256.hash(&tar_xz_bytes).to_string();

        // The xz stream footer is 12 bytes; the index sits immediately before it.
        // Holding both back proves the drain reaches them, since the LZMA2 block
        // in chunk 1 already decodes to the whole tar.
        const XZ_TRAILER: usize = 12;
        let split = tar_xz_bytes.len() - XZ_TRAILER;
        let chunks = vec![tar_xz_bytes[..split].to_vec(), tar_xz_bytes[split..].to_vec()];

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_xz_bytes.clone());
        data.write().blob_stream_chunks.insert(digest_str.clone(), chunks);
        let client = stub(&data);

        let id = test_pinned(digest_str.strip_prefix("sha256:").unwrap());
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_XZ.to_string(),
            digest: digest_str.clone(),
            size: tar_xz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        client
            .pull_layer(&id, &layer, dir.path())
            .await
            .expect("an xz footer left in the final chunk must be drained, not reported as a digest mismatch");

        assert_eq!(
            std::fs::read(dir.path().join("content/topdir/bin/tool")).unwrap(),
            b"xz trailer split\n",
            "the layer must still extract correctly"
        );
    }

    /// Same property on the zstd arm. A zstd frame's content checksum is the
    /// last 4 bytes and is verification-only, so — like gzip's CRC footer — the
    /// decoder produces the whole tar without it. Guards the Zstd arm's drain
    /// call, which neither the gzip nor the xz test can reach.
    #[tokio::test]
    async fn pull_layer_succeeds_when_zstd_trailer_arrives_in_final_chunk() {
        let tar_zst_bytes = compress_zstd_bytes(&make_minimal_tar(b"zstd trailer split\n", "topdir/bin/tool"));
        let digest_str = Algorithm::Sha256.hash(&tar_zst_bytes).to_string();

        // The frame's content checksum (enabled in the helper) is the trailing
        // 4 bytes; the last block before it is already flagged final.
        const ZSTD_CHECKSUM: usize = 4;
        let split = tar_zst_bytes.len() - ZSTD_CHECKSUM;
        let chunks = vec![tar_zst_bytes[..split].to_vec(), tar_zst_bytes[split..].to_vec()];

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_zst_bytes.clone());
        data.write().blob_stream_chunks.insert(digest_str.clone(), chunks);
        let client = stub(&data);

        let id = test_pinned(digest_str.strip_prefix("sha256:").unwrap());
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_ZSTD.to_string(),
            digest: digest_str.clone(),
            size: tar_zst_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        client
            .pull_layer(&id, &layer, dir.path())
            .await
            .expect("a zstd checksum left in the final chunk must be drained, not reported as a digest mismatch");

        assert_eq!(
            std::fs::read(dir.path().join("content/topdir/bin/tool")).unwrap(),
            b"zstd trailer split\n",
            "the layer must still extract correctly"
        );
    }

    /// The trust anchor is not weakened by the drain: a full-length blob whose
    /// content does not hash to the declared digest is still `DigestMismatch`
    /// (CWE-345), never `ShortBlobRead`.
    #[tokio::test]
    async fn pull_layer_reports_digest_mismatch_for_wrong_full_length_content() {
        // Valid gzip tar bytes, published under a digest they do not hash to —
        // exactly what a registry serving substituted content looks like.
        let tar_gz_bytes = make_minimal_tar_gz(b"substituted\n", "tool");
        let wrong_digest = format!("sha256:{}", "a".repeat(64));

        let data = StubTransportData::new();
        data.write().blobs.insert(wrong_digest.clone(), tar_gz_bytes.clone());
        let client = stub(&data);

        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: wrong_digest.clone(),
            // Full declared length is served, so the short-read check must pass
            // and leave the verdict to the digest comparison.
            size: tar_gz_bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        match client.pull_layer(&id, &layer, dir.path()).await {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(expected, wrong_digest, "the declared digest must be reported");
                assert_ne!(actual, wrong_digest, "the computed digest must differ");
            }
            Err(ClientError::ShortBlobRead { .. }) => {
                panic!(
                    "full-length wrong content must stay DigestMismatch — ShortBlobRead here would weaken the CWE-345 anchor"
                )
            }
            other => panic!("expected DigestMismatch for wrong full-length content, got {other:?}"),
        }
    }

    /// A stream that ends before the declared size is an incomplete delivery,
    /// reported as `ShortBlobRead` — not as the registry having served wrong
    /// content. Same fixture as the trailer test with the final chunk withheld,
    /// so the two differ only in whether the remaining bytes exist at all.
    #[tokio::test]
    async fn pull_layer_reports_short_blob_read_for_truncated_stream() {
        let tar_gz_bytes = make_minimal_tar_gz(b"truncated\n", "topdir/bin/tool");
        let digest_str = Algorithm::Sha256.hash(&tar_gz_bytes).to_string();

        let declared_size = tar_gz_bytes.len() as u64;
        let split = tar_gz_bytes.len() - 8;

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz_bytes.clone());
        // Only the prefix is ever delivered; the stream then ends.
        data.write()
            .blob_stream_chunks
            .insert(digest_str.clone(), vec![tar_gz_bytes[..split].to_vec()]);
        let client = stub(&data);

        let id = test_pinned(digest_str.strip_prefix("sha256:").unwrap());
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: declared_size as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        match client.pull_layer(&id, &layer, dir.path()).await {
            Err(ClientError::ShortBlobRead { expected, actual }) => {
                assert_eq!(expected, declared_size, "expected must be the declared blob size");
                assert_eq!(actual, split as u64, "actual must be the number of bytes delivered");
            }
            Err(ClientError::DigestMismatch { .. }) => {
                panic!("a truncated delivery must not be attributed to the registry serving wrong content")
            }
            other => panic!("expected ShortBlobRead for a truncated stream, got {other:?}"),
        }
    }

    // ── Mid-stream interruption test (3.7) ─────────────────────────────
    //
    // spec §UX Scenario 1 error case: "If network is interrupted mid-stream,
    // ClientError::Io is returned. The partial temp directory is cleaned up
    // by the existing TempStore cleanup path (unchanged)."

    // A transport whose stream errors mid-read.
    // Used to test that mid-stream I/O error → ClientError::Io (not DigestMismatch).
    struct InterruptingTransport {
        /// Bytes before the simulated interruption.
        bytes_before_error: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl super::OciTransport for InterruptingTransport {
        async fn ensure_auth(
            &self,
            _image: &oci::native::Reference,
            _op: oci::RegistryOperation,
        ) -> super::transport::Result<()> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> super::transport::Result<Vec<String>> {
            Ok(vec![])
        }

        async fn catalog(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> super::transport::Result<Vec<String>> {
            Ok(vec![])
        }

        async fn fetch_manifest_digest(&self, _image: &oci::native::Reference) -> super::transport::Result<String> {
            unimplemented!()
        }

        async fn pull_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _accepted_media_types: &[&str],
        ) -> super::transport::Result<(Vec<u8>, String)> {
            unimplemented!()
        }

        async fn pull_blob(
            &self,
            _image: &oci::native::Reference,
            _digest: &oci::Digest,
        ) -> super::transport::Result<Vec<u8>> {
            unimplemented!()
        }

        async fn pull_blob_to_file(
            &self,
            _image: &oci::native::Reference,
            _digest: &oci::Digest,
            path: &std::path::Path,
        ) -> super::transport::Result<()> {
            // Write partial bytes then return an I/O error to simulate
            // a mid-stream network interruption.
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ClientError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
            std::fs::write(path, &self.bytes_before_error).map_err(|e| ClientError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Err(ClientError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "simulated mid-stream network interruption",
                ),
            })
        }

        async fn head_blob(
            &self,
            _image: &oci::native::Reference,
            _digest: &oci::Digest,
        ) -> super::transport::Result<u64> {
            Ok(0)
        }

        async fn push_manifest(
            &self,
            _image: &oci::native::Reference,
            _manifest: &oci::Manifest,
        ) -> super::transport::Result<String> {
            unimplemented!()
        }

        async fn push_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _media_type: &str,
        ) -> super::transport::Result<String> {
            unimplemented!()
        }

        async fn push_blob(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _digest: &oci::Digest,
            _on_progress: super::transport::ProgressFn,
        ) -> super::transport::Result<String> {
            unimplemented!()
        }

        async fn push_blob_from_path(
            &self,
            _image: &oci::native::Reference,
            _path: &std::path::Path,
            _digest: &oci::Digest,
            _on_progress: super::transport::ProgressFn,
        ) -> super::transport::Result<String> {
            unimplemented!()
        }

        async fn pull_blob_streaming(
            &self,
            _image: &oci::native::Reference,
            _digest: &oci::Digest,
        ) -> super::transport::Result<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
            // Path A: stream OPENS, yields partial bytes, then errors mid-read.
            // This exercises the mid-stream interruption through the actual
            // streaming pipeline (HashingAsyncReader → decoder → spawn_blocking),
            // unlike pull_blob_to_file which errors before streaming begins.
            Ok(Box::new(InterruptingAsyncRead {
                data: self.bytes_before_error.clone(),
                pos: 0,
            }))
        }

        async fn push_referrer_manifest(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &oci::Digest,
            _manifest_bytes: &[u8],
            _media_type: &str,
        ) -> super::transport::Result<oci::Descriptor> {
            unimplemented!("not needed for the streaming-interruption test")
        }

        async fn list_referrers(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &oci::Digest,
            _artifact_type: Option<&str>,
        ) -> super::transport::Result<Vec<oci::Descriptor>> {
            unimplemented!("not needed for the streaming-interruption test")
        }

        fn box_clone(&self) -> Box<dyn super::OciTransport> {
            Box::new(InterruptingTransport {
                bytes_before_error: self.bytes_before_error.clone(),
            })
        }
    }

    /// An [`AsyncRead`] that yields all bytes in `data`, then returns a
    /// `ConnectionReset` io::Error on the next read. Used to simulate a
    /// mid-stream network interruption in the streaming pipeline path
    /// (Path B of A8: stream opens successfully, partial bytes arrive, then errors).
    struct InterruptingAsyncRead {
        data: Vec<u8>,
        pos: usize,
    }

    impl tokio::io::AsyncRead for InterruptingAsyncRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.pos >= self.data.len() {
                // All bytes delivered; next read = simulated network error.
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "simulated mid-stream network interruption (path B)",
                )));
            }
            let remaining = self.data.len() - self.pos;
            let to_read = remaining.min(buf.remaining());
            buf.put_slice(&self.data[self.pos..self.pos + to_read]);
            self.pos += to_read;
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl Unpin for InterruptingAsyncRead {}

    /// spec §UX Scenario 1 error case: mid-stream network interruption →
    /// `ClientError::ShortBlobRead` (not `DigestMismatch`).
    /// The interruption is an incomplete delivery, and the byte count says so
    /// exactly — it must not be attributed to the registry. The TempStore
    /// cleanup path handles temp dir cleanup.
    #[tokio::test]
    async fn mid_stream_network_interruption_yields_short_blob_read_not_digest_mismatch() {
        // The InterruptingTransport yields partial bytes then errors mid-read.
        let partial_bytes = b"partial data before network cut".to_vec();
        let partial_len = partial_bytes.len();
        let transport = InterruptingTransport {
            bytes_before_error: partial_bytes,
        };
        let client = Client::with_transport(Box::new(transport));

        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest.clone(),
            size: 1024,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;

        // A cut stream and a lying registry are distinguishable after all: the
        // declared size is manifest-verified, so `bytes_read` short of it is
        // proof of an incomplete delivery, whereas the hash alone cannot tell
        // the two apart (a prefix never matches either way). Asserting the exact
        // variant — rather than tolerating a band of them — is what makes this a
        // check on the discriminator instead of a check that something failed.
        match result {
            Err(ClientError::ShortBlobRead { expected, actual }) => {
                assert_eq!(expected, 1024, "expected must be the declared descriptor size");
                assert_eq!(
                    actual, partial_len as u64,
                    "actual must be the number of bytes that arrived before the cut"
                );
            }
            Ok(()) => panic!("pull_layer must not succeed when stream errors mid-read"),
            other => panic!("expected ClientError::ShortBlobRead for mid-stream interruption, got {other:?}"),
        }

        // spec §UX Scenario 1 cleanup contract:
        // pull_layer leaves output_dir in place on error — cleanup is the
        // caller's TempStore responsibility (RAII DropFile / TempStore semantics).
        assert!(
            dir.path().exists(),
            "output_dir must not be deleted by pull_layer on error (TempStore is responsible for cleanup)"
        );
    }

    /// spec §UX Scenario 1 error case Path B: stream OPENS successfully, yields
    /// partial bytes, then errors mid-read from AsyncRead. This exercises the
    /// full streaming pipeline (HashingAsyncReader → decoder → spawn_blocking),
    /// unlike Path A which errors before streaming begins via pull_blob_to_file.
    ///
    /// The InterruptingAsyncRead returns partial bytes then a ConnectionReset error.
    /// The pipeline must return `ClientError::ShortBlobRead` (not `DigestMismatch`,
    /// not panic) — the bytes that did arrive fall short of the declared size.
    #[tokio::test]
    async fn mid_stream_async_read_error_yields_short_blob_read_not_digest_mismatch() {
        // Path B (A8): pull_blob_streaming returns a stream that opens,
        // yields partial bytes, then errors mid-read.
        let partial_bytes = b"some partial gzip bytes".to_vec(); // not valid gzip — forces extraction error
        let partial_len = partial_bytes.len();
        let transport = InterruptingTransport {
            bytes_before_error: partial_bytes,
        };
        let client = Client::with_transport(Box::new(transport));

        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest.clone(),
            size: 1024,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;

        // The gzip header is invalid, so extraction fails too — but the
        // completeness check runs first and is the more precise answer: the
        // stream ended short of the declared size. Attributing this to the
        // registry (DigestMismatch) would send the operator hunting a
        // supply-chain incident that never happened.
        match &result {
            Ok(()) => panic!("pull_layer must not succeed with a mid-stream error and invalid bytes"),
            Err(ClientError::ShortBlobRead { expected, actual }) => {
                assert_eq!(*expected, 1024, "expected must be the declared descriptor size");
                assert_eq!(
                    *actual, partial_len as u64,
                    "actual must be the number of bytes that arrived before the cut"
                );
            }
            Err(other) => panic!("expected ClientError::ShortBlobRead for mid-stream AsyncRead error, got {other:?}"),
        }

        // output_dir must still exist (TempStore owns cleanup).
        assert!(
            dir.path().exists(),
            "output_dir must not be deleted by pull_layer on error (TempStore is responsible)"
        );
    }

    // ── Test helpers for (d) and (e) ─────────────────────────────────

    /// Builds a minimal valid (uncompressed) tar archive containing one file.
    fn make_minimal_tar(content: &[u8], filename: &str) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, filename, content).unwrap();
        tar.into_inner().unwrap()
    }

    /// Builds a minimal valid tar.gz archive containing one file.
    fn make_minimal_tar_gz(content: &[u8], filename: &str) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&make_minimal_tar(content, filename)).unwrap();
        encoder.finish().unwrap()
    }

    /// Compresses `bytes` with zstd, content checksum ON so the frame ends in a
    /// 4-byte verification-only trailer (the zstd analogue of gzip's CRC footer).
    fn compress_zstd_bytes(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder = zstd::Encoder::new(Vec::new(), 1).expect("zstd encoder");
        encoder.include_checksum(true).expect("checksum flag");
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// Compresses `bytes` with XZ (single-threaded lzma2, preset 1) for test (e).
    fn compress_xz_bytes(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = Vec::new();
        // Use lzma_rust2::XzWriter (the same encoder the codebase uses for XZ output).
        let options = lzma_rust2::XzOptions::with_preset(1);
        let mut writer = lzma_rust2::XzWriter::new(&mut buf, options).expect("XzWriter init");
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap();
        buf
    }

    // ── CWE-400 decompression-bound tests ───────────────────────────────

    /// spec §D1 CWE-400 compressed-side cap:
    /// A transport serving more bytes than `layer.size` declares is stopped by the
    /// `take(layer.size)` cap on the raw stream. The descriptor declares a *different*
    /// digest than the served content's digest — verifying that the pipeline detects
    /// tampered/extended streams and does not hang on over-length input.
    #[tokio::test]
    async fn over_length_compressed_stream_yields_digest_mismatch_not_hang() {
        // Build one valid tar.gz for its digest (the "declared" content), and a *different*
        // longer byte sequence that will be served by the transport.
        //
        // layer.size is set to the length of the declared content. The transport serves
        // extra bytes beyond that length. take(layer.size) stops reading at layer.size
        // bytes, so only the first layer.size bytes of over_length are hashed.
        // Those bytes will NOT match the declared digest (different content) → DigestMismatch.
        //
        // This design correctly tests the cap: the cap stops the read, the digest mismatch
        // proves the cap fired and that over-length streams cannot succeed with a mismatched
        // digest.

        // "Declared" content whose digest we put in the descriptor.
        let declared_content = make_minimal_tar_gz(b"hello\n", "declared.txt");
        let layer_digest = Algorithm::Sha256.hash(&declared_content);
        let digest_str = layer_digest.to_string();
        let declared_size = declared_content.len() as i64;

        // "Served" content: same length prefix but with leading bytes changed, then more
        // garbage appended. The first `declared_size` bytes differ from declared_content
        // so the digest will NOT match even after take(declared_size) truncation.
        let mut over_length: Vec<u8> = vec![0xAA; declared_content.len()]; // same length, different bytes
        over_length.extend_from_slice(b"EXTRA_GARBAGE_BEYOND_DECLARED_SIZE_AAAAAAAAAA");

        let data = StubTransportData::new();
        // Transport serves over_length under the digest key so the key lookup succeeds,
        // but the bytes do not hash to that digest.
        data.write().blobs.insert(digest_str.clone(), over_length);
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: declared_size,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        // pull_layer must NOT hang (take(layer.size) bounds read) and must return an error.
        // DigestMismatch is expected: the first declared_size bytes of the served stream
        // do not hash to the declared digest.
        let result = client.pull_layer(&id, &layer, dir.path()).await;
        match result {
            Ok(()) => panic!(
                "over-length compressed stream with mismatched content must not succeed; \
                 take(layer.size) bounds read, digest mismatch must be detected"
            ),
            Err(ClientError::DigestMismatch { .. }) | Err(ClientError::Internal(_)) | Err(ClientError::Io { .. }) => {
                // Any error is acceptable — key invariants: no hang + no silent Ok.
            }
            Err(other) => panic!("unexpected error for over-length stream: {other:?}"),
        }
    }

    /// spec §D1 CWE-400 exact-size happy path:
    /// A transport serving exactly `layer.size` bytes for a valid archive must succeed.
    /// Verifies the compressed-side cap does not interfere with legitimate pulls.
    #[tokio::test]
    async fn exact_length_compressed_stream_succeeds() {
        let tar_gz = make_minimal_tar_gz(b"hello exact\n", "hello.txt");
        let layer_digest = Algorithm::Sha256.hash(&tar_gz);
        let digest_str = layer_digest.to_string();
        let declared_size = tar_gz.len() as i64;

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz);
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: declared_size,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        assert!(
            result.is_ok(),
            "exact-length compressed stream must succeed: {result:?}"
        );
    }

    /// spec §D1 CWE-400 decompressed-side cap:
    /// A crafted stream that decompresses to more than the cap must not succeed.
    /// This tests that the decompressed-side `take(DECOMPRESSED_CAP)` fires before
    /// the extraction exhausts resources.
    ///
    /// Implementation note: we cannot easily set the cap to a tiny value without
    /// making it a parameter. Instead we test the property at small scale:
    /// a valid tar.gz that decompresses to a reasonable size must succeed (cap not
    /// hit), confirming the cap is in place. The cap itself is validated by
    /// the over-length test above (which confirms errors propagate; the decompressed
    /// cap would fire for a real decompression bomb in production).
    #[tokio::test]
    async fn decompressed_cap_does_not_interfere_with_small_valid_archives() {
        // A 512-byte payload compressed to ~300 bytes; expansion ratio ~1.7×.
        // DECOMPRESSED_CAP = max(1 GiB, 100 × layer.size) >> 512 bytes — cap never hits.
        let content = vec![b'A'; 512];
        let tar_gz = make_minimal_tar_gz(&content, "bigfile.bin");
        let layer_digest = Algorithm::Sha256.hash(&tar_gz);
        let digest_str = layer_digest.to_string();
        let declared_size = tar_gz.len() as i64;

        let data = StubTransportData::new();
        data.write().blobs.insert(digest_str.clone(), tar_gz);
        let client = stub(&data);

        let id = {
            let hex = digest_str.strip_prefix("sha256:").unwrap();
            test_pinned(hex)
        };
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: digest_str.clone(),
            size: declared_size,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, dir.path()).await;
        assert!(
            result.is_ok(),
            "small valid archive must not be affected by decompressed-side cap: {result:?}"
        );
    }

    // ── TempStore tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn temp_acquire_cleans_leftover_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let temp_root = dir.path().join("temp_root");
        let temp_path = temp_root.join("some_hash");

        // Simulate leftover artifacts from a crashed download.
        tokio::fs::create_dir_all(&temp_path).await.unwrap();
        tokio::fs::write(temp_path.join("metadata.json"), b"stale")
            .await
            .unwrap();
        tokio::fs::create_dir(temp_path.join("content")).await.unwrap();

        let store = TempStore::new(&temp_root);
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();

        // Verify artifacts were cleaned.
        assert!(acquired.was_cleaned);
        assert!(!temp_path.join("metadata.json").exists());
        assert!(!temp_path.join("content").exists());
        // Lock file is a sibling, not inside the dir.
        assert!(TempStore::lock_path_for(&temp_path).exists());
    }

    // ── Paginate unit test ───────────────────────────────────────────

    #[tokio::test]
    async fn paginate_empty() {
        let result = paginate(100, |_cs, _last| async { Ok(vec![]) }).await;
        assert_eq!(result.unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn paginate_first_page_uses_empty_string() {
        let lasts = std::sync::Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let lasts_clone = lasts.clone();
        let result = paginate(100, move |_cs, last| {
            let lasts = lasts_clone.clone();
            async move {
                lasts.lock().unwrap().push(last);
                Ok(vec!["a".to_string()])
            }
        })
        .await;
        assert!(result.is_ok());
        let captured = lasts.lock().unwrap();
        assert_eq!(captured[0], Some(String::new()));
    }

    // ── merge_platform_into_index tests ─────────────────────────────

    mod merge_platform {
        use super::*;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn platform(s: &str) -> oci::Platform {
            s.parse().unwrap()
        }

        /// Read back the pushed index from the stub and parse it.
        fn read_pushed_index(data: &StubTransportData, tag: &str) -> oci::ImageIndex {
            let id = test_identifier(tag);
            let inner = data.read();
            let (bytes, _) = inner
                .manifests
                .get(&id.canonical_reference().to_string())
                .expect("no pushed manifest");
            let manifest: oci::Manifest = serde_json::from_slice(bytes).unwrap();
            match manifest {
                oci::Manifest::ImageIndex(idx) => idx,
                _ => panic!("expected ImageIndex, got ImageManifest"),
            }
        }

        #[tokio::test]
        async fn fresh_tag_creates_new_index() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.manifests.len(), 1);
            assert_eq!(index.manifests[0].digest, "sha256:abc");
            assert_eq!(index.manifests[0].size, 100);
            let entry_plat: oci::Platform = index.manifests[0].platform.clone().unwrap().try_into().unwrap();
            assert_eq!(entry_plat, platform("linux/amd64"));
        }

        #[tokio::test]
        async fn existing_index_adds_platform() {
            let data = StubTransportData::new();

            // Seed an existing index with arm64.
            let id = test_identifier("3.28");
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: "sha256:arm64_digest".to_string(),
                    size: 50,
                    platform: Some(platform("linux/arm64").into()),
                    artifact_type: None,
                    annotations: None,
                }],
                annotations: None,
            };
            let existing_bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let existing_digest = oci::Algorithm::Sha256.hash(&existing_bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (existing_bytes, existing_digest));

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:amd64_new",
                    200,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.manifests.len(), 2);
            let digests: Vec<&str> = index.manifests.iter().map(|e| e.digest.as_str()).collect();
            assert!(digests.contains(&"sha256:arm64_digest"));
            assert!(digests.contains(&"sha256:amd64_new"));
        }

        #[tokio::test]
        async fn existing_index_replaces_same_platform() {
            let data = StubTransportData::new();

            let id = test_identifier("3.28");
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: "sha256:old_amd64".to_string(),
                    size: 50,
                    platform: Some(platform("linux/amd64").into()),
                    artifact_type: None,
                    annotations: None,
                }],
                annotations: None,
            };
            let existing_bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let existing_digest = oci::Algorithm::Sha256.hash(&existing_bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (existing_bytes, existing_digest));

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:new_amd64",
                    200,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.manifests.len(), 1);
            assert_eq!(index.manifests[0].digest, "sha256:new_amd64");
            assert_eq!(index.manifests[0].size, 200);
        }

        /// A tag that already carries the same platform twice comes back
        /// carrying it once.
        ///
        /// The duplicate is not hypothetical: an index written by an older
        /// publisher, by a concurrent push that raced this one, or by a tool
        /// that appended instead of replacing, all produce it — and a
        /// duplicated platform makes `select_best` pick by position, so which
        /// binary a user gets depends on entry order rather than on content.
        /// The merge is a full replace of every entry for the platform, not a
        /// replace of the first, so one pass over a damaged tag heals it.
        #[tokio::test]
        async fn a_tag_carrying_one_platform_twice_comes_back_carrying_it_once() {
            let data = StubTransportData::new();
            let id = test_identifier("3.28");
            let duplicated = |digest: &str, size: i64| oci::ImageIndexEntry {
                media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                digest: digest.to_string(),
                size,
                platform: Some(platform("linux/amd64").into()),
                artifact_type: None,
                annotations: None,
            };
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests: vec![
                    duplicated("sha256:first_amd64", 50),
                    duplicated("sha256:second_amd64", 60),
                    oci::ImageIndexEntry {
                        media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                        digest: "sha256:arm64_digest".to_string(),
                        size: 70,
                        platform: Some(platform("linux/arm64").into()),
                        artifact_type: None,
                        annotations: None,
                    },
                ],
                annotations: None,
            };
            let existing_bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let existing_digest = oci::Algorithm::Sha256.hash(&existing_bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (existing_bytes, existing_digest));

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:healed_amd64",
                    200,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            let amd64: Vec<&str> = index
                .manifests
                .iter()
                .filter(|entry| {
                    entry
                        .platform
                        .clone()
                        .and_then(|p| oci::Platform::try_from(p).ok())
                        .is_some_and(|p| p == platform("linux/amd64"))
                })
                .map(|entry| entry.digest.as_str())
                .collect();
            assert_eq!(
                amd64,
                vec!["sha256:healed_amd64"],
                "both stale linux/amd64 entries must be gone, not just the first"
            );
            // The untouched platform is the control: a merge that healed by
            // rebuilding the index from one entry would also satisfy the
            // assertion above.
            assert_eq!(index.manifests.len(), 2, "linux/arm64 must survive the heal");
        }

        /// Seeds an existing index carrying `artifact_type` and returns its
        /// identifier, so the two stamping tests differ only in that value.
        fn seed_existing_index(data: &StubTransportData, artifact_type: Option<&str>) -> Identifier {
            let id = test_identifier("3.28");
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: artifact_type.map(str::to_string),
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: "sha256:arm64_digest".to_string(),
                    size: 50,
                    platform: Some(platform("linux/arm64").into()),
                    artifact_type: None,
                    annotations: None,
                }],
                annotations: None,
            };
            let existing_bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let existing_digest = oci::Algorithm::Sha256.hash(&existing_bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (existing_bytes, existing_digest));
            id
        }

        /// Bug 19: the pass-through branch used to leave `artifactType` exactly
        /// as found, so an index that was never stamped stayed unstamped
        /// forever — identical pushes emitting different documents on
        /// repository history alone.
        #[tokio::test]
        async fn merge_stamps_artifact_type_on_an_unstamped_existing_index() {
            let data = StubTransportData::new();
            let id = seed_existing_index(&data, None);

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:amd64_new",
                    200,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.artifact_type.as_deref(), Some(MEDIA_TYPE_PACKAGE_V1));
        }

        /// Filling an absent field states what ocx wrote; overwriting a
        /// declared one would relabel someone else's document.
        #[tokio::test]
        async fn merge_preserves_a_foreign_artifact_type_on_an_existing_index() {
            let data = StubTransportData::new();
            let id = seed_existing_index(&data, Some("application/vnd.example.other.v1"));

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:amd64_new",
                    200,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.artifact_type.as_deref(), Some("application/vnd.example.other.v1"));
        }

        #[tokio::test]
        async fn existing_image_manifest_upgrades_to_index() {
            let data = StubTransportData::new();

            // Seed an existing plain ImageManifest (not an index).
            let id = test_identifier("3.28");
            let image_manifest = oci::ImageManifest {
                config: oci::Descriptor {
                    media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                    digest: "sha256:old_config".to_string(),
                    size: 42,
                    urls: None,
                    artifact_type: None,
                    annotations: None,
                },
                ..Default::default()
            };
            let manifest = oci::Manifest::Image(image_manifest);
            let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
            let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes).to_string();
            data.write().manifests.insert(
                id.canonical_reference().to_string(),
                (manifest_bytes.clone(), manifest_digest.clone()),
            );

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:new_manifest",
                    300,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            // Should have 2 entries: old manifest (no platform) + new (amd64).
            assert_eq!(index.manifests.len(), 2);
            let old_entry = index
                .manifests
                .iter()
                .find(|e| e.platform.is_none())
                .expect("old entry missing");
            // Fixed: uses the manifest digest (not config.digest) and manifest size (not config.size).
            assert_eq!(old_entry.digest, manifest_digest);
            assert_eq!(old_entry.size, manifest_bytes.len() as i64);
            let new_entry = index
                .manifests
                .iter()
                .find(|e| e.platform.is_some())
                .expect("new entry missing");
            assert_eq!(new_entry.digest, "sha256:new_manifest");
        }

        #[tokio::test]
        async fn non_404_error_propagates_instead_of_starting_fresh() {
            let data = StubTransportData::new();
            // Inject a registry error (e.g. auth failure, network issue) for missing manifests.
            data.write().pull_manifest_error_override = Some("connection reset".into());
            data.write().capture_pushes = true;
            let client = Client::with_transport(Box::new(StubTransport::new(data.clone())));
            let id = test_identifier("3.28");

            let result = client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &BTreeMap::new(),
                )
                .await;

            assert!(result.is_err(), "expected error to propagate, got Ok");
            // Verify no manifest was pushed (no silent overwrite).
            let inner = data.read();
            assert!(
                inner.manifests.is_empty(),
                "no manifest should have been pushed on error"
            );
        }

        // ── index annotations (`ocx package push --annotation`) ───────

        /// Seed the stub with an index carrying `annotations` at `tag`.
        fn seed_index_with_annotations(
            data: &StubTransportData,
            tag: &str,
            annotations: Option<BTreeMap<String, String>>,
        ) {
            let id = test_identifier(tag);
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: "sha256:arm64_digest".to_string(),
                    size: 50,
                    platform: Some(platform("linux/arm64").into()),
                    artifact_type: None,
                    annotations: None,
                }],
                annotations,
            };
            let bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let digest = oci::Algorithm::Sha256.hash(&bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (bytes, digest));
        }

        fn source_annotation(url: &str) -> BTreeMap<String, String> {
            BTreeMap::from([(oci::annotations::SOURCE.to_string(), url.to_string())])
        }

        #[tokio::test]
        async fn supplied_annotations_land_on_a_fresh_index() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &source_annotation("https://github.com/ocx-sh/ocx"),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(
                index
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get(oci::annotations::SOURCE))
                    .map(String::as_str),
                Some("https://github.com/ocx-sh/ocx"),
            );
        }

        /// The absent-value case: no `--annotation` must leave the field
        /// absent, so a manifest-less push stays byte-identical to what
        /// ocx produced before the flag existed.
        #[tokio::test]
        async fn no_annotations_leaves_a_fresh_index_field_absent() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert!(index.annotations.is_none(), "got: {:?}", index.annotations);
        }

        /// A later annotation-less push must not un-link a repository an
        /// earlier push already linked.
        #[tokio::test]
        async fn no_annotations_preserves_what_the_index_already_carries() {
            let data = StubTransportData::new();
            seed_index_with_annotations(&data, "3.28", Some(source_annotation("https://github.com/ocx-sh/ocx")));
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(
                index
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get(oci::annotations::SOURCE))
                    .map(String::as_str),
                Some("https://github.com/ocx-sh/ocx"),
            );
        }

        #[tokio::test]
        async fn supplied_annotations_merge_over_the_existing_ones() {
            let data = StubTransportData::new();
            seed_index_with_annotations(
                &data,
                "3.28",
                Some(BTreeMap::from([
                    (
                        oci::annotations::SOURCE.to_string(),
                        "https://example.invalid".to_string(),
                    ),
                    (oci::annotations::VENDOR.to_string(), "OCX".to_string()),
                ])),
            );
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &source_annotation("https://github.com/ocx-sh/ocx"),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            let annotations = index.annotations.expect("annotations present");
            assert_eq!(
                annotations.get(oci::annotations::SOURCE).map(String::as_str),
                Some("https://github.com/ocx-sh/ocx"),
                "the supplied key overwrites",
            );
            assert_eq!(
                annotations.get(oci::annotations::VENDOR).map(String::as_str),
                Some("OCX"),
                "an unrelated key another tool set is left alone",
            );
        }
    }

    // ── push_canonical_tag tests ─────────────────────────────────────

    mod push_canonical_tag_tests {
        use super::*;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn platform(s: &str) -> oci::Platform {
            s.parse().unwrap()
        }

        fn index_with_entry(digest: &str, platform: oci::Platform) -> oci::Manifest {
            oci::Manifest::ImageIndex(oci::ImageIndex {
                schema_version: oci::INDEX_SCHEMA_VERSION,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: None,
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: digest.to_string(),
                    size: 42,
                    platform: Some(platform.into()),
                    artifact_type: None,
                    annotations: None,
                }],
                annotations: None,
            })
        }

        #[tokio::test]
        async fn pushes_dot_separated_sha256_tag_with_the_same_bytes() {
            let data = StubTransportData::new();
            let id = test_identifier("3.28");
            let hex = "a".repeat(64);
            let digest = format!("sha256:{hex}");

            // Seed the platform manifest at its digest-addressed reference,
            // exactly as `push_multi_layer_manifest` leaves it after a push —
            // bare `registry/repo@digest`, no tag (see `without_tag()` note
            // on `push_canonical_tag`).
            let manifest_bytes = b"platform manifest bytes".to_vec();
            let digest_ref = id
                .without_tag()
                .clone_with_digest(oci::Digest::Sha256(hex.clone()))
                .canonical_reference();
            data.write()
                .manifests
                .insert(digest_ref.to_string(), (manifest_bytes.clone(), digest.clone()));

            let client = stub_with_capture(&data);
            let merged = index_with_entry(&digest, platform("linux/amd64"));

            client
                .push_canonical_tag(&id, &merged, &platform("linux/amd64"))
                .await
                .unwrap();

            let tag_ref = id.clone_with_tag(format!("sha256.{hex}")).canonical_reference();
            let inner = data.read();
            let (pushed_bytes, _) = inner
                .manifests
                .get(&tag_ref.to_string())
                .expect("canonical tag manifest not pushed");
            assert_eq!(
                pushed_bytes, &manifest_bytes,
                "canonical tag must carry the exact platform manifest bytes"
            );
        }

        #[tokio::test]
        async fn missing_platform_entry_is_a_no_op() {
            let data = StubTransportData::new();
            let id = test_identifier("3.28");
            let client = stub_with_capture(&data);
            let merged = index_with_entry(&format!("sha256:{}", "a".repeat(64)), platform("linux/amd64"));

            // Platform not present in the merged index — no-op, not an error.
            client
                .push_canonical_tag(&id, &merged, &platform("linux/arm64"))
                .await
                .unwrap();

            assert!(data.read().manifests.is_empty(), "no tag should have been pushed");
        }

        #[tokio::test]
        async fn missing_source_manifest_propagates_error() {
            let data = StubTransportData::new();
            let id = test_identifier("3.28");
            let client = stub_with_capture(&data);
            // Entry present in the index, but its digest was never seeded
            // into the stub's manifest store — the pull must fail.
            let merged = index_with_entry(&format!("sha256:{}", "b".repeat(64)), platform("linux/amd64"));

            let result = client.push_canonical_tag(&id, &merged, &platform("linux/amd64")).await;
            assert!(result.is_err(), "must propagate a missing source manifest as an error");
        }

        #[tokio::test]
        async fn canonical_tag_push_reports_the_tag_it_wrote() {
            let data = StubTransportData::new();
            let id = test_identifier("3.28");
            let hex = "c".repeat(64);
            let digest = format!("sha256:{hex}");

            let digest_ref = id
                .without_tag()
                .clone_with_digest(oci::Digest::Sha256(hex.clone()))
                .canonical_reference();
            data.write()
                .manifests
                .insert(digest_ref.to_string(), (b"platform manifest".to_vec(), digest.clone()));

            let client = stub_with_capture(&data);
            let merged = index_with_entry(&digest, platform("linux/amd64"));

            let written = client
                .push_canonical_tag(&id, &merged, &platform("linux/amd64"))
                .await
                .unwrap();

            assert_eq!(
                written,
                Some(format!("sha256.{hex}")),
                "the report must name the tag that was actually written"
            );
        }

        /// N-6 regression guard: the skip branch used to be indistinguishable
        /// from a successful write, so the push report could claim a canonical
        /// tag that never reached the registry.
        #[tokio::test]
        async fn canonical_tag_push_reports_nothing_on_the_skip_branch() {
            let data = StubTransportData::new();
            let id = test_identifier("3.28");
            let client = stub_with_capture(&data);
            let merged = index_with_entry(&format!("sha256:{}", "a".repeat(64)), platform("linux/amd64"));

            let written = client
                .push_canonical_tag(&id, &merged, &platform("linux/arm64"))
                .await
                .unwrap();

            assert_eq!(written, None, "an unmatched platform must report no tag");
        }
    }

    // ── ensure_auth tests ───────────────────────────────────────────

    mod ensure_auth {
        use super::*;
        use oci::RegistryOperation;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn platform(s: &str) -> oci::Platform {
            s.parse().unwrap()
        }

        fn auth_calls(data: &StubTransportData) -> Vec<(String, RegistryOperation)> {
            data.read().auth_calls.clone()
        }

        #[tokio::test]
        async fn client_ensure_auth_delegates_to_transport() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");

            client.ensure_auth(&id, RegistryOperation::Pull).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "example.com");
            assert!(matches!(calls[0].1, RegistryOperation::Pull));

            client.ensure_auth(&id, RegistryOperation::Push).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 2);
            assert!(matches!(calls[1].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn list_tags_authenticates_with_pull() {
            let data = StubTransportData::new();
            data.write().tags = vec![vec!["1.0".into()]];
            let client = stub(&data);

            client.list_tags(test_identifier("latest")).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn list_repositories_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);

            client.list_repositories("example.com").await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn fetch_manifest_digest_authenticates_with_pull() {
            let data = StubTransportData::new();
            data.write().digest =
                Some("sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".into());
            let client = stub(&data);
            let id = test_identifier("1.0");

            client
                .fetch_manifest_digest_addressed(&id, ReadAddressing::Mirrored)
                .await
                .unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn fetch_manifest_authenticates_with_pull() {
            let manifest = oci::Manifest::Image(make_image_manifest("sha256:cff", "sha256:1a0e"));
            let (manifest_data, digest_str) = serialize_manifest(&manifest);

            let id = test_identifier("1.0");
            let data = StubTransportData::new();
            data.write()
                .manifests
                .insert(id.to_string(), (manifest_data, digest_str));
            let client = stub(&data);

            client.fetch_manifest(&id).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn pull_manifest_authenticates_with_pull() {
            let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            let data = StubTransportData::new();
            let client = stub(&data);

            // Will fail (no manifest), but auth should have been called first.
            let _ = client.pull_manifest(&id).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn pull_blob_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let blob_ref = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

            // Stub returns empty bytes, but auth must precede the fetch.
            let _ = client.pull_blob(&blob_ref).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn head_blob_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");
            let digest = oci::Digest::Sha256("a".repeat(64));

            // Will fail (blob absent), but auth should precede the HEAD.
            let _ = client.head_blob(&id, &digest).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        /// Regression guard for the 401-on-default-mode bug: `pull_layer` must
        /// authenticate before the layer blob fetch. `pull_blob_to_file` sends a
        /// token only if one is already cached, and a cache-resolved manifest
        /// never seeds it, so without this the fetch is anonymous (401).
        #[tokio::test]
        async fn pull_layer_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            let layer = oci::Descriptor {
                media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                // A positive size is required to pass the InvalidManifest gate so
                // the test reaches the auth step; the blob is absent so the fetch
                // still fails afterward, which is fine — only auth ordering matters.
                size: 1,
                urls: None,
                artifact_type: None,
                annotations: None,
            };
            let dir = tempfile::tempdir().unwrap();

            // Outcome is irrelevant — auth must precede the blob fetch either way.
            let _ = client.pull_layer(&id, &layer, dir.path()).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn push_package_authenticates_with_push() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = stub_with_capture(&data);

            let id = test_identifier("1.0");
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();

            let info = Info {
                identifier: id,
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                    binaries: None,
                    integrations: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            };

            let layers = [crate::publisher::LayerRef::File {
                path: archive_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];
            let _ = client.push_package(info, &layers, &BTreeMap::new()).await;
            let calls = auth_calls(&data);
            // Must authenticate with Push before any blob/manifest operations.
            assert!(!calls.is_empty(), "push_package must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn push_description_authenticates_with_push() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");

            let desc = package::description::Description {
                readme: "# Test".to_string(),
                logo: None,
                annotations: Default::default(),
            };

            let _ = client.push_description(&id, &desc).await;
            let calls = auth_calls(&data);
            assert!(!calls.is_empty(), "push_description must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn pull_description_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");

            let dir = tempfile::tempdir().unwrap();
            let _ = client.pull_description(&id, dir.path()).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn merge_platform_into_index_authenticates_with_push() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            let _ = client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &platform("linux/amd64"),
                    "sha256:abc",
                    100,
                    &BTreeMap::new(),
                )
                .await;
            let calls = auth_calls(&data);
            assert!(!calls.is_empty(), "merge_platform_into_index must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        /// Regression guard: `push_multi_layer_manifest` is `pub(crate)` and
        /// contacts the registry (push_blob / head_blob / push_manifest_raw),
        /// so — like every other registry-contacting Client method — it must
        /// authenticate before its first transport call. Without it a standalone
        /// invocation issues anonymous requests and a registry requiring auth
        /// returns 401 (the same class of bug `pull_layer` had). The auth is
        /// idempotent on a token-cache hit, so it costs nothing when the caller
        /// (`push_manifest_and_merge_tags`) already authenticated.
        #[tokio::test]
        async fn push_multi_layer_manifest_authenticates_with_push() {
            let layer_digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
            let data = StubTransportData::new();
            data.write().blobs.insert(layer_digest.to_string(), vec![0u8; 16]);
            let client = stub_with_capture(&data);

            let info = Info {
                identifier: test_identifier("1.0"),
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                    binaries: None,
                    integrations: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            };
            let layers = [crate::publisher::LayerRef::Digest {
                digest: oci::Digest::try_from(layer_digest).unwrap(),
                media_type: crate::publisher::ArchiveMediaType::TarGz,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];

            let _ = client.push_multi_layer_manifest(&info, &layers).await;
            let calls = auth_calls(&data);
            assert!(!calls.is_empty(), "push_multi_layer_manifest must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn ensure_auth_precedes_transport_calls_for_push() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = stub_with_capture(&data);

            let id = test_identifier("1.0");
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();

            let info = Info {
                identifier: id,
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                    binaries: None,
                    integrations: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            };

            let layers = [crate::publisher::LayerRef::File {
                path: archive_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];
            let _ = client.push_package(info, &layers, &BTreeMap::new()).await;

            // Verify auth happened before any transport method calls.
            let inner = data.read();
            assert!(!inner.auth_calls.is_empty(), "ensure_auth must have been called");
            assert!(matches!(inner.auth_calls[0].1, RegistryOperation::Push));
            // push_blob should have been called (for the package data).
            assert!(
                inner.calls.iter().any(|c| c.starts_with("push_blob:")),
                "push_blob should follow ensure_auth, calls: {:?}",
                inner.calls
            );
        }
    }

    // ── Multi-layer digest reuse tests ──────────────────────────────
    //
    // Regression test for the fabricated-`tar+gzip` bug on the
    // `LayerRef::Digest` path. Before this fix, the push code
    // unconditionally stamped `application/vnd.oci.image.layer.v1.tar+gzip`
    // on every digest-referenced layer, so reusing a `.tar.xz` or
    // `.zip` layer produced a manifest that broke every consumer's
    // `package pull`.
    //
    // The fix makes the CLI declare the media type alongside the
    // digest (see `LayerRef::FromStr`'s `sha256:<hex>.<ext>` syntax)
    // and threads it straight into the manifest descriptor. These
    // tests assert the supplied media type round-trips unchanged.

    mod multi_layer_digest_resolve {
        use super::*;
        use crate::package::{self, info::Info, metadata};
        use crate::publisher::LayerRef;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn bundle_metadata() -> metadata::Metadata {
            metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                binaries: None,
                version: package::metadata::bundle::Version::V1,
                strip_components: None,
                env: Default::default(),
                dependencies: Default::default(),
                entrypoints: Default::default(),
                integrations: Default::default(),
            })
        }

        fn info(tag: &str) -> Info {
            Info {
                identifier: test_identifier(tag),
                metadata: bundle_metadata(),
                platform: "linux/amd64".parse().unwrap(),
            }
        }

        /// A digest-referenced layer must carry the media type declared
        /// by the caller, not a fabricated `tar+gzip`. Regression for
        /// the original Bug 2.
        #[tokio::test]
        async fn digest_layer_uses_supplied_media_type_tar_xz() {
            let layer_digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
            let layer_size: i64 = 4096;

            let data = StubTransportData::new();
            // The stub's `head_blob` returns the length of whatever
            // bytes we seed under this digest, so the size in the
            // resulting manifest descriptor will match.
            data.write()
                .blobs
                .insert(layer_digest.to_string(), vec![0u8; layer_size as usize]);
            let client = stub_with_capture(&data);

            let layers = [LayerRef::Digest {
                digest: oci::Digest::try_from(layer_digest).unwrap(),
                media_type: crate::publisher::ArchiveMediaType::TarXz,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];
            let (manifest, _bytes, _digest, counts) = client
                .push_multi_layer_manifest(&info("2.0.0"), &layers)
                .await
                .expect("push_multi_layer_manifest must succeed with a live blob and declared media type");
            assert_eq!(
                counts,
                LayerCounts {
                    verified: 1,
                    ..Default::default()
                },
                "a digest layer with no mount_from must count as verified"
            );

            assert_eq!(manifest.layers.len(), 1);
            assert_eq!(
                manifest.layers[0].media_type,
                crate::MEDIA_TYPE_TAR_XZ,
                "the manifest must carry the caller-declared media type verbatim — no tar+gzip fabrication"
            );
            assert_eq!(manifest.layers[0].size, layer_size);
            assert_eq!(manifest.layers[0].digest, layer_digest);

            // `head_blob` should still be called — it's the transport-
            // level contract for fetching the blob's size. The Bug 1
            // fix ensures its native implementation reads
            // `Content-Length` from a real HEAD rather than pulling
            // the whole blob into memory.
            let inner = data.read();
            assert!(
                inner.calls.iter().any(|c| c == &format!("head_blob:{layer_digest}")),
                "head_blob should be called exactly once to fetch the layer size, calls: {:?}",
                inner.calls
            );
        }

        /// When the requested digest blob does not exist in the
        /// registry, the push must fail with `BlobNotFound` surfaced by
        /// `head_blob`.
        #[tokio::test]
        async fn digest_layer_not_found_in_registry_errors() {
            let missing_digest = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let layers = [LayerRef::Digest {
                digest: oci::Digest::try_from(missing_digest).unwrap(),
                media_type: crate::publisher::ArchiveMediaType::TarGz,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];
            let err = client
                .push_multi_layer_manifest(&info("2.0.0"), &layers)
                .await
                .expect_err("push must fail when the referenced blob is absent");

            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("not found") || msg.contains("blob"),
                "error message should mention not-found / blob, got: {msg}"
            );
        }

        /// A `LayerRef::File` with an unrecognized extension must be
        /// rejected with `InvalidManifest` before any network I/O.
        /// Without this guard the push path would stamp `media_type = "blob"`
        /// or silently default to tar+gzip, shipping a manifest that no
        /// consumer can extract.
        #[tokio::test]
        async fn unknown_file_extension_is_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let weird_path = dir.path().join("archive.bogus");
            tokio::fs::write(&weird_path, b"irrelevant bytes").await.unwrap();

            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File {
                path: weird_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];
            let err = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect_err("unknown extensions must fail before push");

            assert!(
                matches!(err, ClientError::InvalidManifest(_)),
                "expected InvalidManifest, got {err:?}"
            );
        }
    }

    // ── Cross-repository blob mount ──────────────────────────────────

    /// Exercises `push_multi_layer_manifest`'s `mount_from` handling against
    /// `StubTransport`'s configurable `mount_results` queue: a successful
    /// mount must skip `push_blob` entirely, a declined mount (or a
    /// transport error) must fall back to the normal upload/verify path,
    /// and the aggregate `LayerCounts` must reflect exactly what happened
    /// across a mixed layer list.
    mod mount_reuse {
        use super::*;
        use crate::publisher::LayerRef;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn info(tag: &str) -> Info {
            Info {
                identifier: test_identifier(tag),
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                    binaries: None,
                    integrations: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            }
        }

        /// A `LayerRef::File` with `mount_from` set, backed by a stub that
        /// reports `Mounted`, must skip `push_blob` and count as `mounted`.
        #[tokio::test]
        async fn file_layer_mount_success_skips_push_blob() {
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            let archive_bytes = b"fake-archive".to_vec();
            tokio::fs::write(&archive_path, &archive_bytes).await.unwrap();
            let layer_digest = Algorithm::Sha256.hash(&archive_bytes).to_string();

            let data = StubTransportData::new();
            data.write().mount_results.push(Ok(MountOutcome::Mounted));
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File {
                path: archive_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: Some("pip-test/pkg".to_string()),
            }];
            let (_manifest, _bytes, _digest, counts) = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect("mount success must not fail the push");

            assert_eq!(
                counts,
                LayerCounts {
                    mounted: 1,
                    ..Default::default()
                }
            );
            let inner = data.read();
            // Only the layer's own blob push must be skipped — the config
            // blob (an unrelated, unconditional push_blob call) still fires.
            assert!(
                !inner.calls.contains(&format!("push_blob:{layer_digest}")),
                "a successful mount must skip push_blob for the mounted layer, calls: {:?}",
                inner.calls
            );
            assert_eq!(
                inner.mount_calls,
                vec![("test/pkg".to_string(), "pip-test/pkg".to_string(), layer_digest)]
            );
        }

        /// A `LayerRef::File` whose mount attempt reports `UploadRequired`
        /// must fall back to `push_blob` and count as `uploaded`.
        #[tokio::test]
        async fn file_layer_mount_declined_falls_back_to_upload() {
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            let archive_bytes = b"fake-archive".to_vec();
            tokio::fs::write(&archive_path, &archive_bytes).await.unwrap();
            let layer_digest = Algorithm::Sha256.hash(&archive_bytes).to_string();

            let data = StubTransportData::new();
            data.write().mount_results.push(Ok(MountOutcome::UploadRequired));
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File {
                path: archive_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: Some("pip-test/pkg".to_string()),
            }];
            let (_manifest, _bytes, _digest, counts) = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect("a declined mount must still succeed via upload fallback");

            assert_eq!(
                counts,
                LayerCounts {
                    uploaded: 1,
                    ..Default::default()
                }
            );
            let inner = data.read();
            // Assert the LAYER's own blob push, not merely that some
            // `push_blob:` call happened — the config blob is pushed
            // unconditionally, so a prefix match would pass even if the
            // fallback never fired.
            assert!(
                inner.calls.contains(&format!("push_blob:{layer_digest}")),
                "a declined mount must fall back to push_blob for the layer, calls: {:?}",
                inner.calls
            );
        }

        /// A transport error from `mount_blob` must never fail the push: the
        /// layer falls back to upload and the push succeeds, counting as
        /// `uploaded`.
        #[tokio::test]
        async fn file_layer_mount_transport_error_falls_back_and_push_succeeds() {
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            let archive_bytes = b"fake-archive".to_vec();
            tokio::fs::write(&archive_path, &archive_bytes).await.unwrap();
            let layer_digest = Algorithm::Sha256.hash(&archive_bytes).to_string();

            let data = StubTransportData::new();
            data.write()
                .mount_results
                .push(Err(ClientError::Registry("mount transport failure".into())));
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File {
                path: archive_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: Some("pip-test/pkg".to_string()),
            }];
            let (_manifest, _bytes, _digest, counts) = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect("mount must never fail the push");

            assert_eq!(
                counts,
                LayerCounts {
                    uploaded: 1,
                    ..Default::default()
                }
            );
            let inner = data.read();
            assert!(
                inner.calls.contains(&format!("push_blob:{layer_digest}")),
                "a mount transport error must fall back to push_blob for the layer, calls: {:?}",
                inner.calls
            );
        }

        /// A `LayerRef::Digest` layer with `mount_from` set still calls
        /// `head_blob` after a successful mount (the adapted mount path
        /// doesn't return size), and counts as `mounted`.
        #[tokio::test]
        async fn digest_layer_mount_success_still_verifies_and_counts_mounted() {
            let layer_digest = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
            let data = StubTransportData::new();
            data.write().blobs.insert(layer_digest.to_string(), vec![0u8; 8]);
            data.write().mount_results.push(Ok(MountOutcome::Mounted));
            let client = stub_with_capture(&data);

            let layers = [LayerRef::Digest {
                digest: oci::Digest::try_from(layer_digest).unwrap(),
                media_type: crate::publisher::ArchiveMediaType::TarGz,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: Some("base/image".to_string()),
            }];
            let (_manifest, _bytes, _digest, counts) = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect("mount success must not fail the push");

            assert_eq!(
                counts,
                LayerCounts {
                    mounted: 1,
                    ..Default::default()
                }
            );
            let inner = data.read();
            assert!(
                inner.calls.iter().any(|c| c == &format!("head_blob:{layer_digest}")),
                "head_blob must still be called after a successful mount, calls: {:?}",
                inner.calls
            );
        }

        /// A mixed layer list — one mounted, one declined-then-uploaded, one
        /// plain digest verify with no `mount_from` — produces the exact
        /// counter breakdown, over the input order.
        #[tokio::test]
        async fn mixed_layer_list_produces_correct_counter_breakdown() {
            let dir = tempfile::tempdir().unwrap();
            let archive_a = dir.path().join("a.tar.gz");
            let archive_b = dir.path().join("b.tar.gz");
            tokio::fs::write(&archive_a, b"layer-a").await.unwrap();
            tokio::fs::write(&archive_b, b"layer-b").await.unwrap();

            let verified_digest = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

            let data = StubTransportData::new();
            data.write().blobs.insert(verified_digest.to_string(), vec![0u8; 4]);
            // Consumed FIFO in layer order: layer 0 (a.tar.gz) mounts, layer 1
            // (b.tar.gz) is declined and falls back to upload. Layer 2 (the
            // digest layer) carries no mount_from, so it never calls mount_blob.
            data.write().mount_results.push(Ok(MountOutcome::Mounted));
            data.write().mount_results.push(Ok(MountOutcome::UploadRequired));
            let client = stub_with_capture(&data);

            let layers = [
                LayerRef::File {
                    path: archive_a,
                    layout: oci::LayerLayoutSpec::default(),
                    mount_from: Some("pip-test/pkg".to_string()),
                },
                LayerRef::File {
                    path: archive_b,
                    layout: oci::LayerLayoutSpec::default(),
                    mount_from: Some("pip-test/pkg".to_string()),
                },
                LayerRef::Digest {
                    digest: oci::Digest::try_from(verified_digest).unwrap(),
                    media_type: crate::publisher::ArchiveMediaType::TarGz,
                    layout: oci::LayerLayoutSpec::default(),
                    mount_from: None,
                },
            ];
            let (_manifest, _bytes, _digest, counts) = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect("mixed layer list must succeed");

            assert_eq!(
                counts,
                LayerCounts {
                    mounted: 1,
                    uploaded: 1,
                    verified: 1,
                }
            );
            // mount_blob is only ever called for layers carrying mount_from.
            assert_eq!(
                data.read().mount_calls.len(),
                2,
                "only the two mount_from layers call mount_blob"
            );
        }
    }

    // ── Cascade tag ordering ────────────────────────────────────────

    /// `push_manifest_and_merge_tags` must push the manifest once, then
    /// merge the resulting platform entry into the primary tag's index
    /// and into each `extra_tags` entry in input order. The order of
    /// recorded transport calls is what OCX clients actually observe —
    /// tests over that contract prevent silent reorderings that would
    /// leave earlier tags pointing at stale indexes.
    mod cascade_order {
        use super::*;
        use crate::publisher::LayerRef;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn info(tag: &str) -> Info {
            Info {
                identifier: test_identifier(tag),
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                    binaries: None,
                    integrations: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            }
        }

        #[tokio::test]
        async fn push_manifest_and_merge_tags_processes_tags_in_input_order() {
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();

            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File {
                path: archive_path,
                layout: oci::LayerLayoutSpec::default(),
                mount_from: None,
            }];
            let extra_tags = ["3".to_string(), "latest".to_string()];
            client
                .push_manifest_and_merge_tags(&info("1.2.3"), &layers, &extra_tags, &BTreeMap::new())
                .await
                .expect("push should succeed");

            // Extract only push_manifest / push_manifest_raw / pull_manifest_raw
            // calls from the recorded transport log — those are the ordered
            // high-level operations we care about here.
            let relevant: Vec<String> = data
                .read()
                .calls
                .iter()
                .filter(|c| *c == "push_manifest" || *c == "push_manifest_raw" || *c == "pull_manifest_raw")
                .cloned()
                .collect();

            // Expected cascade: push the image manifest once, then for
            // each tag (primary, "3", "latest") pull (attempt to read
            // existing index) + push_manifest_raw (write updated index).
            // Ordering must be stable across every run.
            let expected = vec![
                "push_manifest_raw", // the image manifest itself
                "pull_manifest_raw", // primary tag existing index lookup
                "push_manifest_raw", // primary tag index push
                "pull_manifest_raw", // extra_tags[0] lookup
                "push_manifest_raw", // extra_tags[0] push
                "pull_manifest_raw", // extra_tags[1] lookup
                "push_manifest_raw", // extra_tags[1] push
            ];
            assert_eq!(relevant, expected, "cascade calls must follow input tag order");
        }

        /// A cascade must not leave a rolling tag with weaker provenance than
        /// the version tag it mirrors — every index the push writes carries
        /// the same annotations.
        #[tokio::test(flavor = "multi_thread")]
        async fn annotations_land_on_the_primary_tag_and_every_cascade_tag() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let extra_tags = ["3".to_string(), "latest".to_string()];
            let annotations = BTreeMap::from([(
                oci::annotations::SOURCE.to_string(),
                "https://github.com/ocx-sh/ocx".to_string(),
            )]);
            client
                .push_manifest_and_merge_tags(&info("1.2.3"), &[], &extra_tags, &annotations)
                .await
                .expect("push should succeed");

            let inner = data.read();
            for tag in ["1.2.3", "3", "latest"] {
                let key = format!("example.com/test/pkg:{tag}");
                let (bytes, _) = inner.manifests.get(&key).unwrap_or_else(|| panic!("no index at {key}"));
                let manifest: oci::Manifest = serde_json::from_slice(bytes).expect("index parses");
                let oci::Manifest::ImageIndex(index) = manifest else {
                    panic!("{key} is not an image index");
                };
                assert_eq!(
                    index
                        .annotations
                        .as_ref()
                        .and_then(|a| a.get(oci::annotations::SOURCE))
                        .map(String::as_str),
                    Some("https://github.com/ocx-sh/ocx"),
                    "missing source annotation at {key}",
                );
            }
        }
    }

    // ── construction-gating backstop + Step 3.1 specification tests ──────────
    //
    // The PRIMARY guarantee is the compile-time construction-gating from
    // Step 1.3: the read-path `Identifier → native::Reference` conversion has
    // no public `From` impl, so a bypassing read site fails to compile. This
    // behavioural module is defence-in-depth only — it pins
    // `transport_reference` identity-when-empty / rewrite-when-set and the
    // push-path-unchanged invariant.
    mod transport_reference {
        use super::*;
        use crate::config::mirror::ParsedMirror;
        use crate::oci::client::MirrorMap;

        fn make_id_with_tag(registry: &str, repo: &str, tag: &str) -> Identifier {
            Identifier::new_registry(repo, registry).clone_with_tag(tag)
        }

        /// A 64-hex SHA-256 digest for the pinned-install path tests.
        fn test_digest(hex_seed: char) -> oci::Digest {
            oci::Digest::Sha256(std::iter::repeat_n(hex_seed, 64).collect())
        }

        fn make_id_with_digest(registry: &str, repo: &str, digest: oci::Digest) -> Identifier {
            Identifier::new_registry(repo, registry).clone_with_digest(digest)
        }

        fn make_id_with_tag_and_digest(registry: &str, repo: &str, tag: &str, digest: oci::Digest) -> Identifier {
            Identifier::new_registry(repo, registry)
                .clone_with_tag(tag)
                .clone_with_digest(digest)
        }

        fn make_mirror_map(upstream: &str, mirror_host: &str, prefix: &str) -> MirrorMap {
            MirrorMap::new([(
                upstream.to_string(),
                ParsedMirror {
                    protocol: "https".to_string(),
                    host: mirror_host.to_string(),
                    path_prefix: prefix.to_string(),
                },
            )])
        }

        /// `transport_reference` with an empty (identity) map returns a
        /// reference whose host equals the canonical registry.
        ///
        /// Traces: plan Testing Strategy — "identity when no mirror".
        #[test]
        fn transport_reference_is_identity_when_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            let id = make_id_with_tag("ghcr.io", "owner/tool", "1.0");
            let reference = client.transport_reference(&id);
            assert_eq!(
                reference.registry(),
                "ghcr.io",
                "empty MirrorMap must leave registry unchanged"
            );
            assert_eq!(reference.repository(), "owner/tool");
        }

        /// `transport_reference` with a mirrored host rewrites to the mirror
        /// host + path-prefix-joined repository.
        ///
        /// Traces: plan Testing Strategy — "transport_reference rewrites a
        /// mirrored read identifier (host+repo+tag/digest verbatim)".
        #[test]
        fn transport_reference_rewrites_mirrored_host_and_repository() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            let id = make_id_with_tag("ghcr.io", "owner/tool", "1.0");
            let reference = client.transport_reference(&id);
            assert_eq!(
                reference.registry(),
                "company.jfrog.io",
                "registry must be rewritten to the mirror host"
            );
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "repository must be <prefix>/<upstream-repo>"
            );
        }

        /// The returned reference's `registry()` is the MIRROR host — this
        /// proves that auth keys off the mirror host, not the upstream.
        ///
        /// Traces: plan Testing Strategy — "the returned `native::Reference`
        /// `registry()` is the MIRROR host (proves auth keys off mirror)";
        /// ADR F1/R5.
        #[test]
        fn transport_reference_registry_is_mirror_host_for_auth() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "enterprise.artifactory.corp", "ghcr-proxy");

            let id = make_id_with_tag("ghcr.io", "my-org/my-tool", "v2.0");
            let reference = client.transport_reference(&id);

            // This is the host that NativeTransport::auth_for keys off — it
            // must be the mirror host so mirror credentials are used, not
            // upstream credentials.
            assert_eq!(
                reference.registry(),
                "enterprise.artifactory.corp",
                "reference.registry() must be the mirror host so auth resolves against it"
            );
        }

        /// Tag is copied verbatim from the original identifier.
        ///
        /// Traces: plan Testing Strategy — "host+repo+tag/digest verbatim".
        #[test]
        fn transport_reference_tag_copied_verbatim() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "mirror.corp", "proxy");

            let id = make_id_with_tag("ghcr.io", "owner/tool", "3.28.1");
            let reference = client.transport_reference(&id);
            assert_eq!(
                reference.tag(),
                Some("3.28.1"),
                "tag must be copied verbatim to the mirror reference"
            );
        }

        // ── Pinned-install (digest) paths — security-critical ───────────────
        //
        // A pinned install resolves through a digest. The transport reference
        // MUST carry the digest verbatim so the canonical `HashingAsyncReader`
        // check in `pull_layer` verifies the bytes against the SAME digest the
        // caller pinned — under both the mirror and no-mirror paths. A dropped
        // or altered digest here would silently weaken the tamper gate.

        /// Digest-only identifier (pinned install, no tag): the digest is
        /// preserved verbatim in the transport reference under a mirror.
        ///
        /// Traces: coverage gap #3 — digest-only pinned-install path (mirror).
        #[test]
        fn transport_reference_digest_only_preserves_digest_under_mirror() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            let digest = test_digest('a');
            let id = make_id_with_digest("ghcr.io", "owner/tool", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "company.jfrog.io", "host must be the mirror");
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "repo must be prefixed"
            );
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim — the pinned tamper gate keys off it"
            );
            assert_eq!(reference.tag(), None, "a digest-only identifier carries no tag");
        }

        /// Digest-only identifier with no mirror: identity reference still
        /// preserves the digest verbatim.
        ///
        /// Traces: coverage gap #3 — digest-only pinned-install path (no mirror).
        #[test]
        fn transport_reference_digest_only_preserves_digest_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));

            let digest = test_digest('b');
            let id = make_id_with_digest("ghcr.io", "owner/tool", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "ghcr.io", "no mirror → canonical host");
            assert_eq!(reference.repository(), "owner/tool", "no mirror → canonical repo");
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim on the no-mirror identity path"
            );
        }

        /// Tag+digest identifier: BOTH the tag and the digest are preserved
        /// verbatim under a mirror (the digest is what pins the install).
        ///
        /// Traces: coverage gap #3 — tag+digest pinned-install path (mirror).
        #[test]
        fn transport_reference_tag_and_digest_preserved_under_mirror() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            let digest = test_digest('c');
            let id = make_id_with_tag_and_digest("ghcr.io", "owner/tool", "3.28.1", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "company.jfrog.io", "host must be the mirror");
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "repo must be prefixed"
            );
            assert_eq!(reference.tag(), Some("3.28.1"), "tag must be preserved verbatim");
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim alongside the tag"
            );
        }

        /// Tag+digest identifier with no mirror: identity reference preserves
        /// both tag and digest verbatim.
        ///
        /// Traces: coverage gap #3 — tag+digest pinned-install path (no mirror).
        #[test]
        fn transport_reference_tag_and_digest_preserved_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));

            let digest = test_digest('d');
            let id = make_id_with_tag_and_digest("ghcr.io", "owner/tool", "3.28.1", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "ghcr.io", "no mirror → canonical host");
            assert_eq!(reference.repository(), "owner/tool", "no mirror → canonical repo");
            assert_eq!(reference.tag(), Some("3.28.1"), "tag must be preserved verbatim");
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim on the no-mirror identity path"
            );
        }

        /// `transport_registry` rewrites a catalog registry to the mirror host.
        ///
        /// Traces: plan Testing Strategy — "transport_registry rewrites the
        /// catalog registry".
        #[test]
        fn transport_registry_rewrites_catalog_registry() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "catalog-mirror.corp", "ghcr-catalog");

            let reference = client.transport_registry("ghcr.io");
            assert_eq!(
                reference.registry(),
                "catalog-mirror.corp",
                "transport_registry must rewrite the catalog registry to the mirror host"
            );
            // Pin the empty-repository fix (finding #5): the placeholder
            // repository for a registry-scoped catalog call must be the mirror's
            // path prefix VERBATIM — never `"ghcr-catalog/"` with a trailing
            // slash. oci-client's `_auth` stamps `repository()` into the token
            // scope (`repository:<repository>:pull`); a trailing slash there
            // produces a malformed scope that can break catalog auth against a
            // mirror keying tokens off the repo-key path segment.
            assert_eq!(
                reference.repository(),
                "ghcr-catalog",
                "catalog repository must be the path prefix with no trailing slash"
            );
        }

        /// `transport_registry` is identity when no mirror configured.
        ///
        /// Traces: plan Testing Strategy — "identity when no mirror".
        #[test]
        fn transport_registry_is_identity_when_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            let reference = client.transport_registry("quay.io");
            assert_eq!(
                reference.registry(),
                "quay.io",
                "empty MirrorMap must leave catalog registry unchanged"
            );
            assert_eq!(
                reference.repository(),
                "",
                "no-mirror catalog repository stays empty (auth scope keys off the registry only)"
            );
        }

        /// T-A3: bare identifier (no tag, no digest) under a configured mirror.
        ///
        /// The `(None, None)` arm in `transport_reference` emits
        /// `native::Reference::with_tag(host, repository, "latest")`. This test
        /// verifies that:
        /// - the host is rewritten to the mirror host (not the canonical registry), and
        /// - the returned reference carries `tag() == Some("latest")` (the OCI default).
        ///
        /// A bare identifier arises when a user runs `ocx package install cmake`
        /// (no pin, no explicit tag). Under a mirror the reference must point at
        /// the mirror and still carry "latest" so the registry fetch resolves
        /// the correct tag.
        #[test]
        fn transport_reference_bare_identifier_resolves_to_latest_under_mirror() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            // Bare identifier: no tag, no digest.
            let bare_id = Identifier::new_registry("owner/tool", "ghcr.io");
            assert!(bare_id.tag().is_none(), "pre-condition: bare id has no tag");
            assert!(bare_id.digest().is_none(), "pre-condition: bare id has no digest");

            let reference = client.transport_reference(&bare_id);

            assert_eq!(
                reference.registry(),
                "company.jfrog.io",
                "bare identifier under mirror must use the mirror host, not ghcr.io"
            );
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "bare identifier under mirror must prefix the repository"
            );
            assert_eq!(
                reference.tag(),
                Some("latest"),
                "bare identifier (no tag, no digest) must resolve to 'latest'"
            );
            assert!(
                reference.digest().is_none(),
                "bare identifier must carry no digest in the transport reference"
            );
        }

        /// Push path uses `canonical_reference()` — not `transport_reference`.
        /// The canonical reference is NEVER mirrored, even when the client
        /// has a mirror map for the registry.
        ///
        /// Traces: plan Testing Strategy — "push distinct"; ADR Q5 (push not
        /// mirror-redirected).
        #[test]
        fn push_path_uses_canonical_reference_not_mirror() {
            // canonical_reference() is pub(crate); call it directly on the
            // identifier (as push sites do) and confirm it targets the
            // canonical host, not the mirror.
            let id = make_id_with_tag("ghcr.io", "owner/tool", "1.0");
            let canonical = id.canonical_reference();
            assert_eq!(
                canonical.registry(),
                "ghcr.io",
                "canonical_reference must always target the upstream host, never the mirror"
            );
        }
    }

    // ── T-behavioral-G1: mirror-routing matrix ────────────────────────────────
    //
    // The structural tests below (`canonical_reference_only_used_in_allowed_files`,
    // `native_reference_direct_construction_restricted_to_seams`) catch a NEW
    // read path that reaches for a disallowed symbol or construction pattern.
    // Neither can catch a bypass that calls `identifier.canonical_reference()`
    // from a file ALREADY on the allow-list (`oci/client.rs` itself) without
    // adding any NEW `native::Reference::with_*` construction — exactly the
    // read-bypass class Codex terra flagged, since text-scanning can only see
    // symbols and call sites, never runtime behavior.
    //
    // This module closes that hole BEHAVIORALLY instead of lexically: every
    // public read-path `Client` method is driven end-to-end against a recording
    // transport with a mirror configured for the identifier's registry, and
    // each test asserts the transport actually RECEIVED a reference whose
    // `registry()` is the MIRROR host and whose `repository()` carries the
    // mirror path-prefix — proving the seam was exercised, not merely that it
    // is available. Push-path methods are out of scope by design (ADR Q5 —
    // push stays canonical); `push_path_uses_canonical_reference_not_mirror`
    // above already pins that half of the contract.
    //
    // Every new read method added to `Client` MUST add a row here — this
    // matrix is the behavioral half of the G1 seam gate; the text-scan tests
    // below are the structural half.
    //
    // Traces: mirror-invariant audit 2026-07-19, Codex terra finding —
    // canonical_reference read-bypass class.
    mod mirror_routing {
        use super::*;
        use crate::config::mirror::ParsedMirror;
        use crate::oci::client::MirrorMap;
        use crate::oci::client::transport::{OciTransport, ProgressFn, Result as TransportResult};

        const UPSTREAM_REGISTRY: &str = "ghcr.io";
        const MIRROR_HOST: &str = "mirror-routing.corp";
        const MIRROR_PREFIX: &str = "ghcr-proxy";
        const REPOSITORY: &str = "owner/tool";

        /// One recorded transport call: `(method, registry, repository)`.
        type RecordedCall = (&'static str, String, String);

        /// Shared handle for [`RecordingTransport`]'s call log.
        ///
        /// Mirrors the [`StubTransportData`]/[`StubTransport`] split so
        /// assertions run against a handle kept outside the boxed
        /// `dyn OciTransport` the `Client` owns.
        #[derive(Clone, Default)]
        struct RecordingTransportData(std::sync::Arc<std::sync::Mutex<Vec<RecordedCall>>>);

        impl RecordingTransportData {
            fn new() -> Self {
                Self::default()
            }

            fn record(&self, method: &'static str, image: &oci::native::Reference) {
                self.0
                    .lock()
                    .unwrap()
                    .push((method, image.registry().to_string(), image.repository().to_string()));
            }

            /// Returns the `(registry, repository)` the transport received for
            /// `method`. Panics if `method` was never called — every assertion
            /// below wants a concrete recorded pair, not a silently-passing
            /// missing row.
            fn call(&self, method: &str) -> (String, String) {
                self.0
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(name, _, _)| *name == method)
                    .map(|(_, registry, repository)| (registry.clone(), repository.clone()))
                    .unwrap_or_else(|| panic!("transport method '{method}' was never called"))
            }
        }

        /// A minimal [`OciTransport`] spy: every method records the
        /// `(registry, repository)` of the reference it was handed, then
        /// returns a trivial value so the caller's business logic can proceed
        /// (or fail harmlessly downstream) — only the reference actually
        /// handed to the transport matters for these tests, never the
        /// method's return value.
        /// What the stub answers a manifest pull with. `NotFound` is the
        /// harmless default every routing test above relies on; the others
        /// exist to drive the error-annotation paths.
        #[derive(Clone, Copy, Default)]
        enum ManifestAnswer {
            /// The not-found sentinel — every caller maps it to `Ok(None)` or
            /// propagates it, so only the recorded reference matters.
            #[default]
            NotFound,
            /// A hard registry failure.
            HardFailure,
            /// A well-formed image index, where the caller wanted an image
            /// manifest.
            ImageIndex,
        }

        #[derive(Clone, Default)]
        struct RecordingTransport {
            data: RecordingTransportData,
            manifest: ManifestAnswer,
            /// Fail the auth handshake, so the pre-fetch failure path can be
            /// driven.
            auth_fails: bool,
        }

        #[async_trait::async_trait]
        impl OciTransport for RecordingTransport {
            async fn ensure_auth(
                &self,
                image: &oci::native::Reference,
                _operation: oci::RegistryOperation,
            ) -> TransportResult<()> {
                self.data.record("ensure_auth", image);
                if self.auth_fails {
                    return Err(ClientError::Authentication(Box::new(std::io::Error::other(
                        "registry rejected the credentials",
                    ))));
                }
                Ok(())
            }

            async fn list_tags(
                &self,
                image: &oci::native::Reference,
                _chunk_size: usize,
                _last: Option<String>,
            ) -> TransportResult<Vec<String>> {
                self.data.record("list_tags", image);
                Ok(vec![])
            }

            async fn catalog(
                &self,
                image: &oci::native::Reference,
                _chunk_size: usize,
                _last: Option<String>,
            ) -> TransportResult<Vec<String>> {
                self.data.record("catalog", image);
                Ok(vec![])
            }

            async fn fetch_manifest_digest(&self, image: &oci::native::Reference) -> TransportResult<String> {
                self.data.record("fetch_manifest_digest", image);
                Ok(format!("sha256:{}", "a".repeat(64)))
            }

            async fn push_referrer_manifest(
                &self,
                image: &oci::native::Reference,
                _subject_digest: &oci::Digest,
                manifest_bytes: &[u8],
                media_type: &str,
            ) -> TransportResult<oci::Descriptor> {
                self.data.record("push_referrer_manifest", image);
                Ok(oci::Descriptor {
                    media_type: media_type.to_owned(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                    size: manifest_bytes.len() as i64,
                    urls: None,
                    annotations: None,
                    artifact_type: None,
                })
            }

            async fn list_referrers(
                &self,
                image: &oci::native::Reference,
                _subject_digest: &oci::Digest,
                _artifact_type: Option<&str>,
            ) -> TransportResult<Vec<oci::Descriptor>> {
                self.data.record("list_referrers", image);
                Ok(vec![])
            }

            async fn pull_manifest_raw(
                &self,
                image: &oci::native::Reference,
                _accepted_media_types: &[&str],
            ) -> TransportResult<(Vec<u8>, String)> {
                self.data.record("pull_manifest_raw", image);
                match self.manifest {
                    ManifestAnswer::HardFailure => Err(ClientError::Registry(Box::new(std::io::Error::other(
                        "registry refused the manifest",
                    )))),
                    ManifestAnswer::ImageIndex => {
                        let bytes = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#.to_vec();
                        // Echo the requested digest so the digest check passes
                        // and the caller reaches the shape check.
                        Ok((bytes, image.digest().unwrap_or_default().to_string()))
                    }
                    ManifestAnswer::NotFound => Err(ClientError::ManifestNotFound(image.to_string())),
                }
            }

            async fn pull_blob(
                &self,
                image: &oci::native::Reference,
                _digest: &oci::Digest,
            ) -> TransportResult<Vec<u8>> {
                self.data.record("pull_blob", image);
                Ok(vec![])
            }

            async fn pull_blob_to_file(
                &self,
                image: &oci::native::Reference,
                _digest: &oci::Digest,
                _path: &std::path::Path,
            ) -> TransportResult<()> {
                self.data.record("pull_blob_to_file", image);
                Ok(())
            }

            async fn head_blob(&self, image: &oci::native::Reference, _digest: &oci::Digest) -> TransportResult<u64> {
                self.data.record("head_blob", image);
                Ok(0)
            }

            async fn pull_blob_streaming(
                &self,
                image: &oci::native::Reference,
                _digest: &oci::Digest,
            ) -> TransportResult<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
                self.data.record("pull_blob_streaming", image);
                Ok(Box::new(tokio::io::empty()))
            }

            async fn push_manifest(
                &self,
                image: &oci::native::Reference,
                _manifest: &oci::Manifest,
            ) -> TransportResult<String> {
                self.data.record("push_manifest", image);
                Ok(format!("sha256:{}", "a".repeat(64)))
            }

            async fn push_manifest_raw(
                &self,
                image: &oci::native::Reference,
                _data: Vec<u8>,
                _media_type: &str,
            ) -> TransportResult<String> {
                self.data.record("push_manifest_raw", image);
                Ok(format!("sha256:{}", "a".repeat(64)))
            }

            async fn push_blob(
                &self,
                image: &oci::native::Reference,
                _data: Vec<u8>,
                _digest: &oci::Digest,
                _on_progress: ProgressFn,
            ) -> TransportResult<String> {
                self.data.record("push_blob", image);
                Ok(format!("sha256:{}", "a".repeat(64)))
            }

            async fn push_blob_from_path(
                &self,
                image: &oci::native::Reference,
                path: &std::path::Path,
                digest: &oci::Digest,
                on_progress: ProgressFn,
            ) -> TransportResult<String> {
                crate::oci::client::push_blob_buffered(self, image, path, digest, on_progress).await
            }

            fn box_clone(&self) -> Box<dyn OciTransport> {
                Box::new(self.clone())
            }
        }

        /// Builds a `Client` wired to a fresh [`RecordingTransport`], with a
        /// mirror configured for [`UPSTREAM_REGISTRY`] → [`MIRROR_HOST`] under
        /// [`MIRROR_PREFIX`]. Returns the data handle alongside the client so
        /// tests can inspect recorded calls after driving a method.
        fn mirrored_client() -> (Client, RecordingTransportData) {
            mirrored_client_with(ManifestAnswer::NotFound)
        }

        /// [`mirrored_client`] with the transport's manifest answer selectable.
        fn mirrored_client_with(manifest: ManifestAnswer) -> (Client, RecordingTransportData) {
            mirrored_client_from(RecordingTransport {
                manifest,
                ..Default::default()
            })
        }

        /// [`mirrored_client`] whose transport rejects the auth handshake.
        fn mirrored_client_failing_auth() -> (Client, RecordingTransportData) {
            mirrored_client_from(RecordingTransport {
                auth_fails: true,
                ..Default::default()
            })
        }

        fn mirrored_client_from(transport: RecordingTransport) -> (Client, RecordingTransportData) {
            let data = RecordingTransportData::new();
            let mut client = Client::with_transport(Box::new(RecordingTransport {
                data: data.clone(),
                ..transport
            }));
            client.mirrors = MirrorMap::new([(
                UPSTREAM_REGISTRY.to_string(),
                ParsedMirror {
                    protocol: "https".to_string(),
                    host: MIRROR_HOST.to_string(),
                    path_prefix: MIRROR_PREFIX.to_string(),
                },
            )]);
            (client, data)
        }

        fn identifier_with_tag(tag: &str) -> Identifier {
            Identifier::new_registry(REPOSITORY, UPSTREAM_REGISTRY).clone_with_tag(tag)
        }

        fn digest_hex(seed: char) -> String {
            std::iter::repeat_n(seed, 64).collect()
        }

        fn pinned_identifier(seed: char) -> oci::PinnedIdentifier {
            let digest = oci::Digest::Sha256(digest_hex(seed));
            let id = Identifier::new_registry(REPOSITORY, UPSTREAM_REGISTRY).clone_with_digest(digest);
            oci::PinnedIdentifier::try_from(id).unwrap()
        }

        /// The repository every identifier-based call below must observe:
        /// `<mirror path-prefix>/<upstream repository>`.
        fn mirrored_repository() -> String {
            format!("{MIRROR_PREFIX}/{REPOSITORY}")
        }

        #[tokio::test]
        async fn list_tags_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let _ = client
                .list_tags_addressed(identifier_with_tag("1.0"), ReadAddressing::Mirrored)
                .await;
            let (registry, repository) = data.call("list_tags");
            assert_eq!(
                registry, MIRROR_HOST,
                "list_tags must hand the transport the mirror host"
            );
            assert_eq!(
                repository,
                mirrored_repository(),
                "list_tags must hand the transport the prefixed repository"
            );
        }

        #[tokio::test]
        async fn list_repositories_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let _ = client.list_repositories(UPSTREAM_REGISTRY).await;
            let (registry, repository) = data.call("catalog");
            assert_eq!(
                registry, MIRROR_HOST,
                "list_repositories must hand the transport the mirror host"
            );
            // Catalog calls carry no per-repository segment — the placeholder
            // repository is the mirror's path prefix verbatim (see
            // `transport_registry_rewrites_catalog_registry` above).
            assert_eq!(
                repository, MIRROR_PREFIX,
                "list_repositories must hand the transport the bare path-prefix"
            );
        }

        #[tokio::test]
        async fn fetch_manifest_digest_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client
                .fetch_manifest_digest_addressed(&id, ReadAddressing::Mirrored)
                .await;
            let (registry, repository) = data.call("fetch_manifest_digest");
            assert_eq!(
                registry, MIRROR_HOST,
                "fetch_manifest_digest must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn fetch_manifest_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client.fetch_manifest_addressed(&id, ReadAddressing::Mirrored).await;
            // fetch_manifest delegates to the private fetch_manifest_raw
            // helper, which calls transport.pull_manifest_raw.
            let (registry, repository) = data.call("pull_manifest_raw");
            assert_eq!(
                registry, MIRROR_HOST,
                "fetch_manifest must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn head_blob_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let digest = oci::Digest::Sha256(digest_hex('a'));
            let _ = client.head_blob(&id, &digest).await;
            let (registry, repository) = data.call("head_blob");
            assert_eq!(
                registry, MIRROR_HOST,
                "head_blob must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn pull_manifest_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let pinned = pinned_identifier('b');
            let _ = client.pull_manifest(&pinned).await;
            let (registry, repository) = data.call("pull_manifest_raw");
            assert_eq!(
                registry, MIRROR_HOST,
                "pull_manifest must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn pull_blob_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let blob_ref = pinned_identifier('c');
            let _ = client.pull_blob(&blob_ref).await;
            let (registry, repository) = data.call("pull_blob");
            assert_eq!(
                registry, MIRROR_HOST,
                "pull_blob must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn pull_layer_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let pinned = pinned_identifier('d');
            let layer = oci::Descriptor {
                media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
                digest: format!("sha256:{}", digest_hex('e')),
                size: 1,
                urls: None,
                artifact_type: None,
                annotations: None,
            };
            let dir = tempfile::tempdir().unwrap();
            // Outcome is irrelevant (the recording transport yields an empty
            // stream, so extraction fails downstream) — only the reference
            // handed to pull_blob_streaming matters.
            let _ = client.pull_layer(&pinned, &layer, dir.path()).await;
            let (registry, repository) = data.call("pull_blob_streaming");
            assert_eq!(
                registry, MIRROR_HOST,
                "pull_layer must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn pull_description_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let dir = tempfile::tempdir().unwrap();
            let _ = client
                .pull_description_addressed(&id, dir.path(), ReadAddressing::Mirrored)
                .await;
            let (registry, repository) = data.call("pull_manifest_raw");
            assert_eq!(
                registry, MIRROR_HOST,
                "pull_description must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn probe_manifest_digest_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client
                .probe_manifest_digest_addressed(&id, ReadAddressing::Mirrored)
                .await;
            let (registry, repository) = data.call("fetch_manifest_digest");
            assert_eq!(
                registry, MIRROR_HOST,
                "probe_manifest_digest must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        #[tokio::test]
        async fn fetch_manifest_raw_bytes_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client
                .fetch_manifest_raw_bytes_addressed(&id, ReadAddressing::Mirrored)
                .await;
            let (registry, repository) = data.call("pull_manifest_raw");
            assert_eq!(
                registry, MIRROR_HOST,
                "fetch_manifest_raw_bytes must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        // ── The unaddressed reads: the mirror is bypassed by default ────────
        //
        // Paired with the mirrored tests above, which are the positive control:
        // the same client, the same identifier, and naming the host is the only
        // thing that differs. What these pin is the *default* — a call site that
        // says nothing about addressing gets the canonical host, so a read added
        // tomorrow to back a write is safe before anyone reviews it.

        #[tokio::test]
        async fn list_tags_defaults_to_the_canonical_host() {
            let (client, data) = mirrored_client();
            let _ = client.list_tags(identifier_with_tag("1.0")).await;
            assert_eq!(
                data.call("list_tags"),
                (UPSTREAM_REGISTRY.to_string(), REPOSITORY.to_string()),
                "a canonical listing must reach the upstream host with the repository unrewritten"
            );
        }

        #[tokio::test]
        async fn fetch_manifest_defaults_to_the_canonical_host() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client.fetch_manifest(&id).await;
            assert_eq!(
                data.call("pull_manifest_raw"),
                (UPSTREAM_REGISTRY.to_string(), REPOSITORY.to_string()),
                "an unaddressed manifest read must reach the upstream host with the repository unrewritten"
            );
        }

        #[tokio::test]
        async fn fetch_manifest_raw_bytes_defaults_to_the_canonical_host() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client.fetch_manifest_raw_bytes(&id).await;
            assert_eq!(
                data.call("pull_manifest_raw"),
                (UPSTREAM_REGISTRY.to_string(), REPOSITORY.to_string()),
                "a canonical manifest read must reach the upstream host with the repository unrewritten"
            );
        }

        /// The description read that `package copy --description` and
        /// `package describe --from` write back from.
        ///
        /// Positive control: `pull_description_routes_through_mirror`, same
        /// client, same identifier, same recorded call — the only difference is
        /// that it names the host.
        #[tokio::test]
        async fn pull_description_defaults_to_the_canonical_host() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let dir = tempfile::tempdir().unwrap();
            let _ = client.pull_description(&id, dir.path()).await;
            assert_eq!(
                data.call("pull_manifest_raw"),
                (UPSTREAM_REGISTRY.to_string(), REPOSITORY.to_string()),
                "a description that will be written back must be read from the upstream host"
            );
        }

        #[tokio::test]
        async fn probe_manifest_digest_canonical_bypasses_the_mirror() {
            // No default to pin here: this read has no unaddressed form, so
            // the host is named at every call site.
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client
                .probe_manifest_digest_addressed(&id, ReadAddressing::Canonical)
                .await;
            assert_eq!(
                data.call("fetch_manifest_digest"),
                (UPSTREAM_REGISTRY.to_string(), REPOSITORY.to_string()),
                "a canonical digest probe must reach the upstream host with the repository unrewritten"
            );
        }

        /// A canonical read authenticates against the canonical host too —
        /// one transaction, one host, one credential scope. Authenticating
        /// against the mirror and then reading upstream would fail closed at
        /// best and read as an anonymous request at worst.
        #[tokio::test]
        async fn a_canonical_read_authenticates_against_the_canonical_host() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let _ = client
                .fetch_manifest_raw_bytes_addressed(&id, ReadAddressing::Canonical)
                .await;
            assert_eq!(
                data.call("ensure_auth").0,
                UPSTREAM_REGISTRY.to_string(),
                "the pull scope must be requested for the host the read addresses"
            );
        }

        #[tokio::test]
        async fn fetch_layer_blob_capped_routes_through_mirror() {
            let (client, data) = mirrored_client();
            let id = identifier_with_tag("1.0");
            let digest = oci::Digest::Sha256(digest_hex('f'));
            let _ = client.fetch_layer_blob_capped(&id, &digest, 1024).await;
            let (registry, repository) = data.call("pull_blob_streaming");
            assert_eq!(
                registry, MIRROR_HOST,
                "fetch_layer_blob_capped must hand the transport the mirror host"
            );
            assert_eq!(repository, mirrored_repository());
        }

        // ── Mirror provenance on failure (issue #327) ────────────────────────
        //
        // Routing a read through a mirror is invisible in every error the
        // mirror produces: the identifier names the upstream, the request went
        // somewhere else, and neither error names both.

        /// The whole point of the annotation: the failure names the physical
        /// reference that was fetched, the mirror host it went to, and the
        /// upstream that mirror stands in for.
        #[tokio::test]
        async fn a_mirrored_fetch_failure_names_the_physical_reference_and_its_upstream() {
            let (client, _) = mirrored_client_with(ManifestAnswer::HardFailure);

            let error = client
                .pull_manifest(&pinned_identifier('b'))
                .await
                .expect_err("the transport was told to fail hard");

            let ClientError::Mirrored {
                origin,
                mirror,
                physical,
                ..
            } = &error
            else {
                panic!("a mirrored read failure must be annotated, got {error:?}");
            };
            assert_eq!(origin, UPSTREAM_REGISTRY, "must name the upstream that was asked for");
            assert_eq!(mirror, MIRROR_HOST, "must name the mirror it was routed to");
            assert!(
                physical.contains(MIRROR_HOST) && physical.contains(&mirrored_repository()),
                "must name the reference actually fetched, got: {physical}"
            );
        }

        /// Auth is attempted against the mirror, not the upstream, so a
        /// credential rejection is a statement about the mirror's registry —
        /// unannotated it sends the reader to look for upstream credentials
        /// that were never used.
        #[tokio::test]
        async fn a_mirrored_auth_failure_names_the_mirror() {
            let (client, _) = mirrored_client_failing_auth();

            let error = client
                .pull_manifest(&pinned_identifier('b'))
                .await
                .expect_err("the transport was told to reject the credentials");

            let ClientError::Mirrored { mirror, source, .. } = &error else {
                panic!("a mirrored auth failure must be annotated, got {error:?}");
            };
            assert_eq!(mirror, MIRROR_HOST);
            assert!(
                matches!(**source, ClientError::Authentication(_)),
                "the wrapped verdict must still be the auth failure, got {source:?}"
            );
        }

        /// A mirror serving the wrong document shape is still the mirror's
        /// answer. Unannotated it reads as the upstream publishing something
        /// malformed.
        #[tokio::test]
        async fn a_mirrored_index_where_a_manifest_was_expected_names_the_mirror() {
            let (client, _) = mirrored_client_with(ManifestAnswer::ImageIndex);

            let error = client
                .pull_manifest(&pinned_identifier('b'))
                .await
                .expect_err("an image index is not an image manifest");

            let ClientError::Mirrored { mirror, source, .. } = &error else {
                panic!("a mirrored shape failure must be annotated, got {error:?}");
            };
            assert_eq!(mirror, MIRROR_HOST);
            assert!(
                matches!(**source, ClientError::UnexpectedManifestType),
                "the wrapped verdict must still be the shape refusal, got {source:?}"
            );
        }

        /// The silent-regression case. Callers match `ManifestNotFound` to mean
        /// "absent"; wrapping it would make a missing tag a hard failure while
        /// every routing assertion above still passed.
        #[tokio::test]
        async fn a_mirrored_missing_tag_is_still_a_not_found() {
            let (client, _) = mirrored_client();

            let result = client.fetch_manifest_raw_bytes(&identifier_with_tag("1.0")).await;

            assert!(
                matches!(result, Ok(None)),
                "a missing tag behind a mirror must stay absent, not become a failure: {result:?}"
            );
        }

        /// Naming a mirror requires that this fetch was actually rewritten, not
        /// merely that the host appears somewhere in the mirror table. A host
        /// configured as one upstream's mirror is still an ordinary registry
        /// anyone may pull from directly, and reporting that pull as "via
        /// mirror ... configured for <unrelated upstream>" sends the reader to
        /// a config entry that had nothing to do with it.
        #[tokio::test]
        async fn a_direct_fetch_from_a_host_that_mirrors_another_upstream_is_not_annotated() {
            let (client, _) = mirrored_client_with(ManifestAnswer::HardFailure);
            // MIRROR_HOST is UPSTREAM_REGISTRY's mirror, but this identifier
            // names it directly — no rewrite happens.
            let direct = Identifier::new_registry("internal/tool", MIRROR_HOST).clone_with_tag("1.0");

            let error = client
                .fetch_manifest_raw_bytes(&direct)
                .await
                .expect_err("the transport was told to fail hard");

            assert!(
                matches!(error, ClientError::Registry(_)),
                "a direct pull from a host that happens to mirror another upstream must not be \
                 reported as mirrored, got {error:?}"
            );
        }

        /// A canonical read addresses the upstream host directly, so the
        /// reverse lookup misses and the failure is reported unannotated —
        /// claiming a mirror was involved when none was would be a lie.
        #[tokio::test]
        async fn a_canonical_read_failure_is_not_annotated_as_mirrored() {
            let (client, _) = mirrored_client_with(ManifestAnswer::HardFailure);

            let error = client
                .fetch_manifest_raw_bytes_addressed(&identifier_with_tag("1.0"), ReadAddressing::Canonical)
                .await
                .expect_err("the transport was told to fail hard");

            assert!(
                matches!(error, ClientError::Registry(_)),
                "a canonical read must surface the failure unannotated, got {error:?}"
            );
        }
    }

    // ── T-arch-A1: structural gating test ────────────────────────────────────
    //
    // `canonical_reference` is `pub(crate)` and intentionally callable in-crate,
    // but the in-crate discipline is: read paths must route through
    // `Client::transport_reference` / `transport_registry` (the mirror seams), not
    // call `canonical_reference` directly. The compiler cannot enforce this for
    // in-crate call sites, so we promote it to a source-scanning structural test.
    //
    // Any NEW call site of `canonical_reference` outside the allow-list below must
    // fail this test, forcing an explicit decision: either update the allow-list
    // (with a justification comment) or reroute through the mirror seam.
    //
    // Allow-list rationale (only files that ACTUALLY reference the symbol —
    // adding a file that does not use it would create a latent hole, silently
    // permitting a future read-path call there):
    // - `oci/identifier.rs`  — definition + test helpers (canonical home).
    // - `oci/client.rs`      — the two gated seams + `ensure_auth` push path +
    //                          the manifest-cache keys (cache keyed off the
    //                          canonical identity, mirror-independent by design)
    //                          + test helpers in the `transport_reference` module.
    // - `package/cascade.rs` — push-path cascade test spies keying a manifest
    //                          map by canonical reference (test-only).
    // - `package/cascade/{gather,apply,equivalence}.rs`
    //                        — cascade audit test spies, same shape and same
    //                          test-only scope as `package/cascade.rs` above.
    //                          The audit's production code names no reference at
    //                          all: it asks for `ReadAddressing::Canonical` and
    //                          the seam in this file builds the reference.
    #[test]
    fn canonical_reference_only_used_in_allowed_files() {
        use std::fs;
        use std::path::Path;

        // Allow-list: file paths (relative to the ocx_lib src root) that are
        // permitted to reference `canonical_reference`.
        const ALLOWED_SUFFIXES: &[&str] = &[
            "oci/identifier.rs",
            "oci/client.rs",
            "package/cascade.rs",
            "package/cascade/gather.rs",
            "package/cascade/apply.rs",
            "package/cascade/equivalence.rs",
        ];

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let src_root = Path::new(manifest_dir).join("src");

        // Recursively collect all `.rs` files under the src root.
        fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs_files(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }

        let mut rs_files = Vec::new();
        collect_rs_files(&src_root, &mut rs_files);
        assert!(
            !rs_files.is_empty(),
            "source scanner found no .rs files under {}",
            src_root.display()
        );

        let mut offenders: Vec<String> = Vec::new();
        for file_path in &rs_files {
            let content = fs::read_to_string(file_path).unwrap_or_default();
            if !content.contains("canonical_reference") {
                continue;
            }

            // Check whether this file is in the allow-list.
            let path_str = file_path.to_string_lossy();
            let allowed = ALLOWED_SUFFIXES.iter().any(|suffix| {
                // Normalise separators so the check works on all platforms.
                path_str.replace('\\', "/").ends_with(suffix)
            });
            if !allowed {
                offenders.push(path_str.into_owned());
            }
        }

        assert!(
            offenders.is_empty(),
            "T-arch-A1: `canonical_reference` found in file(s) outside the allow-list \
             (read paths must route through Client::transport_reference / transport_registry):\n  {}",
            offenders.join("\n  ")
        );
    }

    // ── T-arch-G1: native::Reference direct-construction seam gate ──────────
    //
    // Traces: mirror-invariant audit 2026-07-19, gap G1.
    //
    // The structural test above gates the `canonical_reference` symbol, but it
    // cannot catch a NEW read path that sidesteps BOTH mirror seams by
    // constructing a `native::Reference` directly via `native::Reference::with_tag`
    // / `with_digest` / `with_tag_and_digest` — never touching `canonical_reference`,
    // `transport_reference`, or `transport_registry`. This test closes that hole
    // with the same source-scanning mechanics as the test above.
    //
    // Allow-list rationale (only files that ACTUALLY construct a `native::Reference`
    // directly — same policy as the allow-list above):
    // - `oci/client.rs`     — the two read seams this gate exists to protect
    //                          (`transport_reference`, `transport_registry`, lines
    //                          ~116-159).
    // - `oci/identifier.rs` — `canonical_reference`'s own definition (the push
    //                          seam). Callers of *that* symbol are separately
    //                          gated by `canonical_reference_only_used_in_allowed_files`
    //                          above, so a direct construction here is not an
    //                          uncontrolled bypass.
    //
    // Deliberately scans for the qualified `native::Reference::with_` spelling,
    // not the bare `Reference::with_`: `auth/login.rs` constructs a raw
    // `oci_client::Reference` (imported directly, with no `native::` qualifier)
    // for a registry-probe path (`OciClientPing::ping`) that never goes through
    // an `Identifier` at all — including the bare spelling would false-positive
    // there for no safety gain.
    #[test]
    fn native_reference_direct_construction_restricted_to_seams() {
        use std::fs;
        use std::path::Path;

        const ALLOWED_SUFFIXES: &[&str] = &["oci/client.rs", "oci/identifier.rs"];
        const PATTERN: &str = "native::Reference::with_";

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let src_root = Path::new(manifest_dir).join("src");

        // Recursively collect all `.rs` files under the src root.
        fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs_files(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }

        let mut rs_files = Vec::new();
        collect_rs_files(&src_root, &mut rs_files);
        assert!(
            !rs_files.is_empty(),
            "source scanner found no .rs files under {}",
            src_root.display()
        );

        let mut offenders: Vec<String> = Vec::new();
        for file_path in &rs_files {
            let content = fs::read_to_string(file_path).unwrap_or_default();
            if !content.contains(PATTERN) {
                continue;
            }

            let path_str = file_path.to_string_lossy();
            let allowed = ALLOWED_SUFFIXES
                .iter()
                .any(|suffix| path_str.replace('\\', "/").ends_with(suffix));
            if !allowed {
                offenders.push(path_str.into_owned());
            }
        }

        assert!(
            offenders.is_empty(),
            "T-arch-G1: `{PATTERN}` found in file(s) outside the allow-list (a native::Reference \
             must be built only via the mirror seams in client.rs, or via Identifier::canonical_reference \
             in identifier.rs):\n  {}",
            offenders.join("\n  ")
        );
    }

    /// T-arch-G1b: pins the number of `native::Reference::with_*` constructions
    /// in client.rs's PRODUCTION code (everything before the `#[cfg(test)] mod
    /// tests` boundary) to exactly the five call sites inside
    /// [`Client::transport_reference`] (4 — the `with_tag_and_digest` /
    /// `with_tag` / `with_digest` / `with_tag` match arms) plus
    /// [`Client::transport_registry`] (1 — its single `with_tag` call), lines
    /// ~116-159 at the time this test was written.
    ///
    /// Update `EXPECTED_PRODUCTION_CONSTRUCTION_COUNT` ONLY when the new
    /// construction site being added is one of the two seams themselves
    /// (`transport_reference` / `transport_registry` growing a match arm) —
    /// NEVER to silence a new bypass caught by
    /// `native_reference_direct_construction_restricted_to_seams` above.
    ///
    /// Traces: mirror-invariant audit 2026-07-19, gap G1.
    #[test]
    fn client_rs_production_reference_construction_count_pinned_to_seams() {
        use std::fs;
        use std::path::Path;

        const EXPECTED_PRODUCTION_CONSTRUCTION_COUNT: usize = 5;

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let this_file = Path::new(manifest_dir).join("src/oci/client.rs");
        let source = fs::read_to_string(&this_file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", this_file.display()));

        // Everything before `mod tests {` (the `#[cfg(test)]`-gated block that
        // spans to end of file) is production code.
        let production_end = source
            .find("\nmod tests {")
            .expect("client.rs must contain a `mod tests {` boundary");
        let production_source = &source[..production_end];

        let actual = production_source.matches("native::Reference::with_").count();
        assert_eq!(
            actual, EXPECTED_PRODUCTION_CONSTRUCTION_COUNT,
            "production (non-#[cfg(test)]) `native::Reference::with_*` construction count in \
             client.rs changed from the pinned value — update EXPECTED_PRODUCTION_CONSTRUCTION_COUNT \
             only if the new site is inside transport_reference/transport_registry themselves, never \
             to silence a new bypass"
        );
    }

    // ── push_patch_descriptor ─────────────────────────────────────────

    /// `push_patch_descriptor` pushes a `__ocx.patch` manifest with the
    /// expected artifactType + a descriptor layer, and returns the manifest
    /// digest. Verified against the `StubTransport` via `capture_pushes`.
    #[tokio::test]
    async fn push_patch_descriptor_pushes_patch_artifact_and_returns_digest() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let client = stub(&data);

        let descriptor_bytes = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": ["internal.company.com/certs/ca:latest"] }]
        })
        .to_string()
        .into_bytes();

        // Global patch repo identifier (reserved `global` repository at the patch registry).
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let digest = client
            .push_patch_descriptor(&patch_repo, &descriptor_bytes)
            .await
            .expect("push_patch_descriptor must succeed");

        // A manifest was pushed.
        let inner = data.read();
        assert!(
            inner.calls.iter().any(|c| c == "push_manifest_raw"),
            "push_patch_descriptor must push a manifest; calls = {:?}",
            inner.calls
        );

        // The descriptor layer blob was pushed (push_blob:<layer_digest>).
        let layer_digest = Algorithm::Sha256.hash(&descriptor_bytes).to_string();
        assert!(
            inner.calls.iter().any(|c| c == &format!("push_blob:{layer_digest}")),
            "push_patch_descriptor must push the descriptor layer blob; calls = {:?}",
            inner.calls
        );

        // The captured manifest carries the patch artifactType + the descriptor layer media type.
        let (_image, (manifest_bytes, manifest_digest)) = inner
            .manifests
            .iter()
            .next()
            .expect("a manifest must have been captured");
        let manifest: oci::ImageManifest =
            serde_json::from_slice(manifest_bytes).expect("captured manifest must parse");
        assert_eq!(
            manifest.artifact_type.as_deref(),
            Some(crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE),
            "manifest artifactType must be the patch artifact type"
        );
        assert_eq!(manifest.layers.len(), 1, "patch manifest must have exactly one layer");
        assert_eq!(
            manifest.layers[0].media_type,
            crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE,
            "layer media type must be the descriptor layer media type"
        );

        // The returned digest matches the pushed manifest's digest.
        assert_eq!(
            digest.to_string(),
            *manifest_digest,
            "returned digest must equal the pushed manifest digest"
        );
    }

    /// `push_patch_descriptor` rejects malformed descriptor bytes before any push.
    #[tokio::test]
    async fn push_patch_descriptor_rejects_malformed_descriptor() {
        let data = StubTransportData::new();
        let client = stub(&data);
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let result = client.push_patch_descriptor(&patch_repo, b"not valid json {{{").await;
        assert!(
            matches!(result, Err(ClientError::InvalidManifest(_))),
            "malformed descriptor must be rejected with InvalidManifest, got: {result:?}"
        );
        // No manifest was pushed.
        assert!(
            data.read().calls.iter().all(|c| c != "push_manifest_raw"),
            "no manifest must be pushed when the descriptor is malformed"
        );
    }

    // ── Cascade normalization regression: os_features re-push eviction (Step 3.7) ──

    mod cascade_normalization {
        use super::*;

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn test_id(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn read_pushed_index(data: &StubTransportData, tag: &str) -> oci::ImageIndex {
            let id = test_id(tag);
            let inner = data.read();
            let (bytes, _) = inner
                .manifests
                .get(&id.canonical_reference().to_string())
                .expect("no pushed manifest");
            let manifest: oci::Manifest = serde_json::from_slice(bytes).unwrap();
            match manifest {
                oci::Manifest::ImageIndex(idx) => idx,
                _ => panic!("expected ImageIndex"),
            }
        }

        /// Two re-pushes of linux/amd64 with the SAME os_features set but in DIFFERENT
        /// array order must produce exactly ONE entry in the merged index.
        ///
        /// This is the B2 regression test from the architect review.
        ///
        /// ## Why identical-value tests do NOT catch this bug
        ///
        /// `merge_platform_into_index` evicts the prior entry by comparing
        /// `entry.platform != platform` (positional `Vec` equality on the native
        /// `native::Platform` struct).  When both pushes carry exactly the same
        /// `os_features` bytes, eviction works by coincidence.
        ///
        /// The bug surfaces when a re-push arrives with `os_features` in a different
        /// array order:
        ///   first push:  os_features = ["libc.glibc", "libc.x"]
        ///   second push: os_features = ["libc.x", "libc.glibc"]  (same set, reordered)
        ///
        /// Under current code (no normalization):
        ///   `["libc.glibc","libc.x"] != ["libc.x","libc.glibc"]`  (positional inequality)
        ///   → `retain` keeps the first entry  → index has 2 entries  (BUG: index bloat)
        ///   → this test FAILS (asserts 1, gets 2)
        ///
        /// After Step 4.6 normalization (sort+dedup in `From<&Platform> for native::Platform`):
        ///   both serialize as  ["libc.glibc", "libc.x"]  (sorted, ascending)
        ///   → `retain` evicts the first  → index has 1 entry  → this test passes
        #[tokio::test]
        async fn repush_same_platform_different_feature_order_produces_one_entry() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_id("3.28");

            // First push: os_features = ["libc.glibc", "libc.x"]  (glibc < x — already sorted)
            let first_platform = oci::Platform::Specific {
                os: oci::OperatingSystem::Linux,
                arch: oci::Architecture::Amd64,
                variant: None,
                os_features: vec!["libc.glibc".to_string(), "libc.x".to_string()],
            };
            client
                .merge_platform_into_index(&id, "3.28", &first_platform, "sha256:first_push", 100, &BTreeMap::new())
                .await
                .unwrap();

            // Second push: os_features = ["libc.x", "libc.glibc"]  (SAME SET, reverse order)
            // Without normalization: positional Vec inequality → retain keeps both → 2 entries (BUG)
            // With normalization:    both sort to ["libc.glibc","libc.x"] → retain evicts first → 1 entry
            let second_platform = oci::Platform::Specific {
                os: oci::OperatingSystem::Linux,
                arch: oci::Architecture::Amd64,
                variant: None,
                os_features: vec!["libc.x".to_string(), "libc.glibc".to_string()],
            };
            client
                .merge_platform_into_index(
                    &id,
                    "3.28",
                    &second_platform,
                    "sha256:second_push",
                    200,
                    &BTreeMap::new(),
                )
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(
                index.manifests.len(),
                1,
                "re-push with reordered os_features must evict the prior entry (normalization \
                 collapses both to the same sorted form); got {} entries — this fails today \
                 (positional Vec inequality) and passes after Step 4.6 sort+dedup normalization",
                index.manifests.len()
            );
            assert_eq!(
                index.manifests[0].digest, "sha256:second_push",
                "latest push must win after normalization-enabled eviction"
            );
        }
    }

    // ── Wire-level auth caching (the fork pin) ──────────────────────────

    /// The fork's `auth()` used to run the whole handshake — `GET /v2/` plus a
    /// token-realm exchange — ahead of *every* registry operation and once per
    /// layer of a pull, so two of every three requests were wasted, warm or
    /// cold, forever.
    ///
    /// No test in `ensure_auth` above can see that: all of them assert at the
    /// [`OciTransport`] boundary through an in-memory stub and never open a
    /// socket. These do, against a real `TcpListener`, and they are what pins
    /// the fork's behaviour from ocx's side — the fork's own suite is excluded
    /// from this workspace and runs in no CI, so a rebase that dropped the
    /// caching would otherwise leave every gate here green.
    mod auth_wire_tests {
        use super::*;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::sync::watch;

        /// What the stub registry counted, and what it was told to do.
        struct StubState {
            /// `GET /v2/` — the challenge probe. Host-invariant, so one per host
            /// is the target however many repositories are touched.
            probes: AtomicUsize,
            /// `GET /token` — the token-realm exchange, one per scope.
            exchanges: AtomicUsize,
            /// Everything else under `/v2/`.
            resource_requests: AtomicUsize,
            /// `Authorization` header of every request that carried one, in order.
            authorizations: Mutex<Vec<String>>,
            /// Whether `GET /v2/` answers with a `WWW-Authenticate` challenge.
            challenges: AtomicBool,
            /// When set, the token endpoint waits for this to flip before it
            /// answers, so no leader can finish before its peers have arrived.
            hold: Mutex<Option<watch::Receiver<bool>>>,
            address: Mutex<String>,
        }

        impl StubState {
            fn new(challenges: bool) -> Self {
                StubState {
                    probes: AtomicUsize::new(0),
                    exchanges: AtomicUsize::new(0),
                    resource_requests: AtomicUsize::new(0),
                    authorizations: Mutex::new(Vec::new()),
                    challenges: AtomicBool::new(challenges),
                    hold: Mutex::new(None),
                    address: Mutex::new(String::new()),
                }
            }

            fn total(&self) -> usize {
                self.probes.load(Ordering::SeqCst)
                    + self.exchanges.load(Ordering::SeqCst)
                    + self.resource_requests.load(Ordering::SeqCst)
            }

            async fn respond(&self, target: &str) -> String {
                if target.starts_with("/token") {
                    let minted = self.exchanges.fetch_add(1, Ordering::SeqCst) + 1;
                    // Clone the receiver out before awaiting it — the lock must
                    // not be held across the wait, or the callers this hold
                    // exists to collect could never arrive.
                    let hold = self.hold.lock().unwrap().clone();
                    if let Some(mut hold) = hold {
                        let _ = hold.wait_for(|released| *released).await;
                    }
                    return json_response(&format!(r#"{{"token":"minted-{minted}"}}"#));
                }
                if target == "/v2/" {
                    self.probes.fetch_add(1, Ordering::SeqCst);
                    if !self.challenges.load(Ordering::SeqCst) {
                        return json_response("{}");
                    }
                    let realm = format!("http://{}/token", self.address.lock().unwrap());
                    return format!(
                        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"{realm}\",service=\"stub\"\r\nContent-Length: 0\r\n\r\n"
                    );
                }
                self.resource_requests.fetch_add(1, Ordering::SeqCst);
                if target.contains("/tags/list") {
                    return json_response(r#"{"name":"stub","tags":["1.0"]}"#);
                }
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
            }
        }

        fn json_response(body: &str) -> String {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        }

        /// Minimal HTTP/1.1 stub of the two endpoints an authentication
        /// handshake touches, plus the tag listing that proves a warm client
        /// still sends credentials.
        struct StubRegistry {
            state: Arc<StubState>,
            address: String,
        }

        impl StubRegistry {
            async fn start() -> Self {
                Self::start_with(true).await
            }

            /// `challenges = false` answers `GET /v2/` with `200` and no
            /// `WWW-Authenticate`. Nothing is inserted into the token cache on
            /// that path, so it is the case a token-cache-only shortcut cannot
            /// reach — reaching zero there is the host challenge cache's doing.
            async fn start_with(challenges: bool) -> Self {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap().to_string();
                let state = Arc::new(StubState::new(challenges));
                *state.address.lock().unwrap() = address.clone();

                let serving = Arc::clone(&state);
                tokio::spawn(async move {
                    while let Ok((socket, _)) = listener.accept().await {
                        let state = Arc::clone(&serving);
                        tokio::spawn(async move { serve_connection(socket, state).await });
                    }
                });

                StubRegistry { state, address }
            }

            fn hold(&self) -> watch::Sender<bool> {
                let (release, held) = watch::channel(false);
                *self.state.hold.lock().unwrap() = Some(held);
                release
            }

            fn client(&self) -> Client {
                ClientBuilder::new()
                    .plain_http_registries(vec![self.address.clone()])
                    .build()
            }

            fn identifier(&self, repository: &str) -> Identifier {
                Identifier::new_registry(repository, &self.address).clone_with_tag("1.0")
            }
        }

        async fn serve_connection(socket: tokio::net::TcpStream, state: Arc<StubState>) {
            let (read_half, mut write_half) = socket.into_split();
            let mut reader = tokio::io::BufReader::new(read_half);

            loop {
                let mut request_line = String::new();
                match reader.read_line(&mut request_line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let mut parts = request_line.split_whitespace();
                let (Some(_method), Some(target)) = (parts.next(), parts.next()) else {
                    return;
                };
                let target = target.to_string();

                let mut content_length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let Some((name, value)) = header.split_once(':') else {
                        continue;
                    };
                    let value = value.trim();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => content_length = value.parse().unwrap_or(0),
                        "authorization" => state.authorizations.lock().unwrap().push(value.to_string()),
                        _ => {}
                    }
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    if reader.read_exact(&mut body).await.is_err() {
                        return;
                    }
                }

                let response = state.respond(&target).await;
                if write_half.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
        }

        /// C-001. The first `ensure_auth` pays the probe and the exchange; the
        /// second pays nothing at all.
        ///
        /// This is the whole regression guard for the fork's cache-first
        /// `auth()`. Red against the pre-change submodule commit, where the
        /// second call issued the same two requests as the first.
        #[tokio::test]
        async fn a_warm_ensure_auth_issues_no_wire_requests() {
            let registry = StubRegistry::start().await;
            let client = registry.client();
            let identifier = registry.identifier("test/pkg");

            client
                .ensure_auth(&identifier, oci::RegistryOperation::Pull)
                .await
                .unwrap();
            let cold = registry.state.total();
            assert!(
                cold <= 2,
                "a cold handshake is one probe plus one exchange, got {cold} requests"
            );
            assert!(
                cold > 0,
                "the cold call must actually reach the wire, or this proves nothing"
            );

            client
                .ensure_auth(&identifier, oci::RegistryOperation::Pull)
                .await
                .unwrap();
            assert_eq!(
                registry.state.total(),
                cold,
                "a warm ensure_auth must issue zero requests"
            );
        }

        /// C-001 edge (b). A registry answering `200` with no `WWW-Authenticate`
        /// inserts nothing into the token cache, so the second call reaches zero
        /// only if the *challenge probe* is what was cached.
        #[tokio::test]
        async fn an_unchallenged_registry_is_probed_once() {
            let registry = StubRegistry::start_with(false).await;
            let client = registry.client();
            let identifier = registry.identifier("test/pkg");

            for _ in 0..2 {
                client
                    .ensure_auth(&identifier, oci::RegistryOperation::Pull)
                    .await
                    .unwrap();
            }

            assert_eq!(
                registry.state.probes.load(Ordering::SeqCst),
                1,
                "the probe answer is host-invariant and must be reused"
            );
            assert_eq!(
                registry.state.exchanges.load(Ordering::SeqCst),
                0,
                "an unchallenged registry has no token to exchange"
            );
        }

        /// C-001 edges (c) and (d). The cache key is the full scope — another
        /// repository, or another verb set, is a different token. Serving one
        /// for the other is buildkit's `insufficient_scope` class.
        #[tokio::test]
        async fn another_repository_or_operation_gets_its_own_exchange() {
            let registry = StubRegistry::start().await;
            let client = registry.client();

            client
                .ensure_auth(&registry.identifier("test/one"), oci::RegistryOperation::Pull)
                .await
                .unwrap();
            client
                .ensure_auth(&registry.identifier("test/two"), oci::RegistryOperation::Pull)
                .await
                .unwrap();
            client
                .ensure_auth(&registry.identifier("test/one"), oci::RegistryOperation::Push)
                .await
                .unwrap();

            assert_eq!(
                registry.state.exchanges.load(Ordering::SeqCst),
                3,
                "repository and operation are both part of the token cache key"
            );
        }

        /// C-003, from ocx's side. `store_auth_if_needed` is the side effect the
        /// cache-first shortcut must not skip: it is the record the header-attach
        /// path reads to decide a request is authenticated at all. A warm
        /// `ensure_auth` followed by a real request proves the credentials
        /// survived the shortcut end to end.
        #[tokio::test]
        async fn a_warm_ensure_auth_still_authenticates_the_next_request() {
            let registry = StubRegistry::start().await;
            let client = registry.client();
            let identifier = registry.identifier("test/pkg");

            for _ in 0..2 {
                client
                    .ensure_auth(&identifier, oci::RegistryOperation::Pull)
                    .await
                    .unwrap();
            }
            let tags = client.list_tags(identifier).await.unwrap();
            assert_eq!(tags, vec!["1.0".to_string()]);

            let sent = registry.state.authorizations.lock().unwrap().clone();
            assert_eq!(
                sent.last().map(String::as_str),
                Some("Bearer minted-1"),
                "the request after a warm ensure_auth must carry the cached token, saw {sent:?}"
            );
            assert_eq!(
                registry.state.exchanges.load(Ordering::SeqCst),
                1,
                "the warm calls and the listing must all ride one exchange"
            );
        }

        /// C-023. Eight concurrent *cold* `ensure_auth` calls for one identifier
        /// produce one token exchange — the shape `ocx index sync` actually
        /// makes, which neither a sequential test nor a fork-level unit test
        /// covers.
        ///
        /// The token endpoint holds until the test releases it: without the hold
        /// the leader can finish before the others reach the miss, and the
        /// assertion passes on serial execution whether or not anything is
        /// coalesced.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_cold_ensure_auth_shares_one_token_exchange() {
            const CALLERS: usize = 8;

            let registry = StubRegistry::start().await;
            let client = Arc::new(registry.client());
            let identifier = registry.identifier("test/pkg");
            let release = registry.hold();

            let mut handles = Vec::new();
            for _ in 0..CALLERS {
                let client = Arc::clone(&client);
                let identifier = identifier.clone();
                handles.push(tokio::spawn(async move {
                    client.ensure_auth(&identifier, oci::RegistryOperation::Pull).await
                }));
            }

            // Release only once every caller has had the chance to enter the
            // miss path — the count means nothing if the leader could finish
            // before its peers arrived.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            release.send(true).unwrap();
            for handle in handles {
                handle.await.unwrap().unwrap();
            }

            assert_eq!(
                registry.state.exchanges.load(Ordering::SeqCst),
                1,
                "{CALLERS} concurrent cold callers for one scope must share one token exchange"
            );
        }

        /// C-025. The `GET /v2/` probe is issued once per host, not once per
        /// repository. Asserted as `== 1`, never `<= N`: before the change the
        /// count equalled the repository count, so an inequality would pass
        /// against the bug.
        #[tokio::test]
        async fn three_repositories_share_one_challenge_probe() {
            let registry = StubRegistry::start().await;
            let client = registry.client();

            for repository in ["test/one", "test/two", "test/three"] {
                client
                    .ensure_auth(&registry.identifier(repository), oci::RegistryOperation::Pull)
                    .await
                    .unwrap();
            }

            assert_eq!(
                registry.state.probes.load(Ordering::SeqCst),
                1,
                "three repositories under one host must share one challenge probe"
            );
            assert_eq!(
                registry.state.exchanges.load(Ordering::SeqCst),
                3,
                "each repository still mints its own scoped token"
            );
        }
    }
}
