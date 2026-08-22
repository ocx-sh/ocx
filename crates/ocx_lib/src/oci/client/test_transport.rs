// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::error::ClientError;
use super::transport::{MountOutcome, OciTransport, Result};
use crate::oci::{self, Algorithm, RegistryOperation};

/// Test data backing a [`StubTransport`].
///
/// Fields are public so they can be accessed through the lock guards
/// returned by [`StubTransportData::read`] and [`StubTransportData::write`].
#[derive(Default)]
pub(crate) struct StubTransportInner {
    /// Pages of tags returned by successive `list_tags` calls (consumed FIFO).
    pub tags: Vec<Vec<String>>,
    /// Pages of repositories returned by successive `catalog` calls (consumed FIFO).
    pub repositories: Vec<Vec<String>>,
    /// Image string → (raw manifest bytes, digest string).
    pub manifests: HashMap<String, (Vec<u8>, String)>,
    /// Image string → artificial latency before `pull_manifest_raw` answers.
    ///
    /// Exists so a concurrent fetch loop's completion order can be made to
    /// differ from its submission order. Without it an in-memory stub answers
    /// every request in submission order, and an ordering assertion passes just
    /// as happily against an unordered implementation — proving nothing.
    pub manifest_delays: HashMap<String, std::time::Duration>,
    /// Digest string → blob bytes (written to file by `pull_blob_to_file`).
    ///
    /// Content only: which *repository* holds a blob is a separate question,
    /// answered by [`blob_locations`](Self::blob_locations).
    pub blobs: HashMap<String, Vec<u8>>,
    /// `"<registry>/<repository>"` → the digests it holds, for `head_blob`.
    ///
    /// `None` — the default — makes `head_blob` answer from `blobs` alone: one
    /// global store, which is right for every test with a single registry in
    /// play. `Some` scopes presence per repository, which a registry-to-registry
    /// copy needs, because there the whole question is whether the *target*
    /// already has a blob the *source* obviously does. `Option` rather than an
    /// empty map so "not configured" and "configured, holds nothing" stay
    /// distinguishable — an empty map would make a copy test that forgot to set
    /// it up pass for the wrong reason.
    pub blob_locations: Option<HashMap<String, std::collections::BTreeSet<String>>>,

    /// Digest string → read-boundary plan for `pull_blob_streaming`.
    ///
    /// When a plan exists for a digest, the stub yields exactly those chunks in
    /// order, one per read, instead of the whole blob. Exists so a test can put
    /// a layer's codec trailer in a chunk the tar extractor never demands —
    /// the shape a real network produces by chance, and the one that decides
    /// whether the compressed-side digest covers the whole blob or a prefix.
    pub blob_stream_chunks: HashMap<String, Vec<Vec<u8>>>,
    /// Artificial latency before `list_tags` / `fetch_manifest_digest` answer.
    ///
    /// The tag-read sibling of [`manifest_delays`](Self::manifest_delays), and
    /// there for the same reason: an in-memory stub answers instantly, so a
    /// caller that read-checks a cache, misses and fetches can complete before
    /// the next one starts. A "these N concurrent reads made one request"
    /// assertion then passes just as happily against no coalescing at all.
    /// Under `tokio::time::pause()` the clock only advances once every task is
    /// parked, so the delay releases exactly when all N callers have arrived.
    pub tag_read_delay: Option<std::time::Duration>,
    /// Digest returned by `fetch_manifest_digest`.
    pub digest: Option<String>,
    /// Successive results for push operations (consumed FIFO).
    pub push_results: Vec<Result<String>>,
    /// Log of method calls for assertions.
    pub calls: Vec<String>,
    /// Log of `ensure_auth` calls: `(registry, operation)`.
    pub auth_calls: Vec<(String, RegistryOperation)>,
    /// When true, `push_manifest_raw` stores pushed data back into `manifests`
    /// so subsequent reads see the updated content.
    pub capture_pushes: bool,
    /// When set, `pull_manifest_raw` returns a `Registry` error with this
    /// message for any image not in `manifests` (instead of `ManifestNotFound`).
    pub pull_manifest_error_override: Option<String>,
    /// Image string → registry error message for that one image's manifest
    /// fetch, whether or not a manifest is seeded for it.
    ///
    /// Exists because `pull_manifest_error_override` above only fires for
    /// images that are ABSENT, so it cannot express the failure that matters
    /// most here: a healthy, published version whose fetch fails transiently
    /// mid-run. That is the shape a cascade swallows, and reproducing it needs
    /// the error to win over a seeded manifest.
    pub manifest_errors: HashMap<String, String>,
    /// When set, `ensure_auth` returns `ClientError::Authentication` with this
    /// message instead of succeeding. Drives a genuine authentication-failure
    /// path through any transport method that calls `ensure_auth` first (e.g.
    /// `Client::pull_manifest`) — distinct from `pull_manifest_error_override`,
    /// which simulates a generic (non-auth) registry error.
    pub ensure_auth_error_override: Option<String>,
    /// Successive results for `mount_blob` calls (consumed FIFO); an empty
    /// queue falls through to the trait's default `Ok(UploadRequired)`.
    pub mount_results: Vec<Result<MountOutcome>>,
    /// Log of `mount_blob` calls: `(target_repository, source_repository, digest)`.
    pub mount_calls: Vec<(String, String, String)>,
    /// `"<repository>@<subject digest>"` → referrer descriptors for that subject.
    ///
    /// Seeded directly to stand in for a source registry's referrers index;
    /// grown by `push_referrer_manifest` when `capture_pushes` is set, which is
    /// what lets a test push a referrer and then list it back.
    pub referrers: HashMap<String, Vec<oci::Descriptor>>,
    /// When true, both referrer methods fail with
    /// [`ClientError::ReferrersUnsupported`] — a registry with no OCI 1.1
    /// Referrers API. Distinct from an empty `referrers` map, which is a
    /// supporting registry answering "no referrers"; conflating the two is the
    /// bug the hard error exists to prevent.
    pub referrers_unsupported: bool,
}

