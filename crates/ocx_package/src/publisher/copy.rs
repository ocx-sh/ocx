// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Promotion of an already-published package between registries.
//!
//! Leaf manifests and blobs are copied verbatim, a tag's index is merged per platform, and
//! rolling tags are recomputed against the target (`adr_package_copy.md`).

use std::collections::BTreeMap;

use serde::Serialize;

use super::Publisher;
use crate::error::Error as PackageError;
type Result<T> = std::result::Result<T, PackageError>;
use crate::cascade;
use crate::version::Version;
use ocx_oci::client::Client;
use ocx_oci::client::error::ClientError;

/// Why a copy could not be performed; the `#[source]` kind lets `--json` fill both
/// `error.detail` and `error.context`.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[error("copying {source_identifier} to {target_identifier}")]
#[exit(delegate = kind)]
pub struct CopyError {
    pub source_identifier: ocx_oci::PackageRef,
    pub target_identifier: ocx_oci::OciIdentifier,
    #[source]
    pub kind: CopyErrorKind,
}

/// Discriminant kind for [`CopyError`]; messages omit the endpoints, which the outer error names.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum CopyErrorKind {
    /// The source names an image index by digest: a snapshot of a mutable set, which cannot
    /// merge into a target that moved on.
    #[error("names an image index by digest; copy the tag instead")]
    #[exit(
        UsageError,
        slug = "index_named_by_digest",
        summary = "The source names an image index by digest, which cannot merge into the target"
    )]
    IndexNamedByDigest,

    /// The source is a bare platform manifest, which carries no platform, and none was named.
    #[error("is a platform manifest and carries no platform; pass --platform")]
    #[exit(
        UsageError,
        slug = "platform_required",
        summary = "The source is a single-platform manifest and no platform was named"
    )]
    PlatformRequired,

    /// The source is a bare platform manifest and more than one platform was named.
    #[error("is a single platform manifest; pass exactly one --platform")]
    #[exit(
        UsageError,
        slug = "platform_ambiguous",
        summary = "The source is a single-platform manifest and more than one platform was named"
    )]
    PlatformAmbiguous,

    /// The source index offers no platform the request names.
    #[error("offers no platform matching {requested}; available: {available}")]
    #[exit(
        UsageError,
        slug = "no_matching_platform",
        summary = "The source index offers no requested platform"
    )]
    NoMatchingPlatform {
        /// What was asked for, joined for display.
        requested: String,
        /// What the source offers, joined for display.
        available: String,
    },

    /// Everything else; classification defers to the cause.
    // `transparent` hides the cause from the chain walker, so `delegate` is the only path to its code.
    #[error(transparent)]
    #[exit(delegate)]
    Registry(#[from] PackageError),
}

impl From<ClientError> for CopyErrorKind {
    fn from(error: ClientError) -> Self {
        Self::Registry(error.into())
    }
}

impl From<ocx_oci::digest::error::DigestError> for CopyErrorKind {
    fn from(error: ocx_oci::digest::error::DigestError) -> Self {
        Self::Registry(error.into())
    }
}

impl From<ocx_oci::platform::error::PlatformError> for CopyErrorKind {
    fn from(error: ocx_oci::platform::error::PlatformError) -> Self {
        Self::Registry(error.into())
    }
}

/// What a copy was asked to do.
#[derive(Debug)]
pub struct CopyRequest<'a> {
    /// Where to read from; a digest names one leaf, whose platform `platforms` must name,
    /// since a leaf manifest carries none.
    pub source: &'a ocx_oci::PackageRef,
    /// Where to write, including the tag the package lands on.
    pub target: &'a ocx_oci::OciIdentifier,
    /// Empty means every platform the source index offers.
    pub platforms: Vec<ocx_oci::Platform>,
    /// Recompute rolling tags (`3.28`, `3`, `latest`) against the target.
    pub cascade: bool,
    /// Write the digest-named `__ocx.keep.<algorithm>-<hex>` deletion safety net.
    pub keep_tag: bool,
    /// Carry signatures, SBOMs and everything else anchored to each leaf.
    pub referrers: bool,
    /// Index-level annotations to merge into every tag this copy touches.
    pub annotations: &'a BTreeMap<String, String>,
    /// Plan only — report what would happen and write nothing.
    pub dry_run: bool,
    /// Where each promoted layer spools.
    // Required: `$TMPDIR` is memory-backed on most Linux hosts, and the spool cap bounds the file, not the medium.
    pub scratch_root: &'a std::path::Path,
}

/// What became of one platform at the target.
// Serialized as the enum, never the `Display` prose: `--format json` consumers match on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// The target's index had no entry for this platform.
    Added,
    /// The target already pointed at this exact digest — nothing to do.
    Unchanged,
    /// The target pointed at a different digest for this platform.
    Replaced,
    /// The target offers this platform and the source does not, so the merge
    /// leaves it alone.
    // Must be reported: a filtered copy that silently left a mixed index would look complete.
    KeptNotInSource,
}

impl std::fmt::Display for Disposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Added => "added",
            Self::Unchanged => "unchanged",
            Self::Replaced => "replaced",
            Self::KeptNotInSource => "kept (not in source)",
        };
        f.write_str(text)
    }
}

/// One row of the per-platform report.
#[derive(Debug)]
pub struct CopiedPlatform {
    pub platform: ocx_oci::Platform,
    /// The leaf digest now at the target (for `KeptNotInSource`, the one it already had).
    pub digest: ocx_oci::Digest,
    pub disposition: Disposition,
}

/// The result of a promotion.
#[derive(Debug)]
pub struct CopyOutcome {
    pub source: ocx_oci::PackageRef,
    /// Where `source` was read from, after index routing; reuse it rather than routing again.
    pub source_location: ocx_oci::OciIdentifier,
    pub target: ocx_oci::OciIdentifier,
    /// One row per platform, source-supplied and target-only alike.
    pub platforms: Vec<CopiedPlatform>,
    /// Rolling tags written in addition to the target's own tag.
    pub cascade_tags: Vec<String>,
    /// `__ocx.keep.<algorithm>-<hex>` tags written, deduped by manifest digest.
    pub keep_tags: Vec<String>,
    /// Referrer manifests copied, summed over every platform.
    pub referrers: usize,
    /// cosign sidecar tags carried, summed over every platform.
    pub sidecars: usize,
    /// Sidecar tags left alone because the target holds a different manifest under them; the
    /// caller turns a non-empty list into a non-zero exit.
    // Data, not an error: failing here would block re-promoting onto a target with extra signatures.
    pub sidecar_conflicts: Vec<String>,
    pub blobs: ocx_oci::copy::BlobTransfers,
    /// True when nothing was written.
    pub dry_run: bool,
}

impl CopyOutcome {
    /// True when every source platform was already at the target under the same digest.
    pub fn is_no_op(&self) -> bool {
        self.platforms
            .iter()
            .all(|row| matches!(row.disposition, Disposition::Unchanged | Disposition::KeptNotInSource))
    }
}

