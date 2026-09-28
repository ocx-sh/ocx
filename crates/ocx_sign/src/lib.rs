// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Supply-chain signing: keyless Sigstore sign, DSSE attest, full verify,
//! `TrustRoot` verification material, cosign simplesigning, SBOM referrers.

pub mod simplesigning;

// The attest <-> sign/verify error-kind cycle is deliberate: adr_sbom_attestations.md § D-i — Module layout
pub mod attest;

pub mod sign;

pub mod verify;

pub mod sbom;
