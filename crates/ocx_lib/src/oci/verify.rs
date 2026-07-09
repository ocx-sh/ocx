// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Full keyless Sigstore verification.
//!
//! Pulls the Sigstore bundle v0.3 referrer for a target manifest, verifies
//! the Fulcio cert chain against the TUF-rooted trust material, verifies the
//! Rekor SET, verifies the signature over the subject digest, and checks
//! `--certificate-identity` / `--certificate-oidc-issuer` against the cert.
//!
//! Phase 1 scaffolding — bodies use `unimplemented!()` until Phase 5. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full verify state machine.

// `error` is `pub` — `VerifyError`/`VerifyErrorKind` are bound by the CLI layer.
pub mod error;

mod identity;
pub mod pipeline;
pub mod trust_cache;
pub mod trust_root;

pub use error::{VerifyError, VerifyErrorKind};
pub use pipeline::{VerifyContext, VerifyPipeline, VerifyResult};
pub use trust_cache::TrustRootCache;
pub use trust_root::TrustRoot;
