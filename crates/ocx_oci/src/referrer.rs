// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI 1.1 referrer artifacts (signatures, SBOMs, attestations) attached to
//! a subject manifest by digest via the Referrers API.
//!
//! [`capability`] probes and caches a registry's Referrers-API support per
//! host; [`discovery`] names the door a referrer was found through;
//! [`manifest`] builds the referrer manifest the sign pipeline pushes;
//! [`media_types`] holds the artifact-type constants. Consumed by
//! `crate::sign::pipeline` (push) and `crate::verify::pipeline` (discovery).
//! Design record:
//! [`adr_oci_referrers_signing_v1.md`](../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

pub mod capability;
// `DiscoveryMethod` names which referrer-listing door answered — the Referrers
// API, the fallback tag schema, or a cosign sidecar tag. All three are this
// module's doors, and the client populates it, so it lives here rather than
// under `verify`, which only reports the value it is handed (ADR 1.18).
pub mod discovery;
pub mod manifest;
pub mod media_types;

pub use capability::{ReferrersApiCapability, ReferrersSupport};
pub use discovery::DiscoveryMethod;
pub use manifest::ReferrerManifest;
