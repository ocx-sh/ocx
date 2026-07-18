// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cosign-compatible keyless signing (Sigstore bundle v0.3 → OCI referrer).
//!
//! See
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the design record.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`signer`] | [`Signer`] trait — OIDC acquisition separated from bundle push |
//! | [`oidc`] | [`TokenProvider`], [`AmbientProvider`], [`DispatchingTokenProvider`] |
//! | [`oidc_ambient`] | `ambient-id` crate wrapper (v2 seam) |
//! | [`oidc_ambient_inline`] | Inline env-inspection ambient path (GHA/GitLab/CircleCI) |
//! | [`oidc_browser`] | Browser OAuth PKCE (laptop path — deferred) |
//! | [`fulcio`] | Fulcio client (`/api/v2/signingCert`) |
//! | [`rekor`] | Rekor v1 log client |
//! | [`bundle`] | Sigstore bundle v0.3 assembly + parsing |
//! | [`pipeline`] | Push-side state machine |
//! | [`error`] | [`SignErrorKind`] variant inventory |

// `error` is `pub` — `SignError`/`SignErrorKind` are bound by the CLI layer.
pub mod error;
// `endpoint` was lifted to `crate::oci::endpoint` (ADR Amendment 2) — a URL
// primitive shared with `verify`. Reference it there, not under `sign`.

// `bundle` is `pub(crate)` so `oci::verify` reuses `parse_bundle` +
// `BUNDLE_V03_MEDIA_TYPE`.
pub(crate) mod bundle;
mod fulcio;
pub mod oidc;
mod oidc_ambient;
mod oidc_ambient_inline;
mod oidc_browser;
pub mod pipeline;
pub(crate) mod rekor;
pub mod signer;

pub use bundle::SignedBundle;
pub use error::{SignError, SignErrorKind};
pub use oidc::{DispatchingTokenProvider, OidcToken, TokenProvider};
pub use pipeline::{SignContext, SignPipeline, SignResult};
pub use signer::{KeylessSigner, Signer};
