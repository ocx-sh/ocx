// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Supply-chain signing: keyless Sigstore sign, DSSE attest, full verify,
//! `TrustRoot` verification material, cosign simplesigning, SBOM referrers.
//!
//! What `oci` still held inside `ocx_lib` — the four signing/attestation wire
//! formats — plus the SBOM reader that publishes alongside them. Everything
//! generic to the distribution spec (references, digests, manifests, transport,
//! referrers, the SSRF guard, registry auth) is `ocx_oci`, and the resolution
//! index is `ocx_index`; this crate re-exports neither.

// The cosign simplesigning claim — the payload a `sha256-<hex>.sig` sidecar
// layer carries. A peer of `referrer`, not a child of `sign`: verify reads it
// too, and it is a wire format in its own right.
pub mod simplesigning;

// `attest` owns the in-toto/DSSE wire formats. The module graph is NOT
// acyclic here, and that is accepted (D-h: no new error family) rather than a
// design lapse: attest/dsse.rs + attest/statement.rs return VerifyErrorKind
// (attest <-> verify), and attest/pipeline.rs + attest/statement.rs return
// SignErrorKind while sign/{bundle,rekor,signer}.rs import attest's DSSE and
// Statement types back (attest <-> sign). See adr_sbom_attestations.md D-i.
pub mod attest;

pub mod sign;

pub mod verify;

// The SBOM reader: what a referrers-attached SBOM is and how it is read back.
// It ships with the signing tier because the same push attaches both.
pub mod sbom;
