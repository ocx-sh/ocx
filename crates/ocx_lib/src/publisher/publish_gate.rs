// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pre-push dependency-pin gate for `ocx package push`.
//!
//! Push makes no resolution decisions (`adr_dependency_manifest_pinning.md`):
//! it verifies that every dependency projects to a platform **manifest**
//! digest for every fan-out platform, and that each unique projected pin
//! actually exists in its registry as an image manifest — an image INDEX
//! digest is rejected because a tag's index is rewritten (and its old digest
//! garbage-collected) on every platform push.

use std::collections::HashSet;

use futures::stream::{self, StreamExt, TryStreamExt};

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci::{self, Platform, client::error::ClientError};
use crate::package::metadata::authoring::{AuthoringError, AuthoringMetadata};
use crate::{log, oci::Client};

/// Maximum number of unique dependency-pin registry verifications to run
/// concurrently in a single [`verify_dependency_pins`] call.
///
/// Each verification is a small, latency-bound manifest GET (metadata, not a
/// bulk transfer) — the same shape as `TagManager::refresh`'s per-tag digest
/// fetch, which uses the same bounded-`buffer_unordered` idiom. Dependency
/// count is itself capped at
/// [`AuthoringDependencies::MAX_DEPENDENCIES`](crate::package::metadata::authoring::AuthoringDependencies::MAX_DEPENDENCIES)
/// (256), so this only needs to bound simultaneous in-flight requests per
/// push, not overall fan-out; 16 keeps a polite per-registry burst while
/// still parallelizing the common case of a handful of cross-registry deps.
const DEPENDENCY_PIN_VERIFY_CONCURRENCY: usize = 16;

/// Verify every dependency pin of `metadata` for the fan-out `platforms`.
///
/// Three checks, in order:
///
/// 1. every dependency carries a digest or a non-empty pin map
///    ([`PublishGateError::DependencyUnpinned`]);
/// 2. every dependency projects to a pin for every platform in `platforms`
///    ([`PublishGateError::MissingPlatformCoverage`]);
/// 3. every **unique** projected pin resolves in its registry to an image
///    manifest — verified once per pin via [`Client::pull_manifest`], which
///    also authenticates per registry (cross-registry dependencies covered).
///    Unique pins are verified concurrently (bounded by
///    [`DEPENDENCY_PIN_VERIFY_CONCURRENCY`]); the first verification failure
///    short-circuits the rest.
///
/// # Errors
///
/// See [`PublishGateError`]. Registry auth failures pass through so they
/// classify to their own exit code.
pub async fn verify_dependency_pins(
    client: &Client,
    metadata: &AuthoringMetadata,
    platforms: &[Platform],
) -> Result<(), PublishGateError> {
    let mut unique_pins: Vec<oci::PinnedIdentifier> = Vec::new();
    let mut seen: HashSet<oci::PinnedIdentifier> = HashSet::new();

    for dep in metadata.dependencies() {
        if !dep.is_pinned() {
            return Err(PublishGateError::DependencyUnpinned {
                identifier: Box::new(dep.identifier.clone()),
            });
        }
        for platform in platforms {
            let pin = dep.pin_for(platform).map_err(|error| match error {
                AuthoringError::MissingPlatformPin { identifier, platform } => {
                    PublishGateError::MissingPlatformCoverage { identifier, platform }
                }
                // `is_pinned` was checked above; any other projection error is
                // an unpinned dependency in disguise.
                _ => PublishGateError::DependencyUnpinned {
                    identifier: Box::new(dep.identifier.clone()),
                },
            })?;
            // Dedup ignores the advisory tag (content identity is
            // registry+repository+digest) — `strip_advisory` is the
            // established key for exactly this HashSet/HashMap dedup.
            if seen.insert(pin.strip_advisory()) {
                unique_pins.push(pin);
            }
        }
    }

    // Independent reads: verify concurrently, bounded, first error wins.
    stream::iter(unique_pins)
        .map(|pin| {
            let client = client.clone();
            async move {
                log::debug!("verifying dependency pin '{pin}'");
                match client.pull_manifest(&pin).await {
                    Ok(_) => Ok(()),
                    Err(ClientError::UnexpectedManifestType) => Err(PublishGateError::DependencyPinnedToIndex {
                        identifier: Box::new(pin.clone()),
                    }),
                    Err(ClientError::ManifestNotFound(_)) => Err(PublishGateError::DependencyManifestNotFound {
                        identifier: Box::new(pin.clone()),
                    }),
                    Err(source) => Err(PublishGateError::Verification {
                        identifier: Box::new(pin.clone()),
                        source,
                    }),
                }
            }
        })
        .buffer_unordered(DEPENDENCY_PIN_VERIFY_CONCURRENCY)
        .try_collect::<()>()
        .await
}

/// Errors from the pre-push dependency-pin gate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublishGateError {
    /// A dependency has neither a digest nor a platforms pin map.
    #[error("dependency '{identifier}' is not pinned to a manifest digest; re-run `ocx package create` to resolve it")]
    DependencyUnpinned { identifier: Box<oci::Identifier> },
    /// A dependency's pin map has no key covering a fan-out platform.
    #[error(
        "dependency '{identifier}' has no manifest pin covering target platform '{platform}'; re-run `ocx package create`"
    )]
    MissingPlatformCoverage {
        identifier: Box<oci::Identifier>,
        platform: String,
    },
    /// The pinned digest resolves to an image INDEX, not a manifest.
    #[error(
        "dependency '{identifier}' pins an image INDEX digest; a tag's index is rewritten on every platform push and its old digest is garbage-collected, so this pin will break — re-run `ocx package create` to pin platform manifest digests"
    )]
    DependencyPinnedToIndex { identifier: Box<oci::PinnedIdentifier> },
    /// The pinned manifest does not exist in the registry.
    #[error("dependency manifest '{identifier}' not found in the registry")]
    DependencyManifestNotFound { identifier: Box<oci::PinnedIdentifier> },
    /// Pin verification failed for another reason (auth, network, ...).
    #[error("failed to verify dependency pin '{identifier}'")]
    Verification {
        identifier: Box<oci::PinnedIdentifier>,
        #[source]
        source: ClientError,
    },
}

