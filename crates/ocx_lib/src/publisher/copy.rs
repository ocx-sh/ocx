// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Promotion of an already-published package between registries.
//!
//! Three objects, three different copy semantics — the whole design is in that
//! distinction (`adr_package_copy.md`):
//!
//! - a **leaf platform manifest** and its blobs are immutable content, copied
//!   verbatim by [`crate::oci::copy::copy_leaf`] so the digest never moves;
//! - a **tag's image index** is a mutable set keyed by platform, so it is merged
//!   one platform at a time and never byte-copied;
//! - **rolling tags** are derived, so they are recomputed against the *target*
//!   registry's tag list rather than carried over from the source's.

use std::collections::BTreeMap;

use serde::Serialize;

use super::Publisher;
use crate::oci::client::Client;
use crate::oci::client::error::ClientError;
use crate::package::cascade;
use crate::package::version::Version;
use crate::prelude::*;
use crate::{log, oci};

/// Why a copy could not be performed.
///
/// Three-layer, matching `SignError`/`VerifyError`: this struct attaches the
/// per-copy context (which promotion failed), and [`CopyErrorKind`] is the
/// discriminant. The kind is `#[source]`, so a chain walk reaches it and the
/// `--json` envelope can fill both `error.detail` (the kind slug) and
/// `error.context` (the identifiers) — neither of which a bare pass-through
/// wrapper could ever supply.
#[derive(Debug, thiserror::Error)]
#[error("copying {source_identifier} to {target_identifier}")]
pub struct CopyError {
    /// The package the copy was reading from.
    pub source_identifier: oci::Identifier,
    /// The package the copy was writing to.
    pub target_identifier: oci::Identifier,
    /// Discriminant kind of the failure.
    #[source]
    pub kind: CopyErrorKind,
}

impl crate::cli::ClassifyExitCode for CopyError {
    /// Delegates to the kind, except for the pass-through arm.
    ///
    /// [`CopyErrorKind::Registry`] means "no copy-specific code fits this", so
    /// it asks the cause it wraps — a registry 404 keeps 79, a missing
    /// Referrers API keeps 84, a 401 keeps 80, rather than all three being
    /// flattened. Without this impl every structural refusal would classify as
    /// the generic `Failure` (1) and a pipeline could not tell a bad invocation
    /// from an unreachable registry.
    ///
    /// It must delegate explicitly rather than return `None` and leave it to
    /// the chain walker: the arm is `#[error(transparent)]`, which forwards
    /// `source()` *past* the inner error instead of to it, so a walk starting
    /// here never visits the one value that knows its own code.
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        use crate::cli::ClassifyErrorKind;
        match &self.kind {
            CopyErrorKind::Registry(cause) => cause.classify(),
            kind => Some(kind.exit_code()),
        }
    }
}

/// Discriminant kind for [`CopyError`].
///
/// The structural refusals are named variants rather than formatted strings so
/// that a caller can dispatch on them without parsing stderr, and so the
/// `detail` slug is a value rather than prose. Their messages omit the source
/// identifier: the outer [`CopyError`] already names both endpoints, and a
/// chain reporter prints the pair once (ERR-06).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CopyErrorKind {
    /// The source names an image index by digest.
    ///
    /// Exit 64 (`UsageError`). An index digest is a snapshot of a mutable set;
    /// there is no honest way to merge "the platform list as it was" into a
    /// target that has moved on.
    #[error("names an image index by digest; copy the tag instead")]
    IndexNamedByDigest,

    /// The source is a bare platform manifest and no platform was named.
    ///
    /// Exit 64 (`UsageError`). A leaf manifest carries no platform of its own,
    /// and guessing files the package under a platform nobody built it for.
    #[error("is a platform manifest and carries no platform; pass --platform")]
    PlatformRequired,

    /// The source is a bare platform manifest and more than one platform was
    /// named.
    ///
    /// Exit 64 (`UsageError`).
    #[error("is a single platform manifest; pass exactly one --platform")]
    PlatformAmbiguous,

    /// The source index offers no platform the request names.
    ///
    /// Exit 64 (`UsageError`) — the index is intact; the request named
    /// something it does not publish.
    #[error("offers no platform matching {requested}; available: {available}")]
    NoMatchingPlatform {
        /// What the invocation asked for, joined for display.
        requested: String,
        /// What the source actually offers, joined for display. Without it the
        /// reader has to go and list the index by hand to fix their own typo.
        available: String,
    },

    /// Everything else — registry, transport, index. Carries the cause, and
    /// defers classification to it.
    #[error(transparent)]
    Registry(#[from] crate::Error),
}

