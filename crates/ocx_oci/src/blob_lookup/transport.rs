// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The registry-shaped face of the lookup: verification reads blobs, manifests and referrer listings from
//! directories as if from one registry, and in `LookupOnly` mode never reaches a network.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncRead;

use crate::client::error::ClientError;
use crate::client::{Client, DeleteOutcome, ManifestPresence, NotFoundCode, OciTransport, ProgressFn};
use crate::{Descriptor, Digest, Manifest, native};

use super::markers::read_markers;
use super::{BlobLookup, LookupError, MAX_MARKER_NAMES_PER_ROOT, MAX_REFERRERS_PER_SUBJECT};

/// Ceiling on a manifest read through the lookup.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Ceiling on a blob read whole through the lookup.
const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

type Result<T> = std::result::Result<T, ClientError>;

/// How a client with a blob overlay answers reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayMode {
    /// The wrapped transport is asked first for listings; markers add candidates it did not name.
    Live,
    /// Directories only. No transport call, no DNS lookup, no auth.
    LookupOnly,
}

/// What a `Live` overlay delegates to.
enum Inner {
    /// `LookupOnly`, or a `Live` built with no transport; any call that needs one is refused.
    Absent,
    Fixed(Box<dyn OciTransport>),
    /// The client as it was before the overlay: its transport builds lazily from the kept recipe.
    Lazy(Box<Client>),
}

impl Inner {
    fn get(&self) -> Option<&dyn OciTransport> {
        match self {
            Self::Absent => None,
            Self::Fixed(transport) => Some(&**transport),
            Self::Lazy(client) => Some(client.transport()),
        }
    }

    fn duplicate(&self) -> Self {
        match self {
            Self::Absent => Self::Absent,
            Self::Fixed(transport) => Self::Fixed(transport.box_clone()),
            Self::Lazy(client) => Self::Lazy(client.clone()),
        }
    }
}

/// An [`OciTransport`] over the home blob root and a [`BlobLookup`], for one logical registry.
///
/// The `image.registry()` of the reference a caller passes is the physical host after a mirror rewrite and is
/// ignored: reads are keyed on the logical registry given here and on the digest.
///
/// Digest reads try the home, then the lookup, verified. Tag reads are not found; pushes are refused.
pub struct LookupTransport {
    mode: OverlayMode,
    inner: Inner,
    home_blobs: PathBuf,
    /// The lookup with the home read ahead of every directory: the order digest reads walk.
    ordered: BlobLookup,
    lookup: Arc<BlobLookup>,
    registry: String,
    accept_artifact_types: Vec<String>,
}

impl LookupTransport {
    /// A transport with no network behind it.
    pub fn lookup_only(
        home_blobs: PathBuf,
        lookup: Arc<BlobLookup>,
        registry: &str,
        accept_artifact_types: Vec<String>,
    ) -> Self {
        Self::build(
            OverlayMode::LookupOnly,
            Inner::Absent,
            home_blobs,
            lookup,
            registry,
            accept_artifact_types,
        )
    }

    /// A transport over `inner`: in `Live` mode `inner` answers first, in `LookupOnly` it is never called.
    pub fn over(
        inner: Box<dyn OciTransport>,
        mode: OverlayMode,
        home_blobs: PathBuf,
        lookup: Arc<BlobLookup>,
        registry: &str,
        accept_artifact_types: Vec<String>,
    ) -> Self {
        Self::build(
            mode,
            Inner::Fixed(inner),
            home_blobs,
            lookup,
            registry,
            accept_artifact_types,
        )
    }

    /// A `Live` transport over a client that has not built its own transport yet.
    pub(crate) fn live_over_client(
        client: Client,
        home_blobs: PathBuf,
        lookup: Arc<BlobLookup>,
        registry: &str,
        accept_artifact_types: Vec<String>,
    ) -> Self {
        Self::build(
            OverlayMode::Live,
            Inner::Lazy(Box::new(client)),
            home_blobs,
            lookup,
            registry,
            accept_artifact_types,
        )
    }

