// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Remote registry publishing facade.
//!
//! [`Publisher`] owns an OCI [`Client`](crate::oci::Client) and exposes
//! high-level push operations, including cascade tag management.
//! It is the publishing counterpart to [`PackageManager`](crate::package_manager::PackageManager),
//! which handles local-store operations.

pub mod copy;
mod layer_ref;
pub mod publish_gate;

pub use copy::{CopiedPlatform, CopyError, CopyErrorKind, CopyOutcome, CopyRequest, Disposition};
pub use layer_ref::{ArchiveMediaType, LayerRef, LayerRefParseError};
pub use publish_gate::{PublishGateError, verify_dependency_pins};

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    log, oci,
    oci::client::ReadAddressing,
    package::{self, description::Description, info::Info, version::Version},
    prelude::*,
};

/// Remote registry publishing facade.
///
/// Holds an OCI client and provides push operations with optional
/// cascade tag management. Does not depend on local file structure
/// or index — only on the remote registry via the client.
#[derive(Clone)]
pub struct Publisher {
    client: oci::Client,
}

/// Outcome of a successful package push.
///
/// Surfaced so callers (notably the `ocx package push` command) can emit a
/// structured report; `ocx-mirror pipeline push` parses this report to record
/// the cascade tags written and to distinguish a real publish from a no-op.
///
/// `#[non_exhaustive]`: this is an in-process type, not a wire type — the
/// parsed cross-tool contract is `PushReport`. But ocx-mirror takes `ocx_lib`
/// as a path dependency, so a later field would break it at a struct literal.
/// Construct through [`PushOutcome::new`] instead.
#[derive(Debug)]
#[non_exhaustive]
pub struct PushOutcome {
    /// Digest of the pushed multi-platform image index. For a multi-platform
    /// fan-out this is the primary tag's index digest after the LAST platform
    /// merge — the final state of the tag.
    pub manifest_digest: oci::Digest,
    /// Rolling cascade tags written in addition to the primary version tag
    /// (e.g. `3.28`, `3`, `latest`). Empty for a non-cascade push. For a
    /// multi-platform fan-out this is the ordered union across platforms.
    pub cascade_tags: Vec<String>,
    /// Digest-named `__ocx.keep.<algorithm>-<hex>` tags written by this push, in push order,
    /// deduped: one per *distinct platform manifest*, not one per `Info`. The
    /// tag names the platform manifest's digest, and that manifest is the
    /// metadata config blob plus the layers — the platform field is not part
    /// of it. Two platforms built from identical metadata over identical
    /// layers (a noarch bundle, a Rosetta alias) therefore share one manifest,
    /// hence one tag. Empty under `--no-keep-tag`, and empty for any
    /// platform whose entry the merged index did not carry.
    pub keep_tags: Vec<String>,
    /// The platform manifest digest each pushed platform landed on, in push
    /// order. **Independent of keep tagging** — this is `push --sign`'s inline
    /// signing input, so it is populated under `--no-keep-tag` exactly as it
    /// is with the keep tag on.
    ///
    /// Never the index digest: [`manifest_digest`](Self::manifest_digest)
    /// names the tag's image index, which is rewritten on every platform
    /// merge, while a signature has to name the immutable object it covers.
    ///
    /// Two platforms built from identical metadata over identical layers share
    /// one manifest and therefore one digest; the list keys on platform, so
    /// both rows appear carrying the same value. A platform whose entry the
    /// merged index did not carry is **omitted**, never faked — the same rule
    /// [`keep_tags`](Self::keep_tags) already follows, and from the same
    /// descriptor lookup.
    pub platform_digests: Vec<(oci::Platform, oci::Digest)>,
    /// Counts of layer-push outcomes (mounted/uploaded/verified), summed over
    /// every platform this push fanned out to. Layer blobs only — the config
    /// blob and manifest are not layers and are excluded. An `uploaded` count
    /// may still have HEAD-skipped an already-present blob inside
    /// `push_blob`'s blob-exists short-circuit.
    pub layer_counts: oci::LayerCounts,
}

