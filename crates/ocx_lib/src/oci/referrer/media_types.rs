// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Const table of OCI media types and artifact types used by the referrers
//! subsystem.
//!
//! Values are data-only (see plan Step 1.1) — do not add logic. The empty
//! config digest and size are the SHA-256 and byte length of the literal
//! OCI empty-descriptor payload `{}`, embedded to avoid recomputation.

/// Sigstore bundle v0.3 artifact type, used as the `artifactType` on a
/// signature referrer manifest.
pub const SIGSTORE_BUNDLE_V03: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

/// Empty OCI config media type per the empty-descriptor convention
/// (OCI image spec §"Guidelines for Empty Descriptors").
pub const EMPTY_CONFIG: &str = "application/vnd.oci.empty.v1+json";

/// SHA-256 digest of the canonical empty config payload (`{}` + newline free).
///
/// Frozen per plan Step 1.3 and OCI image-spec §"Guidelines for Empty
/// Descriptors".
pub const EMPTY_CONFIG_DIGEST: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

/// Byte length of the canonical empty config payload (literal `{}`).
pub const EMPTY_CONFIG_SIZE: u64 = 2;