    fn build(
        mode: OverlayMode,
        inner: Inner,
        home_blobs: PathBuf,
        lookup: Arc<BlobLookup>,
        registry: &str,
        accept_artifact_types: Vec<String>,
    ) -> Self {
        Self {
            mode,
            inner,
            ordered: lookup.with_leading_dir(home_blobs.clone()),
            home_blobs,
            lookup,
            registry: registry.to_string(),
            accept_artifact_types,
        }
    }

    pub fn mode(&self) -> OverlayMode {
        self.mode
    }

    pub fn registry(&self) -> &str {
        &self.registry
    }

    pub fn home_blobs(&self) -> &Path {
        &self.home_blobs
    }

    pub fn accept_artifact_types(&self) -> &[String] {
        &self.accept_artifact_types
    }

    /// True when a root holds `referrers/.complete` for `subject`: its marker listing is the whole listing.
    pub async fn listing_complete(&self, subject: &Digest) -> bool {
        for root in self.marker_roots() {
            let complete = super::blob_dir_path(&root, &self.registry, subject)
                .join("referrers")
                .join(super::markers::COMPLETE_FILE);
            if super::registry_slug_is_safe(&self.registry)
                && tokio::fs::symlink_metadata(&complete)
                    .await
                    .is_ok_and(|metadata| metadata.is_file())
            {
                return true;
            }
        }
        false
    }

    /// Home, then the blob directories, then the cache: the order candidates are listed in.
    fn marker_roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.home_blobs.clone())
            .chain(self.lookup.dirs().iter().cloned())
            .chain(self.lookup.cache().map(|cache| cache.root().to_path_buf()))
            .collect()
    }

    /// The inner transport, only where this transport may use one.
    fn live_inner(&self) -> Option<&dyn OciTransport> {
        match self.mode {
            OverlayMode::Live => self.inner.get(),
            OverlayMode::LookupOnly => None,
        }
    }

    async fn read_verified(&self, digest: &Digest, cap: u64) -> Result<Option<Vec<u8>>> {
        self.ordered
            .read_metadata(&self.registry, digest, cap)
            .await
            .map_err(lookup_error)
    }

    /// The descriptor of a marker candidate, only when its digest-verified manifest names `subject` and carries an
    /// accepted `artifactType`. A marker is a hint and proves nothing.
    async fn accept_candidate(&self, candidate: &Digest, subject: &Digest, wanted: Option<&str>) -> Option<Descriptor> {
        let bytes = match self.read_verified(candidate, MAX_MANIFEST_BYTES).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return None,
            Err(error) => {
                log::warn!("skipping referrer candidate {candidate}: {error}");
                return None;
            }
        };
        let facts: ReferrerFacts = serde_json::from_slice(&bytes).ok()?;
        if facts.subject?.digest != subject.to_string() {
            return None;
        }
        // The OCI rule: an absent `artifactType` is the config media type.
        let artifact_type = facts.artifact_type.or(facts.config.map(|config| config.media_type))?;
        if !self.accept_artifact_types.contains(&artifact_type) || wanted.is_some_and(|wanted| wanted != artifact_type)
        {
            return None;
        }
        Some(Descriptor {
            media_type: facts
                .media_type
                .unwrap_or_else(|| crate::media_type::MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            digest: candidate.to_string(),
            size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            urls: None,
            annotations: facts.annotations,
            artifact_type: Some(artifact_type),
        })
    }
}

/// What a candidate manifest says about itself; read only after its bytes hashed to its digest.
#[derive(serde::Deserialize)]
struct ReferrerFacts {
    #[serde(rename = "mediaType")]
    media_type: Option<String>,
    #[serde(rename = "artifactType")]
    artifact_type: Option<String>,
    subject: Option<SubjectFacts>,
    config: Option<ConfigFacts>,
    annotations: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(serde::Deserialize)]
struct SubjectFacts {
    digest: String,
}

#[derive(serde::Deserialize)]
struct ConfigFacts {
    #[serde(rename = "mediaType")]
    media_type: String,
}

fn lookup_error(error: LookupError) -> ClientError {
    match error {
        LookupError::DigestMismatch { expected, actual, .. } => ClientError::DigestMismatch { expected, actual },
        other => ClientError::internal(other),
    }
}

