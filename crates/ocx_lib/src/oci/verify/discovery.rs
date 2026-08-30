// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! How a signature was found. Populated by the client layer (referrer
//! listing) and consumed by the verify pipeline, which reports it on each
//! entry of `signatures[]`.
//!
//! # Carried constraint from G0 — read before touching verification time
//!
//! G0's keyless golden fixture carries a Fulcio certificate that expired
//! about ten minutes after capture. **Certificate validity is anchored to the
//! signing-time proof — the Rekor entry / SET — never to wall-clock "is this
//! valid now".** A wall-clock check makes every keyless fixture rot within
//! the hour and would be a real trust bug besides: a short-lived certificate
//! is *designed* to be expired by the time anyone verifies it, and the
//! transparency-log timestamp is the only evidence that the signature
//! happened while the certificate was live.

use serde::{Deserialize, Serialize};

/// Where a signature referrer was discovered.
///
/// Reported verbatim on `signatures[].discovery_method`, so a caller can tell an
/// OCI 1.1 registry from one that only answers the fallback tag schema
/// without inspecting the registry itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMethod {
    /// `GET /v2/<name>/referrers/<digest>` — the OCI 1.1 Referrers API.
    ReferrersApi,
    /// The `sha256-<hex>` fallback referrers index a registry without the
    /// Referrers API gets instead.
    FallbackTag,
    /// The cosign `sha256-<hex>.sig` / `.att` / `.sbom` sidecar tag.
    SidecarTag,
}

impl DiscoveryMethod {
    /// The frozen wire slug.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReferrersApi => "referrers_api",
            Self::FallbackTag => "fallback_tag",
            Self::SidecarTag => "sidecar_tag",
        }
    }
}

impl std::fmt::Display for DiscoveryMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three slugs are a reported wire vocabulary; pin them against the
    /// serde output rather than trusting `rename_all`.
    #[test]
    fn discovery_method_slugs_are_frozen() {
        let frozen = [
            (DiscoveryMethod::ReferrersApi, "referrers_api"),
            (DiscoveryMethod::FallbackTag, "fallback_tag"),
            (DiscoveryMethod::SidecarTag, "sidecar_tag"),
        ];

        for (method, slug) in frozen {
            // Exhaustive on purpose, and the reason no `ALL` constant exists: a
            // new variant stops this test compiling, where a hand-maintained
            // list would just go stale beside the enum it claims to mirror.
            match method {
                DiscoveryMethod::ReferrersApi | DiscoveryMethod::FallbackTag | DiscoveryMethod::SidecarTag => {}
            }
            let value = serde_json::to_value(method).expect("DiscoveryMethod serializes");
            assert_eq!(value, serde_json::Value::String(slug.to_owned()));
            assert_eq!(method.as_str(), slug);
            let parsed: DiscoveryMethod = serde_json::from_value(value).expect("slug parses back");
            assert_eq!(parsed, method);
        }
    }
}
