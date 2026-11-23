// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Async read wrapper copying every byte it yields into a [`BlobWriter`], so a network pull leaves a cache copy.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use crate::blob_lookup::BlobWriter;

/// The writer a [`TeeReader`] feeds, shared so the pipeline can take it back once the reader is buried in decoders.
#[derive(Clone)]
pub(super) struct TeeSlot(Arc<Mutex<Option<BlobWriter>>>);

impl TeeSlot {
    pub(super) fn new(writer: Option<BlobWriter>) -> Self {
        Self(Arc::new(Mutex::new(writer)))
    }

    /// The writer if it is still healthy; `None` once a write failed or when there never was one.
    pub(super) fn take(&self) -> Option<BlobWriter> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// An [`AsyncRead`] passing bytes through unchanged while writing them to the slot's writer.
///
/// The write is a blocking `std::fs` call inside `poll_read`; the layer pipeline polls the whole reader chain from a
/// blocking thread, so it never stalls the runtime. A failed write drops the writer (deleting its temp file) after one
/// warning, and the read carries on: a cache copy is never worth a failed pull.
pub(super) struct TeeReader<R> {
    inner: R,
    slot: TeeSlot,
}

impl<R: AsyncRead + Unpin> TeeReader<R> {
    pub(super) fn new(inner: R, slot: TeeSlot) -> Self {
        Self { inner, slot }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for TeeReader<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let filled_before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let fresh = &buf.filled()[filled_before..];
            if !fresh.is_empty() {
                let mut guard = self.slot.0.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(writer) = guard.as_mut()
                    && let Err(error) = writer.write(fresh)
                {
                    log::warn!("blob cache copy of {} abandoned: {error}", writer.digest());
                    *guard = None;
                }
            }
        }
        poll
    }
}
