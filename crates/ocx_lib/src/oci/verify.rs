// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Full keyless Sigstore verification.
//!
//! Pulls the Sigstore bundle v0.3 referrer for a target manifest, verifies
//! the Fulcio cert chain against the TUF-rooted trust material, verifies the
//! Rekor SET, verifies the signature over the subject digest, and checks
//! `--certificate-identity` / `--certificate-oidc-issuer` against the cert.
//!
//! Wired end to end by [`pipeline::VerifyPipeline`]. Read-only throughout —
//! every registry call routes through the mirror-aware read seam. The
//! A self-hosted stack supplies its trust root out of band — `--sigstore-trusted-root`,
//! `OCX_SIGSTORE_TRUSTED_ROOT`, `[trust.sigstore]`, or
//! `$OCX_HOME/sigstore/trusted-root.json`; see
//! `self-hosted-sigstore.md`. Design record:
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

// `error` is `pub` — `VerifyError`/`VerifyErrorKind` are bound by the CLI layer.
pub mod error;

// `discovery` is `pub`: `DiscoveryMethod` is a reported wire value, and its
// module doc carries the G0 constraint on how certificate validity is anchored.
pub mod discovery;

// `pub(crate)`: the SAN/issuer extractors are the one definition of what an
// identity *is*, and `sign` reports the identity it just had issued through
// them so the two commands cannot disagree about one certificate.
pub(crate) mod identity;
pub mod pipeline;

// `pub`: the cosign sidecar reader is a second, independent discovery door into
// the same verification, and its `SidecarKind` / `sidecar_tag` are the one
// spelling of the `.sig` / `.att` tag suffixes outside `package::tag`'s
// classifier. `.sbom` is deliberately absent — see that module's "Reported
// gaps".
pub mod simplesigning_read;

// The `.att` sidecar's reader. Private to this subtree: unlike `SidecarKind`
// it exposes no wire vocabulary, and `.att` has no artifact type for a caller
// to name — measured against cosign v3.1.1, see the module doc.
mod attestation_sidecar;
pub mod trust_cache;
pub mod trust_resolve;
pub mod trust_root;

// OCX's own layer around the delegated verification of a DSSE attestation:
// the structural half before that call, the row-12 tlog binding after it.
mod dsse;

// Rekor SET + Merkle inclusion, delegated to sigstore-rs.
mod tlog;

// The instant certificate validity is judged against, tagged with its
// provenance so no caller can substitute the wall clock for a signing-time
// proof. Private to this subtree — `SigningInstant` appears only in
// `tlog::verify_integrated_time_within_certificate`'s signature and at the
// pipeline call site, both inside `oci::verify`.
mod signing_instant;

pub use discovery::DiscoveryMethod;
pub use dsse::VerifiedAttestation;
pub use error::{VerifyError, VerifyErrorKind};
pub use pipeline::{
    AttestationMatch, AttestationScan, RefusedCandidate, UnverifiedSbom, VerificationMode, VerifyContentMode,
    VerifyContext, VerifyPipeline, VerifyResult,
};
pub use simplesigning_read::{SidecarKind, SidecarScan, read_sidecar_manifest, read_sidecar_tag, sidecar_tag};
pub use trust_cache::TrustRootCache;
pub use trust_resolve::resolve_trust_root;
pub use trust_root::TrustRoot;
