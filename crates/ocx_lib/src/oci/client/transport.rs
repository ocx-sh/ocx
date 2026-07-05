// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncRead, ReadBuf};

use super::error::ClientError;
use crate::oci;

pub type Result<T> = std::result::Result<T, ClientError>;

/// Progress callback for transfer operations.
pub type ProgressFn = Arc<dyn Fn(u64) + Send + Sync>;

/// Returns a no-op progress callback for callers that don't need progress.
pub fn no_progress() -> ProgressFn {
    Arc::new(|_| {})
}

/// Outcome of a cross-repository blob mount attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountOutcome {
    /// The registry mounted the blob into the target repository; no upload
    /// is needed.
    Mounted,
    /// The registry declined the mount (spec-legal 202 miss, transport error,
    /// or a transport that doesn't implement mounting); the caller must
    /// upload the blob through the normal path.
    UploadRequired,
}

/// Low-level OCI registry transport operations.
///
/// Abstracts the wire-level OCI distribution API calls, enabling the
/// higher-level [`super::Client`] business logic to be tested without
/// hitting a real registry.
///
/// Implementations are expected to handle authentication internally.
/// Every method calls [`ensure_auth`](Self::ensure_auth) with the
/// appropriate operation scope before performing any network I/O, so
/// callers never need to worry about auth ordering.
#[async_trait]
pub trait OciTransport: Send + Sync {
    // ── Authentication ───────────────────────────────────────────────

    /// Pre-authenticate for the given operation scope.
    ///
    /// Ensures credentials are resolved and a token is cached for
    /// `image`'s registry with the requested operation scope (Pull or Push).
    /// Repeated calls for the same scope are no-ops (token cache hit).
    async fn ensure_auth(&self, image: &oci::native::Reference, operation: oci::RegistryOperation) -> Result<()>;

    // ── Read operations ──────────────────────────────────────────────

