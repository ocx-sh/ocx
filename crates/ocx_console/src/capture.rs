// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only sink for everything the [`Printer`](crate::Printer) writes.
//!
//! The in-process CLI seam runs a command inside the test process, where
//! `std::io::stdout()` is the test harness's own stream. While a capture is
//! [`begin`]-ed, every [`Line`](crate::Line) lands here instead, split by the
//! stream it was bound for, and [`end`] hands both back. Process-global like
//! the streams it stands in for; the caller serialises (the seam holds
//! `ocx_util`'s `EnvLock` for the whole run).

use std::sync::{Mutex, MutexGuard, PoisonError};

/// The stream a captured write was bound for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// `(stdout, stderr)` bytes, present only while a capture is active.
static SINK: Mutex<Option<(Vec<u8>, Vec<u8>)>> = Mutex::new(None);

fn sink() -> MutexGuard<'static, Option<(Vec<u8>, Vec<u8>)>> {
    SINK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts capturing, discarding anything a previous capture left behind.
pub fn begin() {
    *sink() = Some((Vec::new(), Vec::new()));
}

/// Stops capturing and returns `(stdout, stderr)`; empty when none was active.
pub fn end() -> (Vec<u8>, Vec<u8>) {
    sink().take().unwrap_or_default()
}

/// Appends `bytes` to `stream`'s buffer; `false` when no capture is active,
/// so the caller writes to the real stream instead.
pub fn write(stream: Stream, bytes: &[u8]) -> bool {
    let mut sink = sink();
    let Some((stdout, stderr)) = sink.as_mut() else {
        return false;
    };
    match stream {
        Stream::Stdout => stdout.extend_from_slice(bytes),
        Stream::Stderr => stderr.extend_from_slice(bytes),
    }
    true
}
