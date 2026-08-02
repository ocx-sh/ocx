// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI 1.1 referrer artifacts (signatures, SBOMs, attestations) attached to
//! a subject manifest by digest via the Referrers API.
//!
//! [`capability`] probes and caches a registry's Referrers-API support per
//! host; [`manifest`] builds the referrer manifest the sign pipeline pushes;
//! [`media_types`] holds the artifact-type constants. Consumed by
//! `oci::sign::pipeline` (push) and `oci::verify::pipeline` (discovery).
//! Design record:
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

pub mod capability;
pub mod manifest;
pub mod media_types;

pub use capability::{ReferrersApiCapability, ReferrersSupport};
pub use manifest::ReferrerManifest;
