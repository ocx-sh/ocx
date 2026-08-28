// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pre-push dependency-pin gate for `ocx package push`.
//!
//! Push makes no resolution decisions (`adr_dependency_manifest_pinning.md`):
//! it reads the already-pinned published metadata and verifies each pin
//! against the registry it names. A dependency with no digest cannot reach
//! here — the published metadata type has no digest-less form, so an
//! unresolved dependency fails at parse.

use futures::stream::{self, StreamExt, TryStreamExt};

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci::client::ReadAddressing;
use crate::oci::{self, Platform, client::error::ClientError};
use crate::package::metadata::Metadata;
use crate::{log, oci::Client};

/// Maximum number of dependency-pin registry verifications to run
/// concurrently in a single [`verify_dependency_pins`] call.
///
/// Each verification is a small, latency-bound manifest GET (metadata, not a
/// bulk transfer) — the same shape as `TagManager::refresh`'s per-tag digest
/// fetch, which uses the same bounded-`buffer_unordered` idiom. Dependency
/// count is itself capped at
/// [`Dependencies::MAX_DEPENDENCIES`](crate::package::metadata::dependency::Dependencies::MAX_DEPENDENCIES)
/// (256), so this only needs to bound simultaneous in-flight requests per
/// push, not overall fan-out; 16 keeps a polite per-registry burst while
/// still parallelizing the common case of a handful of cross-registry deps.
const DEPENDENCY_PIN_VERIFY_CONCURRENCY: usize = 16;

