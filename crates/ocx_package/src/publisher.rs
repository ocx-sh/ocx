// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Remote registry publishing facade.
//!
//! [`Publisher`] owns an OCI [`Client`](ocx_oci::Client) and exposes
//! high-level push operations, including cascade tag management.
//! It is the publishing counterpart to `PackageManager`,
//! which handles local-store operations.

pub mod copy;
pub mod publish_gate;

pub use copy::{CopiedPlatform, CopyError, CopyErrorKind, CopyOutcome, CopyRequest, Disposition};
pub use publish_gate::{PublishGateError, verify_dependency_pins};

use ocx_oci::layer_ref::LayerRef;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::error::Error as PackageError;
use crate::{description::Description, info::Info, version::Version};

/// This tier's own result: the publisher is `ocx_package`'s write half, so
/// its failures are the package tier's own (E1, plan DEC-27).
type Result<T> = std::result::Result<T, PackageError>;
use ocx_oci::client::ReadAddressing;

/// Remote registry publishing facade.
///
/// Holds an OCI client and provides push operations with optional
/// cascade tag management. Does not depend on local file structure
/// or index — only on the remote registry via the client.
#[derive(Clone)]
pub struct Publisher {
    client: ocx_oci::Client,
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
    pub manifest_digest: ocx_oci::Digest,
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
    pub platform_digests: Vec<(ocx_oci::Platform, ocx_oci::Digest)>,
    /// Un-prefixed track tags this push aliased onto the variant it landed, in
    /// write order (bare version first), deduped across platforms. Populated
    /// only under `--default`, and only when the pushed version carries a
    /// variant; empty otherwise.
    ///
    /// These name the *same* manifests `platform_digests` does: the aliases are
    /// index writes over the digests this push already produced, never a second
    /// upload.
    pub aliases_written: Vec<String>,
    /// Counts of layer-push outcomes (mounted/uploaded/verified), summed over
    /// every platform this push fanned out to. Layer blobs only — the config
    /// blob and manifest are not layers and are excluded. An `uploaded` count
    /// may still have HEAD-skipped an already-present blob inside
    /// `push_blob`'s blob-exists short-circuit.
    pub layer_counts: ocx_oci::LayerCounts,
}

impl PushOutcome {
    /// Construct an outcome. The only constructor available outside this
    /// crate, because the struct is `#[non_exhaustive]`.
    pub fn new(
        manifest_digest: ocx_oci::Digest,
        cascade_tags: Vec<String>,
        keep_tags: Vec<String>,
        platform_digests: Vec<(ocx_oci::Platform, ocx_oci::Digest)>,
        aliases_written: Vec<String>,
        layer_counts: ocx_oci::LayerCounts,
    ) -> Self {
        Self {
            manifest_digest,
            cascade_tags,
            keep_tags,
            platform_digests,
            aliases_written,
            layer_counts,
        }
    }
}

