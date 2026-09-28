// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cosign-compatible keyless signing (Sigstore bundle v0.3 → OCI referrer). Design
//! record: [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

pub mod error;

pub(crate) mod bundle;
pub mod format;
mod fulcio;
pub mod key_backend;
pub mod key_signer;
pub mod oidc;
mod oidc_ambient;
mod oidc_ambient_inline;
mod oidc_browser;
pub mod pipeline;
pub(crate) mod referrers;
pub(crate) mod rekor;
pub mod signer;
pub(crate) mod simplesigning_write;
pub mod state;

pub use bundle::SignedBundle;
pub use error::{SignError, SignErrorKind};
pub use format::SignatureFormat;
pub use key_backend::{KeyBackend, KeyBackendError, public_key_hint};
pub use key_signer::KeySigner;
pub use oidc::{DispatchingTokenProvider, OidcToken, TokenProvider};
pub use pipeline::{SignContext, SignPipeline, SignResult};
pub use referrers::map_client_error;
pub use signer::{KeylessSigner, SignedBlob, Signer};
pub use state::SigningStatePaths;
