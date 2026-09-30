// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::error::ClientError;
use super::mirror_map::ParsedMirror;
use super::transport::{DeleteOutcome, ManifestPresence, MountOutcome, OciTransport, Result};
use super::{Client, MirrorMap};
use crate::{Algorithm, RegistryOperation};

/// Test data backing a [`StubTransport`].
#[derive(Default)]
pub struct StubTransportInner {
    /// Pages of tags returned by successive `list_tags` calls (consumed FIFO).
    pub tags: Vec<Vec<String>>,
    /// Pages of repositories returned by successive `catalog` calls (consumed FIFO).
    pub repositories: Vec<Vec<String>>,
    /// Image string → (raw manifest bytes, digest string).
    pub manifests: HashMap<String, (Vec<u8>, String)>,
    /// Image string → artificial latency before `pull_manifest_raw` answers, so completion order can differ
    /// from submission order.
    pub manifest_delays: HashMap<String, std::time::Duration>,
    /// Digest string → blob bytes; which repository holds one is [`blob_locations`](Self::blob_locations).
    pub blobs: HashMap<String, Vec<u8>>,
    /// `"<registry>/<repository>"` → the digests it holds, for `head_blob`; `None` answers from `blobs` alone.
    ///
    /// An `Option`, not an empty map, or a copy test that forgot to seed it passes for the wrong reason.
    pub blob_locations: Option<HashMap<String, std::collections::BTreeSet<String>>>,

    /// Digest string → chunks `pull_blob_streaming` yields one per read, instead of the whole blob.
    pub blob_stream_chunks: HashMap<String, Vec<Vec<u8>>>,
    /// Artificial latency before `list_tags` / `fetch_manifest_digest` answer, so concurrent reads overlap.
    ///
    /// Under `tokio::time::pause()` it releases only once every caller has parked.
    pub tag_read_delay: Option<std::time::Duration>,
    /// Digest returned by `fetch_manifest_digest`.
    pub digest: Option<String>,
    /// Successive results for push operations (consumed FIFO).
    pub push_results: Vec<Result<String>>,
    /// Successive `list_tags` results (consumed FIFO); an empty queue falls through to [`tags`](Self::tags).
    pub list_tags_results: Vec<Result<Vec<String>>>,
    /// Log of method calls for assertions.
    pub calls: Vec<String>,
    /// Log of `ensure_auth` calls: `(registry, operation)`.
    pub auth_calls: Vec<(String, RegistryOperation)>,
    /// The first transport method invoked, `ensure_auth` included.
    ///
    /// An authenticates-first assertion must read this: `calls` and `auth_calls` cannot witness their relative order.
    pub first_call: Option<String>,
    /// `(method, registry, repository)` each manifest/blob read was handed, from the `pull_*` methods that
    /// call `record_target`.
    pub read_targets: Vec<(&'static str, String, String)>,
    /// When true, `push_manifest_raw` stores pushed data back into `manifests`
    /// so subsequent reads see the updated content.
    pub capture_pushes: bool,
    /// When set, `pull_manifest_raw` returns a `Registry` error with this
    /// message for any image not in `manifests` (instead of `ManifestNotFound`).
    pub pull_manifest_error_override: Option<String>,
    /// Image string → registry error for that image's manifest fetch, winning over a seeded manifest.
    pub manifest_errors: HashMap<String, String>,
    /// When set, `ensure_auth` fails with `ClientError::Authentication` carrying this message.
    pub ensure_auth_error_override: Option<String>,
    /// Successive results for `mount_blob` calls (consumed FIFO); an empty
    /// queue falls through to the trait's default `Ok(UploadRequired)`.
    pub mount_results: Vec<Result<MountOutcome>>,
    /// Log of `mount_blob` calls: `(target_repository, source_repository, digest)`.
    pub mount_calls: Vec<(String, String, String)>,
    /// Successive results for `delete_manifest` calls (consumed FIFO); an empty queue is an error.
    pub delete_results: Vec<Result<DeleteOutcome>>,
    /// Log of `delete_manifest` calls: the whole reference, registry included.
    pub delete_calls: Vec<String>,
    /// Successive results for `probe_manifest` calls (consumed FIFO); an empty queue is an error.
    pub probe_results: Vec<Result<ManifestPresence>>,
    /// Log of `probe_manifest` calls: the whole reference, registry included.
    pub probe_calls: Vec<String>,
    /// `"<repository>@<subject digest>"` → referrer descriptors; grown by `push_referrer_manifest` under
    /// `capture_pushes`.
    pub referrers: HashMap<String, Vec<crate::Descriptor>>,
    /// When true, both referrer methods fail with [`ClientError::ReferrersUnsupported`], unlike an empty
    /// `referrers` map.
    pub referrers_unsupported: bool,
    /// Count of captured manifest PUTs to a tag; `manifests` cannot show a tag written twice.
    pub tag_manifest_writes: usize,
    /// Count of captured manifest PUTs to a digest (leaf uploads).
    pub digest_manifest_writes: usize,
}

/// Keys [`StubTransportInner::blob_locations`] by registry and repository, or a cross-host copy of `team/demo` sees
/// the target holding every source blob.
pub fn blob_location_key(image: &crate::native::Reference) -> String {
    format!("{}/{}", image.resolve_registry(), image.repository())
}

/// Keys [`StubTransportInner::referrers`] by repository and subject, never by the caller's tag.
pub fn referrers_key(image: &crate::native::Reference, subject_digest: &crate::Digest) -> String {
    format!("{}@{}", image.repository(), subject_digest)
}

/// Shared, cheaply cloned data handle for [`StubTransport`].
///
/// ```ignore
/// let data = StubTransportData::new();
/// data.write().tags = vec![vec!["1.0".into()]];
/// let client = Client::with_transport(Box::new(StubTransport::new(data.clone())));
/// // later: data.read().calls  — inspect recorded calls
/// // later: data.write().digest = Some("sha256:...".into())  — modify on the fly
/// ```
#[derive(Clone)]
pub struct StubTransportData {
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

/// A [`Client`] speaking to `data`, with no mirror configured.
pub fn stub_client(data: &StubTransportData) -> Client {
    Client::with_transport(Box::new(StubTransport::new(data.clone())))
}

/// [`stub_client`] with `upstream` mirrored to `mirror_host` under `prefix`, for callers outside the crate.
pub fn mirrored_stub_client(data: &StubTransportData, upstream: &str, mirror_host: &str, prefix: &str) -> Client {
    let mut client = stub_client(data);
    client.mirrors = MirrorMap::new([(
        upstream.to_string(),
        ParsedMirror {
            protocol: "https".to_string(),
            host: mirror_host.to_string(),
            path_prefix: prefix.to_string(),
        },
    )]);
    client
}

/// A configurable test double for [`OciTransport`]; clones share one [`StubTransportData`].
#[derive(Clone)]
pub struct StubTransport {
    data: StubTransportData,
}

impl StubTransport {
    pub fn new(data: StubTransportData) -> Self {
        Self { data }
    }