impl Publisher {
    pub fn new(client: ocx_oci::Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &ocx_oci::Client {
        &self.client
    }

    /// Pre-authenticate against the registry for `identifier` with Push scope.
    ///
    /// Call at the start of a publishing command to fail fast on credential
    /// issues before reading files or doing any other preparation.
    pub async fn ensure_auth(&self, identifier: &ocx_oci::OciIdentifier) -> Result<()> {
        Ok(self
            .client
            .ensure_auth(identifier, ocx_oci::RegistryOperation::Push)
            .await?)
    }

    /// Push one platform's package: its layers, its manifest, and the merge of
    /// that manifest's platform entry into the primary tag's image index.
    ///
    /// The orchestration the registry client used to own. `Info` is the
    /// publishing vocabulary — which repository, which platform, which metadata
    /// — and it is assembled into a manifest here
    /// ([`Info::manifest_builder`](crate::info::Info::manifest_builder))
    /// so the client only ever receives an identifier, a layer list and a
    /// half-built manifest.
    ///
    /// The call sequence is fixed and load-bearing: auth → layers → config blob
    /// → manifest → index merge. Cascade tags extend it at the end; see
    /// [`push_cascade`](Self::push_cascade).
    ///
    /// Returns the primary tag's image-index digest, the index itself as a
    /// [`ocx_oci::Manifest`], and the layer-push counts.
    pub async fn push_package(
        &self,
        target: &ocx_oci::OciIdentifier,
        info: &Info,
        layers: &[LayerRef],
        annotations: &BTreeMap<String, String>,
    ) -> Result<(ocx_oci::Digest, ocx_oci::Manifest, ocx_oci::LayerCounts)> {
        let (index_digest, index, layer_counts) = self
            .client
            .push_manifest_and_merge_tags(
                target,
                &info.platform,
                layers,
                &[],
                annotations,
                info.manifest_builder()?,
            )
            .await?;
        Ok((index_digest, ocx_oci::Manifest::ImageIndex(index), layer_counts))
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
    /// Every info is written to `target`. When `build_meta` is `Some`, the
    /// target's tag is parsed as a [`Version`] and the build segment is
    /// attached before push, once, so every platform lands on the same tag. Errors if the tag does not parse, lacks `X.Y.Z` form, or
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
    ///
    /// `default` re-tags the pushed version's own variant onto the un-prefixed
    /// version track. Without cascading there are no rolling tags to mirror, so
    /// it re-tags the bare version alone; a version carrying no variant writes
    /// no alias.
    #[expect(
        clippy::too_many_arguments,
        reason = "one push: where it lands, what to publish, which tracks it moves, and what to stamp on every index it writes"
    )]
    pub async fn push(
        &self,
        target: &ocx_oci::OciIdentifier,
        infos: Vec<Info>,
        layers: &[LayerRef],
        build_meta: Option<&str>,
        keep_tag: bool,
        default: bool,
        annotations: &BTreeMap<String, String>,
    ) -> Result<PushOutcome> {
        let identifier = apply_build_meta(target, build_meta)?;
        let mut manifest_digest: Option<ocx_oci::Digest> = None;
        let mut keep_tags: Vec<String> = Vec::new();
        let mut platform_digests: Vec<(ocx_oci::Platform, ocx_oci::Digest)> = Vec::new();
        let mut aliases_written: Vec<String> = Vec::new();
        let mut layer_counts = ocx_oci::LayerCounts::default();
        for info in infos {
            log::info!(
                "pushing package with identifier {} (platform {})",
                identifier,
                info.platform
            );
            let platform = info.platform.clone();
            let (digest, manifest, counts) = self.push_package(&identifier, &info, layers, annotations).await?;
            layer_counts += counts;
            // Hoisted out of the keep-tag branch on purpose: this is the same
            // descriptor `push_keep_tag` reads, and `platform_digests` has to
            // be there under `--no-keep-tag` too.
            if let Some(platform_digest) = ocx_oci::manifest::platform_manifest_digest(&manifest, &platform) {
                platform_digests.push((platform.clone(), platform_digest));
            }
            // A tag that is not a version carries no variant to match, so it
            // is the flag's no-op rather than a push-time refusal.
            if let Some(version) = Version::parse(identifier.tag_or_latest()) {
                let aliases = crate::cascade::write_default_variant_aliases(
                    &self.client,
                    &identifier,
                    &platform,
                    &manifest,
                    &version,
                    default,
                    None,
                    annotations,
                )
                .await?;
                for tag in aliases {
                    if !aliases_written.contains(&tag) {
                        aliases_written.push(tag);
                    }
                }
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
            manifest_digest: manifest_digest.ok_or(crate::error::Error::EmptyPushSet)?,
            cascade_tags: Vec::new(),
            keep_tags,
            platform_digests,
            aliases_written,
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
    ///
    /// `default` also has the meaning it has there, with the bare track
    /// additionally cascading: it writes the un-prefixed version plus whatever
    /// rolling tags that version's own cascade clears.
    #[expect(
        clippy::too_many_arguments,
        reason = "one push: what to publish, which tracks it moves, and what to stamp on every index it writes"
    )]
    pub async fn push_cascade(
        &self,
        target: &ocx_oci::OciIdentifier,
        infos: Vec<Info>,
        layers: &[LayerRef],
        existing_versions: BTreeSet<Version>,
        build_meta: Option<&str>,
        keep_tag: bool,
        default: bool,
        annotations: &BTreeMap<String, String>,
    ) -> Result<PushOutcome> {
        let identifier = apply_build_meta(target, build_meta)?;
        let version = Version::parse(identifier.tag_or_latest())
            .ok_or_else(|| crate::error::Error::VersionInvalid(identifier.tag_or_latest().to_string()))?;
        let mut manifest_digest: Option<ocx_oci::Digest> = None;
        let mut cascade_tags: Vec<String> = Vec::new();
        let mut keep_tags: Vec<String> = Vec::new();
        let mut platform_digests: Vec<(ocx_oci::Platform, ocx_oci::Digest)> = Vec::new();
        let mut aliases_written: Vec<String> = Vec::new();
        let mut layer_counts = ocx_oci::LayerCounts::default();
        for info in infos {
            log::info!(
                "pushing package with identifier {} (cascade, platform {})",
                identifier,
                info.platform
            );
            let platform = info.platform.clone();
            let outcome = crate::cascade::push_with_cascade(
                &self.client,
                &identifier,
                info,
                layers,
                existing_versions.clone(),
                &version,
                keep_tag,
                default,
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
            for tag in outcome.aliases {
                if !aliases_written.contains(&tag) {
                    aliases_written.push(tag);
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
            manifest_digest: manifest_digest.ok_or(crate::error::Error::EmptyPushSet)?,
            cascade_tags,
            keep_tags,
            platform_digests,
            aliases_written,
            layer_counts,
        })
    }

    /// Push a complete description artifact to the `__ocx.desc` tag.
    pub async fn push_description(&self, identifier: &ocx_oci::OciIdentifier, description: &Description) -> Result<()> {
        log::debug!("Pushing description for {}", identifier);
        crate::description::transport::push_description(&self.client, identifier, description).await
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
    ///
    /// Reads `identifier` where it names, unrouted: a push target's own
    /// description, or a location an index already routed. A source a package
    /// name was typed for is [`pull_source_description`](Self::pull_source_description).
    pub async fn pull_description(
        &self,
        identifier: &ocx_oci::OciIdentifier,
        temp_dir: &Path,
    ) -> Result<Option<Description>> {
        Ok(crate::description::transport::pull_description(&self.client, identifier, temp_dir).await?)
    }

    /// [`pull_description`](Self::pull_description) of `source`, read where
    /// `index` routes it ([`Index::route_for_dial`](ocx_index::Index::route_for_dial)):
    /// an index-served namespace such as `ocx.sh` is not a registry (ocx#504).
    pub async fn pull_source_description(
        &self,
        index: &ocx_index::Index,
        source: &ocx_oci::PackageRef,
        temp_dir: &Path,
    ) -> Result<Option<Description>> {
        let routed = index.route_for_dial(source).await?;
        self.pull_description(&routed, temp_dir).await
    }

    /// [`pull_source_description`](Self::pull_source_description) served by a
    /// configured mirror of the registry `index` routes `identifier` to.
    ///
    /// Only for a description nothing is written from — `ocx package description pull`
    /// renders one and stops. Named rather than implied, because nothing in a
    /// call site's shape says whether its answer will back a write.
    pub async fn pull_description_mirrored(
        &self,
        index: &ocx_index::Index,
        identifier: &ocx_oci::PackageRef,
        temp_dir: &Path,
    ) -> Result<Option<Description>> {
        let routed = index.route_for_dial(identifier).await?;
        Ok(crate::description::transport::pull_description_addressed(
            &self.client,
            &routed,
            temp_dir,
            ReadAddressing::Mirrored,
        )
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
    pub async fn list_tags(&self, identifier: ocx_oci::OciIdentifier) -> Result<Vec<String>> {
        Ok(self
            .client
            .list_tags_or_empty_addressed(identifier, ReadAddressing::Canonical)
            .await?)
    }

    /// Parses a list of tag strings into a set of valid versions,
    /// skipping tags that are not valid versions.
    pub fn parse_versions(tags: &[String]) -> BTreeSet<Version> {
        tags.iter().filter_map(|t| Version::parse(t)).collect()
    }
}

/// If `build_meta` is `Some`, parse `target`'s tag, attach the build segment,
/// and return the target at the new tag. Computed once per push, so every
/// platform of a fan-out lands on the same tag.
fn apply_build_meta(target: &ocx_oci::OciIdentifier, build_meta: Option<&str>) -> Result<ocx_oci::OciIdentifier> {
    let Some(build) = build_meta else {
        return Ok(target.clone());
    };
    let tag = target.tag_or_latest();
    let version = Version::parse(tag).ok_or_else(|| crate::error::Error::VersionInvalid(tag.to_string()))?;
    let with_build = version.with_build(build).map_err(crate::error::Error::from)?;
    Ok(target.clone_with_tag(with_build.to_string()))
}

#[cfg(test)]
mod tests {
    use ocx_util::prelude::VecExt as _;

    use super::*;
    use crate::metadata::{
        Entrypoints, Metadata,
        bundle::{self, Bundle},
        dependency, env as metadata_env,
    };

    fn test_target(tag: &str) -> ocx_oci::OciIdentifier {
        ocx_oci::OciIdentifier::from_parts("ocx", "ocx.sh").clone_with_tag(tag)
    }

    fn test_info() -> Info {
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
            metadata,
            platform: "linux/amd64".parse().expect("platform parses"),
        }
    }

    #[test]
    fn none_returns_info_unchanged() {
        let target = test_target("mirror-0.3.0-dev");
        let out = apply_build_meta(&target, None).expect("no-op succeeds");
        assert_eq!(out.tag_or_latest(), "mirror-0.3.0-dev");
    }

    #[test]
    fn attaches_build_meta_to_variant_prerelease() {
        let target = test_target("mirror-0.3.0-dev");
        let out = apply_build_meta(&target, Some("20260514120000")).expect("attach succeeds");
        // Display normalizes `+` to `_` per OCI tag rules; clone_with_tag does the same.
        assert_eq!(out.tag_or_latest(), "mirror-0.3.0-dev_20260514120000");
    }

    #[test]
    fn attaches_build_meta_to_bare_patch_version() {
        let target = test_target("0.3.0");
        let out = apply_build_meta(&target, Some("20260514120000")).expect("attach succeeds");
        assert_eq!(out.tag_or_latest(), "0.3.0_20260514120000");
    }

    #[test]
    fn rejects_tag_that_already_carries_build_meta() {
        let target = test_target("0.3.0-dev_alreadyhere");
        let err = apply_build_meta(&target, Some("20260514120000")).expect_err("must reject double build meta");
        let msg = err.to_string();
        assert!(msg.contains("already has build metadata"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_tag_that_is_not_a_valid_version() {
        let target = test_target("latest");
        let err = apply_build_meta(&target, Some("20260514120000")).expect_err("must reject non-version tag");
        let msg = err.to_string();
        assert!(msg.contains("invalid package version"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_tag_that_lacks_patch_segment() {
        let target = test_target("1.2");
        let err = apply_build_meta(&target, Some("20260514120000")).expect_err("must reject X.Y tag");
        let msg = err.to_string();
        assert!(msg.contains("X.Y.Z"), "unexpected error: {msg}");
    }

    // ── Multi-platform fan-out — adr_dependency_manifest_pinning.md ──────

    #[tokio::test(flavor = "multi_thread")]
    async fn build_meta_lands_every_platform_on_the_same_tag() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));
        let mut mac = test_info();
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        publisher
            .push(
                &test_target("0.3.0"),
                vec![test_info(), mac],
                &[],
                Some("20260514120000"),
                false,
                false,
                &BTreeMap::new(),
            )
            .await
            .expect("fan-out push succeeds");

        let inner = data.read();
        let (index_bytes, _) = inner
            .manifests
            .get("ocx.sh/ocx:0.3.0_20260514120000")
            .expect("both platforms land on the one built tag");
        let index: serde_json::Value = serde_json::from_slice(index_bytes).expect("index parses");
        assert_eq!(index["manifests"].as_array().expect("manifests array").len(), 2);
        assert!(
            !inner.manifests.contains_key("ocx.sh/ocx:0.3.0"),
            "no platform may land on the unbuilt tag"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_fan_out_set_is_an_error() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            StubTransportData::new(),
        ))));
        let err = publisher
            .push(
                &test_target("1.0.0"),
                Vec::new(),
                &[],
                None,
                false,
                false,
                &BTreeMap::new(),
            )
            .await
            .expect_err("empty set");
        assert!(err.to_string().contains("at least one target platform"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fan_out_merges_every_platform_into_the_primary_index() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let mut mac = test_info();
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        let outcome = publisher
            .push(
                &test_target("1.0.0"),
                vec![test_info(), mac],
                &[],
                None,
                false,
                false,
                &BTreeMap::new(),
            )
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let outcome = publisher
            .push(
                &test_target("1.0.0"),
                vec![test_info()],
                &[],
                None,
                true,
                false,
                &BTreeMap::new(),
            )
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let mut mac = test_info();
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
            .push(
                &test_target("1.0.0"),
                vec![test_info(), mac],
                &[],
                None,
                true,
                false,
                &BTreeMap::new(),
            )
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        // Identical metadata, identical (empty) layers, two platforms — the
        // real-world Rosetta-alias / noarch-bundle shape. Both index entries
        // point at the same leaf manifest, so both platforms yield the same
        // keep tag and the report must carry it once.
        let mut alias = test_info();
        alias.platform = "darwin/arm64".parse().expect("platform parses");

        let outcome = publisher
            .push(
                &test_target("1.0.0"),
                vec![test_info(), alias],
                &[],
                None,
                true,
                false,
                &BTreeMap::new(),
            )
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        // Both platforms compute the same rolling tags, and (identical
        // metadata) the same platform-manifest digest — the cascade loop must
        // report each of them once, not once per `Info`.
        let mut alias = test_info();
        alias.platform = "darwin/arm64".parse().expect("platform parses");

        let outcome = publisher
            .push_cascade(
                &test_target("1.0.0"),
                vec![test_info(), alias],
                &[],
                BTreeSet::new(),
                None,
                true,
                false,
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let outcome = publisher
            .push(
                &test_target("1.0.0"),
                vec![test_info()],
                &[],
                None,
                false,
                false,
                &BTreeMap::new(),
            )
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

    // ── default-variant aliasing (D4) ───────────────────────────────

    /// `--default` without `--cascade`: the pushed version's own variant
    /// re-tags the bare version alone (no rolling tags), and the manifest is
    /// re-tagged, not uploaded a second time. `cascade.rs` covers the cascade
    /// arm; this is the plain-`push` arm the deleted no-op block left untested.
    #[tokio::test(flavor = "multi_thread")]
    async fn default_without_cascade_aliases_the_bare_version() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let outcome = publisher
            .push(
                &test_target("full-1.2.3"),
                vec![test_info()],
                &[],
                None,
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("push succeeds");

        assert_eq!(
            outcome.aliases_written,
            vec!["1.2.3".to_string()],
            "a non-cascade default push re-tags the bare version alone: {:?}",
            outcome.aliases_written
        );

        let inner = data.read();
        assert!(
            inner.manifests.keys().any(|key| key.ends_with(":1.2.3")),
            "the bare 1.2.3 tag must be on the wire: {:?}",
            inner.manifests.keys().collect::<Vec<_>>()
        );
        // Counted, not deduped by digest key: a HashMap keyed on the digest
        // could not witness a second upload of the same manifest.
        assert_eq!(
            inner.digest_manifest_writes, 1,
            "aliasing re-tags the manifest this push produced, never uploads a second"
        );
    }

    /// Two platforms both alias the same bare version, and the report carries
    /// it once — the `if !aliases_written.contains` dedup in the push loop.
    /// Without it the vector would read `["1.0.0", "1.0.0"]`.
    #[tokio::test(flavor = "multi_thread")]
    async fn fan_out_dedupes_default_aliases_across_platforms() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let mut mac = test_info();
        mac.platform = "darwin/arm64".parse().expect("platform parses");

        let outcome = publisher
            .push(
                &test_target("full-1.0.0"),
                vec![test_info(), mac],
                &[],
                None,
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("fan-out push succeeds");

        assert_eq!(
            outcome.aliases_written,
            vec!["1.0.0".to_string()],
            "both platforms alias the same bare version; it must appear once: {:?}",
            outcome.aliases_written
        );
    }

    /// The same dedup on the cascade path (`push_cascade` alias loop): two
    /// platforms each clear the full bare track, and every alias appears once.
    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_fan_out_dedupes_default_aliases_across_platforms() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));

        let mut mac = test_info();
        mac.platform = "darwin/arm64".parse().expect("platform parses");

        let outcome = publisher
            .push_cascade(
                &test_target("full-1.2.3"),
                vec![test_info(), mac],
                &[],
                BTreeSet::new(),
                None,
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade fan-out push succeeds");

        assert_eq!(
            outcome.aliases_written,
            vec![
                "1.2.3".to_string(),
                "1.2".to_string(),
                "1".to_string(),
                "latest".to_string()
            ],
            "each aliased bare-track tag must appear once across the fan-out: {:?}",
            outcome.aliases_written
        );
    }
    // ── Push authentication (moved here from `oci/client.rs` by WP-14b) ──────
    //
    // `Client::push_package` was the orchestration; it is now
    // `Publisher::push_package`, so the guard that the sequence STARTS with a
    // `Push`-scope handshake belongs here. Without it a publish issues
    // anonymous requests and a registry requiring auth answers 401 — a security
    // property, not incidental coverage.

    /// Stage a one-layer publish against the shared stub transport and return
    /// the recorded call log.
    async fn record_a_push() -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::test_transport::{StubTransportData, stub_client};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(stub_client(&data));

        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("pkg.tar.gz");
        tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();
        let layers = [LayerRef::File {
            path: archive_path,
            layout: ocx_oci::LayerLayoutSpec::default(),
            mount_from: None,
        }];

        let _ = publisher
            .push_package(&test_target("1.0.0"), &test_info(), &layers, &BTreeMap::new())
            .await;
        data
    }

    #[tokio::test]
    async fn push_package_authenticates_with_push() {
        let data = record_a_push().await;
        let calls = data.read().auth_calls.clone();
        // Must authenticate with Push before any blob/manifest operations.
        assert!(!calls.is_empty(), "push_package must call ensure_auth");
        assert!(
            matches!(calls[0].1, ocx_oci::RegistryOperation::Push),
            "the first ensure_auth must carry Push scope, got {:?}",
            calls[0].1
        );
    }

    /// The handshake does not merely happen — it happens FIRST.
    ///
    /// The sibling above reads `auth_calls`, which is its own log: its first
    /// entry is `Push` whether the handshake preceded the layer upload or
    /// followed it, so on its own it cannot see the anonymous-request bug at
    /// all. `first_call` is the single observation that orders the auth against
    /// the transport work, and it is why this test is not a duplicate.
    #[tokio::test]
    async fn ensure_auth_precedes_transport_calls_for_push() {
        let data = record_a_push().await;
        let inner = data.read();
        assert!(!inner.auth_calls.is_empty(), "ensure_auth must have been called");
        assert!(matches!(inner.auth_calls[0].1, ocx_oci::RegistryOperation::Push));
        assert_eq!(
            inner.first_call.as_deref(),
            Some("ensure_auth"),
            "the handshake must precede every transport call, got {:?} first (calls: {:?})",
            inner.first_call,
            inner.calls
        );
        // push_blob must actually have been called (for the package data),
        // otherwise the ordering assertion above is vacuous.
        assert!(
            inner.calls.iter().any(|c| c.starts_with("push_blob:")),
            "push_blob should follow ensure_auth, calls: {:?}",
            inner.calls
        );
    }

    // ── Description reads routed through the index (ocx#504) ─────────────

    /// `ghcr.io/served/tool` is served from `ghcr.io/owner/tool`: the logical
    /// name holds nothing, so a read that dials it as typed reads the wrong
    /// repository.
    fn routed_index() -> ocx_index::Index {
        ocx_index::test_source::RoutingSource::rewriting("ghcr.io", "owner/tool").into_index()
    }

    fn logical_identifier() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("served/tool", "ghcr.io")
    }

    /// `(registry, repository)` the description manifest read was handed.
    fn description_read_target(data: &ocx_oci::client::test_transport::StubTransportData) -> (String, String) {
        data.read()
            .read_targets
            .iter()
            .find(|(method, _, _)| *method == "pull_manifest_raw")
            .map(|(_, registry, repository)| (registry.clone(), repository.clone()))
            .expect("the description manifest was never read")
    }

    #[tokio::test]
    async fn pull_description_mirrored_reads_where_the_index_routes() {
        let data = ocx_oci::client::test_transport::StubTransportData::new();
        let publisher = Publisher::new(ocx_oci::client::test_transport::stub_client(&data));
        let dir = tempfile::tempdir().unwrap();

        let _ = publisher
            .pull_description_mirrored(&routed_index(), &logical_identifier(), dir.path())
            .await;

        assert_eq!(
            description_read_target(&data),
            ("ghcr.io".to_string(), "owner/tool".to_string())
        );
    }

    #[tokio::test]
    async fn pull_source_description_reads_where_the_index_routes() {
        let data = ocx_oci::client::test_transport::StubTransportData::new();
        let publisher = Publisher::new(ocx_oci::client::test_transport::stub_client(&data));
        let dir = tempfile::tempdir().unwrap();

        let _ = publisher
            .pull_source_description(&routed_index(), &logical_identifier(), dir.path())
            .await;

        assert_eq!(
            description_read_target(&data),
            ("ghcr.io".to_string(), "owner/tool".to_string())
        );
    }
}