/// Keys [`StubTransportInner::blob_locations`].
///
/// Registry *and* repository: a promotion routinely copies `team/demo` on one
/// host to `team/demo` on another, so a repository-only key would report the
/// target as already holding every blob the source holds.
pub(crate) fn blob_location_key(image: &oci::native::Reference) -> String {
    format!("{}/{}", image.resolve_registry(), image.repository())
}

/// Keys [`StubTransportInner::referrers`]. Repository-scoped, because a
/// referrer belongs to a subject in a repository — not to whatever tag the
/// caller's reference happened to carry.
pub(crate) fn referrers_key(image: &oci::native::Reference, subject_digest: &oci::Digest) -> String {
    format!("{}@{}", image.repository(), subject_digest)
}

/// Shared data handle for [`StubTransport`].
///
/// Wraps `Arc<RwLock<...>>` internally so test code never needs to deal
/// with locking boilerplate. Clones are cheap (Arc clone).
///
/// ```ignore
/// let data = StubTransportData::new();
/// data.write().tags = vec![vec!["1.0".into()]];
/// let client = Client::with_transport(Box::new(StubTransport::new(data.clone())));
/// // later: data.read().calls  — inspect recorded calls
/// // later: data.write().digest = Some("sha256:...".into())  — modify on the fly
/// ```
#[derive(Clone)]
pub(crate) struct StubTransportData {
    inner: Arc<RwLock<StubTransportInner>>,
}

impl Default for StubTransportData {
    fn default() -> Self {
        Self::new()
    }
}

impl StubTransportData {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(StubTransportInner::default())),
        }
    }

    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, StubTransportInner> {
        self.inner.read().unwrap()
    }

    pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, StubTransportInner> {
        self.inner.write().unwrap()
    }
}

/// A configurable, cloneable test double for [`OciTransport`].
///
/// All mutable state lives in a shared [`StubTransportData`] (behind
/// `Arc<RwLock<...>>`), so clones — including via [`OciTransport::box_clone`] —
/// share the same backing data.
#[derive(Clone)]
pub(crate) struct StubTransport {
    data: StubTransportData,
}

impl StubTransport {
    pub fn new(data: StubTransportData) -> Self {
        Self { data }
    }

    fn record(&self, call: &str) {
        self.data.write().calls.push(call.to_string());
    }

    fn next_push_result(&self) -> Result<String> {
        let mut inner = self.data.write();
        if inner.push_results.is_empty() {
            Ok("sha256:stub_digest".to_string())
        } else {
            inner.push_results.remove(0)
        }
    }
}

#[async_trait]
impl OciTransport for StubTransport {
    async fn ensure_auth(&self, image: &oci::native::Reference, operation: oci::RegistryOperation) -> Result<()> {
        self.data
            .write()
            .auth_calls
            .push((image.resolve_registry().to_string(), operation));
        let override_message = self.data.read().ensure_auth_error_override.clone();
        if let Some(message) = override_message {
            return Err(ClientError::Authentication(Box::new(std::io::Error::other(message))));
        }
        Ok(())
    }