impl crate::cli::ClassifyErrorKind for CopyErrorKind {
    fn exit_code(&self) -> crate::cli::ExitCode {
        use crate::cli::ExitCode;
        match self {
            Self::IndexNamedByDigest
            | Self::PlatformRequired
            | Self::PlatformAmbiguous
            // Naming a platform the source does not publish is an invocation
            // fault: the index is intact and the request is what has to change.
            | Self::NoMatchingPlatform { .. } => ExitCode::UsageError,
            // Never reached through `CopyError::classify`, which asks the
            // wrapped cause for its own code instead. Only a caller that
            // classifies a bare kind, with the cause discarded, lands here —
            // and at that point there is nothing left to be more specific.
            Self::Registry(_) => ExitCode::Failure,
        }
    }

    fn kind_detail(&self) -> &'static str {
        // Frozen contract: the snake_case parallel of the variant name.
        // Exhaustive — adding a variant forces a new arm here.
        match self {
            Self::IndexNamedByDigest => "index_named_by_digest",
            Self::PlatformRequired => "platform_required",
            Self::PlatformAmbiguous => "platform_ambiguous",
            Self::NoMatchingPlatform { .. } => "no_matching_platform",
            Self::Registry(_) => "registry",
        }
    }
}

impl From<ClientError> for CopyErrorKind {
    fn from(error: ClientError) -> Self {
        Self::Registry(error.into())
    }
}

impl From<crate::oci::digest::error::DigestError> for CopyErrorKind {
    fn from(error: crate::oci::digest::error::DigestError) -> Self {
        Self::Registry(error.into())
    }
}

/// What a copy was asked to do.
#[derive(Debug)]
pub struct CopyRequest<'a> {
    /// Where to read from. A tag names an image index (or, for a
    /// single-platform package, a bare manifest); a digest names one leaf and
    /// then `platforms` must name its platform, because a leaf manifest does not
    /// carry one — OCX records the platform in the index entry and the build
    /// receipt, never in the manifest (`package/metadata/authoring.rs`).
    pub source: &'a oci::Identifier,
    /// Where to write. Carries the tag the promoted package lands on.
    pub target: &'a oci::Identifier,
    /// Empty means every platform the source index offers.
    pub platforms: Vec<oci::Platform>,
    /// Recompute rolling tags (`3.28`, `3`, `latest`) against the target.
    pub cascade: bool,
    /// Write the digest-named `sha256.<hex>` deletion safety net.
    pub canonical_tag: bool,
    /// Carry signatures, SBOMs and everything else anchored to each leaf.
    pub referrers: bool,
    /// Index-level annotations to merge into every tag this copy touches.
    pub annotations: &'a BTreeMap<String, String>,
    /// Plan only — report what would happen and write nothing.
    pub dry_run: bool,
    /// Where each promoted layer spools on its way through.
    ///
    /// Not optional. `$TMPDIR` is memory-backed on most Linux hosts, so a
    /// caller that left this unset would defeat the bounded spool the copy
    /// relies on — the cap bounds the file, not the medium it lands in. The
    /// CLI owns the `FileStructure` and therefore the `TempStore` root; any
    /// other consumer must name a directory it is willing to fill.
    pub scratch_root: &'a std::path::Path,
}