    /// Lists tags for the given image, returning one page of results.
    async fn list_tags(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>>;

    /// Lists repositories (catalog) for the registry of the given image reference.
    async fn catalog(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>>;

    /// Fetches only the digest of a manifest without pulling the full content.
    async fn fetch_manifest_digest(&self, image: &oci::native::Reference) -> Result<String>;

    /// Pulls raw manifest bytes and returns them with the digest string.
    async fn pull_manifest_raw(
        &self,
        image: &oci::native::Reference,
        accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)>;

    /// Pulls a blob into memory, returning the raw bytes.
    ///
    /// Suitable for small blobs (config, metadata) where writing to disk
    /// and reading back would be wasteful.
    async fn pull_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<Vec<u8>>;

    /// Pulls a blob and writes it to the specified file path.
    async fn pull_blob_to_file(&self, image: &oci::native::Reference, digest: &oci::Digest, path: &Path) -> Result<()>;

    /// HEAD a blob to verify existence and retrieve its content length.
    ///
    /// Returns `Ok(size)` if the blob exists, `Err(ClientError::BlobNotFound)` if not.
    async fn head_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<u64>;

    /// Streams the RAW (compressed) blob bytes from the registry.
    ///
    /// Returns an [`AsyncRead`] over the compressed bytes exactly as served by
    /// the registry. No decompression, hashing, or progress reporting is
    /// performed here — those concerns are assembled by the caller
    /// (`Client::pull_layer`). This keeps the transport boundary wire-level
    /// (SRP: decompression depends on `archive/` and `utility/` which must not
    /// leak into the transport).
    ///
    /// # Default implementation
    ///
    /// The default implementation downloads the blob to a temporary file via
    /// [`Self::pull_blob_to_file`] and then streams the file back. This keeps
    /// [`StubTransport`](super::test_transport::StubTransport) in unit tests
    /// compilable without any changes. The default path has no `VerifyingStream`
    /// — `HashingAsyncReader` in `pull_layer` is the sole verifier there.
    ///
    /// # Errors (from the returned reader)
    ///
    /// - [`ClientError::BlobNotFound`] — blob absent at call time.
    /// - `io::Error` with fork `DigestError` source at stream end when
    ///   `NativeTransport` is used (caller maps to
    ///   [`ClientError::DigestMismatch`]).
    async fn pull_blob_streaming(
        &self,
        image: &oci::native::Reference,
        digest: &oci::Digest,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin + 'static>> {
        // Default: download to a temp file, then open and stream it back.
        // Real implementations (NativeTransport) override this to stream
        // directly from the registry without touching disk.
        let temp_file = tempfile::NamedTempFile::new().map_err(|e| ClientError::Io {
            path: std::path::PathBuf::from("<tempfile>"),
            source: e,
        })?;
        let temp_path = temp_file.path().to_path_buf();
        self.pull_blob_to_file(image, digest, &temp_path).await?;
        let file = tokio::fs::File::open(&temp_path).await.map_err(|e| ClientError::Io {
            path: temp_path.clone(),
            source: e,
        })?;
        // Keep the NamedTempFile alive by leaking it into the reader via a
        // combination of tokio::io::BufReader and a guard struct so the file
        // is not deleted before the reader is done.
        //
        // Simple approach: convert the temp file into a regular file handle
        // that outlives the path reference, then use a wrapper that holds
        // both the `File` and the `NamedTempFile` for cleanup.
        let reader = TempFileReader {
            file,
            _guard: temp_file,
        };
        Ok(Box::new(reader))
    }

    // ── Write operations ─────────────────────────────────────────────

    /// Pushes a typed OCI manifest and returns the resulting digest string.
    async fn push_manifest(&self, image: &oci::native::Reference, manifest: &oci::Manifest) -> Result<String>;

    /// Pushes raw manifest bytes with the given media type string.
    /// Returns the resulting digest string.
    async fn push_manifest_raw(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        media_type: &str,
    ) -> Result<String>;

    /// Pushes in-memory blob data. Returns the resulting digest string.
    ///
    /// The implementation streams the blob to the registry, invoking
    /// `on_progress` with the cumulative byte count as data reaches the wire.
    /// Pass [`no_progress()`] when progress reporting is not needed.
    async fn push_blob(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String>;

    /// Attempts to mount `digest` from `source_repository` into `image`'s
    /// repository, avoiding a redundant upload when the blob is already
    /// present elsewhere in the registry.
    ///
    /// # Default implementation
    ///
    /// Always returns [`MountOutcome::UploadRequired`]. Mounting is a
    /// registry-side optimization, not a correctness requirement — a
    /// transport that doesn't implement it (or a test double) falls back
    /// to the normal upload path unchanged.
    async fn mount_blob(
        &self,
        image: &oci::native::Reference,
        source_repository: &str,
        digest: &oci::Digest,
    ) -> Result<MountOutcome> {
        let _ = (image, source_repository, digest);
        Ok(MountOutcome::UploadRequired)
    }

    // ── Clone support ────────────────────────────────────────────────

    /// Clones the transport into a boxed trait object.
    fn box_clone(&self) -> Box<dyn OciTransport>;
}

// ── Default-impl helpers and tests ───────────────────────────────────────────

/// RAII wrapper that holds a temporary file open for reading while keeping the
/// [`tempfile::NamedTempFile`] guard alive so the underlying path is not
/// deleted until the reader is dropped.
///
/// Used by the default implementation of
/// [`OciTransport::pull_blob_streaming`] to stream an already-downloaded blob
/// back as `AsyncRead`.
struct TempFileReader {
    file: tokio::fs::File,
    /// Keeps the temp file on disk until this reader is dropped.
    _guard: tempfile::NamedTempFile,
}

impl AsyncRead for TempFileReader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;
    use crate::oci::{Algorithm, RegistryOperation};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, RwLock};

    // ── Minimal OciTransport impl for testing default pull_blob_streaming ──

    /// In-memory stub OciTransport that only implements `pull_blob_to_file` using
    /// a simple byte-map. Used to exercise the DEFAULT implementation of
    /// `pull_blob_streaming` without pulling in StubTransport from the test module.
    struct InlineStub {
        blobs: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    }

    impl InlineStub {
        fn new(blobs: HashMap<String, Vec<u8>>) -> Self {
            Self {
                blobs: Arc::new(RwLock::new(blobs)),
            }
        }

        fn box_clone_inner(&self) -> Self {
            Self {
                blobs: Arc::clone(&self.blobs),
            }
        }
    }

    #[async_trait]
    impl OciTransport for InlineStub {
        async fn ensure_auth(&self, _image: &oci::native::Reference, _op: RegistryOperation) -> Result<()> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn catalog(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn fetch_manifest_digest(&self, _image: &oci::native::Reference) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _accepted_media_types: &[&str],
        ) -> Result<(Vec<u8>, String)> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_blob(&self, _image: &oci::native::Reference, _digest: &oci::Digest) -> Result<Vec<u8>> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_blob_to_file(
            &self,
            _image: &oci::native::Reference,
            digest: &oci::Digest,
            path: &Path,
        ) -> Result<()> {
            use super::super::error::ClientError;
            let key = digest.to_string();
            let inner = self.blobs.read().unwrap();
            let bytes = inner.get(&key).cloned().unwrap_or_default();
            drop(inner);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ClientError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
            std::fs::write(path, &bytes).map_err(|e| ClientError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Ok(())
        }

        async fn head_blob(&self, _image: &oci::native::Reference, _digest: &oci::Digest) -> Result<u64> {
            Ok(0)
        }

        async fn push_manifest(&self, _image: &oci::native::Reference, _manifest: &oci::Manifest) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _media_type: &str,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_blob(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _digest: &oci::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.box_clone_inner())
        }
    }

    fn test_reference() -> oci::native::Reference {
        oci::native::Reference::try_from("example.com/test/pkg:1.0").expect("valid reference")
    }

    // ── pull_blob_streaming default impl ─────────────────────────────

    /// spec §OciTransport::pull_blob_streaming default impl:
    /// delegates to pull_blob_to_file into temp file then streams file back.
    /// The returned AsyncRead must yield the same bytes as stored in the blob map.
    #[tokio::test]
    async fn default_pull_blob_streaming_yields_blob_content() {
        let blob_content = b"compressed layer bytes for testing".to_vec();
        let digest = Algorithm::Sha256.hash(&blob_content);

        let mut blobs = HashMap::new();
        blobs.insert(digest.to_string(), blob_content.clone());
        let transport = InlineStub::new(blobs);

        let reference = test_reference();
        let mut stream = transport.pull_blob_streaming(&reference, &digest).await.unwrap();

        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received)
            .await
            .unwrap();

        assert_eq!(
            received, blob_content,
            "default pull_blob_streaming must yield the same bytes as pull_blob_to_file"
        );
    }

