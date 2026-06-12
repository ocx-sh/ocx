// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Async read wrapper that computes a running digest over all bytes read,
//! dispatching over the algorithm declared by the OCI descriptor (sha256 /
//! sha384 / sha512).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use sha2::Digest as _;
use tokio::io::AsyncRead;
use tokio::io::ReadBuf;

use crate::oci;

/// Tracks the running state for one of the three supported OCI hash algorithms.
///
/// Each variant wraps the corresponding `sha2` hasher type.  The enum exists
/// so [`HashingAsyncReader`] can be generic over the algorithm without requiring
/// a type parameter at every call site — the algorithm is chosen at runtime from
/// the layer descriptor.
enum DigestState {
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl DigestState {
    fn new(algorithm: oci::Algorithm) -> Self {
        match algorithm {
            oci::Algorithm::Sha256 => Self::Sha256(sha2::Sha256::new()),
            oci::Algorithm::Sha384 => Self::Sha384(sha2::Sha384::new()),
            oci::Algorithm::Sha512 => Self::Sha512(sha2::Sha512::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(h) => sha2::Digest::update(h, bytes),
            Self::Sha384(h) => sha2::Digest::update(h, bytes),
            Self::Sha512(h) => sha2::Digest::update(h, bytes),
        }
    }

    fn finalize(self) -> oci::Digest {
        match self {
            Self::Sha256(h) => oci::Digest::Sha256(hex::encode(h.finalize())),
            Self::Sha384(h) => oci::Digest::Sha384(hex::encode(h.finalize())),
            Self::Sha512(h) => oci::Digest::Sha512(hex::encode(h.finalize())),
        }
    }
}

/// An [`AsyncRead`] wrapper that computes a running digest over all bytes
/// successfully read, using the algorithm declared by the OCI layer descriptor.
///
/// The digester is updated only on successful reads (i.e. bytes that
/// `poll_read` placed into the caller-supplied buffer). I/O errors do not
/// update the digest; a partial read that returns an error means the erroneous
/// bytes are not included.
///
/// Call [`finalize`](Self::finalize) after the stream ends to obtain the
/// digest and the total byte count. The returned digest is in the same
/// algorithm variant that was passed to [`new`](Self::new).
///
/// # Algorithm dispatch
///
/// Pass the [`oci::Algorithm`] extracted from the layer descriptor's digest.
/// The internal `DigestState` enum dispatches over `sha2::Sha256`,
/// `sha2::Sha384`, and `sha2::Sha512` — the same set supported by
/// [`oci::Algorithm::hash_file`]. All three OCI-spec digest algorithms are
/// verified correctly; hardcoding SHA-256 would be a CWE-345 regression for
/// sha384 and sha512 descriptors.
///
/// # Ordering note (`pull_layer` governs)
///
/// `pull_layer` calls `finalize()` even when extraction fails (e.g. invalid
/// gzip header), then compares the partial-read digest against the expected
/// one before propagating the extraction error. This is deliberate: wrong bytes
/// from a misbehaving registry cause extraction to fail with a format error;
/// reporting `DigestMismatch` first correctly attributes the failure to the
/// registry (CWE-345), not a local archive problem.
///
/// # Example
///
/// ```rust,ignore
/// let algorithm = layer_digest.algorithm();
/// let reader = HashingAsyncReader::new(inner_stream, algorithm);
/// tokio::io::copy(&mut reader, &mut sink).await?;
/// let (digest, byte_count) = reader.finalize();
/// assert_eq!(digest, expected_digest);
/// ```
pub(super) struct HashingAsyncReader<R> {
    inner: R,
    state: DigestState,
    bytes_read: u64,
}

impl<R: AsyncRead + Unpin> HashingAsyncReader<R> {
    /// Wraps `inner` with a hashing layer using `algorithm`.
    ///
    /// `algorithm` must match the algorithm of the OCI descriptor's digest so
    /// that [`finalize`](Self::finalize) returns a digest in the same variant
    /// as the expected digest for comparison.
    pub fn new(inner: R, algorithm: oci::Algorithm) -> Self {
        Self {
            inner,
            state: DigestState::new(algorithm),
            bytes_read: 0,
        }
    }

    /// Finalises the digest computation and returns `(digest, bytes_read)`.
    ///
    /// The returned [`oci::Digest`] is in the same algorithm variant passed
    /// to [`new`](Self::new). `bytes_read` equals the total number of bytes
    /// successfully delivered to callers through this reader.
    ///
    /// Consumes `self`; after this point no further reads are possible.
    #[must_use]
    pub fn finalize(self) -> (oci::Digest, u64) {
        (self.state.finalize(), self.bytes_read)
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for HashingAsyncReader<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let filled_before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let filled_after = buf.filled().len();
            let n = filled_after - filled_before;
            if n > 0 {
                // Only update the digest for bytes actually placed into the
                // buffer on a successful read. I/O errors do not update the
                // digest; partially-filled reads that subsequently error leave
                // the digest covering only the bytes that were successfully
                // delivered.
                self.state.update(&buf.filled()[filled_before..filled_after]);
                self.bytes_read += n as u64;
            }
        }
        poll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    // ── known-vector digest (sha256) ──────────────────────────────────────

    /// spec §HashingAsyncReader contract: finalize returns digest of bytes read,
    /// in the algorithm passed at construction.
    #[tokio::test]
    async fn known_vector_hello_world_matches_sha256() {
        let input = b"hello world";
        let expected = crate::oci::Algorithm::Sha256.hash(input.as_ref());

        let mut reader = HashingAsyncReader::new(&input[..], crate::oci::Algorithm::Sha256);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buf)
            .await
            .unwrap();
        let (digest, byte_count) = reader.finalize();

        assert_eq!(
            digest, expected,
            "digest must match sha2 known-vector for b\"hello world\""
        );
        assert_eq!(byte_count, input.len() as u64, "byte count must equal input length");
    }

    // ── algorithm dispatch: sha512 ────────────────────────────────────────

    /// Algorithm dispatch: sha512 known-vector matches.
    /// Regression for CWE-345: hardcoding sha256 would always produce
    /// DigestMismatch for sha512 descriptors.
    #[tokio::test]
    async fn sha512_known_vector_matches() {
        let input = b"hello world sha512";
        let expected = crate::oci::Algorithm::Sha512.hash(input.as_ref());

        let mut reader = HashingAsyncReader::new(&input[..], crate::oci::Algorithm::Sha512);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buf)
            .await
            .unwrap();
        let (digest, byte_count) = reader.finalize();

        assert_eq!(digest, expected, "sha512 digest must match known vector");
        assert_eq!(byte_count, input.len() as u64);
        // The returned digest must be in the Sha512 variant, not Sha256.
        assert!(
            matches!(digest, crate::oci::Digest::Sha512(_)),
            "finalize() must return digest in the algorithm passed at construction"
        );
    }

