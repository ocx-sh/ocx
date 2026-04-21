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
//! Phase 1 stub — bodies use `unimplemented!()`. Dead-code lint suppressed
//! because this module is private; Phase 5c wires the call sites.
#![allow(dead_code)]

use super::error::SignErrorKind;

/// Rekor log entry with Signed Entry Timestamp.
///
/// Used by [`super::bundle::BundleBuilder`] in Phase 5c.
pub(super) struct RekorEntry {
    pub(super) log_index: u64,
    pub(super) integrated_time: u64,
    pub(super) log_id: String,
    pub(super) signed_entry_timestamp: Vec<u8>,
}

/// Rekor v1 client — Phase 5c implements the log upload.
struct RekorClient {
    /// C-S1-3 injection seam: `https://rekor.sigstore.dev` in production;
    /// `http://127.0.0.1:<port>` in tests.
    url: String,
}

impl RekorClient {
    fn new(url: String) -> Self {
        Self { url }
    }

    async fn upload_entry(
        &self,
        _signature: &[u8],
        _cert_leaf_pem: &[u8],
        _payload_digest: &str,
    ) -> Result<RekorEntry, SignErrorKind> {
        unimplemented!("RekorClient::upload_entry — Phase 5 delegates to sigstore-rs Rekor v1")
    }
}