impl PushOutcome {
    /// Construct an outcome. The only constructor available outside this
    /// crate, because the struct is `#[non_exhaustive]`.
    pub fn new(
        manifest_digest: oci::Digest,
        cascade_tags: Vec<String>,
        keep_tags: Vec<String>,
        platform_digests: Vec<(oci::Platform, oci::Digest)>,
        layer_counts: oci::LayerCounts,
    ) -> Self {
        Self {
            manifest_digest,
            cascade_tags,
            keep_tags,
            platform_digests,
            layer_counts,
        }
    }
}

impl Publisher {
    pub fn new(client: oci::Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &oci::Client {
        &self.client
    }

    /// Pre-authenticate against the registry for `identifier` with Push scope.
    ///
    /// Call at the start of a publishing command to fail fast on credential
    /// issues before reading files or doing any other preparation.
    pub async fn ensure_auth(&self, identifier: &oci::Identifier) -> Result<()> {
        self.client.ensure_auth(identifier, oci::RegistryOperation::Push).await
    }

    /// Push a package — one [`Info`] per target platform — with one or more
    /// layers to the registry.
    ///
    /// Each `LayerRef::File` is uploaded as a new blob. Each `LayerRef::Digest`
    /// is verified to exist via HEAD. The manifest contains one descriptor per
    /// layer in the order provided. Platforms are pushed **sequentially**:
    /// the per-tag index merge is a read-modify-write, so concurrent merges
    /// would race.
    ///
    /// When `build_meta` is `Some`, each identifier's tag is parsed as a
    /// [`Version`] and the build segment is attached before push (the infos
    /// share one identifier by construction, so every platform lands on the
    /// same tag). Errors if the tag does not parse, lacks `X.Y.Z` form, or
    /// already carries build metadata.
    ///
    /// When `keep_tag` is `true` (the default from `ocx package push`),
    /// each pushed platform manifest additionally gets a digest-named
    /// `__ocx.keep.<algorithm>-<hex>` tag pointing directly at it — a pure registry-side
    /// deletion safety net (`adr_index_indirection.md` Decision E). Applies
    /// only to the platform manifest pushed by this call, never to
    /// pre-existing entries the merge picks up from the registry.
    ///
    /// `annotations` are publisher-stated OCI annotations (`ocx package push
    /// --annotation`) written onto the image index of every tag this push
    /// touches. An empty map writes nothing at all.
    pub async fn push(
        &self,
        infos: Vec<Info>,
        layers: &[LayerRef],
        build_meta: Option<&str>,
        keep_tag: bool,
        annotations: &BTreeMap<String, String>,
    ) -> Result<PushOutcome> {
        let infos = apply_build_meta_all(infos, build_meta)?;
        let mut manifest_digest: Option<oci::Digest> = None;
        let mut keep_tags: Vec<String> = Vec::new();
        let mut platform_digests: Vec<(oci::Platform, oci::Digest)> = Vec::new();
        let mut layer_counts = oci::LayerCounts::default();
        for info in infos {
            log::info!(
                "pushing package with identifier {} (platform {})",
                info.identifier,
                info.platform
            );
            let identifier = info.identifier.clone();
            let platform = info.platform.clone();
            let (digest, manifest, counts) = self.client.push_package(info, layers, annotations).await?;
            layer_counts += counts;
            // Hoisted out of the keep-tag branch on purpose: this is the same
            // descriptor `push_keep_tag` reads, and `platform_digests` has to
            // be there under `--no-keep-tag` too.
            if let Some(platform_digest) = oci::manifest::platform_manifest_digest(&manifest, &platform) {
                platform_digests.push((platform.clone(), platform_digest));
            }
            if keep_tag
                && let Some(tag) = self.client.push_keep_tag(&identifier, &manifest, &platform).await?
                && !keep_tags.contains(&tag)
            {
                keep_tags.push(tag);
            }
            manifest_digest = Some(digest);
        }
        Ok(PushOutcome {
            manifest_digest: manifest_digest.ok_or(crate::package::error::Error::EmptyPushSet)?,
            cascade_tags: Vec::new(),
            keep_tags,
            platform_digests,
            layer_counts,
        })
    }

    /// Push a package — one [`Info`] per target platform — with cascade tag
    /// management.
    ///
    /// `existing_versions` is the set of versions already in the registry,
    /// used to compute which rolling tags each platform's push should update
    /// (cascade blocker checks are platform-aware). The same `build_meta`
    /// semantics as [`Self::push`] apply. The outcome's `cascade_tags` is the
    /// ordered union across platforms. `keep_tag` and `annotations` have
    /// the same meaning as in [`Self::push`].
    pub async fn push_cascade(
        &self,
        infos: Vec<Info>,
        layers: &[LayerRef],
        existing_versions: BTreeSet<Version>,
        build_meta: Option<&str>,
        keep_tag: bool,
        annotations: &BTreeMap<String, String>,
    ) -> Result<PushOutcome> {
        let infos = apply_build_meta_all(infos, build_meta)?;
        let mut manifest_digest: Option<oci::Digest> = None;
        let mut cascade_tags: Vec<String> = Vec::new();
        let mut keep_tags: Vec<String> = Vec::new();
        let mut platform_digests: Vec<(oci::Platform, oci::Digest)> = Vec::new();
        let mut layer_counts = oci::LayerCounts::default();
        for info in infos {
            log::info!(
                "pushing package with identifier {} (cascade, platform {})",
                info.identifier,
                info.platform
            );
            let version = Version::parse(info.identifier.tag_or_latest()).ok_or_else(|| {
                crate::package::error::Error::VersionInvalid(info.identifier.tag_or_latest().to_string())
            })?;
            let platform = info.platform.clone();
            let outcome = package::cascade::push_with_cascade(
                &self.client,
                info,
                layers,
                existing_versions.clone(),
                &version,
                keep_tag,
                annotations,
            )
            .await?;
            manifest_digest = Some(outcome.index_digest);
            layer_counts += outcome.layer_counts;
            for tag in outcome.cascade_tags {
                if !cascade_tags.contains(&tag) {
                    cascade_tags.push(tag);
                }
            }
            if let Some(tag) = outcome.keep_tag
                && !keep_tags.contains(&tag)
            {
                keep_tags.push(tag);
            }
            if let Some(platform_digest) = outcome.platform_digest {
                platform_digests.push((platform, platform_digest));
            }
        }
        Ok(PushOutcome {
            manifest_digest: manifest_digest.ok_or(crate::package::error::Error::EmptyPushSet)?,
            cascade_tags,
            keep_tags,
            platform_digests,
            layer_counts,
        })
    }

    /// Push a complete description artifact to the `__ocx.desc` tag.
    pub async fn push_description(&self, identifier: &oci::Identifier, description: &Description) -> Result<()> {
        log::debug!("Pushing description for {}", identifier);
        self.client.push_description(identifier, description).await?;
        Ok(())
    }

    /// Pull the existing description from the `__ocx.desc` tag, from the
    /// canonical registry.
    ///
    /// Returns `Ok(None)` if no description exists yet.
    ///
    /// Canonical because every caller of this form writes back what it returns —
    /// `package copy --description`, `package description push --from`, and the merge in
    /// `package description push` (invariant 5, `subsystem-oci.md`). A read that only
    /// renders is [`pull_description_mirrored`](Self::pull_description_mirrored).
    pub async fn pull_description(&self, identifier: &oci::Identifier, temp_dir: &Path) -> Result<Option<Description>> {
        Ok(self.client.pull_description(identifier, temp_dir).await?)
    }

    /// [`pull_description`](Self::pull_description) served by a configured
    /// mirror.
    ///
    /// Only for a description nothing is written from — `ocx package description pull`
    /// renders one and stops. Named rather than implied, because nothing in a
    /// call site's shape says whether its answer will back a write.
    pub async fn pull_description_mirrored(
        &self,
        identifier: &oci::Identifier,
        temp_dir: &Path,
    ) -> Result<Option<Description>> {
        Ok(self
            .client
            .pull_description_addressed(identifier, temp_dir, ReadAddressing::Mirrored)
            .await?)
    }

    /// The cascade prelude: which tags the push target already publishes.
    ///
    /// Canonical, never a mirror. Callers feed these tags straight to
    /// [`push_cascade`](Self::push_cascade), so this listing decides which
    /// rolling tags get re-pointed on the canonical registry — deciding that
    /// from a mirror is the Invariant #5 / CWE-345 fail-open the copy path
    /// already fixed, and a stale mirror missing a repository the canonical
    /// registry does publish would silently move `latest` backwards.
    ///
    /// A repository nobody has pushed to yet answers with a 404, which is the
    /// empty list, not a failure — `Client::list_tags_or_empty_addressed`
    /// carries why that fold is exactly this narrow.
    pub async fn list_tags(&self, identifier: oci::Identifier) -> Result<Vec<String>> {
        self.client
            .list_tags_or_empty_addressed(identifier, ReadAddressing::Canonical)
            .await
    }

    /// Parses a list of tag strings into a set of valid versions,
    /// skipping tags that are not valid versions.
    pub fn parse_versions(tags: &[String]) -> BTreeSet<Version> {
        tags.iter().filter_map(|t| Version::parse(t)).collect()
    }
}

/// Apply [`apply_build_meta`] to every [`Info`] of a fan-out set.
///
/// The infos share one identifier (only metadata + platform differ), and the
/// build segment is a fixed string computed once by the caller — every
/// platform therefore lands on the same tag.
fn apply_build_meta_all(infos: Vec<Info>, build_meta: Option<&str>) -> Result<Vec<Info>> {
    infos
        .into_iter()
        .map(|info| apply_build_meta(info, build_meta))
        .collect()
}

/// If `build_meta` is `Some`, parse the identifier's tag, attach the build
/// segment, and return an [`Info`] whose identifier carries the new tag.
fn apply_build_meta(mut info: Info, build_meta: Option<&str>) -> Result<Info> {
    let Some(build) = build_meta else { return Ok(info) };
    let tag = info.identifier.tag_or_latest();
    let version = Version::parse(tag).ok_or_else(|| crate::package::error::Error::VersionInvalid(tag.to_string()))?;
    let with_build = version.with_build(build).map_err(crate::package::error::Error::from)?;
    info.identifier = info.identifier.clone_with_tag(with_build.to_string());
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::{
        Entrypoints, Metadata,
        bundle::{self, Bundle},
        dependency, env as metadata_env,
    };

    fn test_info(tag: &str) -> Info {
        let identifier = oci::Identifier::new_registry("ocx", "ocx.sh").clone_with_tag(tag);
        let metadata = Metadata::Bundle(Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        Info {
            identifier,
            metadata,
            platform: "linux/amd64".parse().expect("platform parses"),
        }
    }

    #[test]
    fn none_returns_info_unchanged() {
        let info = test_info("mirror-0.3.0-dev");
        let out = apply_build_meta(info.clone(), None).expect("no-op succeeds");
        assert_eq!(out.identifier.tag_or_latest(), "mirror-0.3.0-dev");
    }

    #[test]
    fn attaches_build_meta_to_variant_prerelease() {
        let info = test_info("mirror-0.3.0-dev");
        let out = apply_build_meta(info, Some("20260514120000")).expect("attach succeeds");
        // Display normalizes `+` to `_` per OCI tag rules; clone_with_tag does the same.
        assert_eq!(out.identifier.tag_or_latest(), "mirror-0.3.0-dev_20260514120000");
    }

    #[test]
    fn attaches_build_meta_to_bare_patch_version() {
        let info = test_info("0.3.0");
        let out = apply_build_meta(info, Some("20260514120000")).expect("attach succeeds");
        assert_eq!(out.identifier.tag_or_latest(), "0.3.0_20260514120000");
    }

    #[test]
    fn rejects_tag_that_already_carries_build_meta() {
        let info = test_info("0.3.0-dev_alreadyhere");
        let err = apply_build_meta(info, Some("20260514120000")).expect_err("must reject double build meta");
        let msg = err.to_string();
        assert!(msg.contains("already has build metadata"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_tag_that_is_not_a_valid_version() {
        let info = test_info("latest");
        let err = apply_build_meta(info, Some("20260514120000")).expect_err("must reject non-version tag");
        let msg = err.to_string();
        assert!(msg.contains("invalid package version"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_tag_that_lacks_patch_segment() {
        let info = test_info("1.2");
        let err = apply_build_meta(info, Some("20260514120000")).expect_err("must reject X.Y tag");
        let msg = err.to_string();
        assert!(msg.contains("X.Y.Z"), "unexpected error: {msg}");
    }

    // ── Multi-platform fan-out — adr_dependency_manifest_pinning.md ──────

    #[test]
    fn build_meta_all_lands_every_platform_on_the_same_tag() {
        let mut mac = test_info("0.3.0");
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        let infos =
            apply_build_meta_all(vec![test_info("0.3.0"), mac], Some("20260514120000")).expect("attach succeeds");
        let tags: Vec<_> = infos.iter().map(|info| info.identifier.tag_or_latest()).collect();
        assert_eq!(tags, vec!["0.3.0_20260514120000", "0.3.0_20260514120000"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_fan_out_set_is_an_error() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(
            StubTransportData::new(),
        ))));
        let err = publisher
            .push(Vec::new(), &[], None, false, &BTreeMap::new())
            .await
            .expect_err("empty set");
        assert!(err.to_string().contains("at least one target platform"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fan_out_merges_every_platform_into_the_primary_index() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        let mut mac = test_info("1.0.0");
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        let outcome = publisher
            .push(vec![test_info("1.0.0"), mac], &[], None, false, &BTreeMap::new())
            .await
            .expect("fan-out push succeeds");

        // The captured primary-tag index must carry BOTH platform entries —
        // the second (sequential) merge read the first platform back and
        // appended, never clobbered.
        let inner = data.read();
        let (index_bytes, digest) = inner
            .manifests
            .get("ocx.sh/ocx:1.0.0")
            .expect("primary tag index captured");
        let index: serde_json::Value = serde_json::from_slice(index_bytes).expect("index parses");
        let platforms: Vec<String> = index["manifests"]
            .as_array()
            .expect("manifests array")
            .iter()
            .map(|entry| {
                format!(
                    "{}/{}",
                    entry["platform"]["os"].as_str().unwrap_or("?"),
                    entry["platform"]["architecture"].as_str().unwrap_or("?")
                )
            })
            .collect();
        assert_eq!(platforms, vec!["linux/amd64", "darwin/arm64"]);
        assert_eq!(
            outcome.manifest_digest.to_string(),
            *digest,
            "outcome digest must be the final (last-merge) index digest"
        );
    }

    // ── keep_tag gating — adr_index_indirection.md Decision E ───────

    #[tokio::test(flavor = "multi_thread")]
    async fn keep_tag_true_pushes_the_sha256_dot_hex_tag() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        let outcome = publisher
            .push(vec![test_info("1.0.0")], &[], None, true, &BTreeMap::new())
            .await
            .expect("push succeeds");

        assert_eq!(
            outcome.keep_tags.len(),
            1,
            "keep_tag=true must report exactly one written tag: {:?}",
            outcome.keep_tags
        );
        let reported = &outcome.keep_tags[0];
        assert!(
            reported.starts_with("__ocx.keep.sha256-"),
            "unexpected tag shape: {reported}"
        );
        let inner = data.read();
        assert!(
            inner.manifests.keys().any(|key| key.ends_with(&format!(":{reported}"))),
            "the reported tag must be the one on the wire: reported {reported}, wire {:?}",
            inner.manifests.keys().collect::<Vec<_>>()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fan_out_reports_one_keep_tag_per_platform_in_push_order() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        let mut mac = test_info("1.0.0");
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        // The keep tag names the *platform manifest* digest, and that
        // manifest is the metadata config blob plus the layers — neither of
        // which the platform field touches. Two platforms carrying identical
        // metadata therefore share one digest and one keep tag (a Rosetta
        // alias is the real-world case). Diverge the metadata so this fan-out
        // produces the two distinct manifests the assertion is about.
        let Metadata::Bundle(ref mut bundle) = mac.metadata;
        bundle.strip_components = Some(1);

        let outcome = publisher
            .push(vec![test_info("1.0.0"), mac], &[], None, true, &BTreeMap::new())
            .await
            .expect("fan-out push succeeds");

        assert_eq!(
            outcome.keep_tags.len(),
            2,
            "a two-platform fan-out writes one keep tag per distinct platform manifest: {:?}",
            outcome.keep_tags
        );
        assert_ne!(
            outcome.keep_tags[0], outcome.keep_tags[1],
            "each platform manifest has its own digest"
        );
        let inner = data.read();
        for tag in &outcome.keep_tags {
            assert!(
                inner.manifests.keys().any(|key| key.ends_with(&format!(":{tag}"))),
                "reported tag {tag} missing from the wire: {:?}",
                inner.manifests.keys().collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn platforms_sharing_one_manifest_report_a_single_keep_tag() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        // Identical metadata, identical (empty) layers, two platforms — the
        // real-world Rosetta-alias / noarch-bundle shape. Both index entries
        // point at the same leaf manifest, so both platforms yield the same
        // keep tag and the report must carry it once.
        let mut alias = test_info("1.0.0");
        alias.platform = "darwin/arm64".parse().expect("platform parses");

        let outcome = publisher
            .push(vec![test_info("1.0.0"), alias], &[], None, true, &BTreeMap::new())
            .await
            .expect("fan-out push succeeds");

        assert_eq!(
            outcome.keep_tags.len(),
            1,
            "platforms sharing one manifest digest share one keep tag: {:?}",
            outcome.keep_tags
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_fan_out_reports_each_tag_once() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        // Both platforms compute the same rolling tags, and (identical
        // metadata) the same platform-manifest digest — the cascade loop must
        // report each of them once, not once per `Info`.
        let mut alias = test_info("1.0.0");
        alias.platform = "darwin/arm64".parse().expect("platform parses");

        let outcome = publisher
            .push_cascade(
                vec![test_info("1.0.0"), alias],
                &[],
                BTreeSet::new(),
                None,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade fan-out push succeeds");

        assert_eq!(
            outcome.cascade_tags.clone().unique_clone(),
            outcome.cascade_tags,
            "cascade tags must not repeat across platforms: {:?}",
            outcome.cascade_tags
        );
        assert!(
            !outcome.cascade_tags.is_empty(),
            "a 1.0.0 cascade push writes rolling tags"
        );
        assert_eq!(
            outcome.keep_tags.len(),
            1,
            "platforms sharing one manifest digest share one keep tag: {:?}",
            outcome.keep_tags
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keep_tag_false_skips_the_extra_tag_push() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        let outcome = publisher
            .push(vec![test_info("1.0.0")], &[], None, false, &BTreeMap::new())
            .await
            .expect("push succeeds");

        assert!(
            outcome.keep_tags.is_empty(),
            "keep_tag=false must report no tags: {:?}",
            outcome.keep_tags
        );
        let inner = data.read();
        assert!(
            inner.manifests.keys().all(|key| !key.contains(":__ocx.keep.")),
            "keep_tag=false must not push the extra tag: {:?}",
            inner.manifests.keys().collect::<Vec<_>>()
        );
    }
}