/// Verify every dependency pin of the published `metadata` for the single
/// target `platform`.
///
/// Three checks:
///
/// 1. for an `any`-targeted bundle, every pin is a *genuine* `any` offer in
///    the dependency's own image index ([`verify_any_pin_provenance`]) — a
///    leaf manifest carries no platform descriptor, so nothing about the pin
///    itself says whether the dependency runs everywhere or only on one
///    platform, and a sidecar cannot be taken at its word for it;
/// 2. every pin resolves in its registry to an image **manifest** — an image
///    INDEX digest is rejected because a tag's index is rewritten (and its
///    old digest garbage-collected) on every platform push, so such a pin is
///    guaranteed to break;
/// 3. that resolution succeeds at all — verified via
///    [`Client::pull_manifest`], which also authenticates per registry, so
///    cross-registry dependencies are covered.
///
/// All three run concurrently per dependency (bounded by
/// [`DEPENDENCY_PIN_VERIFY_CONCURRENCY`]); the first failure short-circuits
/// the rest. Check 1 is skipped entirely for a concrete-target bundle — no
/// extra network beyond the fetch checks 2 and 3 already make.
///
/// # Errors
///
/// See [`PublishGateError`]. Registry auth failures pass through so they
/// classify to their own exit code.
pub async fn verify_dependency_pins(
    client: &Client,
    metadata: &Metadata,
    platform: &Platform,
) -> Result<(), PublishGateError> {
    // `Dependencies` enforces a unique (registry, repository) per entry, so
    // distinct dependencies can never carry the same pin — no dedup pass is
    // needed before verifying. The un-digested identifier is derived
    // alongside each pin so an `any`-target provenance check
    // (`verify_any_pin_provenance`) can re-fetch the dependency's own
    // manifest by its advisory tag.
    let pins: Vec<(oci::Identifier, oci::PinnedIdentifier)> = metadata
        .dependencies()
        .iter()
        .map(|dep| (dep.identifier.without_digest(), dep.identifier.clone()))
        .collect();

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
/// (`adr_platform_model_unification.md` D5): a dependency pin is a
/// sidecar-authored claim, not registry evidence. Because a leaf manifest
/// carries no platform descriptor, a hand-edited sidecar could pin a
/// platform-specific leaf in a bundle published as universal, and nothing in
/// the metadata itself could detect the forgery.
///
/// This re-derives the fact from the dependency's own image index: fetch
/// `dependency_identifier`'s current manifest by its advisory tag and require
/// an entry whose declared platform is `any` **and** whose digest equals
/// `pin`'s. A flat (non-index) manifest is `any`-offered by construction —
/// the same convention
/// [`Index::fetch_candidates`](crate::oci::Index::fetch_candidates) uses for
/// `Manifest::Image` — so it passes only when its own digest equals `pin`'s
/// (there is no other leaf it could be).
///
/// A dependency pinned without an advisory tag is fetched at `latest`
/// ([`Identifier::tag_or_latest`](crate::oci::Identifier::tag_or_latest)), so
/// it passes exactly when the registry currently advertises the pinned digest
/// as `any` under `latest` — a moving tag deciding a fixed pin. Otherwise it
/// is [`AnyPinNotAdvertisedAsAny`](PublishGateError::AnyPinNotAdvertisedAsAny)
/// when `latest` resolves but does not carry the digest as `any`, and
/// [`AnyPinProvenanceUnavailable`](PublishGateError::AnyPinProvenanceUnavailable)
/// when there is no `latest` to fetch at all.
async fn verify_any_pin_provenance(
    client: &Client,
    dependency_identifier: &oci::Identifier,
    pin: &oci::PinnedIdentifier,
) -> Result<(), PublishGateError> {
    // Canonical, never a mirror: this read gates a publish, and Invariant #5
    // says a read that decides a write names the same host the write lands on.
    // A mirror advertising a platform-specific leaf as `any` — stale, or
    // hostile — would otherwise admit exactly the forged provenance claim this
    // function exists to refuse, and the mirror never has to fail to do it.
    let (digest, manifest) = client
        .fetch_manifest_addressed(dependency_identifier, ReadAddressing::Canonical)
        .await
        .map_err(|source| PublishGateError::AnyPinProvenanceUnavailable {
            identifier: Box::new(dependency_identifier.clone()),
            source,
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
    /// The pinned digest resolves to an image INDEX, not a manifest.
    #[error(
        "dependency '{identifier}' pins an image INDEX digest; a tag's index is rewritten on every platform push and its old digest is garbage-collected, so this pin will break — re-run `ocx package create` to pin platform manifest digests"
    )]
    DependencyPinnedToIndex { identifier: Box<oci::PinnedIdentifier> },
    /// D5 provenance check: a dependency of an `any`-targeted bundle is not
    /// advertised as `any` in the dependency's own image index — the pin is a
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
            PublishGateError::DependencyPinnedToIndex { .. } | PublishGateError::AnyPinNotAdvertisedAsAny { .. } => {
                Some(ExitCode::DataError)
            }
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

    fn metadata(deps_json: &str) -> Metadata {
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
    // A pinned dependency keeps its advisory tag on `identifier` (create only
    // attaches the digest), so `verify_any_pin_provenance` fetches by TAG
    // (`example.com/dep:1.0`) to read the dependency's own image index, then
    // verifies the leaf via the pin's tag+digest reference
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

    /// Seed the stub so `example.com/dep:1.0@sha256:<hex>` (the reference a
    /// tag-bearing pinned dependency resolves to) resolves to `body`.
    fn seed_manifest_by_tag_and_digest(data: &StubTransportData, digest_hex: &str, body: &str) {
        data.write().manifests.insert(
            format!("example.com/dep:1.0@sha256:{digest_hex}"),
            (body.as_bytes().to_vec(), format!("sha256:{digest_hex}")),
        );
    }

    /// A dependency pinned to `digest_hex` with its advisory tag intact — the
    /// shape `ocx package create` writes, and the shape a hand-edited sidecar
    /// could forge by substituting a platform-specific leaf digest.
    fn metadata_with_any_pin(digest_hex: &str) -> Metadata {
        metadata(&format!(
            r#"{{"identifier":"example.com/dep:1.0@sha256:{digest_hex}"}}"#
        ))
    }

    /// D5 provenance check: a hand-edited sidecar pins a leaf in a bundle
    /// published as `any`, but the dependency's own image index advertises
    /// that leaf under `linux/amd64` only — a forged any-provenance claim must
    /// be rejected, not merely "does the manifest exist" (which
    /// `pull_manifest` alone cannot distinguish from a genuine `any` offer).
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

    /// The honest counterpart, and the sanctioned loosening: push used to
    /// refuse a bare `@digest` in an `any`-targeted bundle outright, because
    /// the pin alone could not be shown to be `any`-offered. That structural
    /// pre-filter is subsumed by the registry-verified provenance check above
    /// — a digest the dependency's own index advertises as `any` IS
    /// `any`-offered, whatever shape it was written in — so the gate now
    /// accepts it. Create still refuses one it did not resolve itself
    /// (`dependency_pinning::reject_digest_pins_in_any_target`): it has no
    /// registry evidence to substitute.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_accepts_a_bare_digest_the_index_advertises_as_any() {
        let data = StubTransportData::new();
        seed_manifest_by_tag(&data, &image_index_with_entry(&hex('a'), ANY_ENTRY));
        seed_manifest_by_tag_and_digest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data);
        let metadata = metadata_with_any_pin(&hex('a'));

        verify_dependency_pins(&client, &metadata, &Platform::any())
            .await
            .expect("a genuinely `any`-offered leaf must pass the provenance check and the gate");
    }

    /// The provenance read decides whether a publish is allowed, so it must
    /// read the **canonical** registry — Invariant #5, the same rule the
    /// cascade prelude and the blocker probe already follow.
    ///
    /// The two hosts are seeded with *different* answers, which is what makes
    /// this discriminate: the mirror advertises the leaf as `any` (the cheap
    /// half of the attack — a stale or hostile mirror never has to fail), the
    /// canonical registry advertises it as `linux/amd64` only. A mirrored read
    /// therefore admits a forged any-provenance claim and the gate passes; a
    /// canonical read rejects it. Asserting only that the mirror 404s would
    /// pass for a mirrored implementation too, via the fail-closed arm.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_any_provenance_read_uses_the_canonical_registry_not_a_mirror() {
        let data = StubTransportData::new();
        let client = stub_client(data.clone()).with_test_mirror("example.com", "mirror.invalid", "upstream");

        let dependency = "example.com/dep:1.0"
            .parse::<oci::Identifier>()
            .expect("identifier parses");
        let mirror_reference = client.read_reference(&dependency, ReadAddressing::Mirrored).to_string();
        assert_ne!(
            mirror_reference, "example.com/dep:1.0",
            "the fixture only discriminates while the two hosts differ"
        );

        // The mirror lies: it offers the leaf as `any`. The canonical registry
        // carries the truth: that leaf is `linux/amd64` only.
        data.write().manifests.insert(
            mirror_reference,
            (
                image_index_with_entry(&hex('a'), ANY_ENTRY).into_bytes(),
                format!("sha256:{}", hex('f')),
            ),
        );
        seed_manifest_by_tag(&data, &image_index_with_entry(&hex('a'), LINUX_AMD64_ENTRY));

        // The leaf resolves on BOTH hosts, so the pin-existence check that
        // follows the provenance check can never be what fails. Without this
        // the test would go red on a missing mirror leaf and pass for a
        // mirrored implementation — red for the wrong reason is not a proof.
        seed_manifest_by_tag_and_digest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        data.write().manifests.insert(
            format!("mirror.invalid/upstream/dep:1.0@sha256:{}", hex('a')),
            (IMAGE_MANIFEST_JSON.as_bytes().to_vec(), format!("sha256:{}", hex('a'))),
        );

        let err = verify_dependency_pins(&client, &metadata_with_any_pin(&hex('a')), &Platform::any())
            .await
            .expect_err("a mirror-advertised `any` claim must not admit a publish the canonical registry refuses");
        assert!(
            matches!(err, PublishGateError::AnyPinNotAdvertisedAsAny { .. }),
            "got: {err}"
        );
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
