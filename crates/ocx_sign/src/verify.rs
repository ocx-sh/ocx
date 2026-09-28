// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Full keyless Sigstore verification. Design record:
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

pub mod candidates;

pub mod error;

// `sign` reports its issued identity through these extractors, so sign and verify cannot disagree about one cert.
pub(crate) mod identity;
pub mod pipeline;

// `SidecarKind`/`sidecar_tag` are the one spelling of the `.sig`/`.att` suffixes outside `package::tag`'s classifier.
pub mod simplesigning_read;

mod attestation_sidecar;
pub mod trust_cache;
pub mod trust_resolve;
pub mod trust_root;

mod dsse;

mod tlog;

// Tagged with provenance so the wall clock cannot stand in for a signing-time proof.
mod signing_instant;

pub use candidates::{SignerCandidate, list_signature_candidates};
pub use dsse::VerifiedAttestation;
pub use error::{TrustRootLoadReason, VerifyError, VerifyErrorKind};
pub use ocx_oci::referrer::DiscoveryMethod;
pub use pipeline::{
    AttestationMatch, AttestationScan, RefusedCandidate, UnverifiedSbom, VerificationMode, VerifyContentMode,
    VerifyContext, VerifyPipeline, VerifyResult,
};
pub use simplesigning_read::{SidecarKind, SidecarScan, read_sidecar_manifest, read_sidecar_tag, sidecar_tag};
pub use trust_cache::TrustRootCache;
pub use trust_resolve::{MAX_TRUSTED_ROOT_BYTES, resolve_trust_root};
pub use trust_root::TrustRoot;