    async fn list_tags(
        &self,
        _image: &oci::native::Reference,
        _chunk_size: usize,
        _last: Option<String>,
    ) -> Result<Vec<String>> {
        self.record("list_tags");
        // Bind the delay out first: the read guard must not span the await.
        let delay = self.data.read().tag_read_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut inner = self.data.write();
        if inner.tags.is_empty() {
            Ok(vec![])
        } else {
            Ok(inner.tags.remove(0))
        }
    }

    async fn catalog(
        &self,
        _image: &oci::native::Reference,
        _chunk_size: usize,
        _last: Option<String>,
    ) -> Result<Vec<String>> {
        self.record("catalog");
        let mut inner = self.data.write();
        if inner.repositories.is_empty() {
            Ok(vec![])
        } else {
            Ok(inner.repositories.remove(0))
        }
    }

    async fn fetch_manifest_digest(&self, image: &oci::native::Reference) -> Result<String> {
        self.record("fetch_manifest_digest");
        let delay = self.data.read().tag_read_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let key = image.to_string();
        let inner = self.data.read();
        // A per-image error wins over every other answer, including a seeded
        // manifest: it stands in for a registry that is failing this one read.
        if let Some(message) = inner.manifest_errors.get(&key) {
            return Err(ClientError::Registry(message.clone().into()));
        }
        // Explicit `digest` override wins; otherwise mirror a real registry's
        // HEAD semantics: the digest of the manifest stored at the reference,
        // or 404 (`ManifestNotFound`) when nothing is stored there.
        if let Some(digest) = inner.digest.clone() {
            return Ok(digest);
        }
        inner
            .manifests
            .get(&key)
            .map(|(_, digest)| digest.clone())
            .ok_or(ClientError::ManifestNotFound(key))
    }

    async fn pull_manifest_raw(
        &self,
        image: &oci::native::Reference,
        _accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)> {
        self.record("pull_manifest_raw");
        let key = image.to_string();
        // Read the delay and release the lock before awaiting — the guard is not
        // held across the sleep, and concurrent callers must not serialise here.
        let delay = self.data.read().manifest_delays.get(&key).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let inner = self.data.read();
        if let Some(message) = inner.manifest_errors.get(&key) {
            Err(ClientError::Registry(message.clone().into()))
        } else if let Some(manifest) = inner.manifests.get(&key).cloned() {
            Ok(manifest)
        } else if let Some(msg) = &inner.pull_manifest_error_override {
            Err(ClientError::Registry(msg.clone().into()))
        } else {
            Err(ClientError::ManifestNotFound(key))
        }
    }

    async fn head_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<u64> {
        let digest_key = digest.to_string();
        self.record(&format!("head_blob:{}", digest_key));
        let inner = self.data.read();
        let present = match &inner.blob_locations {
            Some(locations) => locations
                .get(&blob_location_key(image))
                .is_some_and(|digests| digests.contains(&digest_key)),
            None => inner.blobs.contains_key(&digest_key),
        };
        match (present, inner.blobs.get(&digest_key)) {
            (true, Some(blob)) => Ok(blob.len() as u64),
            // Listed as present but with no seeded content: still a HEAD hit, and
            // the size is all a caller gets from it.
            (true, None) => Ok(0),
            (false, _) => Err(ClientError::blob_not_found(image, digest)),
        }
    }

    async fn pull_blob(&self, _image: &oci::native::Reference, digest: &oci::Digest) -> Result<Vec<u8>> {
        let digest_key = digest.to_string();
        self.record(&format!("pull_blob:{}", digest_key));
        let inner = self.data.read();
        Ok(inner.blobs.get(&digest_key).cloned().unwrap_or_default())
    }

