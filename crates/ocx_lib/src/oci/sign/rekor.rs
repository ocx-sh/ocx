// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rekor v1 log client.
//!
//! Thin wrapper over `sigstore::rekor::RekorClient`. Rekor v2 support is
//! deferred pending sigstore-rs v2 client support (see ADR Risks).
//!
//! The wrapper flags SET absence: if Rekor returns an entry with
//! `integrated_time: 0` AND missing `set`, the error path is
//! [`SignErrorKind::RekorSetMalformed`] (v1 signing does not accept v2 entries).
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use super::error::SignErrorKind;

/// Rekor log entry with Signed Entry Timestamp.
pub struct RekorEntry {
    /// Integer log index (unique entry identifier).
    pub log_index: u64,
    /// Wall-clock time the entry was integrated into the log (UTC epoch seconds).
    pub integrated_time: u64,
    /// Stable log identifier (public key hash).
    pub log_id: String,
    /// Signed Entry Timestamp as raw bytes.
    pub signed_entry_timestamp: Vec<u8>,
}

/// Rekor v1 client.
pub struct RekorClient {
    #[allow(dead_code)]
    url: String,
}

impl RekorClient {
    /// Construct a Rekor client targeting the given URL.
    ///
    /// Production: `https://rekor.sigstore.dev`.
    /// Tests: `http://127.0.0.1:<fake_rekor port>` (C-S1-3 seam).
    pub fn new(url: String) -> Self {
        Self { url }
    }

    /// Upload a signature entry and return the integrated log entry.
    pub async fn upload_entry(
        &self,
        _signature: &[u8],
        _cert_leaf_pem: &[u8],
        _payload_digest: &str,
    ) -> Result<RekorEntry, SignErrorKind> {
        unimplemented!("RekorClient::upload_entry — Phase 5 delegates to sigstore-rs Rekor v1")
    }
}