/// What became of one platform at the target.
///
/// Two renderings, one vocabulary. `Serialize` is the machine-readable form a
/// `--format json` consumer matches on; `Display` below is the terminal prose,
/// which is free to read like English precisely because nothing parses it.
/// Pre-formatting the prose into a JSON string made a sentence with a space and
/// two parentheses into a wire value (`subsystem-cli-api.md`, "Typed Enums Over
/// Strings").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    /// The target's index had no entry for this platform.
    Added,
    /// The target already pointed at this exact digest — nothing to do.
    Unchanged,
    /// The target pointed at a different digest for this platform.
    Replaced,
    /// The target offers this platform and the source does not, so the merge
    /// leaves it alone. Reported because a filtered copy that silently left a
    /// mixed index behind would be indistinguishable from a complete one.
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
    pub platform: oci::Platform,
    /// The leaf digest now at the target for this platform. For a
    /// `KeptNotInSource` row this is the digest the target already had.
    pub digest: oci::Digest,
    pub disposition: Disposition,
}

/// The result of a promotion.
#[derive(Debug)]
pub struct CopyOutcome {
    pub source: oci::Identifier,
    pub target: oci::Identifier,
    /// One row per platform, source-supplied and target-only alike.
    pub platforms: Vec<CopiedPlatform>,
    /// Rolling tags written in addition to the target's own tag.
    pub cascade_tags: Vec<String>,
    /// Digest-named `sha256.<hex>` tags written, deduped by manifest digest.
    pub canonical_tags: Vec<String>,
    /// Referrer manifests copied, summed over every platform.
    pub referrers: usize,
    pub blobs: oci::copy::BlobTransfers,
    /// True when nothing was written.
    pub dry_run: bool,
}

impl CopyOutcome {
    /// True when every source platform was already at the target under the same
    /// digest — a re-run of a completed promotion.
    pub fn is_no_op(&self) -> bool {
        self.platforms
            .iter()
            .all(|row| matches!(row.disposition, Disposition::Unchanged | Disposition::KeptNotInSource))
    }
}

impl Publisher {
    /// Promote an already-published package to another registry or repository.
    ///
    /// See [`CopyRequest`]. Leaf manifests and blobs are copied first — pure
    /// additions, invisible until a tag names them — and only then are the
    /// indexes merged and the rolling tags moved. An interruption during the
    /// first phase therefore leaves the target's tags exactly as they were.
    pub async fn copy(&self, request: CopyRequest<'_>) -> std::result::Result<CopyOutcome, CopyError> {
        // The endpoints are attached once, here, so every `?` inside `run` can
        // stay on the bare kind — the alternative is threading two identifiers
        // through each conversion, which is what makes wrapper errors get
        // written as bare pass-throughs in the first place.
        let source_identifier = request.source.clone();
        let target_identifier = request.target.clone();
        run(&self.client, request).await.map_err(|kind| CopyError {
            source_identifier,
            target_identifier,
            kind,
        })
    }
}