    async fn pull_blob_to_file(
        &self,
        _image: &oci::native::Reference,
        digest: &oci::Digest,
        path: &std::path::Path,
    ) -> Result<()> {
        let digest_key = digest.to_string();
        self.record(&format!("pull_blob_to_file:{}", digest_key));
        let inner = self.data.read();
        if let Some(blob) = inner.blobs.get(&digest_key) {
            let blob = blob.clone();
            drop(inner); // release lock before I/O
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ClientError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
            std::fs::write(path, &blob).map_err(|e| ClientError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        }
        Ok(())
    }

    async fn pull_blob_streaming(
        &self,
        _image: &oci::native::Reference,
        digest: &oci::Digest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
        let digest_key = digest.to_string();
        self.record(&format!("pull_blob_streaming:{digest_key}"));
        // Overriding the trait default (temp file round-trip) is what makes read
        // boundaries controllable. Without a plan the whole blob is one chunk,
        // which is what the default delivers on its first fill anyway.
        let chunks = {
            let inner = self.data.read();
            inner
                .blob_stream_chunks
                .get(&digest_key)
                .cloned()
                .unwrap_or_else(|| vec![inner.blobs.get(&digest_key).cloned().unwrap_or_default()])
        };
        // `StreamReader` hands out at most one stream item per `poll_read`, so
        // each planned chunk is exactly one read at the pipeline's bottom.
        let stream = futures::stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(chunk))),
        );
        Ok(Box::new(tokio_util::io::StreamReader::new(stream)))
    }

    async fn push_manifest(&self, _image: &oci::native::Reference, _manifest: &oci::Manifest) -> Result<String> {
        self.record("push_manifest");
        self.next_push_result()
    }

    async fn push_manifest_raw(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        _media_type: &str,
    ) -> Result<String> {
        self.record("push_manifest_raw");
        let digest = Algorithm::Sha256.hash(&data).to_string();
        // Consult the queued outcome before recording: a manifest push that
        // fails did not land, and storing it first made a failed push
        // indistinguishable from a successful one in `manifests` — which is
        // what stopped a test modelling a failed index merge.
        let outcome = {
            let mut inner = self.data.write();
            if inner.push_results.is_empty() {
                Ok(digest.clone())
            } else {
                inner.push_results.remove(0)
            }
        };
        if outcome.is_ok() && self.data.read().capture_pushes {
            self.data.write().manifests.insert(image.to_string(), (data, digest));
        }
        outcome
    }

    async fn push_blob(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        digest: &oci::Digest,
        on_progress: super::transport::ProgressFn,
    ) -> Result<String> {
        self.record(&format!("push_blob:{}", digest));
        // Simulate progress: report full size in one shot.
        on_progress(data.len() as u64);
        // A pushed blob is present in the target repository afterwards, so a
        // second copy of the same content HEADs it and skips the upload. Without
        // this an idempotency test could never observe the skip.
        {
            let mut inner = self.data.write();
            let digest_key = digest.to_string();
            inner.blobs.entry(digest_key.clone()).or_insert_with(|| data.clone());
            if let Some(locations) = inner.blob_locations.as_mut() {
                locations
                    .entry(blob_location_key(image))
                    .or_default()
                    .insert(digest_key);
            }
        }
        self.next_push_result()
    }

    /// Buffers, because a stub's blobs are already in memory. Stated here
    /// rather than inherited: the trait has no default, so a real transport
    /// cannot reach this shape by forgetting to write one.
    async fn push_blob_from_path(
        &self,
        image: &oci::native::Reference,
        path: &std::path::Path,
        digest: &oci::Digest,
        on_progress: super::transport::ProgressFn,
    ) -> Result<String> {
        super::transport::push_blob_buffered(self, image, path, digest, on_progress).await
    }

    async fn mount_blob(
        &self,
        image: &oci::native::Reference,
        source_repository: &str,
        digest: &oci::Digest,
    ) -> Result<MountOutcome> {
        self.record("mount_blob");
        let mut inner = self.data.write();
        inner.mount_calls.push((
            image.repository().to_string(),
            source_repository.to_string(),
            digest.to_string(),
        ));
        let outcome = if inner.mount_results.is_empty() {
            Ok(MountOutcome::UploadRequired)
        } else {
            inner.mount_results.remove(0)
        };
        if matches!(outcome, Ok(MountOutcome::Mounted))
            && let Some(locations) = inner.blob_locations.as_mut()
        {
            locations
                .entry(blob_location_key(image))
                .or_default()
                .insert(digest.to_string());
        }
        outcome
    }

    async fn push_referrer_manifest(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        manifest_bytes: &[u8],
        media_type: &str,
    ) -> Result<oci::Descriptor> {
        self.record("push_referrer_manifest");
        if self.data.read().referrers_unsupported {
            return Err(ClientError::ReferrersUnsupported {
                registry: image.resolve_registry().to_string(),
            });
        }
        let digest = Algorithm::Sha256.hash(manifest_bytes).to_string();
        let size = i64::try_from(manifest_bytes.len()).map_err(|_| {
            ClientError::InvalidManifest(format!(
                "referrer manifest size {} exceeds i64::MAX",
                manifest_bytes.len()
            ))
        })?;
        let descriptor = oci::Descriptor {
            media_type: media_type.to_string(),
            digest: digest.clone(),
            size,
            urls: None,
            // A real registry copies `artifactType` out of the manifest into the
            // referrers-index descriptor, and that is the field `list_referrers`
            // filters on — so a stub that left it `None` would make every
            // filtered listing come back empty and read as "nothing to copy".
            artifact_type: referrer_artifact_type(manifest_bytes),
            annotations: None,
        };
        if self.data.read().capture_pushes {
            let mut inner = self.data.write();
            inner.manifests.insert(
                image.clone_with_digest(digest).to_string(),
                (manifest_bytes.to_vec(), descriptor.digest.clone()),
            );
            inner
                .referrers
                .entry(referrers_key(image, subject_digest))
                .or_default()
                .push(descriptor.clone());
        }
        Ok(descriptor)
    }

    async fn list_referrers(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<oci::Descriptor>> {
        self.record("list_referrers");
        let inner = self.data.read();
        if inner.referrers_unsupported {
            return Err(ClientError::ReferrersUnsupported {
                registry: image.resolve_registry().to_string(),
            });
        }
        Ok(inner
            .referrers
            .get(&referrers_key(image, subject_digest))
            .map(|descriptors| {
                descriptors
                    .iter()
                    .filter(|descriptor| match artifact_type {
                        Some(wanted) => descriptor.artifact_type.as_deref() == Some(wanted),
                        None => true,
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

/// Reads the `artifactType` a referrer manifest declares, mirroring what a
/// registry lifts into its referrers index. Falls back to `None` for bytes that
/// are not a JSON object carrying the field.
fn referrer_artifact_type(manifest_bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(manifest_bytes)
        .ok()?
        .get("artifactType")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Digest;

    fn reference(repository: &str) -> oci::native::Reference {
        format!("registry.test/{repository}:1.0").parse().expect("reference")
    }

    fn subject() -> Digest {
        Digest::Sha256("a".repeat(64))
    }

    fn signature_manifest() -> Vec<u8> {
        br#"{"artifactType":"application/vnd.dev.sigstore.bundle.v0.3+json"}"#.to_vec()
    }

    /// A pushed referrer must be listable afterwards — otherwise every
    /// copy-then-verify test would pass against a stub that dropped the push.
    #[tokio::test]
    async fn pushed_referrer_is_listed_back() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let transport = StubTransport::new(data);
        let image = reference("app");

        let pushed = transport
            .push_referrer_manifest(
                &image,
                &subject(),
                &signature_manifest(),
                "application/vnd.oci.image.manifest.v1+json",
            )
            .await
            .expect("push referrer");

        let listed = transport.list_referrers(&image, &subject(), None).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].digest, pushed.digest);
    }

    /// The `artifact_type` filter must actually discriminate. A stub leaving the
    /// descriptor's `artifact_type` at `None` returns an empty list for every
    /// filtered query, which reads as "this subject has no signatures".
    #[tokio::test]
    async fn artifact_type_filter_selects_and_rejects() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let transport = StubTransport::new(data);
        let image = reference("app");

        transport
            .push_referrer_manifest(
                &image,
                &subject(),
                &signature_manifest(),
                "application/vnd.oci.image.manifest.v1+json",
            )
            .await
            .expect("push referrer");

        let matching = transport
            .list_referrers(
                &image,
                &subject(),
                Some("application/vnd.dev.sigstore.bundle.v0.3+json"),
            )
            .await
            .expect("list");
        assert_eq!(matching.len(), 1, "the declared artifactType must match");

        let other = transport
            .list_referrers(&image, &subject(), Some("application/spdx+json"))
            .await
            .expect("list");
        assert!(other.is_empty(), "a different artifactType must not match");
    }

    /// "No referrers" and "no Referrers API" are different answers, and the
    /// second one must not degrade into the first.
    #[tokio::test]
    async fn unsupported_registry_errors_where_a_supporting_one_answers_empty() {
        let supporting = StubTransport::new(StubTransportData::new());
        assert!(
            supporting
                .list_referrers(&reference("app"), &subject(), None)
                .await
                .expect("supporting registry answers")
                .is_empty()
        );

        let data = StubTransportData::new();
        data.write().referrers_unsupported = true;
        let unsupported = StubTransport::new(data);
        assert!(matches!(
            unsupported.list_referrers(&reference("app"), &subject(), None).await,
            Err(ClientError::ReferrersUnsupported { .. })
        ));
        assert!(matches!(
            unsupported
                .push_referrer_manifest(
                    &reference("app"),
                    &subject(),
                    &signature_manifest(),
                    "application/vnd.oci.image.manifest.v1+json"
                )
                .await,
            Err(ClientError::ReferrersUnsupported { .. })
        ));
    }
}