    fn record(&self, call: &str) {
        let mut inner = self.data.write();
        if inner.first_call.is_none() {
            inner.first_call = Some(call.to_string());
        }
        inner.calls.push(call.to_string());
    }

    /// Record which host and repository `method` was handed.
    fn record_target(&self, method: &'static str, image: &crate::native::Reference) {
        self.data.write().read_targets.push((
            method,
            image.resolve_registry().to_string(),
            image.repository().to_string(),
        ));
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

impl crate::sealed::Sealed for StubTransport {}

#[async_trait]
impl OciTransport for StubTransport {
    async fn ensure_auth(&self, image: &crate::native::Reference, operation: crate::RegistryOperation) -> Result<()> {
        {
            let mut inner = self.data.write();
            if inner.first_call.is_none() {
                inner.first_call = Some("ensure_auth".to_string());
            }
            inner.auth_calls.push((image.resolve_registry().to_string(), operation));
        }
        let override_message = self.data.read().ensure_auth_error_override.clone();
        if let Some(message) = override_message {
            return Err(ClientError::Authentication(Box::new(std::io::Error::other(message))));
        }
        Ok(())
    }

    async fn list_tags(
        &self,
        _image: &crate::native::Reference,
        _chunk_size: usize,
        _last: Option<String>,
    ) -> Result<Vec<String>> {
        self.record("list_tags");
        // Bound first: the read guard must not span the await.
        let delay = self.data.read().tag_read_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut inner = self.data.write();
        if !inner.list_tags_results.is_empty() {
            return inner.list_tags_results.remove(0);
        }
        if inner.tags.is_empty() {
            Ok(vec![])
        } else {
            Ok(inner.tags.remove(0))
        }
    }

    async fn catalog(
        &self,
        _image: &crate::native::Reference,
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

    async fn fetch_manifest_digest(&self, image: &crate::native::Reference) -> Result<String> {
        self.record("fetch_manifest_digest");
        let delay = self.data.read().tag_read_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let key = image.to_string();
        let inner = self.data.read();
        if let Some(message) = inner.manifest_errors.get(&key) {
            return Err(ClientError::Registry(message.clone().into()));
        }
        // An explicit `digest` override wins; otherwise a real registry's HEAD answer.
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
        image: &crate::native::Reference,
        _accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)> {
        self.record("pull_manifest_raw");
        self.record_target("pull_manifest_raw", image);
        let key = image.to_string();
        // Lock released before the sleep, or concurrent callers serialise here.
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

    async fn head_blob(&self, image: &crate::native::Reference, digest: &crate::Digest) -> Result<u64> {
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
            (true, None) => Ok(0),
            (false, _) => Err(ClientError::blob_not_found(image, digest)),
        }
    }

    async fn pull_blob(&self, image: &crate::native::Reference, digest: &crate::Digest) -> Result<Vec<u8>> {
        let digest_key = digest.to_string();
        self.record(&format!("pull_blob:{}", digest_key));
        self.record_target("pull_blob", image);
        let inner = self.data.read();
        Ok(inner.blobs.get(&digest_key).cloned().unwrap_or_default())
    }

    async fn pull_blob_to_file(
        &self,
        image: &crate::native::Reference,
        digest: &crate::Digest,
        path: &std::path::Path,
    ) -> Result<()> {
        let digest_key = digest.to_string();
        self.record(&format!("pull_blob_to_file:{}", digest_key));
        self.record_target("pull_blob_to_file", image);
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
        image: &crate::native::Reference,
        digest: &crate::Digest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
        let digest_key = digest.to_string();
        self.record(&format!("pull_blob_streaming:{digest_key}"));
        self.record_target("pull_blob_streaming", image);
        // Overridden (not the temp-file default) so read boundaries are controllable.
        let chunks = {
            let inner = self.data.read();
            inner
                .blob_stream_chunks
                .get(&digest_key)
                .cloned()
                .unwrap_or_else(|| vec![inner.blobs.get(&digest_key).cloned().unwrap_or_default()])
        };
        // `StreamReader` yields one item per `poll_read`, so each chunk is exactly one read.
        let stream = futures::stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(chunk))),
        );
        Ok(Box::new(tokio_util::io::StreamReader::new(stream)))
    }