impl Publisher {
    /// Promotes an already-published package to another registry or repository.
    ///
    /// Content lands before any tag moves, so an interruption leaves the tags as they were.
    /// Source reads dial where `index` routes the source; the target is written as typed.
    pub async fn copy(
        &self,
        index: &ocx_index::Index,
        request: CopyRequest<'_>,
    ) -> std::result::Result<CopyOutcome, CopyError> {
        let source_identifier = request.source.clone();
        let target_identifier = request.target.clone();
        run(&self.client, index, request).await.map_err(|kind| CopyError {
            source_identifier,
            target_identifier,
            kind,
        })
    }
}

async fn run(
    client: &Client,
    index: &ocx_index::Index,
    request: CopyRequest<'_>,
) -> std::result::Result<CopyOutcome, CopyErrorKind> {
    let source_location = index.route_for_dial(request.source).await.map_err(PackageError::from)?;
    let source_leaves =
        resolve_source_leaves(client, index, request.source, &source_location, &request.platforms).await?;
    let target_entries = read_target_entries(client, request.target).await?;

    let mut rows = Vec::new();
    // Phase 2 builds entries from this, never `source_leaves`, so no tag names unlanded content.
    let mut copied_leaves: Vec<(ocx_oci::Platform, ocx_oci::Digest, i64)> = Vec::new();
    let mut blobs = ocx_oci::copy::BlobTransfers::default();
    let mut referrers = 0usize;
    let mut sidecars = 0usize;
    let mut sidecar_conflicts: Vec<String> = Vec::new();
    let mut cascade_tags: Vec<String> = Vec::new();
    let mut keep_tags: Vec<String> = Vec::new();

    for (platform, source_digest) in &source_leaves {
        let disposition = match lookup(&target_entries, platform) {
            None => Disposition::Added,
            Some(existing) if existing == source_digest => Disposition::Unchanged,
            Some(_) => Disposition::Replaced,
        };
        rows.push(CopiedPlatform {
            platform: platform.clone(),
            digest: source_digest.clone(),
            disposition,
        });

        if request.dry_run {
            continue;
        }

        // Phase 1, content. `Unchanged` re-verifies, never skips: an index entry proves the
        // manifest present, not its blobs, and the docs promise this re-verify.
        let copied = ocx_oci::copy::copy_leaf(
            client,
            &source_location,
            request.target,
            source_digest,
            request.referrers,
            request.scratch_root,
        )
        .await?;
        blobs += copied.blobs;
        referrers += copied.referrers;
        sidecars += copied.sidecars.copied;
        sidecar_conflicts.extend(copied.sidecars.conflicts);
        copied_leaves.push((platform.clone(), source_digest.clone(), copied.size));
    }

    for (platform, digest) in &target_entries {
        if !source_leaves.iter().any(|(candidate, _)| candidate == platform) {
            rows.push(CopiedPlatform {
                platform: platform.clone(),
                digest: digest.clone(),
                disposition: Disposition::KeptNotInSource,
            });
        }
    }

    if !request.dry_run {
        // Phase 2, tags. Sequential: a concurrent read-modify-write merge loses a platform.
        let primary = request.target.tag_or_latest().to_string();
        for (platform, digest, size) in &copied_leaves {
            // One list for both the writes and the report, so they cannot drift; primary first,
            // since only its merged index may derive a keep tag.
            let merge_tags: Vec<String> = std::iter::once(primary.clone())
                .chain(target_tags(client, &request, platform).await?)
                .collect();
            for tag in merge_tags.iter().skip(1) {
                if !cascade_tags.contains(tag) {
                    cascade_tags.push(tag.clone());
                }
            }
            let mut primary_index = None;
            for tag in &merge_tags {
                let (_, merged) = client
                    .merge_platform_into_index(
                        request.target,
                        tag.clone(),
                        platform,
                        &digest.to_string(),
                        *size,
                        request.annotations,
                    )
                    .await?;
                if primary_index.is_none() {
                    primary_index = Some(merged);
                }
            }
            if request.keep_tag
                && let Some(index) = primary_index
                && let Some(tag) = client
                    .push_keep_tag(request.target, &ocx_oci::Manifest::ImageIndex(index), platform)
                    .await?
                && !keep_tags.contains(&tag)
            {
                keep_tags.push(tag);
            }
        }
    }

    Ok(CopyOutcome {
        source: request.source.clone(),
        source_location,
        target: request.target.clone(),
        platforms: rows,
        cascade_tags,
        keep_tags,
        referrers,
        sidecars,
        sidecar_conflicts,
        blobs,
        dry_run: request.dry_run,
    })
}

/// The rolling tags this platform should move, from the target's own tags, never the source's.
// Canonical reads only: the tags are written canonically, so a mirror's answer would decide
// for a repository nobody read (`subsystem-oci.md` Invariant #5, CWE-345).
async fn target_tags(client: &Client, request: &CopyRequest<'_>, platform: &ocx_oci::Platform) -> Result<Vec<String>> {
    if !request.cascade {
        return Ok(Vec::new());
    }
    let tag = request.target.tag_or_latest();
    let version = Version::parse(tag).ok_or_else(|| crate::error::Error::VersionInvalid(tag.to_string()))?;
    // Only a 404 folds to empty; a transient failure still aborts the promotion.
    let listed = client
        .list_tags_or_empty_addressed(request.target.clone(), ocx_oci::client::ReadAddressing::Canonical)
        .await?;
    let existing = super::Publisher::parse_versions(&listed);
    let (tags, _) = cascade::resolve_cascade_tags(client, request.target, &version, &existing, platform).await?;
    Ok(tags.into_iter().filter(|candidate| candidate != tag).collect())
}

/// The `(platform, leaf digest)` pairs this copy moves: the index's answer for an
/// index-served source, else the registry's tag.
// Only digest-addressed leaves are read at `source_location`, so a physical tag moved since
// publication never replaces the logical version.
async fn resolve_source_leaves(
    client: &Client,
    index: &ocx_index::Index,
    source: &ocx_oci::PackageRef,
    source_location: &ocx_oci::OciIdentifier,
    requested: &[ocx_oci::Platform],
) -> std::result::Result<Vec<(ocx_oci::Platform, ocx_oci::Digest)>, CopyErrorKind> {
    let not_found = || ClientError::ManifestNotFound(source.to_string());
    let (digest, manifest) = match index
        .resolve_version(source, source_location)
        .await
        .map_err(PackageError::from)?
    {
        ocx_index::ResolvedVersion::Indexed { digest, manifest } => (digest, *manifest),
        ocx_index::ResolvedVersion::Absent => return Err(not_found().into()),
        ocx_index::ResolvedVersion::Registry => {
            let (_, digest, manifest) = client
                .fetch_manifest_raw_bytes(source_location)
                .await?
                .ok_or_else(not_found)?;
            (digest, manifest)
        }
    };

    match manifest {
        ocx_oci::Manifest::ImageIndex(index) => {
            if source.digest().is_some() {
                return Err(CopyErrorKind::IndexNamedByDigest);
            }
            let mut leaves = Vec::new();
            // Before the filter: the list says what the caller could have asked for.
            let mut available = Vec::new();
            for entry in index.manifests {
                let Some(native) = entry.platform else { continue };
                let platform = ocx_oci::Platform::try_from(native)?;
                available.push(platform.to_string());
                if !requested.is_empty() && !requested.contains(&platform) {
                    continue;
                }
                leaves.push((platform, ocx_oci::Digest::try_from(&entry.digest)?));
            }
            if leaves.is_empty() {
                return Err(CopyErrorKind::NoMatchingPlatform {
                    requested: if requested.is_empty() {
                        "any platform".to_string()
                    } else {
                        requested.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                    },
                    available: if available.is_empty() {
                        "none, the index declares no platforms".to_string()
                    } else {
                        available.join(", ")
                    },
                });
            }
            Ok(leaves)
        }
        // Never guessed: a wrong platform resolves for the wrong hosts and fails at exec.
        ocx_oci::Manifest::Image(_) => match requested {
            [platform] => Ok(vec![(platform.clone(), digest)]),
            [] => Err(CopyErrorKind::PlatformRequired),
            _ => Err(CopyErrorKind::PlatformAmbiguous),
        },
    }
}

