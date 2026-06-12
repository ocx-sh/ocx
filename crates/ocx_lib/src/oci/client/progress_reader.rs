// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Async read wrapper that reports cumulative download progress via callback.
//!
//! Replaces [`super::progress_writer::ProgressWriter`] on the download path.
//! `ProgressWriter` is retained for the upload path (`push_package`).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::AsyncRead;
use tokio::io::ReadBuf;

use super::transport::ProgressFn;

/// An [`AsyncRead`] wrapper that calls `on_progress(bytes_read_so_far)` after
/// every successful read.
///
/// Unlike [`super::progress_writer::ProgressWriter`] — which capped writes at
/// 32 KiB to produce frequent callbacks — `ProgressReader` does not impose any
/// size cap. Progress is reported on natural chunk boundaries (typically 8–64 KiB
/// HTTP/2 frames), producing smoother progress without artificial I/O
/// fragmentation.
///
/// The callback is non-blocking and is invoked with the **cumulative** total of
/// bytes read so far, matching the contract of `ProgressWriter`.
pub(super) struct ProgressReader<R> {
    inner: R,
    on_progress: ProgressFn,
    bytes_read: u64,
}

impl<R: AsyncRead + Unpin> ProgressReader<R> {
    /// Creates a new `ProgressReader`.
    ///
    /// - `inner` — the underlying byte source.
    /// - `on_progress` — called with cumulative bytes read after each
    ///   successful read. Use [`super::transport::no_progress`] when progress
    ///   reporting is not needed.
    pub fn new(inner: R, on_progress: ProgressFn) -> Self {
        Self {
            inner,
            on_progress,
            bytes_read: 0,
        }
    }

    /// Unwraps the reader, returning the inner reader.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let filled_before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let n = buf.filled().len() - filled_before;
            if n > 0 {
                self.bytes_read += n as u64;
                // Invoke the progress callback with the cumulative total.
                // The callback is non-blocking — it must not perform any I/O
                // or blocking work; typically it updates an atomic counter or
                // enqueues a message.
                (self.on_progress)(self.bytes_read);
            }
        }
        poll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::AsyncReadExt as _;

    type ProgressLog = std::sync::Arc<Mutex<Vec<u64>>>;

    /// Returns an `on_progress` callback that appends each value to the returned log.
    fn make_progress_log() -> (ProgressFn, ProgressLog) {
        let log: ProgressLog = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log_clone = std::sync::Arc::clone(&log);
        let cb: ProgressFn = std::sync::Arc::new(move |n| {
            log_clone.lock().unwrap().push(n);
        });
        (cb, log)
    }

    // ── cumulative callback values ────────────────────────────────────

    /// spec §ProgressReader invariant: callback called with CUMULATIVE total after each successful read.
    /// Values must be monotonically increasing and last value must equal total bytes read.
    #[tokio::test]
    async fn callbacks_are_cumulative_and_monotonic() {
        let data = b"hello world"; // 11 bytes
        let (cb, log) = make_progress_log();
        let mut reader = ProgressReader::new(&data[..], cb);

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();

        let log = log.lock().unwrap();
        assert!(!log.is_empty(), "at least one callback must have fired");

        // Monotonic: each value >= previous
        for window in log.windows(2) {
            assert!(
                window[1] >= window[0],
                "progress values must be monotonically non-decreasing: {:?}",
                &*log
            );
        }

        // Last value equals total bytes consumed
        assert_eq!(
            *log.last().unwrap(),
            data.len() as u64,
            "last callback value must equal total bytes read"
        );
    }

    // ── natural chunk boundaries ──────────────────────────────────────

    /// spec §D3 QW1: ProgressReader does NOT impose any write-size cap.
    /// Feed >32 KiB in a single chunk and verify the callback fires with
    /// the full chunk size — not artificially capped at 32 KiB.
    #[tokio::test]
    async fn large_chunk_fires_single_callback_at_full_chunk_size() {
        // spec §ProgressReader: "no size cap — calls back on natural chunk boundaries"
        let chunk_size = 64 * 1024; // 64 KiB — larger than old ProgressWriter cap of 32 KiB
        let data = vec![0u8; chunk_size];
        let (cb, log) = make_progress_log();
        let mut reader = ProgressReader::new(&data[..], cb);

        // Use a single large read buffer to allow the inner slice to satisfy the
        // poll_read in one call (natural chunk granularity from slice source).
        let mut buf = vec![0u8; chunk_size * 2];
        let n = reader.read(&mut buf).await.unwrap();

        let log = log.lock().unwrap();
        // The slice source can satisfy the full 64 KiB in one read — callback
        // must report the full chunk, NOT be capped at 32 KiB.
        if n == chunk_size {
            assert_eq!(log.len(), 1, "a single 64 KiB read must produce exactly one callback");
            assert_eq!(
                log[0], chunk_size as u64,
                "callback value must equal the full read size of {chunk_size}"
            );
        } else {
            // If runtime splits it: each callback must still report cumulative total
            for window in log.windows(2) {
                assert!(window[1] > window[0], "must be strictly increasing on partial reads");
            }
            assert_eq!(*log.last().unwrap(), n as u64);
        }
    }

    // ── zero-byte stream edge ─────────────────────────────────────────

    /// spec §ProgressReader: zero-byte stream edge — no callbacks fire on empty input.
    #[tokio::test]
    async fn zero_byte_stream_fires_no_callbacks() {
        let (cb, log) = make_progress_log();
        let mut reader = ProgressReader::new(&[][..], cb);

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();

        let log = log.lock().unwrap();
        assert!(
            log.is_empty(),
            "empty stream must fire zero progress callbacks, got {:?}",
            &*log
        );
    }

    // ── bytes passed through unchanged ────────────────────────────────

    /// ProgressReader must not alter bytes — all data from inner source
    /// must be forwarded to the caller unchanged.
    #[tokio::test]
    async fn bytes_pass_through_unchanged() {
        let data: Vec<u8> = (0u8..=127u8).collect();
        let (cb, _log) = make_progress_log();
        let mut reader = ProgressReader::new(&data[..], cb);

        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();

        assert_eq!(out, data, "ProgressReader must forward all bytes unchanged");
    }
}