fn read_only() -> ClientError {
    ClientError::Registry(Box::new(std::io::Error::other("a blob lookup transport is read-only")))
}

fn digest_of(image: &native::Reference) -> Option<Digest> {
    image.digest().and_then(|digest| Digest::try_from(digest).ok())
}

impl Clone for LookupTransport {
    fn clone(&self) -> Self {
        Self {
            mode: self.mode,
            inner: self.inner.duplicate(),
            home_blobs: self.home_blobs.clone(),
            ordered: self.ordered.clone(),
            lookup: Arc::clone(&self.lookup),
            registry: self.registry.clone(),
            accept_artifact_types: self.accept_artifact_types.clone(),
        }
    }
}

impl crate::sealed::Sealed for LookupTransport {}

#[async_trait]
impl OciTransport for LookupTransport {
    async fn ensure_auth(&self, _image: &native::Reference, _operation: crate::RegistryOperation) -> Result<()> {
        Ok(())
    }

    async fn list_tags(
        &self,
        image: &native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>> {
        match self.live_inner() {
            Some(inner) => inner.list_tags(image, chunk_size, last).await,
            None => Ok(Vec::new()),
        }
    }

    async fn catalog(&self, image: &native::Reference, chunk_size: usize, last: Option<String>) -> Result<Vec<String>> {
        match self.live_inner() {
            Some(inner) => inner.catalog(image, chunk_size, last).await,
            None => Ok(Vec::new()),
        }
    }

    async fn fetch_manifest_digest(&self, image: &native::Reference) -> Result<String> {
        if let Some(digest) = digest_of(image)
            && self.read_verified(&digest, MAX_MANIFEST_BYTES).await?.is_some()
        {
            return Ok(digest.to_string());
        }
        match self.live_inner() {
            Some(inner) => inner.fetch_manifest_digest(image).await,
            None => Err(ClientError::ManifestNotFound(image.to_string())),
        }
    }

    async fn pull_manifest_raw(
        &self,
        image: &native::Reference,
        accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)> {
        if let Some(digest) = digest_of(image)
            && let Some(bytes) = self.read_verified(&digest, MAX_MANIFEST_BYTES).await?
        {
            return Ok((bytes, digest.to_string()));
        }
        match self.live_inner() {
            Some(inner) => inner.pull_manifest_raw(image, accepted_media_types).await,
            None => Err(ClientError::ManifestNotFound(image.to_string())),
        }
    }

    async fn pull_blob(&self, image: &native::Reference, digest: &Digest) -> Result<Vec<u8>> {
        if let Some(bytes) = self.read_verified(digest, MAX_BLOB_BYTES).await? {
            return Ok(bytes);
        }
        match self.live_inner() {
            Some(inner) => inner.pull_blob(image, digest).await,
            None => Err(ClientError::blob_not_found(image, digest)),
        }
    }

    async fn pull_blob_to_file(&self, image: &native::Reference, digest: &Digest, path: &Path) -> Result<()> {
        if let Some(bytes) = self.read_verified(digest, MAX_BLOB_BYTES).await? {
            return tokio::fs::write(path, bytes).await.map_err(|source| ClientError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
        match self.live_inner() {
            Some(inner) => inner.pull_blob_to_file(image, digest, path).await,
            None => Err(ClientError::blob_not_found(image, digest)),
        }
    }

    async fn head_blob(&self, image: &native::Reference, digest: &Digest) -> Result<u64> {
        if let Some(stream) = self
            .ordered
            .open_stream(&self.registry, digest)
            .await
            .map_err(lookup_error)?
        {
            return Ok(stream.size);
        }
        match self.live_inner() {
            Some(inner) => inner.head_blob(image, digest).await,
            None => Err(ClientError::blob_not_found(image, digest)),
        }
    }

    /// Reads the verified blob whole and serves it from memory: what verification streams is a bundle or a
    /// manifest, bounded by `MAX_BLOB_BYTES`.
    async fn pull_blob_streaming(
        &self,
        image: &native::Reference,
        digest: &Digest,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin + 'static>> {
        if let Some(bytes) = self.read_verified(digest, MAX_BLOB_BYTES).await? {
            return Ok(Box::new(std::io::Cursor::new(bytes)));
        }
        match self.live_inner() {
            Some(inner) => inner.pull_blob_streaming(image, digest).await,
            None => Err(ClientError::blob_not_found(image, digest)),
        }
    }

    async fn push_manifest(&self, _image: &native::Reference, _manifest: &Manifest) -> Result<String> {
        Err(read_only())
    }

    async fn push_manifest_raw(&self, _image: &native::Reference, _data: Vec<u8>, _media_type: &str) -> Result<String> {
        Err(read_only())
    }

    async fn push_blob(
        &self,
        _image: &native::Reference,
        _data: Vec<u8>,
        _digest: &Digest,
        _on_progress: ProgressFn,
    ) -> Result<String> {
        Err(read_only())
    }

    async fn push_blob_from_path(
        &self,
        _image: &native::Reference,
        _path: &Path,
        _digest: &Digest,
        _on_progress: ProgressFn,
    ) -> Result<String> {
        Err(read_only())
    }

    async fn delete_manifest(&self, _image: &native::Reference) -> Result<DeleteOutcome> {
        Err(read_only())
    }

    async fn probe_manifest(&self, image: &native::Reference) -> Result<ManifestPresence> {
        if let Some(digest) = digest_of(image)
            && self.read_verified(&digest, MAX_MANIFEST_BYTES).await?.is_some()
        {
            return Ok(ManifestPresence::Present(digest));
        }
        match self.live_inner() {
            Some(inner) => inner.probe_manifest(image).await,
            None => Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown)),
        }
    }

    async fn push_referrer_manifest(
        &self,
        _image: &native::Reference,
        _subject_digest: &Digest,
        _manifest_bytes: &[u8],
        _media_type: &str,
    ) -> Result<Descriptor> {
        Err(read_only())
    }

    /// In `Live` the inner listing comes first; marker candidates from the home, the blob directories, then the
    /// cache follow, each kept only when its verified manifest names `subject` and carries an accepted
    /// `artifactType`, and counted against `MAX_REFERRERS_PER_SUBJECT` per root only then.
    async fn list_referrers(
        &self,
        image: &native::Reference,
        subject_digest: &Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<Descriptor>> {
        let mut listed = Vec::new();
        if let Some(inner) = self.live_inner() {
            listed = match inner.list_referrers(image, subject_digest, artifact_type).await {
                Ok(listed) => listed,
                Err(ClientError::ReferrersUnsupported { .. }) => {
                    inner
                        .list_referrers_with_fallback(image, subject_digest, artifact_type)
                        .await?
                        .descriptors
                }
                Err(other) => return Err(other),
            };
        }
        let mut seen: HashSet<String> = listed.iter().map(|descriptor| descriptor.digest.clone()).collect();
        for root in self.marker_roots() {
            let (registry, subject) = (self.registry.clone(), subject_digest.clone());
            let markers = tokio::task::spawn_blocking(move || {
                read_markers(&root, &registry, &subject, MAX_MARKER_NAMES_PER_ROOT)
            })
            .await
            .map_err(ClientError::internal)?
            .map_err(lookup_error)?;
            let mut accepted = 0usize;
            for candidate in markers.digests {
                if seen.contains(&candidate.to_string()) {
                    continue;
                }
                if accepted == MAX_REFERRERS_PER_SUBJECT {
                    log::warn!(
                        "more than {MAX_REFERRERS_PER_SUBJECT} accepted referrers of {subject_digest} in one root; the rest are ignored"
                    );
                    break;
                }
                if let Some(descriptor) = self.accept_candidate(&candidate, subject_digest, artifact_type).await {
                    seen.insert(descriptor.digest.clone());
                    listed.push(descriptor);
                    accepted += 1;
                }
            }
        }
        Ok(listed)
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_lookup::markers::{write_complete, write_marker};
    use crate::blob_lookup::test_support::{put_blob, sha256};
    use crate::blob_lookup::{BlobCacheDir, MAX_MARKER_NAMES_PER_ROOT, MAX_REFERRERS_PER_SUBJECT};
    use crate::client::test_transport::{StubTransport, StubTransportData, referrers_key};

    const REG: &str = "registry.test";
    const SIGNATURE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
    const SBOM: &str = "application/vnd.cyclonedx+json";

    struct Rig {
        home: tempfile::TempDir,
        dir: tempfile::TempDir,
        cache: tempfile::TempDir,
        subject: Digest,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().unwrap(),
                dir: tempfile::tempdir().unwrap(),
                cache: tempfile::tempdir().unwrap(),
                subject: sha256(b"subject"),
            }
        }

        fn lookup(&self) -> Arc<BlobLookup> {
            Arc::new(BlobLookup::new(
                vec![self.dir.path().to_path_buf()],
                Some(BlobCacheDir::at(self.cache.path().to_path_buf())),
            ))
        }

        fn only(&self, registry: &str) -> LookupTransport {
            LookupTransport::lookup_only(
                self.home.path().to_path_buf(),
                self.lookup(),
                registry,
                vec![SIGNATURE.to_string()],
            )
        }

        /// Stores a referrer manifest of `artifact_type` naming `subject` under `root` and marks it.
        fn referrer(&self, root: &Path, subject: &Digest, artifact_type: &str, nonce: u32) -> Digest {
            let bytes = referrer_manifest(subject, artifact_type, nonce);
            let digest = put_blob(root, REG, &bytes);
            write_marker(root, REG, &self.subject, &digest).unwrap();
            digest
        }
    }

    fn referrer_manifest(subject: &Digest, artifact_type: &str, nonce: u32) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": artifact_type,
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": sha256(b"{}").to_string(), "size": 2},
            "layers": [],
            "subject": {"mediaType": "application/vnd.oci.image.index.v1+json", "digest": subject.to_string(), "size": 7},
            "annotations": {"nonce": nonce.to_string()},
        }))
        .unwrap()
    }

    fn reference() -> native::Reference {
        format!("mirror.invalid/team/tool@{}", sha256(b"subject"))
            .parse()
            .expect("a well-formed reference")
    }

    fn digests(listed: &[Descriptor]) -> Vec<String> {
        listed.iter().map(|d| d.digest.clone()).collect()
    }

    #[tokio::test]
    async fn lookup_only_never_calls_inner() {
        let rig = Rig::new();
        let data = StubTransportData::new();
        let transport = LookupTransport::over(
            Box::new(StubTransport::new(data.clone())),
            OverlayMode::LookupOnly,
            rig.home.path().to_path_buf(),
            rig.lookup(),
            REG,
            vec![SIGNATURE.to_string()],
        );
        rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, 0);

        let listed = transport
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();
        let _ = transport.pull_blob(&reference(), &sha256(b"absent")).await;
        let _ = transport
            .list_referrers_with_fallback(&reference(), &rig.subject, None)
            .await;

        assert_eq!(listed.len(), 1);
        assert!(data.read().calls.is_empty(), "calls: {:?}", data.read().calls);
    }

    #[tokio::test]
    async fn live_listing_first() {
        let rig = Rig::new();
        let data = StubTransportData::new();
        let live_descriptor = Descriptor {
            media_type: "application/vnd.oci.image.manifest.v1+json".into(),
            digest: sha256(b"live referrer").to_string(),
            size: 10,
            urls: None,
            annotations: None,
            artifact_type: Some(SIGNATURE.into()),
        };
        data.write()
            .referrers
            .insert(referrers_key(&reference(), &rig.subject), vec![live_descriptor.clone()]);
        let transport = LookupTransport::over(
            Box::new(StubTransport::new(data)),
            OverlayMode::Live,
            rig.home.path().to_path_buf(),
            rig.lookup(),
            REG,
            vec![SIGNATURE.to_string()],
        );
        let marked = rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, 1);

        let listed = transport
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();

        assert_eq!(digests(&listed), vec![live_descriptor.digest, marked.to_string()]);
    }

    #[tokio::test]
    async fn planted_marker_without_subject_dropped() {
        let rig = Rig::new();
        let other_subject = sha256(b"another subject");
        let wrong = rig.referrer(rig.dir.path(), &other_subject, SIGNATURE, 0);
        write_marker(rig.dir.path(), REG, &rig.subject, &sha256(b"names no blob")).unwrap();
        let right = rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, 1);

        let listed = rig
            .only(REG)
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();

        assert_eq!(digests(&listed), vec![right.to_string()]);
        assert!(!digests(&listed).contains(&wrong.to_string()));
    }

    #[tokio::test]
    async fn non_signature_markers_do_not_consume_cap() {
        let rig = Rig::new();
        for nonce in 0..(MAX_REFERRERS_PER_SUBJECT as u32 + 40) {
            rig.referrer(rig.dir.path(), &rig.subject, SBOM, nonce);
        }
        let signature = rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, 9999);

        let listed = rig
            .only(REG)
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();

        assert_eq!(digests(&listed), vec![signature.to_string()]);
    }

    #[tokio::test]
    async fn marker_cap_per_root() {
        let rig = Rig::new();
        for nonce in 0..(MAX_REFERRERS_PER_SUBJECT as u32 + 40) {
            rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, nonce);
        }

        let listed = rig
            .only(REG)
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();

        assert_eq!(listed.len(), MAX_REFERRERS_PER_SUBJECT);
    }

    #[tokio::test]
    async fn listing_name_bound() {
        let rig = Rig::new();
        let dir = crate::blob_lookup::blob_dir_path(rig.dir.path(), REG, &rig.subject).join("referrers");
        std::fs::create_dir_all(&dir).unwrap();
        for nonce in 0..(MAX_MARKER_NAMES_PER_ROOT as u32 + 20) {
            std::fs::write(
                dir.join(sha256(&nonce.to_le_bytes()).to_string().replace(':', "-")),
                b"",
            )
            .unwrap();
        }

        let markers = rig.lookup().referrers(REG, &rig.subject).await.unwrap();

        assert_eq!(markers.digests.len(), MAX_MARKER_NAMES_PER_ROOT);
    }

    #[tokio::test]
    async fn markers_keyed_by_logical_registry() {
        let rig = Rig::new();
        let marked = rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, 0);

        let found = rig
            .only(REG)
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();
        let elsewhere = rig
            .only("other.test")
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();

        assert_eq!(
            digests(&found),
            vec![marked.to_string()],
            "the physical host is ignored"
        );
        assert!(elsewhere.is_empty(), "another logical registry has its own subtree");
    }

    #[tokio::test]
    async fn cache_markers_after_dir_markers() {
        let rig = Rig::new();
        let expected = vec![
            rig.referrer(rig.home.path(), &rig.subject, SIGNATURE, 1).to_string(),
            rig.referrer(rig.dir.path(), &rig.subject, SIGNATURE, 2).to_string(),
            rig.referrer(rig.cache.path(), &rig.subject, SIGNATURE, 3).to_string(),
        ];

        let listed = rig
            .only(REG)
            .list_referrers(&reference(), &rig.subject, None)
            .await
            .unwrap();

        assert_eq!(
            digests(&listed),
            expected,
            "home, then directories, then cache, whatever the digest order"
        );
    }

    #[tokio::test]
    async fn tag_read_not_found() {
        let rig = Rig::new();
        let tagged = "mirror.invalid/team/tool:1".parse().expect("a well-formed reference");

        let error = rig
            .only(REG)
            .pull_manifest_raw(&tagged, &[])
            .await
            .expect_err("a tag is never answered from a directory");

        assert!(matches!(error, ClientError::ManifestNotFound(_)), "{error:?}");
        assert!(rig.only(REG).fetch_manifest_digest(&tagged).await.is_err());
    }

    #[tokio::test]
    async fn push_refused() {
        let rig = Rig::new();
        let transport = rig.only(REG);

        assert!(
            transport
                .push_manifest_raw(&reference(), b"{}".to_vec(), "application/json")
                .await
                .is_err()
        );
        assert!(transport.delete_manifest(&reference()).await.is_err());
    }

    #[tokio::test]
    async fn listing_complete_reads_marker() {
        let rig = Rig::new();
        let transport = rig.only(REG);
        assert!(!transport.listing_complete(&rig.subject).await);

        write_complete(rig.dir.path(), REG, &rig.subject).unwrap();

        assert!(transport.listing_complete(&rig.subject).await);
        assert!(!transport.listing_complete(&sha256(b"another subject")).await);
    }
}