    async fn push_manifest(&self, _image: &crate::native::Reference, _manifest: &crate::Manifest) -> Result<String> {
        self.record("push_manifest");
        self.next_push_result()
    }

    async fn push_manifest_raw(
        &self,
        image: &crate::native::Reference,
        data: Vec<u8>,
        _media_type: &str,
    ) -> Result<String> {
        self.record("push_manifest_raw");
        let digest = Algorithm::Sha256.hash(&data).to_string();
        // Outcome before recording, or a failed push lands in `manifests` like a successful one.
        let outcome = {
            let mut inner = self.data.write();
            if inner.push_results.is_empty() {
                Ok(digest.clone())
            } else {
                inner.push_results.remove(0)
            }
        };
        if outcome.is_ok() && self.data.read().capture_pushes {
            let key = image.to_string();
            let mut inner = self.data.write();
            if key.contains('@') {
                inner.digest_manifest_writes += 1;
            } else {
                inner.tag_manifest_writes += 1;
            }
            inner.manifests.insert(key, (data, digest));
        }
        outcome
    }

    async fn push_blob(
        &self,
        image: &crate::native::Reference,
        data: Vec<u8>,
        digest: &crate::Digest,
        on_progress: super::transport::ProgressFn,
    ) -> Result<String> {
        self.record(&format!("push_blob:{}", digest));
        on_progress(data.len() as u64);
        // Recorded as present, or an idempotency test never observes the skipped re-upload.
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

    /// Buffers, since a stub's blobs are already in memory.
    async fn push_blob_from_path(
        &self,
        image: &crate::native::Reference,
        path: &std::path::Path,
        digest: &crate::Digest,
        on_progress: super::transport::ProgressFn,
    ) -> Result<String> {
        super::transport::push_blob_buffered(self, image, path, digest, on_progress).await
    }

    async fn mount_blob(
        &self,
        image: &crate::native::Reference,
        source_repository: &str,
        digest: &crate::Digest,
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

    async fn delete_manifest(&self, image: &crate::native::Reference) -> Result<DeleteOutcome> {
        self.record("delete_manifest");
        let mut inner = self.data.write();
        inner.delete_calls.push(image.to_string());
        if inner.delete_results.is_empty() {
            return Err(unseeded("delete_manifest"));
        }
        inner.delete_results.remove(0)
    }

    async fn probe_manifest(&self, image: &crate::native::Reference) -> Result<ManifestPresence> {
        self.record("probe_manifest");
        let mut inner = self.data.write();
        inner.probe_calls.push(image.to_string());
        if inner.probe_results.is_empty() {
            return Err(unseeded("probe_manifest"));
        }
        inner.probe_results.remove(0)
    }

    async fn push_referrer_manifest(
        &self,
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
        manifest_bytes: &[u8],
        media_type: &str,
    ) -> Result<crate::Descriptor> {
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
        let descriptor = crate::Descriptor {
            media_type: media_type.to_string(),
            digest: digest.clone(),
            size,
            urls: None,
            // Lifted like a real registry does, or every filtered `list_referrers` comes back empty.
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
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<crate::Descriptor>> {
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

/// The `artifactType` a referrer manifest declares, or `None` for bytes not carrying it.
fn referrer_artifact_type(manifest_bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(manifest_bytes)
        .ok()?
        .get("artifactType")?
        .as_str()
        .map(str::to_string)
}

/// An unseeded queue fails the call, so a test that forgot to seed cannot pass on a default answer.
fn unseeded(method: &str) -> ClientError {
    ClientError::Internal(format!("StubTransport: no seeded result for {method}").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Digest;

    fn reference(repository: &str) -> crate::native::Reference {
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
