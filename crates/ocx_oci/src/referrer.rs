// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI 1.1 referrer artifacts (signatures, SBOMs, attestations) attached to
//! a subject manifest by digest via the Referrers API.
//! Design record: [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

pub mod capability;
pub mod discovery;
pub mod manifest;
pub mod media_types;

pub use capability::{ReferrersApiCapability, ReferrersSupport};
pub use discovery::DiscoveryMethod;
pub use manifest::ReferrerManifest;