async fn run(client: &Client, request: CopyRequest<'_>) -> std::result::Result<CopyOutcome, CopyErrorKind> {
    let source_leaves = resolve_source_leaves(client, request.source, &request.platforms).await?;
    let target_entries = read_target_entries(client, request.target).await?;

    let mut rows = Vec::new();
    // What phase 1 wrote, and the leaf size it measured while writing it —
    // phase 2's index entries are built from this, never from `source_leaves`,
    // so a platform can only be named in a tag after its content landed.
    let mut copied_leaves: Vec<(oci::Platform, oci::Digest, i64)> = Vec::new();
    let mut blobs = oci::copy::BlobTransfers::default();
    let mut referrers = 0usize;
    let mut cascade_tags: Vec<String> = Vec::new();
    let mut canonical_tags: Vec<String> = Vec::new();

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

        // Phase 1 — content. `Unchanged` re-verifies rather than short-circuits,
        // and that is deliberate: an index entry proves the *manifest* is
        // present under that digest, and says nothing about whether every blob
        // it names still is. A target that was garbage-collected, partially
        // pushed, or restored from an incomplete backup carries an entry whose
        // blobs are gone, and a no-op would report `unchanged` over a package
        // that cannot be pulled. Re-verifying costs one HEAD per already-present
        // blob and one re-PUT of the leaf, which a registry deduplicates. Do not
        // "optimise" this into a skip — the ADR and the user docs state the
        // re-verify contract to match.
        //
        let copied = oci::copy::copy_leaf(
            client,
            request.source,
            request.target,
            source_digest,
            request.referrers,
            request.scratch_root,
        )
        .await?;
        blobs += copied.blobs;
        referrers += copied.referrers;
        copied_leaves.push((platform.clone(), source_digest.clone(), copied.size));
    }

    // Every platform the target offers that this copy did not touch. Listed so a
    // filtered promotion says out loud what it left behind.
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
        // Phase 2 — tags. Every merge is a read-modify-write of one index, so
        // platforms are merged sequentially; concurrent merges would race and
        // the loser's platform would vanish.
        let primary = request.target.tag_or_latest().to_string();
        for (platform, digest, size) in &copied_leaves {
            // One list, primary first, built once. `target_tags` has already
            // dropped the primary from the rolling set, so the report's
            // `cascade_tags` is exactly this list's tail — deriving both from
            // one value is what stops the tags reported from drifting away from
            // the tags written. The primary leads because only its merged index
            // is a subject a canonical tag may be derived from.
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
            if request.canonical_tag
                && let Some(index) = primary_index
                && let Some(tag) = client
                    .push_canonical_tag(request.target, &oci::Manifest::ImageIndex(index), platform)
                    .await?
                && !canonical_tags.contains(&tag)
            {
                canonical_tags.push(tag);
            }
        }
    }

    Ok(CopyOutcome {
        source: request.source.clone(),
        target: request.target.clone(),
        platforms: rows,
        cascade_tags,
        canonical_tags,
        referrers,
        blobs,
        dry_run: request.dry_run,
    })
}

/// The rolling tags this platform should move at the **target**.
///
/// Computed from the target's own tag list, never the source's: whether `3.28`
/// should point at `3.28.1` depends on what the target already publishes, and a
/// staging registry that is ahead of production has a different answer.
///
/// Both registry reads below address the **canonical** target
/// ([`ReadAddressing::Canonical`]), never a configured mirror — this listing
/// and the blocker probe inside `resolve_cascade_tags` together decide which
/// rolling tags get re-pointed, and the tags are written canonically.
/// Answering that question from a mirror and applying it to the canonical
/// registry is a decision about a repository nobody read
/// (`subsystem-oci.md` Invariant #5, CWE-345/367).
///
/// A target repository nobody has pushed to yet answers the tag listing with a
/// 404, and that is an answer, not a failure: none of the rolling tags are
/// taken. `list_tags_or_empty_addressed` folds exactly that case and nothing
/// else, so a transient failure still aborts the promotion.
async fn target_tags(client: &Client, request: &CopyRequest<'_>, platform: &oci::Platform) -> Result<Vec<String>> {
    if !request.cascade {
        return Ok(Vec::new());
    }
    let tag = request.target.tag_or_latest();
    let version = Version::parse(tag).ok_or_else(|| crate::package::error::Error::VersionInvalid(tag.to_string()))?;
    let listed = client
        .list_tags_or_empty_addressed(request.target.clone(), crate::oci::client::ReadAddressing::Canonical)
        .await?;
    let existing = super::Publisher::parse_versions(&listed);
    let (tags, _) = cascade::resolve_cascade_tags(client, request.target, &version, &existing, platform).await?;
    Ok(tags.into_iter().filter(|candidate| candidate != tag).collect())
}