    /// Algorithm dispatch: sha384 known-vector matches.
    #[tokio::test]
    async fn sha384_known_vector_matches() {
        let input = b"hello world sha384";
        let expected = crate::oci::Algorithm::Sha384.hash(input.as_ref());

        let mut reader = HashingAsyncReader::new(&input[..], crate::oci::Algorithm::Sha384);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buf)
            .await
            .unwrap();
        let (digest, byte_count) = reader.finalize();

        assert_eq!(digest, expected, "sha384 digest must match known vector");
        assert_eq!(byte_count, input.len() as u64);
        assert!(
            matches!(digest, crate::oci::Digest::Sha384(_)),
            "finalize() must return digest in the algorithm passed at construction"
        );
    }

    // ── empty stream ─────────────────────────────────────────────────

    /// spec §Edge case 3 + §HashingAsyncReader contract: empty stream → SHA-256 of zero bytes
    /// SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    #[tokio::test]
    async fn empty_stream_produces_sha256_of_zero_bytes() {
        // This is the NIST SHA-256 known value for the empty message.
        const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb924\
                                    27ae41e4649b934ca495991b7852b855";

        let mut reader = HashingAsyncReader::new(&[][..], crate::oci::Algorithm::Sha256);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buf)
            .await
            .unwrap();
        let (digest, byte_count) = reader.finalize();

        assert_eq!(
            digest,
            crate::oci::Digest::Sha256(EMPTY_SHA256.to_string()),
            "empty stream must produce the NIST SHA-256 of the empty message"
        );
        assert_eq!(byte_count, 0, "byte count for empty stream must be zero");
    }

    // ── error propagation ─────────────────────────────────────────────

    /// A mock `AsyncRead` that returns `n_ok` bytes then errors.
    struct FailAfter {
        data: Vec<u8>,
        pos: usize,
        fail_after: usize,
    }

    impl FailAfter {
        fn new(data: Vec<u8>, fail_after: usize) -> Self {
            Self {
                data,
                pos: 0,
                fail_after,
            }
        }
    }

    impl AsyncRead for FailAfter {
        fn poll_read(mut self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            if self.pos >= self.fail_after {
                // Simulate mid-stream network error after fail_after bytes.
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "simulated mid-stream error",
                )));
            }
            let remaining = self.fail_after.min(self.data.len()) - self.pos;
            let to_read = remaining.min(buf.remaining());
            if to_read == 0 {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "simulated mid-stream error",
                )));
            }
            buf.put_slice(&self.data[self.pos..self.pos + to_read]);
            self.pos += to_read;
            Poll::Ready(Ok(()))
        }
    }

    impl Unpin for FailAfter {}

    /// spec §HashingAsyncReader contract: digest computed ONLY over successful reads.
    /// If the inner reader errors after N bytes, `finalize()` returns the digest
    /// of those first N bytes — NOT a digest of the full (unseen) data.
    #[tokio::test]
    async fn error_mid_stream_digest_covers_only_successful_reads() {
        // spec §HashingAsyncReader "error behavior": bytes from errored reads never fed to digester
        let data = b"abcdefghij".to_vec(); // 10 bytes total
        let fail_after = 5; // first 5 succeed, then io::Error
        let expected_partial = crate::oci::Algorithm::Sha256.hash(b"abcde");

        let inner = FailAfter::new(data.clone(), fail_after);
        let mut reader = HashingAsyncReader::new(inner, crate::oci::Algorithm::Sha256);

        let mut buf = [0u8; 16];
        // First read: gets up to 5 bytes (whatever poll_read returns)
        let _ = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;
        // Second read: expect error
        let result = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;
        assert!(result.is_err(), "inner reader error must propagate");

        let (digest, byte_count) = reader.finalize();

        assert_eq!(
            digest, expected_partial,
            "digest must only cover the successfully-read bytes before the error"
        );
        assert_eq!(
            byte_count, fail_after as u64,
            "byte_count must equal successfully-read byte count"
        );
    }

    // ── multi-chunk accumulation ──────────────────────────────────────

    /// spec §HashingAsyncReader contract: multi-chunk reads accumulate correctly.
    /// Feed data with a tiny buffer (1 byte per read) so poll_read is called many times.
    #[tokio::test]
    async fn multi_chunk_reads_produce_correct_cumulative_digest() {
        // spec §HashingAsyncReader "error behavior" + cumulative contract:
        // multiple small poll_read calls accumulate digest identically to one large read.
        let data: Vec<u8> = (0u8..=255u8).collect(); // 256 bytes
        let expected = crate::oci::Algorithm::Sha256.hash(&data);

        let mut reader = HashingAsyncReader::new(&data[..], crate::oci::Algorithm::Sha256);
        // Force many small reads via a 1-byte buffer passed to read() repeatedly.
        let mut collected = Vec::new();
        let mut tiny_buf = [0u8; 1];
        loop {
            match tokio::io::AsyncReadExt::read(&mut reader, &mut tiny_buf).await.unwrap() {
                0 => break,
                n => collected.extend_from_slice(&tiny_buf[..n]),
            }
        }
        let (digest, byte_count) = reader.finalize();

        assert_eq!(collected, data, "all bytes must be passed through");
        assert_eq!(
            digest, expected,
            "digest over 256 one-byte chunks must equal digest of the whole buffer"
        );
        assert_eq!(byte_count, data.len() as u64);
    }

    // ── finalize returns (digest, byte_count) tuple ───────────────────

    /// spec §HashingAsyncReader contract: finalize() returns (oci::Digest, u64) tuple
    #[tokio::test]
    async fn finalize_returns_digest_and_byte_count_tuple() {
        let input = b"specification test";
        let expected_digest = crate::oci::Algorithm::Sha256.hash(input.as_ref());

        let mut reader = HashingAsyncReader::new(&input[..], crate::oci::Algorithm::Sha256);
        let mut sink = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut sink)
            .await
            .unwrap();

        let (digest, byte_count): (crate::oci::Digest, u64) = reader.finalize();
        assert_eq!(digest, expected_digest);
        assert_eq!(byte_count, input.len() as u64);
    }
}
