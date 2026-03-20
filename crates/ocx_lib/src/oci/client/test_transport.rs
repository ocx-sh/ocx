// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::error::ClientError;
use super::transport::{OciTransport, Result};
use crate::oci::{self, Digest, RegistryOperation};

/// Test data backing a [`StubTransport`].
///
/// Fields are public so they can be accessed through the lock guards
/// returned by [`StubTransportData::read`] and [`StubTransportData::write`].
pub(crate) struct StubTransportInner {
    /// Pages of tags returned by successive `list_tags` calls (consumed FIFO).
    pub tags: Vec<Vec<String>>,
    /// Pages of repositories returned by successive `catalog` calls (consumed FIFO).
    pub repositories: Vec<Vec<String>>,
    /// Image string → (raw manifest bytes, digest string).
    pub manifests: HashMap<String, (Vec<u8>, String)>,
    /// Digest string → blob bytes (written to file by `pull_blob_to_file`).
    pub blobs: HashMap<String, Vec<u8>>,
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
}

impl Default for StubTransportInner {
    fn default() -> Self {
        Self {
            tags: vec![],
            repositories: vec![],
            manifests: HashMap::new(),
            blobs: HashMap::new(),
            digest: None,
            push_results: vec![],
            calls: vec![],
            auth_calls: vec![],
            capture_pushes: false,
            pull_manifest_error_override: None,
        }
    }
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
        Ok(())
    }

    async fn list_tags(
        &self,
        _image: &oci::native::Reference,
        _chunk_size: usize,
        _last: Option<String>,
    ) -> Result<Vec<String>> {
        self.record("list_tags");
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

    async fn fetch_manifest_digest(&self, _image: &oci::native::Reference) -> Result<String> {
        self.record("fetch_manifest_digest");
        self.data
            .read()
            .digest
            .clone()
            .ok_or_else(|| ClientError::Registry("no digest configured".into()))
    }

    async fn pull_manifest_raw(
        &self,
        image: &oci::native::Reference,
        _accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)> {
        self.record("pull_manifest_raw");
        let key = image.to_string();
        let inner = self.data.read();
        if let Some(manifest) = inner.manifests.get(&key).cloned() {
            Ok(manifest)
        } else if let Some(msg) = &inner.pull_manifest_error_override {
            Err(ClientError::Registry(msg.clone()))
        } else {
            Err(ClientError::ManifestNotFound(key))
        }
    }

    async fn pull_blob_to_file(
        &self,
        _image: &oci::native::Reference,
        digest: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        self.record(&format!("pull_blob_to_file:{}", digest));
        let inner = self.data.read();
        if let Some(blob) = inner.blobs.get(digest) {
            let blob = blob.clone();
            drop(inner); // release lock before I/O
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ClientError::Io(parent.to_path_buf(), e))?;
            }
            std::fs::write(path, &blob).map_err(|e| ClientError::Io(path.to_path_buf(), e))?;
        }
        Ok(())
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
        let digest = Digest::sha256(&data).to_string();
        if self.data.read().capture_pushes {
            self.data
                .write()
                .manifests
                .insert(image.to_string(), (data, digest.clone()));
        }
        let mut inner = self.data.write();
        if inner.push_results.is_empty() {
            Ok(digest)
        } else {
            inner.push_results.remove(0)
        }
    }

    async fn push_blob(&self, _image: &oci::native::Reference, _data: Vec<u8>, digest: &str) -> Result<String> {
        self.record(&format!("push_blob:{}", digest));
        self.next_push_result()
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}