    /// spec §OciTransport::pull_blob_streaming default impl:
    /// empty blob returns empty stream (not an error).
    #[tokio::test]
    async fn default_pull_blob_streaming_empty_blob_yields_empty_stream() {
        let blob_content: Vec<u8> = vec![];
        let digest = Algorithm::Sha256.hash(&blob_content);

        let mut blobs = HashMap::new();
        blobs.insert(digest.to_string(), blob_content.clone());
        let transport = InlineStub::new(blobs);

        let reference = test_reference();
        let mut stream = transport.pull_blob_streaming(&reference, &digest).await.unwrap();

        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received)
            .await
            .unwrap();

        assert!(
            received.is_empty(),
            "empty blob must yield empty stream from default impl"
        );
    }

    /// spec §OciTransport::pull_blob_streaming default impl:
    /// default path has NO VerifyingStream — HashingAsyncReader in pull_layer is
    /// the sole verifier. This test confirms the default impl does not itself
    /// verify the digest (it just streams bytes as-is from the temp file).
    /// A corrupted blob served via InlineStub flows through unchanged —
    /// the CALLER (pull_layer + HashingAsyncReader) detects the mismatch.
    #[tokio::test]
    async fn default_pull_blob_streaming_passes_through_bytes_without_verifying() {
        // Store bytes that do NOT match the declared digest.
        // The default impl must stream them as-is (no verification at transport layer).
        let honest_content = b"honest bytes".to_vec();
        let evil_content = b"evil bytes corrupted".to_vec();
        let honest_digest = Algorithm::Sha256.hash(&honest_content);

        // Register evil bytes under the honest digest key.
        let mut blobs = HashMap::new();
        blobs.insert(honest_digest.to_string(), evil_content.clone());
        let transport = InlineStub::new(blobs);

        let reference = test_reference();
        let mut stream = transport.pull_blob_streaming(&reference, &honest_digest).await.unwrap();

        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received)
            .await
            .unwrap();

        // Default impl does NOT verify — bytes flow through unchanged.
        // The mismatch is the caller's responsibility (HashingAsyncReader).
        assert_eq!(
            received, evil_content,
            "default pull_blob_streaming must not verify digest; bytes flow through as-is for caller verification"
        );
    }
}
