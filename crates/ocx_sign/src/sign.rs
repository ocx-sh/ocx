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
//! | [`format`] | [`SignatureFormat`] — bundle referrer vs `.sig` sidecar |
//! | [`key_backend`] | [`KeyBackend`] signing primitive + the wire-fixed [`public_key_hint`] |
//!
//! The `--key` grammar itself is `ocx_trust::key_ref`: a signer-identity
//! reference is trust-policy vocabulary, shared by `signers` entries and the
//! flag alike, and it names nothing under `sign`.
//! | [`oidc`] | [`TokenProvider`], [`AmbientProvider`], [`DispatchingTokenProvider`] |
//! | [`oidc_ambient`] | `ambient-id` crate wrapper (v2 seam) |
//! | [`oidc_ambient_inline`] | Inline env-inspection ambient path (GHA/GitLab/CircleCI) |
//! | [`oidc_browser`] | Browser OAuth PKCE (laptop path — deferred) |
//! | [`fulcio`] | Fulcio client (`/api/v2/signingCert`) |
//! | [`rekor`] | Rekor v1 log client |
//! | [`bundle`] | Sigstore bundle v0.3 assembly + parsing |
//! | [`key_signer`] | [`KeySigner`] — key-pair signing, delegated to a [`KeyBackend`] |
//! | [`pipeline`] | Push-side state machine |
//! | [`simplesigning_write`] | The cosign `sha256-<hex>.sig` sidecar writer |
//! | [`referrers`] | Referrer attach — manifest PUT plus the OCI tag-schema fallback |
//! | [`state`] | [`SigningStatePaths`] — the signing tier of the state-root layout |
//! | [`error`] | [`SignErrorKind`] variant inventory |

// `error` is `pub` — `SignError`/`SignErrorKind` are bound by the CLI layer.
pub mod error;
// `endpoint` was lifted to `ocx_oci::endpoint` (ADR Amendment 2) — a URL
// primitive shared with `verify`. Reference it there, not under `sign`.

// `bundle` is `pub(crate)` so `oci::verify` reuses `parse_bundle` +
// `BUNDLE_V03_MEDIA_TYPE`.
pub(crate) mod bundle;
mod fulcio;
// `key_backend` is `pub`: `KeyBackend`/`public_key_hint` are bound by the CLI
// layer.
// `format` is `pub`: `SignatureFormat` is the CLI's `--signature-format`
// vocabulary and a config value, read by sign, attest and verify alike.
pub mod format;
pub mod key_backend;
pub mod key_signer;
pub mod oidc;
mod oidc_ambient;
mod oidc_ambient_inline;
mod oidc_browser;
pub mod pipeline;
// `referrers` is `pub(crate)`: the attach seam is shared with `oci::attest`,
// which is a sibling module rather than a child of `sign`.
pub(crate) mod referrers;
pub(crate) mod rekor;
// `state` is `pub`: a caller holding a `StateStore` builds the paths the
// pipelines take, which is the whole point of the type (ADR 1.9).
pub mod state;
// `simplesigning_write` is `pub(crate)`: `oci::attest` writes `.att`/`.sbom`
// sidecars through the same append loop.
pub mod signer;
pub(crate) mod simplesigning_write;

pub use bundle::SignedBundle;
pub use error::{SignError, SignErrorKind};
// The `ClientError -> SignErrorKind` seam decides the exit code of a
// registry fault on the sign path; the assertion lives with the impl.
pub use format::SignatureFormat;
pub use key_backend::{KeyBackend, KeyBackendError, public_key_hint};
pub use key_signer::KeySigner;
pub use oidc::{DispatchingTokenProvider, OidcToken, TokenProvider};
pub use pipeline::{SignContext, SignPipeline, SignResult};
pub use referrers::map_client_error;
pub use signer::{KeylessSigner, SignedBlob, Signer};
pub use state::SigningStatePaths;
