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

// `error` is `pub` — `SignError`/`SignErrorKind` are bound by the CLI layer.
pub mod error;
// `endpoint` is `pub` — `validate_sigstore_url` is bound by the CLI layer
// (and any future library consumer that constructs a `SignContext`).
pub mod endpoint;

// Phase 5c-blocked modules. Bodies are `unimplemented!()` until sigstore-rs
// integration lands; promoting their types to the public API today would
// advertise an unstable shape. Modules stay crate-internal; Phase 5c re-promotes
// the specific items the CLI binds against.
#[allow(dead_code)]
mod bundle;
// `fulcio` and `rekor` already carry `#![allow(dead_code)]` at the file head.
mod fulcio;
#[allow(dead_code)] // Phase 5c will wire callers.
pub(crate) mod oidc;
#[allow(dead_code)]
pub(crate) mod oidc_ambient;
#[allow(dead_code)]
pub(crate) mod oidc_ambient_inline;
#[allow(dead_code)]
pub(crate) mod oidc_browser;
#[allow(dead_code)]
pub(crate) mod pipeline;
mod rekor;
#[allow(dead_code)]
mod signer;

pub use error::{SignError, SignErrorKind};