/// The `(platform, leaf digest)` pairs this copy will move.
async fn resolve_source_leaves(
    client: &Client,
    source: &oci::Identifier,
    requested: &[oci::Platform],
) -> std::result::Result<Vec<(oci::Platform, oci::Digest)>, CopyErrorKind> {
    let (_, digest, manifest) = client
        .fetch_manifest_raw_bytes(source)
        .await?
        .ok_or_else(|| ClientError::ManifestNotFound(source.to_string()))?;

    match manifest {
        oci::Manifest::ImageIndex(index) => {
            if source.digest().is_some() {
                // An index digest is a snapshot of a mutable set. Promoting it
                // would carry the source's whole platform list as if it were
                // content, and there is no honest way to merge "the set as it
                // was" into a target that has moved on. Name the tag instead.
                return Err(CopyErrorKind::IndexNamedByDigest);
            }
            let mut leaves = Vec::new();
            // Collected before the filter, not after: the whole value of this
            // list is that it says what the caller could have asked for.
            let mut available = Vec::new();
            for entry in index.manifests {
                let Some(native) = entry.platform else { continue };
                let platform = oci::Platform::try_from(native)?;
                available.push(platform.to_string());
                if !requested.is_empty() && !requested.contains(&platform) {
                    continue;
                }
                leaves.push((platform, oci::Digest::try_from(&entry.digest)?));
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
        // A bare manifest carries no platform of its own, so the caller has to
        // declare one. Guessing would file the package under a platform nobody
        // built it for, which resolves for the wrong hosts and fails at exec.
        oci::Manifest::Image(_) => match requested {
            [platform] => Ok(vec![(platform.clone(), digest)]),
            [] => Err(CopyErrorKind::PlatformRequired),
            _ => Err(CopyErrorKind::PlatformAmbiguous),
        },
    }
}

fn lookup<'a>(entries: &'a [(oci::Platform, oci::Digest)], platform: &oci::Platform) -> Option<&'a oci::Digest> {
    entries
        .iter()
        .find(|(candidate, _)| candidate == platform)
        .map(|(_, digest)| digest)
}

