// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

/// Number of bytes between periodic debug log messages during transfers.
const LOG_BYTE_INTERVAL: u64 = 32 * 1024 * 1024;

/// An `AsyncWrite` wrapper that reports progress after every write.
///
/// Wraps an inner `AsyncWrite` and calls `on_progress(bytes_written_so_far)`
/// after each successful write. Also emits periodic `tracing::debug!` events
/// every [`LOG_BYTE_INTERVAL`] bytes for non-TTY visibility.
pub(super) struct ProgressWriter<W> {
    inner: Pin<Box<W>>,
    on_progress: Arc<dyn Fn(u64) + Send + Sync>,
    bytes_written: u64,
    total: u64,
    last_logged: u64,
}

impl<W: AsyncWrite> ProgressWriter<W> {
    pub fn new(inner: W, total: u64, on_progress: Arc<dyn Fn(u64) + Send + Sync>) -> Self {
        Self {
            inner: Box::pin(inner),
            on_progress,
            bytes_written: 0,
            total,
            last_logged: 0,
        }
    }
}

impl<W: AsyncWrite> AsyncWrite for ProgressWriter<W> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        match this.inner.as_mut().poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                this.bytes_written += n as u64;
                (this.on_progress)(this.bytes_written);
                if this.total > 0 && this.bytes_written / LOG_BYTE_INTERVAL > this.last_logged / LOG_BYTE_INTERVAL {
                    tracing::debug!(
                        bytes_written = this.bytes_written,
                        total = this.total,
                        "Transferred {} / {} bytes",
                        this.bytes_written,
                        this.total
                    );
                    this.last_logged = this.bytes_written;
                }
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.as_mut().poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::AsyncWriteExt;

    type ProgressValues = Arc<Mutex<Vec<u64>>>;

    /// Collects all progress values reported by the callback.
    fn progress_tracker() -> (Arc<dyn Fn(u64) + Send + Sync>, ProgressValues) {
        let values = Arc::new(Mutex::new(Vec::new()));
        let values_clone = Arc::clone(&values);
        let callback: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
            values_clone.lock().unwrap().push(n);
        });
        (callback, values)
    }

    #[tokio::test]
    async fn write_through_and_cumulative_progress() {
        let buf = Vec::new();
        let (on_progress, values) = progress_tracker();
        let mut writer = ProgressWriter::new(buf, 100, on_progress);

        writer.write_all(b"hello").await.unwrap();
        writer.write_all(b" world").await.unwrap();
        writer.flush().await.unwrap();
        writer.shutdown().await.unwrap();

        // Data reaches the inner writer.
        let inner = Pin::into_inner(writer.inner);
        assert_eq!(*inner, b"hello world");

        // Progress reports cumulative byte counts.
        let vals = values.lock().unwrap();
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0], 5);
        assert_eq!(vals[1], 11);
    }

    #[tokio::test]
    async fn zero_byte_write_reports_zero_progress() {
        let buf = Vec::new();
        let (on_progress, values) = progress_tracker();
        let mut writer = ProgressWriter::new(buf, 0, on_progress);

        let n = writer.write(b"").await.unwrap();
        assert_eq!(n, 0);

        let vals = values.lock().unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0], 0);
    }

    #[tokio::test]
    async fn flush_and_shutdown_delegate() {
        let buf = Vec::new();
        let (on_progress, _) = progress_tracker();
        let mut writer = ProgressWriter::new(buf, 0, on_progress);

        // These should not panic or error — they delegate to Vec's no-op impls.
        writer.flush().await.unwrap();
        writer.shutdown().await.unwrap();
    }
}