impl ClassifyExitCode for PublishGateError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            PublishGateError::DependencyUnpinned { .. }
            | PublishGateError::MissingPlatformCoverage { .. }
            | PublishGateError::DependencyPinnedToIndex { .. } => Some(ExitCode::DataError),
            PublishGateError::DependencyManifestNotFound { .. } => Some(ExitCode::NotFound),
            // Delegate to the inner client cause (auth → 80, network → 69).
            PublishGateError::Verification { .. } => None,
        }
    }
}

// ── Specification tests — adr_dependency_manifest_pinning.md Phase 4 ─────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::classify_error;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn stub_client(data: StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data)))
    }

    fn metadata(deps_json: &str) -> AuthoringMetadata {
        serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[{deps_json}]}}"#
        ))
        .expect("metadata parses")
    }

    fn platform(value: &str) -> Platform {
        value.parse().expect("platform parses")
    }

    const IMAGE_MANIFEST_JSON: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;
    const IMAGE_INDEX_JSON: &str =
        r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;

    /// Seed the stub so `example.com/dep@sha256:<hex>` resolves to `body`.
    fn seed_manifest(data: &StubTransportData, digest_hex: &str, body: &str) {
        data.write().manifests.insert(
            format!("example.com/dep@sha256:{digest_hex}"),
            (body.as_bytes().to_vec(), format!("sha256:{digest_hex}")),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpinned_dependency_rejected() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(r#"{"identifier":"example.com/dep:1.0"}"#);

        let err = verify_dependency_pins(&client, &metadata, &[platform("linux/amd64")])
            .await
            .expect_err("unpinned dep must be rejected");
        assert!(matches!(err, PublishGateError::DependencyUnpinned { .. }), "got: {err}");
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn map_gap_rejected() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(&format!(
            r#"{{"identifier":"example.com/dep","platforms":{{"linux/amd64":"sha256:{}"}}}}"#,
            hex('a')
        ));

        let err = verify_dependency_pins(&client, &metadata, &[platform("linux/amd64"), platform("darwin/arm64")])
            .await
            .expect_err("map gap must be rejected");
        assert!(
            matches!(err, PublishGateError::MissingPlatformCoverage { ref platform, .. } if platform == "darwin/arm64"),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn index_pinned_dependency_rejected() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_INDEX_JSON);
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &[platform("linux/amd64")])
            .await
            .expect_err("an index digest pin must be rejected");
        assert!(
            matches!(err, PublishGateError::DependencyPinnedToIndex { .. }),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn manifest_pinned_dependency_accepted() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        verify_dependency_pins(&client, &metadata, &[platform("linux/amd64")])
            .await
            .expect("manifest pin passes the gate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_manifest_is_not_found() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &[platform("linux/amd64")])
            .await
            .expect_err("absent manifest must be rejected");
        assert!(
            matches!(err, PublishGateError::DependencyManifestNotFound { .. }),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unique_pin_verified_once_across_platforms() {
        // A direct digest pin projects identically for every fan-out
        // platform — the registry check must run once, not per platform.
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data.clone());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        verify_dependency_pins(
            &client,
            &metadata,
            &[platform("linux/amd64"), platform("darwin/arm64"), Platform::any()],
        )
        .await
        .expect("gate passes");

        let pulls = data
            .read()
            .calls
            .iter()
            .filter(|call| *call == "pull_manifest_raw")
            .count();
        assert_eq!(pulls, 1, "unique pin must be verified exactly once");
    }

    /// W8: renamed from `auth_failure_passes_through` — this drives a
    /// *generic* registry error (`pull_manifest_error_override`), not an
    /// authentication failure. It verifies that a non-auth registry error
    /// surfaces as `PublishGateError::Verification` and classifies to
    /// `Unavailable` (69) via the inner `ClientError` chain. See
    /// `auth_failure_classifies_as_auth_error` below for the genuine
    /// authentication-failure path.
    #[tokio::test(flavor = "multi_thread")]
    async fn registry_error_classifies_as_unavailable() {
        let data = StubTransportData::new();
        data.write().pull_manifest_error_override = Some("boom".to_string());
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &[platform("linux/amd64")])
            .await
            .expect_err("registry error must surface");
        assert!(matches!(err, PublishGateError::Verification { .. }), "got: {err}");
        // Registry error → Unavailable via the inner ClientError chain.
        assert_eq!(classify_error(&err), ExitCode::Unavailable);
    }

    /// W8: a genuine `ClientError::Authentication` (not a generic registry
    /// error) must classify to `AuthError` (80), not `Unavailable`.
    #[tokio::test(flavor = "multi_thread")]
    async fn auth_failure_classifies_as_auth_error() {
        let data = StubTransportData::new();
        data.write().ensure_auth_error_override = Some("bad creds".to_string());
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &[platform("linux/amd64")])
            .await
            .expect_err("authentication failure must surface");
        assert!(matches!(err, PublishGateError::Verification { .. }), "got: {err}");
        assert_eq!(classify_error(&err), ExitCode::AuthError);
    }
}
