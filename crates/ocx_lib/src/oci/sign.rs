// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cosign-compatible keyless signing (Sigstore bundle v0.3 → OCI referrer).
//!
//! Phase 1 scaffolding — bodies use `unimplemented!()` until Phase 5. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the design record and
//! [`plan_slice1_sign_and_verify.md`](../../../../.claude/state/plans/plan_slice1_sign_and_verify.md)
//! for the implementation plan.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`signer`] | [`Signer`] trait — OIDC acquisition separated from bundle push |
//! | [`oidc`] | [`TokenProvider`], [`AmbientProvider`], [`DispatchingTokenProvider`] |
//! | [`oidc_ambient`] | `ambient-id` crate wrapper (primary ambient path) |
//! | [`oidc_ambient_inline`] | Inline env-inspection fallback (CI regression hedge) |
//! | [`oidc_browser`] | `sigstore-rs` OAuth PKCE wrapper (laptop path) |
//! | [`fulcio`] | Fulcio CSR client (`/api/v2/signingCert`) |
//! | [`rekor`] | Rekor v1 log client |
//! | [`bundle`] | Sigstore bundle v0.3 JSON builder |
//! | [`pipeline`] | 15-step push state machine; implements [`Signer`] |
//! | [`error`] | [`SignErrorKind`] variant inventory |

pub mod bundle;
pub mod error;
pub mod fulcio;
pub mod oidc;
pub mod oidc_ambient;
pub mod oidc_ambient_inline;
pub mod oidc_browser;
pub mod pipeline;
pub mod rekor;
pub mod signer;

pub use bundle::SignedBundle;
pub use error::{SignError, SignErrorKind};
pub use oidc::{AmbientProvider, DispatchingTokenProvider, OidcToken, TokenProvider};
pub use pipeline::{SignContext, SignPipeline, SignResult};
pub use signer::{KeylessSigner, Signer};
