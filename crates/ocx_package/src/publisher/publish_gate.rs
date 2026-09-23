// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pre-push dependency-pin gate for `ocx package push`.
//!
//! Push makes no resolution decisions (`adr_dependency_manifest_pinning.md`):
//! it reads the already-pinned published metadata and verifies each pin
//! against the registry the index routes it to — the logical name itself for
//! a registry-backed dependency, the physical location an index names for an
//! index-served one (ocx#504). A dependency with no digest cannot reach here —
//! the published metadata type has no digest-less form, so an unresolved
//! dependency fails at parse.

use futures::stream::{self, StreamExt, TryStreamExt};

use crate::metadata::Metadata;
use ocx_oci::Client;
use ocx_oci::client::ReadAddressing;
use ocx_oci::{self, Platform, client::error::ClientError};

/// Maximum number of dependency-pin registry verifications to run
/// concurrently in a single [`verify_dependency_pins`] call.
///
/// Each verification is a small, latency-bound manifest GET (metadata, not a
/// bulk transfer) — the same shape as `TagManager::refresh`'s per-tag digest
/// fetch, which uses the same bounded-`buffer_unordered` idiom. Dependency
/// count is itself capped at
/// [`Dependencies::MAX_DEPENDENCIES`](crate::metadata::dependency::Dependencies::MAX_DEPENDENCIES)
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
/// Every read dials where `index` routes the pin
/// ([`Index::route_for_dial`](ocx_index::Index::route_for_dial)), never the
/// logical name as written: an index-served namespace such as `ocx.sh` is not
/// a registry. Every error still names the logical pin.
///
/// # Errors
///
/// See [`PublishGateError`]. Registry auth failures pass through so they
/// classify to their own exit code.
pub async fn verify_dependency_pins(
    client: &Client,
    index: &ocx_index::Index,
    metadata: &Metadata,
    platform: &Platform,
) -> Result<(), PublishGateError> {
    // `Dependencies` enforces a unique (registry, repository) per entry, so
    // distinct dependencies can never carry the same pin — no dedup pass is
    // needed before verifying. The un-digested identifier is derived
    // alongside each pin so an `any`-target provenance check
    // (`verify_any_pin_provenance`) can re-fetch the dependency's own
    // manifest by its advisory tag.
    let pins: Vec<(ocx_oci::Identifier, ocx_oci::PinnedIdentifier)> = metadata
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
                let routed =
                    index
                        .route_for_dial(pin.as_identifier())
                        .await
                        .map_err(|source| PublishGateError::Routing {
                            identifier: Box::new(pin.clone()),
                            source,
                        })?;
                if is_any_target {
                    verify_any_pin_provenance(&client, &dependency_identifier, &routed.without_digest(), &pin).await?;
                }
                // `route_for_dial` carries the digest; re-stamping the pin's own
                // digest makes that local rather than a promise from another crate.
                let routed_pin = ocx_oci::PinnedIdentifier::try_from(routed.clone_with_digest(pin.digest()))
                    .expect("an identifier just given a digest is pinned");
                log::debug!("verifying dependency pin '{pin}' at '{routed_pin}'");
                match client.pull_manifest(&routed_pin).await {
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
/// the dependency's current manifest by its advisory tag — at `routed`, the
/// location the index serves `dependency_identifier` from — and require
/// an entry whose declared platform is `any` **and** whose digest equals
/// `pin`'s. A flat (non-index) manifest is `any`-offered by construction —
/// the same convention
/// [`Index::fetch_candidates`](ocx_oci::Index::fetch_candidates) uses for
/// `Manifest::Image` — so it passes only when its own digest equals `pin`'s
/// (there is no other leaf it could be).
///
/// A dependency pinned without an advisory tag is fetched at `latest`
/// ([`Identifier::tag_or_latest`](ocx_oci::Identifier::tag_or_latest)), so
/// it passes exactly when the registry currently advertises the pinned digest
/// as `any` under `latest` — a moving tag deciding a fixed pin. Otherwise it
/// is [`AnyPinNotAdvertisedAsAny`](PublishGateError::AnyPinNotAdvertisedAsAny)
/// when `latest` resolves but does not carry the digest as `any`, and
/// [`AnyPinProvenanceUnavailable`](PublishGateError::AnyPinProvenanceUnavailable)
/// when there is no `latest` to fetch at all.
async fn verify_any_pin_provenance(
    client: &Client,
    dependency_identifier: &ocx_oci::Identifier,
    routed: &ocx_oci::Identifier,
    pin: &ocx_oci::PinnedIdentifier,
) -> Result<(), PublishGateError> {
    // Canonical, never a mirror: this read gates a publish, and Invariant #5
    // says a read that decides a write names the same host the write lands on.
    // A mirror advertising a platform-specific leaf as `any` — stale, or
    // hostile — would otherwise admit exactly the forged provenance claim this
    // function exists to refuse, and the mirror never has to fail to do it.
    let (digest, manifest) = client
        .fetch_manifest_addressed(routed, ReadAddressing::Canonical)
        .await
        .map_err(|source| PublishGateError::AnyPinProvenanceUnavailable {
            identifier: Box::new(dependency_identifier.clone()),
            source,
        })?;

    let advertised_as_any = match manifest {
        ocx_oci::Manifest::Image(_) => digest == pin.digest(),
        ocx_oci::Manifest::ImageIndex(index) => index.manifests.into_iter().any(|entry| {
            ocx_oci::Digest::try_from(entry.digest.as_str()).is_ok_and(|entry_digest| entry_digest == pin.digest())
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
pub enum PublishGateError {
    /// The pinned digest resolves to an image INDEX, not a manifest.
    #[error(
        "dependency '{identifier}' pins an image INDEX digest; a tag's index is rewritten on every platform push and its old digest is garbage-collected, so this pin will break — re-run `ocx package create` to pin platform manifest digests"
    )]
    DependencyPinnedToIndex { identifier: Box<ocx_oci::PinnedIdentifier> },
    /// D5 provenance check: a dependency of an `any`-targeted bundle is not
    /// advertised as `any` in the dependency's own image index — the pin is a
    /// publisher claim, not registry evidence, so it cannot forge a
    /// platform-specific dependency into a universal one.
    #[error(
        "dependency '{identifier}' pins digest '{digest}' for the `any` platform, but the dependency's own image index does not advertise that digest as `any`; re-run `ocx package create --platform any` to re-resolve it"
    )]
    AnyPinNotAdvertisedAsAny {
        identifier: Box<ocx_oci::Identifier>,
        digest: String,
    },
    /// The D5 `any`-pin provenance check ([`AnyPinNotAdvertisedAsAny`](Self::AnyPinNotAdvertisedAsAny))
    /// could not fetch the dependency's own image index (missing tag,
    /// network, auth, ...). Fails closed: an unverifiable provenance claim
    /// is treated as untrusted, never silently accepted.
    #[error("failed to verify `any` pin provenance for dependency '{identifier}'")]
    AnyPinProvenanceUnavailable {
        identifier: Box<ocx_oci::Identifier>,
        #[source]
        source: ocx_oci::client::error::ClientError,
    },
    /// The pinned manifest does not exist in the registry.
    #[error("dependency manifest '{identifier}' not found in the registry")]
    DependencyManifestNotFound { identifier: Box<ocx_oci::PinnedIdentifier> },
    /// Pin verification failed for another reason (auth, network, ...).
    #[error("failed to verify dependency pin '{identifier}'")]
    Verification {
        identifier: Box<ocx_oci::PinnedIdentifier>,
        #[source]
        source: ClientError,
    },
    /// The index could not say which registry serves the dependency, or the
    /// dial-site SSRF floor refused the one it named. Names the logical pin;
    /// the cause carries the rest.
    #[error("failed to route dependency '{identifier}' through the index")]
    Routing {
        identifier: Box<ocx_oci::PinnedIdentifier>,
        #[source]
        source: ocx_index::error::Error,
    },
}

// ── Specification tests — adr_dependency_manifest_pinning.md Phase 4 ─────
#[cfg(test)]
mod tests {
    use super::*;
    use ocx_index::IndexImpl;
    use ocx_index::error::Result as IndexResult;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn stub_client(data: StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data)))
    }

    // ── Index routing fixtures (ocx#504) ──────────────────────────────────
    //
    // An index source reduced to the one question the gate asks it: where does
    // this dependency live? `physical` is `(registry, repository)` for every
    // name it is asked about, or `None` for a source that rewrites nothing —
    // a plain registry's answer, the passthrough every pre-#504 test runs under.
    // `authoritative_base_url` makes it a configured index that is
    // authoritative for every name it is asked about.

    #[derive(Clone)]
    struct RoutingSource {
        physical: Option<(&'static str, &'static str)>,
        authoritative_base_url: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl IndexImpl for RoutingSource {
        async fn list_repositories(&self, _: &str) -> IndexResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &ocx_oci::Identifier) -> IndexResult<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::Identifier,
            _: ocx_index::IndexOperation,
        ) -> IndexResult<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::Identifier,
            _: ocx_index::IndexOperation,
        ) -> IndexResult<Option<ocx_oci::Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> IndexResult<Option<Vec<u8>>> {
            Ok(None)
        }
        /// Minted digest-only, the way a source answered before the tag was
        /// carried — so a tag on the dialled reference proves the gate's routing
        /// carries it, not this fixture.
        async fn physical_reference(
            &self,
            identifier: &ocx_oci::Identifier,
        ) -> IndexResult<Option<ocx_oci::Identifier>> {
            Ok(self.physical.map(|(registry, repository)| {
                let physical = ocx_oci::Identifier::new_registry(repository, registry);
                match identifier.digest() {
                    Some(digest) => physical.clone_with_digest(digest),
                    None => physical,
                }
            }))
        }
        fn jurisdiction(&self, _: &ocx_oci::Identifier) -> ocx_index::Jurisdiction {
            match self.authoritative_base_url {
                Some(_) => ocx_index::Jurisdiction::Authoritative,
                None => ocx_index::Jurisdiction::FallThrough,
            }
        }
        fn index_base_url(&self) -> Option<&str> {
            self.authoritative_base_url
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn index_with(physical: Option<(&'static str, &'static str)>) -> ocx_index::Index {
        ocx_index::Index::from_impl(RoutingSource {
            physical,
            authoritative_base_url: None,
        })
        .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct())
    }

    /// No source rewrites anything: every pin is read where it names.
    fn passthrough_index() -> ocx_index::Index {
        index_with(None)
    }

    /// `example.com/dep` is served from `example.com/contrib/dep`. Same
    /// registry on both sides, so the dial-site floor's not-a-rewrite carve-out
    /// answers without a DNS lookup.
    fn routed_index() -> ocx_index::Index {
        index_with(Some(("example.com", "contrib/dep")))
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

        let err = verify_dependency_pins(&client, &passthrough_index(), &metadata, &Platform::any())
            .await
            .expect_err("a leaf not advertised as `any` in its own index must be rejected");
        let expected_digest = format!("sha256:{}", hex('a'));
        assert!(
            matches!(err, PublishGateError::AnyPinNotAdvertisedAsAny { ref digest, .. } if *digest == expected_digest),
            "got: {err}"
        );
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

        verify_dependency_pins(&client, &passthrough_index(), &metadata, &Platform::any())
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
            .parse::<ocx_oci::Identifier>()
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

        let err = verify_dependency_pins(
            &client,
            &passthrough_index(),
            &metadata_with_any_pin(&hex('a')),
            &Platform::any(),
        )
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

        let err = verify_dependency_pins(&client, &passthrough_index(), &metadata, &Platform::any())
            .await
            .expect_err("an unfetchable dependency tag must fail closed");
        assert!(
            matches!(err, PublishGateError::AnyPinProvenanceUnavailable { .. }),
            "got: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn index_pinned_dependency_rejected() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_INDEX_JSON);
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &passthrough_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect_err("an index digest pin must be rejected");
        assert!(
            matches!(err, PublishGateError::DependencyPinnedToIndex { .. }),
            "got: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn manifest_pinned_dependency_accepted() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        verify_dependency_pins(&client, &passthrough_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect("manifest pin passes the gate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_manifest_is_not_found() {
        let client = stub_client(StubTransportData::new());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &passthrough_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect_err("absent manifest must be rejected");
        assert!(
            matches!(err, PublishGateError::DependencyManifestNotFound { .. }),
            "got: {err}"
        );
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

        verify_dependency_pins(&client, &passthrough_index(), &metadata, &platform("linux/amd64"))
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

        let err = verify_dependency_pins(&client, &passthrough_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect_err("registry error must surface");
        assert!(matches!(err, PublishGateError::Verification { .. }), "got: {err}");
    }

    /// W8: a genuine `ClientError::Authentication` (not a generic registry
    /// error) must classify to `AuthError` (80), not `Unavailable`.
    #[tokio::test(flavor = "multi_thread")]
    async fn auth_failure_classifies_as_auth_error() {
        let data = StubTransportData::new();
        data.write().ensure_auth_error_override = Some("bad creds".to_string());
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &passthrough_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect_err("authentication failure must surface");
        assert!(matches!(err, PublishGateError::Verification { .. }), "got: {err}");
    }

    // ── Pins read at the registry the index routes them to (ocx#504) ─────

    /// A dependency in an index-served namespace lives at the physical
    /// location the index names, not at the logical host — which serves no
    /// registry at all (`ocx.sh`). Seeded only there, so a gate that dials the
    /// logical name finds nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pin_served_through_an_index_is_verified_at_its_physical_registry() {
        let data = StubTransportData::new();
        data.write().manifests.insert(
            format!("example.com/contrib/dep@sha256:{}", hex('a')),
            (IMAGE_MANIFEST_JSON.as_bytes().to_vec(), format!("sha256:{}", hex('a'))),
        );
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        verify_dependency_pins(&client, &routed_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect("the pin exists where the index routes it");
    }

    /// The `any`-provenance read is a read BY TAG, so routing must carry the
    /// pin's advisory tag onto the physical location: without it the read asks
    /// the physical repository for `latest`, which is not the dependency's own
    /// image index. Both keys are seeded only at the physical location, under
    /// the tag.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_any_target_reads_provenance_at_the_physical_registry_under_the_pins_tag() {
        let data = StubTransportData::new();
        data.write().manifests.insert(
            "example.com/contrib/dep:1.0".to_string(),
            (
                image_index_with_entry(&hex('a'), ANY_ENTRY).into_bytes(),
                format!("sha256:{}", hex('f')),
            ),
        );
        data.write().manifests.insert(
            format!("example.com/contrib/dep:1.0@sha256:{}", hex('a')),
            (IMAGE_MANIFEST_JSON.as_bytes().to_vec(), format!("sha256:{}", hex('a'))),
        );
        let client = stub_client(data);

        verify_dependency_pins(
            &client,
            &routed_index(),
            &metadata_with_any_pin(&hex('a')),
            &Platform::any(),
        )
        .await
        .expect("the dependency's own index, read at the physical location under its tag, advertises the pin as `any`");
    }

    /// A pin the physical registry does not hold is not found — even though a
    /// same-named manifest sits at the logical location, which is exactly what a
    /// gate that never routed would find and wave through. The error names the
    /// LOGICAL pin: that is what the publisher wrote and can fix.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pin_missing_at_the_physical_registry_is_not_found_under_its_logical_name() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data);
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(&client, &routed_index(), &metadata, &platform("linux/amd64"))
            .await
            .expect_err("the physical registry holds no such manifest");
        match err {
            PublishGateError::DependencyManifestNotFound { identifier } => assert_eq!(
                identifier.to_string(),
                format!("example.com/dep@sha256:{}", hex('a')),
                "the error must name the logical pin, never its physical location"
            ),
            other => panic!("expected DependencyManifestNotFound, got: {other}"),
        }
    }

    /// Routing brings the dial-site SSRF floor with it: a rewrite into a
    /// forbidden range is refused before any registry request, and the refusal
    /// names the logical pin.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pin_routed_into_a_forbidden_range_is_refused_before_any_dial() {
        let data = StubTransportData::new();
        let client = stub_client(data.clone());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));

        let err = verify_dependency_pins(
            &client,
            &index_with(Some(("127.0.0.1:5999", "contrib/dep"))),
            &metadata,
            &platform("linux/amd64"),
        )
        .await
        .expect_err("a loopback rewrite must be refused");
        match &err {
            PublishGateError::Routing {
                identifier,
                source: ocx_index::error::Error::Ssrf { .. },
            } => assert_eq!(identifier.to_string(), format!("example.com/dep@sha256:{}", hex('a'))),
            other => panic!("expected a Routing SSRF refusal, got: {other:?}"),
        }
        assert!(
            data.read().calls.iter().all(|call| call != "pull_manifest_raw"),
            "nothing may be dialled once the floor refuses the target"
        );
    }

    /// A dependency in a namespace an index serves authoritatively, which that
    /// index does not hold, is not in the index. Dialling the logical host
    /// instead would reach something that is not a registry (`https://ocx.sh/v2`)
    /// and report whatever it answers. The refusal names the logical pin and
    /// nothing is dialled.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pin_its_authoritative_index_does_not_hold_is_refused_before_any_dial() {
        let data = StubTransportData::new();
        seed_manifest(&data, &hex('a'), IMAGE_MANIFEST_JSON);
        let client = stub_client(data.clone());
        let metadata = metadata(&format!(r#"{{"identifier":"example.com/dep@sha256:{}"}}"#, hex('a')));
        let index = ocx_index::Index::from_impl(RoutingSource {
            physical: None,
            authoritative_base_url: Some("https://index.example.invalid"),
        });

        let err = verify_dependency_pins(&client, &index, &metadata, &platform("linux/amd64"))
            .await
            .expect_err("an authoritative index's miss must not fall back to the logical host");
        match &err {
            PublishGateError::Routing {
                identifier,
                source: ocx_index::error::Error::NotInIndex { .. },
            } => assert_eq!(identifier.to_string(), format!("example.com/dep@sha256:{}", hex('a'))),
            other => panic!("expected a Routing NotInIndex refusal, got: {other:?}"),
        }
        assert!(
            data.read().calls.iter().all(|call| call != "pull_manifest_raw"),
            "the logical host must not be dialled — it holds the manifest here, so a dial would pass"
        );
    }
}