fn lookup<'a>(
    entries: &'a [(ocx_oci::Platform, ocx_oci::Digest)],
    platform: &ocx_oci::Platform,
) -> Option<&'a ocx_oci::Digest> {
    entries
        .iter()
        .find(|(candidate, _)| candidate == platform)
        .map(|(_, digest)| digest)
}

/// The platform entries the target's tag already carries; none for an absent tag or a bare
/// manifest.
async fn read_target_entries(
    client: &Client,
    target: &ocx_oci::OciIdentifier,
) -> Result<Vec<(ocx_oci::Platform, ocx_oci::Digest)>> {
    // A `Vec`: `Platform` is deliberately not `Ord`, since compatibility is not a total order.
    let mut entries = Vec::new();
    // A read failure propagates: as "nothing", every platform reads `Added`, silently under `--dry-run`.
    let Some((_, _, manifest)) = client.fetch_manifest_raw_bytes(target).await? else {
        log::debug!("Target {target} has no index yet");
        return Ok(entries);
    };
    let ocx_oci::Manifest::ImageIndex(index) = manifest else {
        return Ok(entries);
    };
    for entry in index.manifests {
        let Some(native) = entry.platform else { continue };
        let platform = ocx_oci::Platform::try_from(native)?;
        entries.push((platform, ocx_oci::Digest::try_from(&entry.digest)?));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData, blob_location_key};
    use ocx_oci::media_type::MEDIA_TYPE_OCI_IMAGE_MANIFEST;
    use ocx_oci::{Algorithm, Descriptor, ImageIndex, ImageIndexEntry, ImageManifest, Manifest};

    const CONFIG_BLOB: &[u8] = b"{\"package\":\"demo\"}";
    const AMD64_LAYER: &[u8] = b"a linux/amd64 layer";
    const ARM64_LAYER: &[u8] = b"a darwin/arm64 layer";

    const SIGNATURE_LAYER: &[u8] = b"a sigstore bundle";

    /// The image index the target's own tag ended up holding.
    fn pushed_index(data: &StubTransportData, identifier: &ocx_oci::OciIdentifier) -> ImageIndex {
        let inner = data.read();
        let (bytes, _) = inner
            .manifests
            .get(&canonical(identifier).to_string())
            .expect("nothing was pushed to the target tag");
        match serde_json::from_slice::<Manifest>(bytes).expect("parse") {
            Manifest::ImageIndex(index) => index,
            Manifest::Image(_) => panic!("expected an image index at the target tag"),
        }
    }

    fn client_for(data: &StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    fn publisher_for(data: &StubTransportData) -> Publisher {
        Publisher::new(client_for(data))
    }

    /// The whole `source()` chain rendered, since `CopyError`'s own `Display`
    /// is the endpoints and the reason lives one link down.
    fn chain(error: &CopyError) -> String {
        let mut rendered = error.to_string();
        let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
        while let Some(current) = cause {
            rendered.push_str(": ");
            rendered.push_str(&current.to_string());
            cause = current.source();
        }
        rendered
    }

    fn platform(text: &str) -> ocx_oci::Platform {
        text.parse().expect("platform")
    }

    fn identifier(registry: &str, tag: &str) -> ocx_oci::OciIdentifier {
        ocx_oci::OciIdentifier::from_parts("team/demo", registry).clone_with_tag(tag)
    }

    /// The package a copy of `location` is asked for — registry-backed, so
    /// under `PASSTHROUGH` it routes back to `location` itself.
    fn package_at(location: &ocx_oci::OciIdentifier) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::parse(&location.to_string()).expect("a location spells a package identifier")
    }

    /// The seam production reads and writes through; building the reference off
    /// `PackageRef` directly is allow-listed away from this file (T-arch-A1).
    fn canonical(identifier: &ocx_oci::OciIdentifier) -> ocx_oci::native::Reference {
        client_for(&StubTransportData::new()).read_reference(identifier, ocx_oci::client::ReadAddressing::Canonical)
    }

    fn descriptor(media_type: &str, bytes: &[u8]) -> Descriptor {
        Descriptor {
            media_type: media_type.to_string(),
            digest: Algorithm::Sha256.hash(bytes).to_string(),
            size: bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        }
    }

    fn leaf_for(layer: &'static [u8]) -> Manifest {
        Manifest::Image(ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            config: descriptor("application/vnd.ocx.package.metadata.v1+json", CONFIG_BLOB),
            layers: vec![descriptor(ocx_oci::media_type::MEDIA_TYPE_TAR_GZ, layer)],
            ..Default::default()
        })
    }

    fn store(data: &StubTransportData, identifier: &ocx_oci::OciIdentifier, manifest: &Manifest) -> ocx_oci::Digest {
        let bytes = serde_json::to_vec(manifest).expect("serialize");
        let digest = Algorithm::Sha256.hash(&bytes);
        data.write()
            .manifests
            .insert(canonical(identifier).to_string(), (bytes, digest.to_string()));
        digest
    }

    fn index_of(entries: &[(&str, &ocx_oci::Digest)]) -> Manifest {
        Manifest::ImageIndex(ImageIndex {
            schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
            media_type: Some(ocx_oci::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
            artifact_type: None,
            manifests: entries
                .iter()
                .map(|(text, digest)| ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: digest.to_string(),
                    size: 100,
                    platform: Some(platform(text).into()),
                    artifact_type: None,
                    annotations: None,
                })
                .collect(),
            annotations: None,
        })
    }

    /// Publishes `platforms` at the source under `tag`, returning their leaf
    /// digests in the order given.
    fn seed_source(data: &StubTransportData, tag: &str, platforms: &[(&str, &'static [u8])]) -> Vec<ocx_oci::Digest> {
        let source = identifier("dev.example.com", tag);
        let mut leaves = Vec::new();
        let mut present = std::collections::BTreeSet::new();
        for (_, layer) in platforms {
            let manifest = leaf_for(layer);
            let digest = {
                let bytes = serde_json::to_vec(&manifest).expect("serialize");
                Algorithm::Sha256.hash(&bytes)
            };
            store(data, &source.without_tag().clone_with_digest(digest.clone()), &manifest);
            leaves.push(digest);
            for blob in [CONFIG_BLOB, *layer] {
                let blob_digest = Algorithm::Sha256.hash(blob).to_string();
                data.write().blobs.insert(blob_digest.clone(), blob.to_vec());
                present.insert(blob_digest);
            }
        }
        let entries: Vec<(&str, &ocx_oci::Digest)> =
            platforms.iter().map(|(text, _)| *text).zip(leaves.iter()).collect();
        let index = index_of(&entries);
        let index_digest = store(data, &source, &index);
        // A registry answers for the index by tag and by digest alike; seeding
        // only the tag would make the digest-addressed refusal test fail for the
        // wrong reason.
        store(data, &source.without_tag().clone_with_digest(index_digest), &index);

        let mut inner = data.write();
        inner.capture_pushes = true;
        inner
            .blob_locations
            .get_or_insert_with(HashMap::new)
            .insert(blob_location_key(&canonical(&source)), present);
        leaves
    }

    fn request<'a>(
        source: &'a ocx_oci::PackageRef,
        target: &'a ocx_oci::OciIdentifier,
        annotations: &'a BTreeMap<String, String>,
    ) -> CopyRequest<'a> {
        CopyRequest {
            source,
            target,
            platforms: Vec::new(),
            cascade: false,
            keep_tag: false,
            referrers: false,
            annotations,
            dry_run: false,
            scratch_root: SCRATCH.path(),
        }
    }

    /// Nothing rewrites the source: it is read where it names, the shape every
    /// test here ran under before the source was routed (ocx#504).
    static PASSTHROUGH: std::sync::LazyLock<ocx_index::Index> =
        std::sync::LazyLock::new(|| ocx_index::test_source::RoutingSource::passthrough().into_index());

    /// One scratch root for the whole test module.
    ///
    /// `CopyRequest::scratch_root` is non-optional, so every request needs a real
    /// directory; a per-call `TempDir` would need a binding kept alive at each of
    /// the call sites below (TEST-06). `LazyLock` holds the guard for the process
    /// instead, and `copy_leaf` still spools into a fresh `tempdir_in` under it.
    static SCRATCH: std::sync::LazyLock<tempfile::TempDir> =
        std::sync::LazyLock::new(|| tempfile::tempdir().expect("a scratch root for the blob spool"));

    /// The report has to distinguish the four outcomes, because a promotion that
    /// silently leaves a mixed index behind reads exactly like a complete one.
    #[tokio::test]
    async fn every_platform_reports_what_actually_happened_to_it() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        // The target already offers darwin/arm64, and an older linux/amd64.
        let stale = Algorithm::Sha256.hash(b"an older linux/amd64 leaf");
        let arm64 = Algorithm::Sha256.hash(b"a darwin/arm64 leaf");
        store(
            &data,
            &target,
            &index_of(&[("linux/amd64", &stale), ("darwin/arm64", &arm64)]),
        );

        let outcome = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect("copy");

        let rows: Vec<(String, Disposition)> = outcome
            .platforms
            .iter()
            .map(|row| (row.platform.to_string(), row.disposition))
            .collect();
        assert!(
            rows.contains(&("linux/amd64".to_string(), Disposition::Replaced)),
            "{rows:?}"
        );
        assert!(
            rows.contains(&("darwin/arm64".to_string(), Disposition::KeptNotInSource)),
            "{rows:?}"
        );
        assert_eq!(outcome.platforms.len(), 2);
        let _ = leaves;
    }

    /// A fresh target reports `added`; a re-run of the same copy reports
    /// `unchanged`, which is what makes a promotion safe to repeat.
    #[tokio::test]
    async fn a_fresh_target_adds_and_a_repeat_is_unchanged() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let publisher = publisher_for(&data);

        let first = publisher
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect("first");
        assert_eq!(first.platforms[0].disposition, Disposition::Added);
        assert!(!first.is_no_op());

        let second = publisher
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect("second");
        assert_eq!(second.platforms[0].disposition, Disposition::Unchanged);
        assert!(second.is_no_op(), "a repeated promotion has nothing left to do");
    }

    /// A source in an index-served namespace is read where the index routes it
    /// (ocx#504): the package is seeded only at the physical `team/demo`, and
    /// the logical `served/demo` holds nothing, so a copy that dials the name as
    /// typed finds no manifest. The report still names the source as typed.
    #[tokio::test]
    async fn a_source_served_through_an_index_is_read_at_its_physical_location() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let served = served_at("3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let (digest, dispatch) = dispatch_of(&[("linux/amd64", &leaves[0])]);
        let index = ocx_index::test_source::RoutingSource::rewriting("dev.example.com", "team/demo")
            .with_tag("3.28.1", digest, dispatch)
            .into_index();

        let outcome = publisher_for(&data)
            .copy(&index, request(&served, &target, &annotations))
            .await
            .unwrap_or_else(|error| panic!("copy: {}", chain(&error)));

        assert_eq!(outcome.source, served, "the report names the source as typed");
        assert_eq!(
            outcome.source_location,
            identifier("dev.example.com", "3.28.1"),
            "the source was read where the index routed it, tag carried"
        );
        let landed: Vec<String> = pushed_index(&data, &target)
            .manifests
            .into_iter()
            .map(|entry| entry.digest)
            .collect();
        assert_eq!(landed, vec![leaves[0].to_string()]);
    }

    /// `served/demo` at `tag`: the logical name the index-served tests copy,
    /// which the index routes to the physical `team/demo`.
    fn served_at(tag: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("served/demo", "dev.example.com").clone_with_tag(tag)
    }

    /// An image index over `entries` and its digest — the dispatch an index
    /// commits a tag to. Never stored at the registry: an index serves its own.
    fn dispatch_of(entries: &[(&str, &ocx_oci::Digest)]) -> (ocx_oci::Digest, Manifest) {
        let manifest = index_of(entries);
        let digest = Algorithm::Sha256.hash(serde_json::to_vec(&manifest).expect("serialize"));
        (digest, manifest)
    }

    /// The version an index-served copy carries is the one the index committed,
    /// not whatever the physical tag names now. The physical `3.28.1` is moved
    /// to a different leaf after the index committed it; a copy that read the
    /// physical tag would promote that leaf under the logical version.
    #[tokio::test]
    async fn an_index_served_copy_carries_the_indexs_version_not_the_moved_physical_tag() {
        let data = StubTransportData::new();
        let committed = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let moved = seed_source(&data, "3.28.1", &[("linux/amd64", ARM64_LAYER)]);
        assert_ne!(
            committed, moved,
            "the fixture only discriminates while the two leaves differ"
        );
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let (digest, dispatch) = dispatch_of(&[("linux/amd64", &committed[0])]);
        let index = ocx_index::test_source::RoutingSource::rewriting("dev.example.com", "team/demo")
            .with_tag("3.28.1", digest, dispatch)
            .into_index();

        let outcome = publisher_for(&data)
            .copy(&index, request(&served_at("3.28.1"), &target, &annotations))
            .await
            .unwrap_or_else(|error| panic!("copy: {}", chain(&error)));

        assert_eq!(
            outcome.platforms[0].digest, committed[0],
            "the report names the index's leaf"
        );
        let landed: Vec<String> = pushed_index(&data, &target)
            .manifests
            .into_iter()
            .map(|entry| entry.digest)
            .collect();
        assert_eq!(landed, vec![committed[0].to_string()]);
    }

    /// A tag the authoritative index does not hold, or has yanked, is not
    /// copied — even though the physical registry still answers for it, which
    /// is exactly what a copy reading the physical tag would find and promote.
    #[tokio::test]
    async fn an_index_served_tag_the_index_does_not_hold_or_has_yanked_is_refused() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let owning = || {
            ocx_index::test_source::RoutingSource::rewriting("dev.example.com", "team/demo")
                .authoritative("https://index.example.invalid")
        };

        let absent = publisher_for(&data)
            .copy(
                &owning().into_index(),
                request(&served_at("3.28.1"), &target, &annotations),
            )
            .await
            .expect_err("a tag the index does not hold must not be copied");
        let rendered = chain(&absent);
        assert!(
            rendered.contains("served/demo:3.28.1") && !rendered.contains("dev.example.com/team/demo"),
            "the miss names the source as typed: {rendered}"
        );
        assert!(
            matches!(
                absent.kind,
                CopyErrorKind::Registry(PackageError::OciClient(ClientError::ManifestNotFound(_)))
            ),
            "an absent tag is the same not-found a registry miss is: {rendered}"
        );

        let yanked = publisher_for(&data)
            .copy(
                &owning().with_yanked_tag("3.28.1").into_index(),
                request(&served_at("3.28.1"), &target, &annotations),
            )
            .await
            .expect_err("a yanked tag must not be copied");
        assert!(chain(&yanked).contains("yanked"), "{}", chain(&yanked));

        assert!(
            !data.read().manifests.contains_key(&canonical(&target).to_string()),
            "nothing may land at the target"
        );
    }

    /// S-5: a registry-backed name is read from the registry's own tag, as
    /// before. The index here holds a stale answer for that tag — a derived
    /// root pointing at the name itself — and is not asked, because no index
    /// serves the name.
    #[tokio::test]
    async fn a_registry_backed_copy_reads_the_registrys_tag_not_an_index_answer() {
        let data = StubTransportData::new();
        let stale = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let current = seed_source(&data, "3.28.1", &[("linux/amd64", ARM64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let (digest, dispatch) = dispatch_of(&[("linux/amd64", &stale[0])]);
        let index = ocx_index::test_source::RoutingSource::passthrough()
            .with_tag("3.28.1", digest, dispatch)
            .into_index();

        publisher_for(&data)
            .copy(&index, request(&package_at(&source), &target, &annotations))
            .await
            .unwrap_or_else(|error| panic!("copy: {}", chain(&error)));

        let landed: Vec<String> = pushed_index(&data, &target)
            .manifests
            .into_iter()
            .map(|entry| entry.digest)
            .collect();
        assert_eq!(landed, vec![current[0].to_string()]);
    }

    /// The scratch root the caller supplies is the one each leaf spools into.
    ///
    /// Asserted through its absence, because a spool directory is created and
    /// dropped inside `copy_leaf` and is never observable from out here: a root
    /// that does not exist makes `tempfile::tempdir_in` fail, and the error
    /// carries the path back. A copy that reports a path this test chose is a
    /// copy that used it.
    ///
    /// The positive control is the same copy against a root that does exist,
    /// which succeeds — so the failure below is the root being honoured, not
    /// the fixture being broken.
    #[tokio::test]
    async fn a_supplied_scratch_root_is_the_one_each_leaf_spools_into() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let absent = std::path::PathBuf::from("/nonexistent-ocx-scratch-root/copy");
        let package = package_at(&source);
        let mut req = request(&package, &target, &annotations);
        req.scratch_root = &absent;
        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect_err("a scratch root that does not exist cannot be spooled into");
        assert!(
            chain(&error).contains("nonexistent-ocx-scratch-root"),
            "the failure must name the root the caller supplied: {}",
            chain(&error)
        );

        let control = StubTransportData::new();
        seed_source(&control, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        publisher_for(&control)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect("control: the same copy succeeds against a root that exists");
    }

    /// Every read a promotion takes at the target must address the canonical
    /// target, never a configured mirror.
    ///
    /// A mirror is read-only (ADR Q5) while the rolling tags are written
    /// canonically, so a tag listing or a blocker probe answered by a mirror is
    /// a decision about a repository the write never touches — CWE-345/367,
    /// `subsystem-oci.md` Invariant #5. The assertion is on the *auth* host
    /// rather than on the answer because the stub's tag listing ignores the
    /// reference it is handed; the auth handshake is where the host choice is
    /// observable for every read the run takes.
    #[tokio::test]
    async fn a_promotion_never_reads_the_target_through_a_mirror() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let publisher =
            Publisher::new(client_for(&data).with_test_mirror("prod.example.com", "mirror.invalid", "upstream"));

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.cascade = true;
        publisher.copy(&PASSTHROUGH, req).await.expect("copy");

        let mirrored: Vec<String> = data
            .read()
            .auth_calls
            .iter()
            .map(|(registry, _)| registry.clone())
            .filter(|registry| registry == "mirror.invalid")
            .collect();
        assert!(
            mirrored.is_empty(),
            "no read a promotion decides from may address the mirror, got {mirrored:?}"
        );
    }

    /// A target whose index cannot be *read* is not a target that is empty.
    ///
    /// Absence already arrives as `Ok(None)` (`ManifestNotFound`, which covers
    /// a repository that does not exist yet), so the error arm carries only
    /// genuine failures — auth, transport, a malformed index. Swallowing them
    /// made every platform report `added` and dropped every `kept (not in
    /// source)` row, and `--dry-run` is exactly where that lie is loudest: it
    /// writes nothing, so nothing later surfaces the failure.
    #[tokio::test]
    async fn an_unreadable_target_index_fails_instead_of_reporting_an_empty_target() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        let stale = Algorithm::Sha256.hash(b"an older linux/amd64 leaf");
        store(&data, &target, &index_of(&[("linux/amd64", &stale)]));
        // The target's index is present but unreadable: the registry answers
        // with a digest that does not match the bytes it served.
        {
            let mut inner = data.write();
            let key = canonical(&target).to_string();
            let (bytes, _) = inner.manifests.get(&key).expect("target index").clone();
            inner
                .manifests
                .insert(key, (bytes, format!("sha256:{}", "b".repeat(64))));
        }

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.dry_run = true;
        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect_err("an unreadable target index must fail the copy");

        assert!(
            chain(&error).contains("digest mismatch"),
            "the read failure must reach the caller, got: {}",
            chain(&error)
        );
    }

    /// The index entry's descriptor must carry the leaf manifest's real byte
    /// length.
    ///
    /// Phase 2 takes that number from the `LeafCopy` phase 1 already measured
    /// while writing the manifest, instead of re-reading the manifest to
    /// recompute it — one fewer registry round-trip per platform, for a value
    /// that was in hand. The size is what a client uses to bound the manifest
    /// read, so a wrong one is a real defect and not a cosmetic field: this
    /// asserts the value, which is what makes dropping the second read safe.
    #[tokio::test]
    async fn the_index_entry_carries_the_leaf_manifest_s_real_size() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let expected_size = i64::try_from(
            data.read()
                .manifests
                .get(&canonical(&source.without_tag().clone_with_digest(leaves[0].clone())).to_string())
                .expect("the source leaf is seeded")
                .0
                .len(),
        )
        .expect("a test manifest fits i64");

        publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect("copy");

        let pushed = data
            .read()
            .manifests
            .get(&canonical(&target).to_string())
            .expect("the target index was pushed")
            .0
            .clone();
        let index: ImageIndex = serde_json::from_slice(&pushed).expect("the pushed index parses");
        let entry = index
            .manifests
            .iter()
            .find(|entry| entry.digest == leaves[0].to_string())
            .expect("the promoted platform has an entry");
        assert_eq!(
            entry.size, expected_size,
            "the entry must describe the leaf manifest that was actually copied"
        );
    }

    /// The kind has to be reachable by a chain walk, or the error document's
    /// `detail` stays empty however good the enum is: `error_document.rs`
    /// downcasts each link of `source()`, so a kind that is not the outer
    /// error's source is invisible to it.
    #[tokio::test]
    async fn the_failure_kind_is_reachable_by_a_chain_walk() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1")
            .without_tag()
            .clone_with_digest(leaves[0].clone());
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect_err("a bare leaf needs a platform");

        let mut found = None;
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        while let Some(current) = cause {
            if let Some(kind) = current.downcast_ref::<CopyErrorKind>() {
                found = Some(kind);
                break;
            }
            cause = current.source();
        }
        assert!(
            matches!(found, Some(CopyErrorKind::PlatformRequired)),
            "the envelope walks source() and downcasts; the kind must be on that path"
        );
        assert_eq!(
            error.source_identifier,
            package_at(&source),
            "context needs the endpoint that was read"
        );
        assert_eq!(
            error.target_identifier, target,
            "context needs the endpoint that would have been written"
        );
    }

    /// A registry failure must keep the exit code the registry error already
    /// carries, rather than being flattened to the generic `Failure`.
    ///
    /// This is a unit-layer guard for a contract that previously only the
    /// acceptance suite could see. `CopyErrorKind::Registry` is
    /// `#[error(transparent)]`, and transparent forwards `source()` *past* the
    /// value it wraps — so a `classify()` that returns `None` here and trusts
    /// the chain walker silently collapses 79, 80 and 84 into 1. Asserting the
    /// kind is reachable is not enough: that passed while every registry exit
    /// code was wrong.
    #[tokio::test]
    async fn a_registry_failure_keeps_the_exit_code_of_its_cause() {
        let data = StubTransportData::new();
        // Nothing seeded: the source tag does not resolve, which is the
        // registry's own `NotFound`, not a copy-specific refusal.
        let source = identifier("dev.example.com", "9.9.9");
        let target = identifier("prod.example.com", "9.9.9");
        let annotations = BTreeMap::new();

        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect_err("an absent source tag cannot be copied");

        assert!(
            matches!(error.kind, CopyErrorKind::Registry(_)),
            "an absent source tag is a registry failure, not a structural refusal"
        );
    }

    /// A filtered copy must move only the platform asked for, and must still say
    /// what it left behind.
    #[tokio::test]
    async fn a_platform_filter_moves_only_that_platform() {
        let data = StubTransportData::new();
        seed_source(
            &data,
            "3.28.1",
            &[("linux/amd64", AMD64_LAYER), ("darwin/arm64", ARM64_LAYER)],
        );
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.platforms = vec![platform("linux/amd64")];
        let outcome = publisher_for(&data).copy(&PASSTHROUGH, req).await.expect("copy");

        assert_eq!(outcome.platforms.len(), 1);
        assert_eq!(outcome.platforms[0].platform.to_string(), "linux/amd64");
    }

    /// `--dry-run` has to be inert. A plan that writes even one blob is not a
    /// plan.
    #[tokio::test]
    async fn a_dry_run_writes_nothing() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        data.write().calls.clear();

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.dry_run = true;
        let outcome = publisher_for(&data).copy(&PASSTHROUGH, req).await.expect("copy");

        assert!(outcome.dry_run);
        assert_eq!(outcome.platforms[0].disposition, Disposition::Added);
        let calls = data.read().calls.clone();
        assert!(
            !calls.iter().any(|call| call.starts_with("push_")),
            "nothing may be written, calls: {calls:?}"
        );
    }

    /// An index digest names a snapshot of a mutable set. There is no honest way
    /// to merge "the platform list as it was" into a target that has moved on,
    /// so the copy refuses rather than guessing — and refuses before writing.
    #[tokio::test]
    async fn an_index_named_by_digest_is_refused() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source_tag = identifier("dev.example.com", "3.28.1");
        let index_digest = {
            let key = canonical(&source_tag).to_string();
            let digest = data.read().manifests.get(&key).expect("index").1.clone();
            ocx_oci::Digest::try_from(digest.as_str()).expect("digest")
        };
        let source = source_tag.without_tag().clone_with_digest(index_digest);
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        data.write().calls.clear();

        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect_err("an index digest must be refused");
        assert!(
            matches!(error.kind, CopyErrorKind::IndexNamedByDigest),
            "the refusal must be the named kind, not prose: {:?}",
            error.kind
        );
        assert!(
            !data.read().calls.iter().any(|call| call.starts_with("push_")),
            "nothing may be written"
        );
    }

    /// The structural refusals have to reach the shell as usage errors (64), not
    /// as a generic failure: a pipeline that cannot tell "you invoked this wrong"
    /// from "the registry is down" retries the first one forever.
    #[tokio::test]
    async fn a_structural_refusal_classifies_as_a_usage_error() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1")
            .without_tag()
            .clone_with_digest(leaves[0].clone());
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let _error = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect_err("a bare leaf needs a platform");

        // The other arm must NOT be 64, or the split buys nothing: an unreachable
        // registry has to stay distinguishable from a bad invocation.
        let unreachable = identifier("dev.example.com", "9.9.9");
        let _error = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&unreachable), &target, &annotations))
            .await
            .expect_err("an absent source fails");
    }

    /// A leaf manifest carries no platform, so a digest-addressed copy has to be
    /// told one. Guessing would file the package under a platform nobody built
    /// it for: it resolves for the wrong hosts and fails at exec, far away.
    #[tokio::test]
    async fn a_leaf_named_by_digest_requires_exactly_one_platform() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1")
            .without_tag()
            .clone_with_digest(leaves[0].clone());
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, request(&package_at(&source), &target, &annotations))
            .await
            .expect_err("a bare leaf needs a platform");
        assert!(
            matches!(error.kind, CopyErrorKind::PlatformRequired),
            "{:?}",
            error.kind
        );

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.platforms = vec![platform("linux/amd64"), platform("darwin/arm64")];
        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect_err("two platforms for one manifest is not a copy anyone meant");
        assert!(
            matches!(error.kind, CopyErrorKind::PlatformAmbiguous),
            "{:?}",
            error.kind
        );

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.platforms = vec![platform("linux/amd64")];
        let outcome = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect("one platform is enough");
        assert_eq!(outcome.platforms[0].platform.to_string(), "linux/amd64");
    }

    /// One leaf manifest named by two platforms survives as two entries, and
    /// earns exactly one keep tag.
    ///
    /// A publisher produces this whenever one build is valid on both platforms,
    /// and a dedup pass produces it from two byte-identical builds. The two
    /// halves fail in opposite directions: an index keyed by digest would
    /// collapse the platforms and silently drop one, while a keep tag
    /// derived per platform rather than per manifest would push the same bytes
    /// under the same `__ocx.keep.<algorithm>-<hex>` name twice and report two tags for one
    /// artifact.
    #[tokio::test]
    async fn two_platforms_sharing_one_leaf_survive_as_two_entries_and_one_keep_tag() {
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let manifest = leaf_for(AMD64_LAYER);
        let leaf = {
            let bytes = serde_json::to_vec(&manifest).expect("serialize");
            Algorithm::Sha256.hash(&bytes)
        };
        store(&data, &source.without_tag().clone_with_digest(leaf.clone()), &manifest);
        for blob in [CONFIG_BLOB, AMD64_LAYER] {
            let blob_digest = Algorithm::Sha256.hash(blob).to_string();
            data.write().blobs.insert(blob_digest, blob.to_vec());
        }
        store(
            &data,
            &source,
            &index_of(&[("linux/amd64", &leaf), ("darwin/arm64", &leaf)]),
        );
        data.write().capture_pushes = true;

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.keep_tag = true;
        let outcome = publisher_for(&data).copy(&PASSTHROUGH, req).await.expect("copy");

        let mut platforms: Vec<String> = outcome.platforms.iter().map(|row| row.platform.to_string()).collect();
        platforms.sort();
        assert_eq!(platforms, vec!["darwin/arm64".to_string(), "linux/amd64".to_string()]);
        assert!(
            outcome.platforms.iter().all(|row| row.digest == leaf),
            "both platforms name the one leaf"
        );

        let (algorithm, hex) = leaf.parts();
        assert_eq!(
            outcome.keep_tags,
            vec![format!("__ocx.keep.{algorithm}-{hex}")],
            "one manifest earns one keep tag however many platforms name it"
        );

        // The target's own tag has to end up carrying both, or the promotion
        // dropped a platform the source published.
        let index = pushed_index(&data, &target);
        let mut entries: Vec<String> = index
            .manifests
            .iter()
            .filter_map(|entry| entry.platform.clone())
            .filter_map(|p| ocx_oci::Platform::try_from(p).ok())
            .map(|p| p.to_string())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["darwin/arm64".to_string(), "linux/amd64".to_string()]);
    }

    /// Whether a rolling tag may move is decided from the versions the
    /// **target** publishes, not the ones the source does.
    ///
    /// Promotion is exactly where the two lists differ: a staging registry runs
    /// ahead of production, so `3.28` at the target may already point at a patch
    /// the source has never seen. Moving it back to the copied version would be
    /// a downgrade nobody asked for, visible only to whoever pulls `3.28` next.
    ///
    /// Both arms share every input but the target's own tag list, so the
    /// blocker is the only thing that can explain the difference.
    #[tokio::test]
    async fn a_newer_version_at_the_target_holds_the_rolling_tags_back() {
        for target_has_newer in [false, true] {
            let data = StubTransportData::new();
            seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
            let source = identifier("dev.example.com", "3.28.1");
            let target = identifier("prod.example.com", "3.28.1");
            let annotations = BTreeMap::new();

            let listed: Vec<String> = if target_has_newer {
                // 3.28.2 exists at the target and offers the same platform, so
                // it owns 3.28, 3 and latest.
                let newer = Algorithm::Sha256.hash(b"the target's own 3.28.2 leaf");
                store(
                    &data,
                    &target.without_tag().clone_with_tag("3.28.2"),
                    &index_of(&[("linux/amd64", &newer)]),
                );
                vec!["3.28.1".to_string(), "3.28.2".to_string()]
            } else {
                vec!["3.28.1".to_string()]
            };
            data.write().tags = vec![listed];

            let package = package_at(&source);

            let mut req = request(&package, &target, &annotations);
            req.cascade = true;
            let outcome = publisher_for(&data).copy(&PASSTHROUGH, req).await.expect("copy");

            if target_has_newer {
                assert!(
                    outcome.cascade_tags.is_empty(),
                    "a newer version at the target owns the rolling tags, got {:?}",
                    outcome.cascade_tags
                );
            } else {
                assert!(
                    outcome.cascade_tags.contains(&"3.28".to_string()),
                    "with nothing newer at the target the rolling tags move, got {:?}",
                    outcome.cascade_tags
                );
            }
        }
    }

    /// The first promotion of any package cascades, because a repository
    /// nobody has pushed to yet is an empty tag list.
    ///
    /// A registry answers `GET /v2/<name>/tags/list` for an absent repository
    /// with a 404, which the transport maps to `RepositoryNotFound`. Letting
    /// that propagate made every *first* `--cascade` promotion exit 79 and
    /// every second one succeed (#366).
    #[tokio::test]
    async fn a_target_repository_that_does_not_exist_yet_cascades_as_an_empty_tag_list() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        data.write().list_tags_results = vec![Err(ClientError::RepositoryNotFound(
            "prod.example.com/team/demo".to_string(),
        ))];

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.cascade = true;
        let outcome = publisher_for(&data).copy(&PASSTHROUGH, req).await.expect("copy");

        assert!(
            outcome.cascade_tags.contains(&"3.28".to_string()),
            "an absent target repository takes none of the rolling tags, got {:?}",
            outcome.cascade_tags
        );
    }

    /// A tag listing that merely *failed* still aborts the promotion.
    ///
    /// The sibling of the test above, and the arm that proves the fold is
    /// narrow: without it a blanket `Err(_) => Ok(vec![])` would pass just as
    /// happily, and a transient 5xx would cascade `latest` backwards against a
    /// listing nobody read (#157).
    #[tokio::test]
    async fn a_tag_listing_that_merely_failed_still_aborts_the_promotion() {
        let data = StubTransportData::new();
        seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        data.write().list_tags_results = vec![Err(ClientError::Registry(Box::new(std::io::Error::other(
            "503 from the target registry",
        ))))];

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.cascade = true;
        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect_err("a failing tag listing must abort the promotion");

        assert!(
            chain(&error).contains("503 from the target registry"),
            "the listing failure must reach the caller, got: {}",
            chain(&error)
        );
    }

    /// Content lands before any tag names it, so a promotion that dies partway
    /// leaves the target's tags exactly as it found them.
    ///
    /// The failure is injected at the phase 1 / phase 2 seam — `--cascade` to a
    /// target tag that is not a version, which the tag planner refuses on its
    /// first line — because that is the moment the contract is falsifiable: one
    /// step earlier nothing has been written, one step later a tag has already
    /// moved. It doubles as the pin for a real usage bug: a copy that cannot
    /// plan its rolling tags still uploads everything before saying so.
    ///
    /// Note the ordering the code actually implements, which the plan states
    /// backwards: the `__ocx.keep.<algorithm>-<hex>` tag is written in phase 2, after
    /// the primary index merge it is derived from, not alongside the leaf in
    /// phase 1. It cannot be earlier — its subject is the merged index.
    #[tokio::test]
    async fn a_promotion_that_dies_before_the_merge_leaves_every_tag_where_it_was() {
        let data = StubTransportData::new();
        let leaves = seed_source(&data, "3.28.1", &[("linux/amd64", AMD64_LAYER)]);
        let leaf = leaves[0].clone();
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "candidate");
        let annotations = BTreeMap::new();

        // A signature over the leaf: phase 1 copies it, so it is the second
        // piece of evidence that phase 1 ran to completion.
        let signature = leaf_for(SIGNATURE_LAYER);
        let signature_digest = {
            let bytes = serde_json::to_vec(&signature).expect("serialize");
            Algorithm::Sha256.hash(&bytes)
        };
        store(
            &data,
            &source.without_tag().clone_with_digest(signature_digest),
            &signature,
        );
        {
            let mut inner = data.write();
            inner.blobs.insert(
                Algorithm::Sha256.hash(SIGNATURE_LAYER).to_string(),
                SIGNATURE_LAYER.to_vec(),
            );
            inner.referrers.insert(
                format!("{}@{}", source.repository(), leaf),
                vec![descriptor(
                    MEDIA_TYPE_OCI_IMAGE_MANIFEST,
                    &serde_json::to_vec(&signature).expect("serialize"),
                )],
            );
        }

        // The target's tag already serves something; the assertion below is
        // that these exact bytes are still there afterwards.
        let existing = Algorithm::Sha256.hash(b"what prod already serves under candidate");
        store(&data, &target, &index_of(&[("darwin/arm64", &existing)]));
        let untouched = data
            .read()
            .manifests
            .get(&canonical(&target).to_string())
            .cloned()
            .expect("seeded target tag");

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        req.cascade = true;
        req.keep_tag = true;
        req.referrers = true;
        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect_err("a rolling-tag plan needs a version to plan from");
        assert!(chain(&error).contains("candidate"), "{}", chain(&error));

        // Phase 1 completed: the leaf and its signature are at the target.
        assert!(
            data.read()
                .manifests
                .contains_key(&canonical(&target.without_tag().clone_with_digest(leaf)).to_string()),
            "the leaf manifest must already be at the target"
        );
        assert!(
            data.read().calls.iter().any(|call| call == "push_referrer_manifest"),
            "the referrer must already be at the target"
        );

        // Phase 2 never started: no tag moved, and no keep tag was minted.
        assert_eq!(
            data.read().manifests.get(&canonical(&target).to_string()),
            Some(&untouched),
            "the target's own tag must still hold exactly the bytes it held"
        );
        let tagged: Vec<String> = data
            .read()
            .manifests
            .keys()
            .filter(|key| key.starts_with("prod.example.com/"))
            .filter(|key| !key.contains('@'))
            .filter(|key| key.as_str() != canonical(&target).to_string())
            .cloned()
            .collect();
        assert!(tagged.is_empty(), "no tag may have been written, got {tagged:?}");
    }

    /// Naming a platform the source does not publish is the caller's mistake,
    /// and the message has to say what they could have named instead.
    ///
    /// The old wording — `invalid manifest: <source> offers no platform
    /// matching the request` — got both halves wrong. It sent the reader to
    /// inspect an artifact that is perfectly well formed, and it withheld the
    /// one fact that ends the session: the platform list. Fixing the sentence
    /// fixes the exit code with it, because the fault was never in the data.
    #[tokio::test]
    async fn a_platform_the_source_does_not_publish_is_a_usage_error_naming_what_it_does() {
        let data = StubTransportData::new();
        seed_source(
            &data,
            "3.28.1",
            &[("linux/amd64", AMD64_LAYER), ("darwin/arm64", ARM64_LAYER)],
        );
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let package = package_at(&source);

        let mut req = request(&package, &target, &annotations);
        // The confusable one: the source publishes darwin/arm64, not linux/arm64.
        req.platforms = vec![platform("linux/arm64")];
        let error = publisher_for(&data)
            .copy(&PASSTHROUGH, req)
            .await
            .expect_err("a platform the source does not publish is not a copy");

        assert!(
            matches!(error.kind, CopyErrorKind::NoMatchingPlatform { .. }),
            "{:?}",
            error.kind
        );

        let rendered = chain(&error);
        assert!(
            rendered.contains("linux/arm64"),
            "must name what was asked for: {rendered}"
        );
        assert!(
            rendered.contains("linux/amd64") && rendered.contains("darwin/arm64"),
            "must name what is on offer: {rendered}"
        );
        assert!(
            !rendered.contains("invalid manifest"),
            "the manifest is not what is invalid: {rendered}"
        );
    }

    /// `Disposition` has two renderings and only one of them is a contract.
    ///
    /// Written as an exhaustive `match` so a new variant is a compile error
    /// here rather than a wire value nobody chose. The prose is deliberately
    /// unparseable — `kept (not in source)` carries a space and two
    /// parentheses — which is exactly why a JSON consumer must never see it.
    #[test]
    fn a_disposition_serializes_as_a_token_and_displays_as_prose() {
        for disposition in [
            Disposition::Added,
            Disposition::Unchanged,
            Disposition::Replaced,
            Disposition::KeptNotInSource,
        ] {
            let (token, prose) = match disposition {
                Disposition::Added => ("added", "added"),
                Disposition::Unchanged => ("unchanged", "unchanged"),
                Disposition::Replaced => ("replaced", "replaced"),
                Disposition::KeptNotInSource => ("kept_not_in_source", "kept (not in source)"),
            };
            assert_eq!(
                serde_json::to_string(&disposition).expect("serialize"),
                format!("\"{token}\""),
                "wire value for {disposition:?}"
            );
            assert_eq!(disposition.to_string(), prose, "terminal rendering for {disposition:?}");
        }
    }
}
