// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pre-push dependency-pin gate for `ocx package push`.
//!
//! Push makes no resolution decisions (`adr_dependency_manifest_pinning.md`):
//! it verifies that every dependency projects to a platform **manifest**
//! digest for the single target platform (`adr_platform_model_unification.md`
//! D5), and that each projected pin actually exists in its registry as an
//! image manifest — an image INDEX digest is rejected because a tag's index
//! is rewritten (and its old digest garbage-collected) on every platform
//! push.

use futures::stream::{self, StreamExt, TryStreamExt};

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci::{self, Platform, client::error::ClientError};
use crate::package::dependency_pinning::reject_digest_pins_in_any_target;
use crate::package::metadata::authoring::{AuthoringError, AuthoringMetadata};
use crate::{log, oci::Client};

/// Maximum number of dependency-pin registry verifications to run
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

/// Verify every dependency pin of `metadata` for the single target
/// `platform`.
///
/// Five checks, in order:
///
/// 1. D5: an `any`-targeted bundle carries no direct digest pin on any
///    dependency ([`PublishGateError::DirectDigestPinInAnyTarget`]) — a leaf
///    manifest carries no platform descriptor, so a bare `@digest` pin cannot
///    be verified to be `any`-offered;
/// 2. every dependency carries a digest or a non-empty pin map
///    ([`PublishGateError::DependencyUnpinned`]);
/// 3. every dependency projects to a pin for `platform`
///    ([`PublishGateError::MissingPlatformCoverage`]);
/// 4. for an `any`-targeted bundle, every pin (necessarily projected from a
///    `platforms` map — check 1 already rejected direct digest pins) is a
///    *genuine* `any` offer in the dependency's own image index, not merely
///    an `"any"`-keyed sidecar claim ([`verify_any_pin_provenance`]) — this
///    is what check 1 alone cannot catch: a hand-edited sidecar could carry
///    `platforms: {"any": "<a platform-specific leaf digest>"}` and
///    [`AuthoringDependency::pin_for`](crate::package::metadata::authoring::AuthoringDependency::pin_for)
///    has no way to tell (leaf manifests carry no platform descriptor);
/// 5. every projected pin resolves in its registry to an image manifest —
///    verified via [`Client::pull_manifest`], which also authenticates per
///    registry (cross-registry dependencies covered).
///
/// Checks 4 and 5 run concurrently per dependency (bounded by
/// [`DEPENDENCY_PIN_VERIFY_CONCURRENCY`]); the first verification failure
/// short-circuits the rest. Check 4 is skipped entirely for a concrete-target
/// bundle — no extra network beyond the existing check 5 fetch.
///
/// # Errors
///
/// See [`PublishGateError`]. Registry auth failures pass through so they
/// classify to their own exit code.
pub async fn verify_dependency_pins(
    client: &Client,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> Result<(), PublishGateError> {
    if platform.is_any()
        && let Some(identifier) = reject_digest_pins_in_any_target(metadata.dependencies())
    {
        return Err(PublishGateError::DirectDigestPinInAnyTarget { identifier });
    }

    // `AuthoringDependencies` enforces a unique (registry, repository) per
    // entry, so distinct dependencies can never project to the same pin —
    // no dedup pass is needed before verifying. `dependency_identifier` is
    // carried alongside each pin so an `any`-target provenance check
    // (`verify_any_pin_provenance`) can re-fetch the dependency's own
    // manifest by its advisory tag.
    let mut pins: Vec<(oci::Identifier, oci::PinnedIdentifier)> = Vec::new();
    for dep in metadata.dependencies() {
        if !dep.is_pinned() {
            return Err(PublishGateError::DependencyUnpinned {
                identifier: Box::new(dep.identifier.clone()),
            });
        }
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
        pins.push((dep.identifier.clone(), pin));
    }

    let is_any_target = platform.is_any();

    // Independent reads: verify concurrently, bounded, first error wins.
    stream::iter(pins)
        .map(|(dependency_identifier, pin)| {
            let client = client.clone();
            async move {
                if is_any_target {
                    verify_any_pin_provenance(&client, &dependency_identifier, &pin).await?;
                }
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

/// D5 fail-closed provenance check for an `any`-targeted bundle
/// (`adr_platform_model_unification.md` D5): a dependency's `platforms` map
/// key is a sidecar-authored claim, not registry evidence. Because a leaf
/// manifest carries no platform descriptor, a hand-edited sidecar could pin a
/// platform-specific leaf under the `"any"` key and
/// [`AuthoringDependency::pin_for`](crate::package::metadata::authoring::AuthoringDependency::pin_for)
/// has no way to detect the forgery — publishing a platform-specific
/// dependency as if it were universal.
///
/// This re-derives the fact from the dependency's own image index: fetch
/// `dependency_identifier`'s current manifest by its advisory tag and require
/// an entry whose declared platform is `any` **and** whose digest equals
/// `pin`'s. A flat (non-index) manifest is `any`-offered by construction —
/// the same convention
/// [`Index::fetch_candidates`](crate::oci::Index::fetch_candidates) uses for
/// `Manifest::Image` — so it passes only when its own digest equals `pin`'s
/// (there is no other leaf it could be).
async fn verify_any_pin_provenance(
    client: &Client,
    dependency_identifier: &oci::Identifier,
    pin: &oci::PinnedIdentifier,
) -> Result<(), PublishGateError> {
    let (digest, manifest) = client.fetch_manifest(dependency_identifier).await.map_err(|source| {
        PublishGateError::AnyPinProvenanceUnavailable {
            identifier: Box::new(dependency_identifier.clone()),
            source,
        }
    })?;

    let advertised_as_any = match manifest {
        oci::Manifest::Image(_) => digest == pin.digest(),
        oci::Manifest::ImageIndex(index) => index.manifests.into_iter().any(|entry| {
            oci::Digest::try_from(entry.digest.as_str()).is_ok_and(|entry_digest| entry_digest == pin.digest())
                && Platform::try_from(entry.platform).is_ok_and(|platform| platform.is_any())
        }),
    };

    if advertised_as_any {
        Ok(())
    } else {
        Err(PublishGateError::AnyPinNotAdvertisedAsAny {
            identifier: Box::new(dependency_identifier.clone()),
            digest: pin.digest().to_string(),
        })
    }
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
    /// D5: an `any`-targeted bundle carries a direct digest pin on a
    /// dependency. A leaf manifest carries no platform descriptor, so a bare
    /// `@digest` pin cannot be verified to be `any`-offered.
    #[error(
        "dependency '{identifier}' carries a direct digest pin in an `any`-targeted bundle; run `ocx package create --platform any` to resolve it"
    )]
    DirectDigestPinInAnyTarget { identifier: Box<oci::Identifier> },
    /// D5 provenance check: a dependency pinned via its `platforms` map for
    /// an `any`-targeted bundle is not advertised as `any` in the
    /// dependency's own image index — the sidecar's `"any"` key is a
    /// publisher claim, not registry evidence, so it cannot forge a
    /// platform-specific dependency into a universal one.
    #[error(
        "dependency '{identifier}' pins digest '{digest}' for the `any` platform, but the dependency's own image index does not advertise that digest as `any`; re-run `ocx package create --platform any` to re-resolve it"
    )]
    AnyPinNotAdvertisedAsAny {
        identifier: Box<oci::Identifier>,
        digest: String,
    },
    /// The D5 `any`-pin provenance check ([`AnyPinNotAdvertisedAsAny`](Self::AnyPinNotAdvertisedAsAny))
    /// could not fetch the dependency's own image index (missing tag,
    /// network, auth, ...). Fails closed: an unverifiable provenance claim
    /// is treated as untrusted, never silently accepted.
    #[error("failed to verify `any` pin provenance for dependency '{identifier}'")]
    AnyPinProvenanceUnavailable {
        identifier: Box<oci::Identifier>,
        #[source]
        source: crate::Error,
    },
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
            | PublishGateError::DependencyPinnedToIndex { .. }
            | PublishGateError::DirectDigestPinInAnyTarget { .. }
            | PublishGateError::AnyPinNotAdvertisedAsAny { .. } => Some(ExitCode::DataError),
            PublishGateError::DependencyManifestNotFound { .. } => Some(ExitCode::NotFound),
            // Delegate to the inner cause (auth → 80, network → 69, a missing
            // dependency tag → 79 via the wrapped `crate::Error`).
            PublishGateError::Verification { .. } | PublishGateError::AnyPinProvenanceUnavailable { .. } => None,
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

    // ── D5 any-provenance check fixtures ────────────────────────────────
    //
    // A `platforms`-map dependency keeps its advisory tag on `identifier`
    // (`pin_for` only attaches the digest), so `verify_any_pin_provenance`
    // fetches by TAG (`example.com/dep:1.0`) to read the dependency's own
    // image index, then verifies the leaf via the pin's tag+digest reference
    // (`example.com/dep:1.0@sha256:<hex>`), matching `pull_manifest`'s
    // reference-building. Both keys must be seeded independently.

    const LINUX_AMD64_ENTRY: &str = r#"{"os":"linux","architecture":"amd64"}"#;
    const ANY_ENTRY: &str = r#"{"os":"any","architecture":"any"}"#;

    /// Build an image-index body with a single entry at `leaf_digest_hex`
    /// declaring `platform_json`.
    fn image_index_with_entry(leaf_digest_hex: &str, platform_json: &str) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{leaf_digest_hex}","size":1,"platform":{platform_json}}}]}}"#
        )
    }

    /// Seed the stub so `example.com/dep:1.0` (tag-only, no digest — the D5
    /// any-provenance fetch reference) resolves to `body`. The index's own
    /// digest is distinct from any leaf digest used in the same test (`'f'`
    /// is never used as a leaf digest character below).
    fn seed_manifest_by_tag(data: &StubTransportData, body: &str) {
        data.write().manifests.insert(
            "example.com/dep:1.0".to_string(),
            (body.as_bytes().to_vec(), format!("sha256:{}", hex('f'))),
        );
    }

    /// Seed the stub so `example.com/dep:1.0@sha256:<hex>` (the reference
    /// `pin_for` projects for a `platforms`-map dependency) resolves to `body`.
    fn seed_manifest_by_tag_and_digest(data: &StubTransportData, digest_hex: &str, body: &str) {
        data.write().manifests.insert(
            format!("example.com/dep:1.0@sha256:{digest_hex}"),
            (body.as_bytes().to_vec(), format!("sha256:{digest_hex}")),
        );
    }

    /// A `platforms`-map dependency pinning `digest_hex` under the `"any"`
    /// key — the shape `ocx package create --platform any` writes, and the
    /// shape a hand-edited sidecar could forge.
    fn metadata_with_any_pin(digest_hex: &str) -> AuthoringMetadata {
        metadata(&format!(
            r#"{{"identifier":"example.com/dep:1.0","platforms":{{"any":"sha256:{digest_hex}"}}}}"#
        ))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpinned_dependency_rejected() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(r#"{"identifier":"example.com/dep:1.0"}"#);

        let err = verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
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

        let err = verify_dependency_pins(&client, &metadata, &platform("darwin/arm64"))
            .await
            .expect_err("map gap must be rejected");
        assert!(
            matches!(err, PublishGateError::MissingPlatformCoverage { ref platform, .. } if platform == "darwin/arm64"),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    /// D5: an `any`-targeted bundle rejects a direct digest pin — the push
    /// gate re-checks the same invariant `pin_dependencies` enforces at
    /// create time, since a hand-edited sidecar can carry one without ever
    /// going through `ocx package create --platform any`.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_rejects_direct_digest_pin() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &Platform::any())
            .await
            .expect_err("a direct digest pin in an any-targeted bundle must be rejected");
        assert!(
            matches!(err, PublishGateError::DirectDigestPinInAnyTarget { .. }),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    /// D5 provenance check (the terra-gate finding this fix closes): a
    /// hand-edited sidecar claims a leaf is `"any"`-offered via the
    /// `platforms` map, but the dependency's own image index advertises that
    /// leaf under `linux/amd64` only — a forged any-provenance claim must be
    /// rejected, not merely "does the manifest exist" (which `pull_manifest`
    /// alone cannot distinguish from a genuine `any` offer).
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_rejects_pin_not_advertised_as_any() {
        let data = StubTransportData::new();
        seed_manifest_by_tag(&data, &image_index_with_entry(&hex('a'), LINUX_AMD64_ENTRY));
        let client = stub_client(data);
        let metadata = metadata_with_any_pin(&hex('a'));

        let err = verify_dependency_pins(&client, &metadata, &Platform::any())
            .await
            .expect_err("a leaf not advertised as `any` in its own index must be rejected");
        let expected_digest = format!("sha256:{}", hex('a'));
        assert!(
            matches!(err, PublishGateError::AnyPinNotAdvertisedAsAny { ref digest, .. } if *digest == expected_digest),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    /// The honest counterpart: a `platforms`-map pin whose leaf genuinely IS
    /// advertised as `any` in the dependency's own index passes the
    /// provenance check (and the rest of the gate).
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_accepts_pin_advertised_as_any() {
        let data = StubTransportData::new();
        seed_manifest_by_tag(&data, &image_index_with_entry(&hex('a'), ANY_ENTRY));
        seed_manifest_by_tag_and_digest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data);
        let metadata = metadata_with_any_pin(&hex('a'));

        verify_dependency_pins(&client, &metadata, &Platform::any())
            .await
            .expect("a genuinely `any`-offered leaf must pass the provenance check and the gate");
    }

    /// Fail-closed: if the dependency's own tag cannot be fetched at all
    /// (missing, network, auth, ...), the provenance claim is unverifiable
    /// and must be treated as untrusted — never silently accepted.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_fails_closed_when_dependency_tag_unfetchable() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata_with_any_pin(&hex('a'));

        let err = verify_dependency_pins(&client, &metadata, &Platform::any())
            .await
            .expect_err("an unfetchable dependency tag must fail closed");
        assert!(
            matches!(err, PublishGateError::AnyPinProvenanceUnavailable { .. }),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn index_pinned_dependency_rejected() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_INDEX_JSON);
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
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

        verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
            .await
            .expect("manifest pin passes the gate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_manifest_is_not_found() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
            .await
            .expect_err("absent manifest must be rejected");
        assert!(
            matches!(err, PublishGateError::DependencyManifestNotFound { .. }),
            "got: {err}"
        );
        assert_eq!(classify_error(&err), ExitCode::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_dependency_pin_verified() {
        // Two distinct dependencies, each verified independently.
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        data.write().manifests.insert(
            format!("example.com/other@sha256:{}", hex('b')),
            (IMAGE_MANIFEST_JSON.as_bytes().to_vec(), format!("sha256:{}", hex('b'))),
        );
        let client = stub_client(data.clone());
        let metadata = metadata(&format!(
            r#"{{"identifier":"example.com/dep@sha256:{a}"}},{{"identifier":"example.com/other@sha256:{b}"}}"#,
            a = hex('a'),
            b = hex('b'),
        ));

        verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
            .await
            .expect("gate passes");

        let pulls = data
            .read()
            .calls
            .iter()
            .filter(|call| *call == "pull_manifest_raw")
            .count();
        assert_eq!(pulls, 2, "each dependency's pin must be verified");
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

        let err = verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
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

        let err = verify_dependency_pins(&client, &metadata, &platform("linux/amd64"))
            .await
            .expect_err("authentication failure must surface");
        assert!(matches!(err, PublishGateError::Verification { .. }), "got: {err}");
        assert_eq!(classify_error(&err), ExitCode::AuthError);
    }
}