/// The platform entries the target's tag already carries.
///
/// Absent tag, or a tag naming a bare manifest, both mean "nothing to compare
/// against" — the first because the tag is new, the second because a
/// single-platform tag has no per-platform entry the merge could preserve.
///
/// A *failure* to read is neither, and is propagated. Absence already arrives
/// as `Ok(None)`: `fetch_manifest_raw_bytes_addressed` maps
/// `ClientError::ManifestNotFound` there, and the transport folds
/// `MANIFEST_UNKNOWN`, `NOT_FOUND` and `NAME_UNKNOWN` into it — so a target
/// repository that does not exist yet, the first-promotion case, is still an
/// empty list rather than an error. What remains in the `Err` arm is an auth
/// denial, a transient 5xx, an SSRF refusal, a digest mismatch on the target's
/// own index, or a malformed index. Reporting any of those as "the target has
/// nothing" makes every platform read `Added`, silently drops every
/// `KeptNotInSource` row, and does it most visibly under `--dry-run`, which
/// writes nothing and so never reaches the "it surfaces on the first write"
/// backstop — exactly the review step before a production promotion.
async fn read_target_entries(client: &Client, target: &oci::Identifier) -> Result<Vec<(oci::Platform, oci::Digest)>> {
    // A `Vec` rather than a map: `Platform` is deliberately not `Ord` (its
    // ordering would have to encode compatibility, which is a directed relation,
    // not a total order), and an index carries a handful of entries.
    let mut entries = Vec::new();
    let Some((_, _, manifest)) = client.fetch_manifest_raw_bytes(target).await? else {
        log::debug!("Target {target} has no index yet");
        return Ok(entries);
    };
    let oci::Manifest::ImageIndex(index) = manifest else {
        return Ok(entries);
    };
    for entry in index.manifests {
        let Some(native) = entry.platform else { continue };
        let platform = oci::Platform::try_from(native)?;
        entries.push((platform, oci::Digest::try_from(&entry.digest)?));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::MEDIA_TYPE_OCI_IMAGE_MANIFEST;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData, blob_location_key};
    use crate::oci::{Algorithm, Descriptor, ImageIndex, ImageIndexEntry, ImageManifest, Manifest};

    const CONFIG_BLOB: &[u8] = b"{\"package\":\"demo\"}";
    const AMD64_LAYER: &[u8] = b"a linux/amd64 layer";
    const ARM64_LAYER: &[u8] = b"a darwin/arm64 layer";

    const SIGNATURE_LAYER: &[u8] = b"a sigstore bundle";

    /// The image index the target's own tag ended up holding.
    fn pushed_index(data: &StubTransportData, identifier: &oci::Identifier) -> ImageIndex {
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

    fn platform(text: &str) -> oci::Platform {
        text.parse().expect("platform")
    }

    fn identifier(registry: &str, tag: &str) -> oci::Identifier {
        oci::Identifier::new_registry("team/demo", registry).clone_with_tag(tag)
    }

    /// The seam production reads and writes through; building the reference off
    /// `Identifier` directly is allow-listed away from this file (T-arch-A1).
    fn canonical(identifier: &oci::Identifier) -> oci::native::Reference {
        client_for(&StubTransportData::new()).read_reference(identifier, crate::oci::client::ReadAddressing::Canonical)
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
            layers: vec![descriptor(crate::MEDIA_TYPE_TAR_GZ, layer)],
            ..Default::default()
        })
    }

    fn store(data: &StubTransportData, identifier: &oci::Identifier, manifest: &Manifest) -> oci::Digest {
        let bytes = serde_json::to_vec(manifest).expect("serialize");
        let digest = Algorithm::Sha256.hash(&bytes);
        data.write()
            .manifests
            .insert(canonical(identifier).to_string(), (bytes, digest.to_string()));
        digest
    }

    fn index_of(entries: &[(&str, &oci::Digest)]) -> Manifest {
        Manifest::ImageIndex(ImageIndex {
            schema_version: crate::oci::INDEX_SCHEMA_VERSION,
            media_type: Some(crate::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
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
    fn seed_source(data: &StubTransportData, tag: &str, platforms: &[(&str, &'static [u8])]) -> Vec<oci::Digest> {
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
        let entries: Vec<(&str, &oci::Digest)> = platforms.iter().map(|(text, _)| *text).zip(leaves.iter()).collect();
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
        source: &'a oci::Identifier,
        target: &'a oci::Identifier,
        annotations: &'a BTreeMap<String, String>,
    ) -> CopyRequest<'a> {
        CopyRequest {
            source,
            target,
            platforms: Vec::new(),
            cascade: false,
            canonical_tag: false,
            referrers: false,
            annotations,
            dry_run: false,
            scratch_root: SCRATCH.path(),
        }
    }

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
            .copy(request(&source, &target, &annotations))
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
            .copy(request(&source, &target, &annotations))
            .await
            .expect("first");
        assert_eq!(first.platforms[0].disposition, Disposition::Added);
        assert!(!first.is_no_op());

        let second = publisher
            .copy(request(&source, &target, &annotations))
            .await
            .expect("second");
        assert_eq!(second.platforms[0].disposition, Disposition::Unchanged);
        assert!(second.is_no_op(), "a repeated promotion has nothing left to do");
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
        let mut req = request(&source, &target, &annotations);
        req.scratch_root = &absent;
        let error = publisher_for(&data)
            .copy(req)
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
            .copy(request(&source, &target, &annotations))
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

        let mut req = request(&source, &target, &annotations);
        req.cascade = true;
        publisher.copy(req).await.expect("copy");

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

        let mut req = request(&source, &target, &annotations);
        req.dry_run = true;
        let error = publisher_for(&data)
            .copy(req)
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
            .copy(request(&source, &target, &annotations))
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

    /// The kind slugs are a frozen consumer contract, and every kind must carry
    /// an exit code — the `--json` envelope's `detail` field is what a script
    /// dispatches on instead of parsing stderr.
    ///
    /// Written as an exhaustive `match` rather than a list of asserts so that
    /// adding a variant is a compile error here, not a silently absent slug.
    #[test]
    fn every_copy_error_kind_has_a_frozen_slug_and_an_exit_code() {
        use crate::cli::{ClassifyErrorKind, ExitCode};

        let kinds = [
            CopyErrorKind::IndexNamedByDigest,
            CopyErrorKind::PlatformRequired,
            CopyErrorKind::PlatformAmbiguous,
            CopyErrorKind::NoMatchingPlatform {
                requested: "linux/riscv64".to_string(),
                available: "linux/amd64".to_string(),
            },
        ];
        for kind in &kinds {
            let (slug, code) = match kind {
                CopyErrorKind::IndexNamedByDigest => ("index_named_by_digest", ExitCode::UsageError),
                CopyErrorKind::PlatformRequired => ("platform_required", ExitCode::UsageError),
                CopyErrorKind::PlatformAmbiguous => ("platform_ambiguous", ExitCode::UsageError),
                CopyErrorKind::NoMatchingPlatform { .. } => ("no_matching_platform", ExitCode::UsageError),
                CopyErrorKind::Registry(_) => unreachable!("the pass-through arm is not in this fixture"),
            };
            assert_eq!(kind.kind_detail(), slug, "slug for {kind:?}");
            assert_eq!(kind.exit_code(), code, "exit code for {kind:?}");
        }
    }

    /// The kind has to be reachable by a chain walk, or the envelope's
    /// `detail` stays empty however good the enum is: `error_envelope.rs`
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
            .copy(request(&source, &target, &annotations))
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
            error.source_identifier, source,
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
        use crate::cli::{ClassifyExitCode, ExitCode};

        let data = StubTransportData::new();
        // Nothing seeded: the source tag does not resolve, which is the
        // registry's own `NotFound`, not a copy-specific refusal.
        let source = identifier("dev.example.com", "9.9.9");
        let target = identifier("prod.example.com", "9.9.9");
        let annotations = BTreeMap::new();

        let error = publisher_for(&data)
            .copy(request(&source, &target, &annotations))
            .await
            .expect_err("an absent source tag cannot be copied");

        assert!(
            matches!(error.kind, CopyErrorKind::Registry(_)),
            "an absent source tag is a registry failure, not a structural refusal"
        );
        assert_eq!(
            error.classify(),
            Some(ExitCode::NotFound),
            "the cause knows it is a 404; the wrapper must ask it rather than \
             defer to a chain walk that transparent has already skipped past"
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

        let mut req = request(&source, &target, &annotations);
        req.platforms = vec![platform("linux/amd64")];
        let outcome = publisher_for(&data).copy(req).await.expect("copy");

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

        let mut req = request(&source, &target, &annotations);
        req.dry_run = true;
        let outcome = publisher_for(&data).copy(req).await.expect("copy");

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
            oci::Digest::try_from(digest.as_str()).expect("digest")
        };
        let source = source_tag.without_tag().clone_with_digest(index_digest);
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();
        data.write().calls.clear();

        let error = publisher_for(&data)
            .copy(request(&source, &target, &annotations))
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

        let error = publisher_for(&data)
            .copy(request(&source, &target, &annotations))
            .await
            .expect_err("a bare leaf needs a platform");
        assert_eq!(
            crate::cli::classify_error(&error),
            crate::cli::ExitCode::UsageError,
            "a structural refusal must be exit 64"
        );

        // The other arm must NOT be 64, or the split buys nothing: an unreachable
        // registry has to stay distinguishable from a bad invocation.
        let unreachable = identifier("dev.example.com", "9.9.9");
        let error = publisher_for(&data)
            .copy(request(&unreachable, &target, &annotations))
            .await
            .expect_err("an absent source fails");
        assert_ne!(crate::cli::classify_error(&error), crate::cli::ExitCode::UsageError);
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
            .copy(request(&source, &target, &annotations))
            .await
            .expect_err("a bare leaf needs a platform");
        assert!(
            matches!(error.kind, CopyErrorKind::PlatformRequired),
            "{:?}",
            error.kind
        );

        let mut req = request(&source, &target, &annotations);
        req.platforms = vec![platform("linux/amd64"), platform("darwin/arm64")];
        let error = publisher_for(&data)
            .copy(req)
            .await
            .expect_err("two platforms for one manifest is not a copy anyone meant");
        assert!(
            matches!(error.kind, CopyErrorKind::PlatformAmbiguous),
            "{:?}",
            error.kind
        );

        let mut req = request(&source, &target, &annotations);
        req.platforms = vec![platform("linux/amd64")];
        let outcome = publisher_for(&data).copy(req).await.expect("one platform is enough");
        assert_eq!(outcome.platforms[0].platform.to_string(), "linux/amd64");
    }

    /// One leaf manifest named by two platforms survives as two entries, and
    /// earns exactly one canonical tag.
    ///
    /// A publisher produces this whenever one build is valid on both platforms,
    /// and a dedup pass produces it from two byte-identical builds. The two
    /// halves fail in opposite directions: an index keyed by digest would
    /// collapse the platforms and silently drop one, while a canonical tag
    /// derived per platform rather than per manifest would push the same bytes
    /// under the same `sha256.<hex>` name twice and report two tags for one
    /// artifact.
    #[tokio::test]
    async fn two_platforms_sharing_one_leaf_survive_as_two_entries_and_one_canonical_tag() {
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

        let mut req = request(&source, &target, &annotations);
        req.canonical_tag = true;
        let outcome = publisher_for(&data).copy(req).await.expect("copy");

        let mut platforms: Vec<String> = outcome.platforms.iter().map(|row| row.platform.to_string()).collect();
        platforms.sort();
        assert_eq!(platforms, vec!["darwin/arm64".to_string(), "linux/amd64".to_string()]);
        assert!(
            outcome.platforms.iter().all(|row| row.digest == leaf),
            "both platforms name the one leaf"
        );

        let (algorithm, hex) = leaf.parts();
        assert_eq!(
            outcome.canonical_tags,
            vec![format!("{algorithm}.{hex}")],
            "one manifest earns one canonical tag however many platforms name it"
        );

        // The target's own tag has to end up carrying both, or the promotion
        // dropped a platform the source published.
        let index = pushed_index(&data, &target);
        let mut entries: Vec<String> = index
            .manifests
            .iter()
            .filter_map(|entry| entry.platform.clone())
            .filter_map(|p| oci::Platform::try_from(p).ok())
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

            let mut req = request(&source, &target, &annotations);
            req.cascade = true;
            let outcome = publisher_for(&data).copy(req).await.expect("copy");

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

        let mut req = request(&source, &target, &annotations);
        req.cascade = true;
        let outcome = publisher_for(&data).copy(req).await.expect("copy");

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

        let mut req = request(&source, &target, &annotations);
        req.cascade = true;
        let error = publisher_for(&data)
            .copy(req)
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
    /// backwards: the canonical `sha256.<hex>` tag is written in phase 2, after
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

        let mut req = request(&source, &target, &annotations);
        req.cascade = true;
        req.canonical_tag = true;
        req.referrers = true;
        let error = publisher_for(&data)
            .copy(req)
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

        // Phase 2 never started: no tag moved, and no canonical tag was minted.
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
        use crate::cli::{ClassifyExitCode, ExitCode};

        let data = StubTransportData::new();
        seed_source(
            &data,
            "3.28.1",
            &[("linux/amd64", AMD64_LAYER), ("darwin/arm64", ARM64_LAYER)],
        );
        let source = identifier("dev.example.com", "3.28.1");
        let target = identifier("prod.example.com", "3.28.1");
        let annotations = BTreeMap::new();

        let mut req = request(&source, &target, &annotations);
        // The confusable one: the source publishes darwin/arm64, not linux/arm64.
        req.platforms = vec![platform("linux/arm64")];
        let error = publisher_for(&data)
            .copy(req)
            .await
            .expect_err("a platform the source does not publish is not a copy");

        assert!(
            matches!(error.kind, CopyErrorKind::NoMatchingPlatform { .. }),
            "{:?}",
            error.kind
        );
        assert_eq!(
            error.classify(),
            Some(ExitCode::UsageError),
            "the request is what is wrong, not the index"
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
                Disposition::KeptNotInSource => ("kept-not-in-source", "kept (not in source)"),
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
