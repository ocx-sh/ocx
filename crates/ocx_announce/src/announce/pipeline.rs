// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure decision logic and the registry observe loop for the announce pipeline;
//! the forge-touching orchestration lives in [`super`].

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use futures::stream::{self, StreamExt, TryStreamExt};
use serde_json::{Map, Value, json};

use super::error::AnnounceError;
use super::request::TagSelection;
use crate::forge::{FileChange, Forge, RepoCoordinate};
use ocx_oci::annotations;
use ocx_oci::client::{ManifestPresence, NotFoundCode, ReadAddressing};
use ocx_oci::tag::InternalTag;
use ocx_package::publisher::Publisher;
use ocx_package::tag::Tag;

/// One curated tag's freshly observed state, verbatim from the registry.
///
/// Never re-encoded, or the CAS payload stops being byte-identical to the
/// artifact the publisher pushed.
pub struct Observed {
    pub tag: String,
    /// The image-index digest the registry served (the tag's new `content`).
    pub content: ocx_oci::Digest,
    /// The registry's image-index bytes, unmodified (the CAS payload).
    pub bytes: Vec<u8>,
}

/// One given tag as the canonical registry answered it.
pub(crate) enum TagObservation {
    /// The registry serves the tag.
    Present(Observed),
    /// The registry answered 404; only [`NotFoundCode::ManifestUnknown`] may remove a row.
    Absent { tag: String, code: NotFoundCode },
}

/// What one run does to the rows of its given tags.
#[derive(Default)]
pub(crate) struct TagPlan {
    /// Tags the registry serves, in observation order: each row is added or refreshed.
    pub observed: Vec<Observed>,
    /// Tags confirmed gone whose rows are removed, in observation order.
    pub removed: Vec<String>,
    /// Durable rows whose tag is gone, kept because the run reached them without naming them.
    pub durable_missing: Vec<String>,
}

/// The observed `__ocx.desc` artifact: the rebuilt `desc` object when it moved,
/// plus the payload blobs the root points at.
pub struct ObservedDesc {
    /// The new `desc` object in the index bot's field order, or `None` when the
    /// `__ocx.desc` digest did not move and the committed `desc` rides through.
    /// An absent logo omits the key: the index schema has no `null` form for it.
    pub desc: Option<Value>,
    /// The readme blob, and the logo blob when there is one, verbatim.
    ///
    /// Carried on every run, not only when `desc` moved, or `--out` writes a root
    /// naming a `desc.readme` object it never wrote and the index rejects it.
    pub blobs: Vec<DescBlob>,
}

/// One `__ocx.desc` payload blob, stored as this package's own CAS object.
pub struct DescBlob {
    /// The index's own SHA-256 over `bytes`, not the registry's blob digest.
    pub digest: ocx_oci::Digest,
    /// The CAS filename extension: `md` for the readme, `png`/`svg` for a logo.
    pub extension: &'static str,
    /// The blob bytes exactly as the registry served them.
    pub bytes: Vec<u8>,
}

/// The outcome of collapsing a [`TagSelection`] against the committed tags.
pub struct ResolvedTags {
    /// The curated tags to observe, in resolution order.
    pub tags: Vec<String>,
    /// Reserved tags dropped from the selection, in resolution order.
    pub reserved_dropped: Vec<String>,
}

/// The physical registry target dereferenced from a root's `repository` pointer.
#[derive(Debug)]
pub struct Physical {
    /// The registry host (for the SSRF pre-flight).
    pub host: String,
    /// The registry port (443 unless the pointer carried an explicit `:port`).
    pub port: u16,
    pub identifier: ocx_oci::OciIdentifier,
    /// The verbatim `oci://…` pointer, echoed in observe error messages.
    pub display: String,
}

/// The `observed` timestamp for new or changed tags; computed once per run so a
/// tag map is internally consistent.
pub fn current_timestamp() -> String {
    ocx_index::current_timestamp()
}

/// Require a committed root at `base_ref`.
///
/// # Errors
///
/// [`AnnounceError::UnclaimedPackage`] when `bytes` is `None`.
pub fn require_root(
    package: &str,
    path: &str,
    base_ref: &str,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<u8>, AnnounceError> {
    bytes.ok_or_else(|| AnnounceError::UnclaimedPackage {
        package: package.to_string(),
        path: path.to_string(),
        base_ref: base_ref.to_string(),
    })
}

/// The tag names in the committed root, in on-disk order.
pub fn committed_tag_names(root: &Value) -> Vec<String> {
    root.get("tags")
        .and_then(Value::as_object)
        .map(|tags| tags.keys().cloned().collect())
        .unwrap_or_default()
}

/// Resolve the curated tag set from the selection, the committed tags and
/// `discovered` (the registry's tags, used only by `FromRegistry`).
///
/// Duplicates drop, first occurrence wins. Reserved tags are dropped and
/// reported in `reserved_dropped`, never refused.
///
/// # Errors
///
/// [`AnnounceError::NoCuratedTags`] when nothing survives under
/// [`TagSelection::Replace`].
pub fn resolve_curated_tags(
    selection: &TagSelection,
    committed: &[String],
    discovered: &[String],
) -> Result<ResolvedTags, AnnounceError> {
    let resolved = match selection {
        TagSelection::Replace(tags) => dedup_in_order(tags),
        // Only the listed tags: a committed row the file leaves out is carried verbatim, never re-observed.
        TagSelection::UnionFile(file_tags) => dedup_in_order(file_tags),
        TagSelection::Refresh => dedup_in_order(committed),
        TagSelection::FromRegistry => union_onto_committed(committed, discovered),
    };
    // After the collapse, or a reserved tag already in the committed root is
    // re-announced forever by `--refresh` and `--tags-from-registry`.
    let (reserved_dropped, tags): (Vec<String>, Vec<String>) =
        resolved.into_iter().partition(|tag| Tag::is_reserved_str(tag));
    // Only `--tags` refuses the empty set: a fresh claim writes `"tags": {}`, and an empty
    // tags file is a no-op. Named positively so a new selection cannot silently join the refusing side.
    if tags.is_empty() && matches!(selection, TagSelection::Replace(_)) {
        return Err(AnnounceError::NoCuratedTags { reserved_dropped });
    }
    Ok(ResolvedTags { tags, reserved_dropped })
}

/// The committed tags a regenerated root would delete, minus `reserved_dropped`
/// (`adr_announce_diverged_branch_rebuild.md § The invariant, restated`).
///
/// `committed` must be the root the commit is parented on, or the check passes
/// on a tree the commit never lands on.
pub fn dropped_committed_tags(committed: &[String], regenerated: &Value, reserved_dropped: &[String]) -> Vec<String> {
    let kept = committed_tag_names(regenerated);
    committed
        .iter()
        .filter(|tag| !kept.contains(tag) && !reserved_dropped.contains(tag))
        .cloned()
        .collect()
}

/// The committed set in on-disk order, then the new `additions`.
fn union_onto_committed(committed: &[String], additions: &[String]) -> Vec<String> {
    let mut union = dedup_in_order(committed);
    for tag in additions {
        if !union.contains(tag) {
            union.push(tag.clone());
        }
    }
    union
}

/// List the physical repository's tags, silently dropping reserved ones.
///
/// `physical` must come from [`guarded_physical`]: this is the run's first
/// registry request. Reserved tags are not reported, or every published
/// version's keep tag floods `reserved_dropped`.
///
/// # Errors
///
/// [`AnnounceError::ListTags`] when the listing fails; an empty repository is
/// not an error.
pub async fn list_registry_tags(publisher: &Publisher, physical: &Physical) -> Result<Vec<String>, AnnounceError> {
    let tags = publisher
        .list_tags(physical.identifier.clone())
        .await
        .map_err(|source| AnnounceError::ListTags {
            repository: physical.display.clone(),
            source: Box::new(source),
        })?;
    Ok(tags.into_iter().filter(|tag| !Tag::is_reserved_str(tag)).collect())
}

fn dedup_in_order(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.iter().filter(|tag| seen.insert((*tag).clone())).cloned().collect()
}

/// Strictly parse an `oci://host/path` pointer into its physical registry target.
pub fn extract_physical(pointer: &str) -> Result<Physical, AnnounceError> {
    let identifier = ocx_oci::OciIdentifier::parse_repository_pointer(pointer).map_err(|_| {
        AnnounceError::MalformedPhysicalRepository {
            value: pointer.to_string(),
        }
    })?;
    let (host, port) = ocx_oci::ssrf::split_host_port(identifier.registry());
    Ok(Physical {
        host: host.to_string(),
        port,
        identifier,
        display: pointer.to_string(),
    })
}

/// Resolve and SSRF-validate the physical host before any registry request.
///
/// Thread the one returned `Physical` to every registry call, or the guarded host
/// and the requested host can diverge. `insecure_hosts` picks the dial scheme,
/// which decides whether `HTTP_PROXY` or `HTTPS_PROXY` applies.
///
/// # Errors
///
/// [`AnnounceError::MalformedPhysicalRepository`] if `pointer` is not an
/// `oci://host/path` reference; [`AnnounceError::Ssrf`] if the host is
/// forbidden or unresolvable.
pub async fn guarded_physical(
    pointer: &str,
    namespace: &str,
    trusted_hosts: &[String],
    insecure_hosts: &[String],
    rules: &ocx_oci::ssrf::ProxyRules,
) -> Result<Physical, AnnounceError> {
    let physical = extract_physical(pointer)?;
    ocx_oci::ssrf::guard_destination(
        ocx_oci::ssrf::DialScheme::for_registry(insecure_hosts, physical.identifier.registry()),
        &physical.host,
        physical.port,
        trusted_hosts,
        rules,
    )
    .await
    .map_err(|source| AnnounceError::Ssrf {
        namespace: namespace.to_string(),
        source,
    })?;
    Ok(physical)
}

/// How many curated tags are observed at once; raising it risks a registry `429`.
const OBSERVE_CONCURRENCY: usize = 64;

/// Observe every given tag against the physical repository.
///
/// `physical` must come from [`guarded_physical`]; this runs no pre-flight.
///
/// # Errors
///
/// [`AnnounceError::ObserveRaced`] when a tag read as absent is present on the
/// follow-up probe; [`AnnounceError::TagIsNotAnImageIndex`];
/// [`AnnounceError::Observe`] on a transport failure.
pub async fn observe_curated(
    publisher: &Publisher,
    physical: &Physical,
    curated: &[String],
) -> Result<Vec<TagObservation>, AnnounceError> {
    // `buffered`, not `buffer_unordered`, or the root's tag order and the
    // reported error depend on which request finished first.
    stream::iter(
        curated
            .iter()
            .map(|tag| async move { observe_one_tag(publisher, physical, tag).await }),
    )
    .buffered(OBSERVE_CONCURRENCY)
    .try_collect()
    .await
}

/// Observe one given tag on the canonical registry, refusing a bare image manifest.
///
/// A not-found fetch is confirmed by [`ocx_oci::Client::probe_manifest_canonical`],
/// which keeps the envelope code the removal decision needs.
async fn observe_one_tag(
    publisher: &Publisher,
    physical: &Physical,
    tag: &str,
) -> Result<TagObservation, AnnounceError> {
    let observe_error = |source| AnnounceError::Observe {
        tag: tag.to_string(),
        repository: physical.display.clone(),
        source: Box::new(source),
    };
    let tagged = physical.identifier.clone_with_tag(tag);
    // Canonical, never a mirror: a lagging copy would remove a row whose tag still exists.
    let fetched = publisher
        .client()
        .fetch_manifest_raw_bytes_addressed(&tagged, ReadAddressing::Canonical)
        .await
        .map_err(observe_error)?;
    let Some((bytes, content, manifest)) = fetched else {
        return match publisher
            .client()
            .probe_manifest_canonical(&tagged)
            .await
            .map_err(observe_error)?
        {
            ManifestPresence::Absent(code) => Ok(TagObservation::Absent {
                tag: tag.to_string(),
                code,
            }),
            // A push landed between the two reads; neither answer is safe to act on.
            ManifestPresence::Present(_) => Err(AnnounceError::ObserveRaced {
                tag: tag.to_string(),
                repository: physical.display.clone(),
            }),
        };
    };
    if !matches!(manifest, ocx_oci::Manifest::ImageIndex(_)) {
        return Err(AnnounceError::TagIsNotAnImageIndex {
            tag: tag.to_string(),
            repository: physical.display.clone(),
        });
    }
    Ok(TagObservation::Present(Observed {
        tag: tag.to_string(),
        content,
        bytes,
    }))
}

/// Decide each given tag's row against the committed root.
///
/// A present tag is upserted. A tag answered `MANIFEST_UNKNOWN` removes an
/// ephemeral row, and a durable row only when `selection` names it
/// (`Replace`, `UnionFile`); a durable row reached by `Refresh` or
/// `FromRegistry` is kept and listed in [`TagPlan::durable_missing`].
///
/// # Errors
///
/// [`AnnounceError::UnresolvedTag`] for a gone tag with no committed row, and
/// for any other not-found code (nothing is removed).
pub(crate) fn plan_tags(
    observations: Vec<TagObservation>,
    committed: &Value,
    selection: &TagSelection,
    repository: &str,
) -> Result<TagPlan, AnnounceError> {
    let committed_tags = committed.get("tags").and_then(Value::as_object);
    let named = matches!(selection, TagSelection::Replace(_) | TagSelection::UnionFile(_));
    let mut plan = TagPlan::default();
    for observation in observations {
        let (tag, code) = match observation {
            TagObservation::Present(observed) => {
                plan.observed.push(observed);
                continue;
            }
            TagObservation::Absent { tag, code } => (tag, code),
        };
        let row = committed_tags.and_then(|tags| tags.get(&tag));
        // `NAME_UNKNOWN` or a bare 404 says nothing about the tag, and a gone tag with no row is a typo.
        let Some(row) = row.filter(|_| code == NotFoundCode::ManifestUnknown) else {
            return Err(AnnounceError::UnresolvedTag {
                tag,
                repository: repository.to_string(),
            });
        };
        if is_ephemeral(row) || named {
            plan.removed.push(tag);
        } else {
            plan.durable_missing.push(tag);
        }
    }
    Ok(plan)
}

/// Only an explicit `true` marks a row removable without review.
fn is_ephemeral(row: &Value) -> bool {
    row.get("ephemeral")
        .is_some_and(ocx_index::RootTag::is_ephemeral_marker)
}

/// The commit and request body: the package, then its added, removed and durable-missing tags.
///
/// `dropped` names the committed rows the run leaves out besides the confirmed-gone
/// ones (a `--tags` omission), listed as removed so a reviewer sees every row that leaves.
pub(crate) fn change_body(package: &str, committed: &Value, plan: &TagPlan, dropped: &[String]) -> String {
    let committed_tags = committed.get("tags").and_then(Value::as_object);
    let changed: Vec<&str> = plan
        .observed
        .iter()
        .filter(|entry| {
            let committed_content = committed_tags
                .and_then(|tags| tags.get(&entry.tag))
                .and_then(|row| row.get("content"))
                .and_then(Value::as_str);
            committed_content != Some(entry.content.to_string().as_str())
        })
        .map(|entry| entry.tag.as_str())
        .collect();
    let mut body = format!("Publisher-curated tag update for `{package}`.\n");
    for (label, tags) in [
        ("Added or updated", changed),
        (
            "Removed",
            plan.removed.iter().chain(dropped).map(String::as_str).collect(),
        ),
        (
            "Kept, gone from the registry",
            plan.durable_missing.iter().map(String::as_str).collect(),
        ),
    ] {
        // The body is a push option that must carry no markdown link or mention, so a
        // name outside the tag grammar (a committed-root key is unchecked) is left out.
        let tags: Vec<&str> = tags
            .into_iter()
            .filter(|tag| ocx_oci::client::is_valid_oci_tag(tag))
            .collect();
        if !tags.is_empty() {
            body.push_str(&format!("\n{label}: {}\n", tags.join(", ")));
        }
    }
    body
}

/// Observe the `__ocx.desc` artifact, comparing its tag digest with the
/// committed `desc.digest`.
///
/// `physical` must come from [`guarded_physical`]; this runs no pre-flight.
///
/// # Errors
///
/// [`AnnounceError::DescDisappeared`] when the committed root records a
/// description the registry no longer serves; [`AnnounceError::ObserveDesc`] on
/// a transport failure or a malformed description artifact.
pub async fn observe_desc(
    publisher: &Publisher,
    physical: &Physical,
    committed_root: &Value,
) -> Result<ObservedDesc, AnnounceError> {
    let committed_digest = committed_root
        .get("desc")
        .and_then(|desc| desc.get("digest"))
        .and_then(Value::as_str);
    let desc_identifier = physical.identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);
    // The description probe decides no removal, so it stays mirrored.
    let observed = publisher
        .client()
        .probe_manifest_digest_addressed(&desc_identifier, ReadAddressing::Mirrored)
        .await
        .map_err(|source| AnnounceError::ObserveDesc {
            repository: physical.display.clone(),
            source: Box::new(source),
        })?
        .map(|digest| digest.to_string());
    let Some(observed_digest) = observed else {
        if let Some(committed_digest) = committed_digest {
            return Err(AnnounceError::DescDisappeared {
                repository: physical.display.clone(),
                digest: committed_digest.to_string(),
            });
        }
        return Ok(ObservedDesc {
            desc: None,
            blobs: Vec::new(),
        });
    };
    let moved = Some(observed_digest.as_str()) != committed_digest;

    // Private scratch dir, or a concurrent run's layer downloads collide on filenames.
    let temporary = tempfile::tempdir().map_err(|source| AnnounceError::OutputWrite {
        path: std::env::temp_dir().display().to_string(),
        source,
    })?;
    let description = ocx_package::description::transport::pull_description_addressed(
        publisher.client(),
        &physical.identifier,
        temporary.path(),
        ReadAddressing::Mirrored,
    )
    .await
    .map_err(|source| AnnounceError::ObserveDesc {
        repository: physical.display.clone(),
        source: Box::new(source),
    })?
    // Gone between the HEAD and the GET: the same retraction as above.
    .ok_or_else(|| AnnounceError::DescDisappeared {
        repository: physical.display.clone(),
        digest: observed_digest.clone(),
    })?;

    let manifest_annotations = description.annotations;
    let readme = description.readme.into_bytes();
    let readme_digest = ocx_oci::Algorithm::Sha256.hash(&readme);
    let logo = description.logo.map(|logo| {
        let extension = if logo.media_type == ocx_oci::media_type::MEDIA_TYPE_PNG {
            "png"
        } else {
            "svg"
        };
        DescBlob {
            digest: ocx_oci::Algorithm::Sha256.hash(&logo.data),
            extension,
            bytes: logo.data,
        }
    });

    let mut desc = Map::new();
    desc.insert("digest".to_string(), Value::String(observed_digest));
    desc.insert(
        "title".to_string(),
        Value::String(title(&manifest_annotations, committed_root, physical)),
    );
    desc.insert(
        "description".to_string(),
        annotation(&manifest_annotations, annotations::DESCRIPTION),
    );
    desc.insert(
        "keywords".to_string(),
        Value::Array(parse_keywords(manifest_annotations.get(annotations::KEYWORDS))),
    );
    desc.insert("readme".to_string(), Value::String(readme_digest.to_string()));
    if let Some(logo) = &logo {
        desc.insert("logo".to_string(), Value::String(logo.digest.to_string()));
    }

    let mut blobs = vec![DescBlob {
        digest: readme_digest,
        extension: "md",
        bytes: readme,
    }];
    blobs.extend(logo);
    Ok(ObservedDesc {
        desc: moved.then(|| Value::Object(desc)),
        blobs,
    })
}

/// A manifest annotation as a JSON string, empty when absent: a non-null `desc`
/// requires `description`.
fn annotation(annotations: &BTreeMap<String, String>, key: &str) -> Value {
    Value::String(annotations.get(key).cloned().unwrap_or_default())
}

/// The `desc.title`: the title annotation, else the last segment of the root's
/// `name`, else of the physical repository.
///
/// Never empty: an empty title passes the pull request checks, then fails
/// schema validation and blocks the index deploy for every package.
fn title(annotations: &BTreeMap<String, String>, committed_root: &Value, physical: &Physical) -> String {
    let annotated = annotations
        .get(annotations::TITLE)
        .map(String::as_str)
        .unwrap_or_default();
    let name = committed_root.get("name").and_then(Value::as_str).unwrap_or_default();
    for candidate in [
        annotated,
        last_segment(name),
        last_segment(physical.identifier.repository()),
    ] {
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }
    // Unreachable via `guarded_physical`; never emit the empty title.
    physical.display.clone()
}

/// The part after the last `/`, or the whole string when it holds none.
fn last_segment(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, last)| last)
}

/// Split the comma-separated `sh.ocx.keywords` annotation.
fn parse_keywords(raw: Option<&String>) -> Vec<Value> {
    raw.map(String::as_str)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .map(|keyword| Value::String(keyword.to_string()))
        .collect()
}

/// Append every `branch_tags` key the base root lacks, in branch order, after
/// the base's tags (`adr_announce_diverged_branch_rebuild.md`).
///
/// Tag order is byte-visible ([`serialize_root`](ocx_index::serialize_root)
/// never sorts), so base order first is a wire contract.
// ponytail: union, not a 3-way merge — a tag the base dropped through a `--tags`
// replace while the branch still carried it is re-proposed. Upgrade: a 3-way
// against the compare API's merge base (`merge_base_commit` on GitHub,
// `/repository/merge_base` on GitLab), once someone hits the re-proposal.
pub fn carry_branch_tags(base: &mut Value, branch_tags: &Value) {
    let Some(branch_tags) = branch_tags.as_object() else {
        return;
    };
    let Some(root) = base.as_object_mut() else {
        return;
    };
    let mut merged = root.get("tags").and_then(Value::as_object).cloned().unwrap_or_default();
    // The base entry wins, or an unreviewed branch reverts the base's merged yank.
    for (tag, entry) in branch_tags {
        merged.entry(tag.clone()).or_insert_with(|| entry.clone());
    }
    root.insert("tags".to_string(), Value::Object(merged));
}

/// Rebuild the root's `tags` map from `plan`.
///
/// Walks the committed rows in committed order: an observed row is regenerated,
/// a removed row dropped, a row `Replace` does not name dropped, a reserved row
/// the selection gives dropped, any other row cloned verbatim. Newly observed
/// tags follow in observation order, marked ephemeral when `ephemeral` is set.
/// An unmoved digest keeps its committed entry verbatim, or a no-op re-observe
/// stops being byte-identical; a moved digest keeps the whole entry and changes
/// only `content` and `observed`. A committed `variants` key is removed without
/// reordering the root.
pub fn regenerate(committed: &Value, plan: &TagPlan, selection: &TagSelection, ephemeral: bool, now: &str) -> Value {
    let observed: BTreeMap<&str, &Observed> = plan.observed.iter().map(|entry| (entry.tag.as_str(), entry)).collect();
    let given = |tag: &str| match selection {
        TagSelection::Replace(tags) | TagSelection::UnionFile(tags) => tags.iter().any(|named| named == tag),
        TagSelection::Refresh | TagSelection::FromRegistry => true,
    };
    let mut new_tags = Map::new();
    let committed_tags = committed.get("tags").and_then(Value::as_object);
    for (tag, row) in committed_tags.into_iter().flatten() {
        let dropped = plan.removed.contains(tag)
            || (matches!(selection, TagSelection::Replace(_)) && !given(tag))
            // A reserved row is never a version: dropped whenever the run is given it, as the resolve step reports.
            || (Tag::is_reserved_str(tag) && given(tag));
        if dropped {
            continue;
        }
        let regenerated = match observed.get(tag.as_str()) {
            Some(entry) => regenerate_row(row, &entry.content.to_string(), now),
            None => row.clone(),
        };
        new_tags.insert(tag.clone(), regenerated);
    }
    for entry in &plan.observed {
        if !new_tags.contains_key(&entry.tag) {
            new_tags.insert(
                entry.tag.clone(),
                new_tag_entry(&entry.content.to_string(), now, ephemeral),
            );
        }
    }
    // Remove `variants`, or a stale committed set rides through and the index
    // bot's gate rejects every announce once it stops matching `tags`.
    let mut new_root = committed.clone();
    if let Some(root) = new_root.as_object_mut() {
        // `shift_remove`, never `remove`: `swap_remove` refills the hole from the
        // end and reorders the serialized root.
        root.shift_remove("variants");
        root.insert("tags".to_string(), Value::Object(new_tags));
    }
    new_root
}

/// A committed row re-observed at `content`: verbatim when unmoved, else the
/// whole object with only `content` and `observed` rewritten in place, so the
/// yank, the marker and any field a newer writer added survive.
fn regenerate_row(row: &Value, content: &str, now: &str) -> Value {
    if row.get("content").and_then(Value::as_str) == Some(content) {
        return row.clone();
    }
    let Some(object) = row.as_object() else {
        return new_tag_entry(content, now, false);
    };
    let mut object = object.clone();
    object.insert("content".to_string(), Value::String(content.to_string()));
    object.insert("observed".to_string(), Value::String(now.to_string()));
    Value::Object(object)
}

/// A fresh tag entry in the index bot's `TagEntry` field order: `content,
/// observed, yanked, ephemeral`; a yank lands later, before the marker.
fn new_tag_entry(content: &str, now: &str, ephemeral: bool) -> Value {
    let mut entry = Map::new();
    entry.insert("content".to_string(), Value::String(content.to_string()));
    entry.insert("observed".to_string(), Value::String(now.to_string()));
    if ephemeral {
        entry.insert("ephemeral".to_string(), Value::Bool(true));
    }
    Value::Object(entry)
}

/// Apply yank/unyank markers to the regenerated root's curated tags.
///
/// # Errors
///
/// [`AnnounceError::YankUnyankOverlap`] when a tag is in both lists;
/// [`AnnounceError::YankTagNotCurated`] / [`AnnounceError::UnyankTagNotCurated`]
/// when a named tag is not in the curated (regenerated) set.
pub fn apply_yank_markers(
    root: &mut Value,
    yank: &[String],
    unyank: &[String],
    reason: &str,
    now: &str,
) -> Result<(), AnnounceError> {
    let mut overlap: Vec<String> = yank.iter().filter(|tag| unyank.contains(tag)).cloned().collect();
    if !overlap.is_empty() {
        overlap.sort();
        overlap.dedup();
        return Err(AnnounceError::YankUnyankOverlap { tags: overlap });
    }
    if yank.is_empty() && unyank.is_empty() {
        return Ok(());
    }
    for tag in yank {
        let Some(entry) = root
            .get_mut("tags")
            .and_then(|tags| tags.get_mut(tag))
            .and_then(Value::as_object_mut)
        else {
            return Err(AnnounceError::YankTagNotCurated { tag: tag.clone() });
        };
        let yanked = json!({ "reason": reason, "at": now });
        // Right after `observed`, or a row that already carries `ephemeral` breaks the bot's field order.
        match (
            entry.contains_key("yanked"),
            entry.keys().position(|key| key == "observed"),
        ) {
            (false, Some(observed)) => {
                entry.shift_insert(observed + 1, "yanked".to_string(), yanked);
            }
            _ => {
                entry.insert("yanked".to_string(), yanked);
            }
        }
    }
    for tag in unyank {
        let Some(entry) = root
            .get_mut("tags")
            .and_then(|tags| tags.get_mut(tag))
            .and_then(Value::as_object_mut)
        else {
            return Err(AnnounceError::UnyankTagNotCurated { tag: tag.clone() });
        };
        entry.remove("yanked");
    }
    Ok(())
}

/// Count observed CAS objects no committed tag's `content` references.
pub fn new_cas_count(committed: &Value, observed: &[Observed]) -> usize {
    let committed_contents: HashSet<&str> = committed
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.values()
                .filter_map(|entry| entry.get("content").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    observed
        .iter()
        .filter(|entry| !committed_contents.contains(entry.content.to_string().as_str()))
        .count()
}

/// A CAS object's wire path under its package.
///
/// Takes a parsed [`ocx_oci::Digest`], never a string, or remote root data
/// reaches the path verbatim.
fn object_path(package_repo: &str, digest: &ocx_oci::Digest, extension: &str) -> String {
    let (algorithm, hex) = digest.parts();
    format!("p/{package_repo}/o/{algorithm}/{hex}.{extension}")
}

/// Every CAS object a root references, with its path extension (`None` for the
/// logo, whose extension the root does not record).
///
/// Malformed digests are dropped: the output builds paths from remote data.
fn referenced_objects(root: &Value) -> Vec<(ocx_oci::Digest, Option<&'static str>)> {
    let mut objects = Vec::new();
    let mut push = |raw: Option<&Value>, extension: Option<&'static str>| {
        let Some(raw) = raw.and_then(Value::as_str) else {
            return;
        };
        match ocx_oci::Digest::try_from(raw) {
            Ok(digest) => objects.push((digest, extension)),
            Err(_) => tracing::debug!(reference = %raw, "index root references a malformed digest; skipped"),
        }
    };
    if let Some(tags) = root.get("tags").and_then(Value::as_object) {
        for entry in tags.values() {
            push(entry.get("content"), Some("json"));
        }
    }
    let description = root.get("desc");
    push(description.and_then(|desc| desc.get("readme")), Some("md"));
    push(description.and_then(|desc| desc.get("logo")), None);
    objects
}

/// The object paths `previous_root` referenced that `new_root` does not.
///
/// A logo is probed as `.png` then `.svg` at `base_ref`; neither existing is a
/// silent skip.
///
/// # Errors
///
/// [`AnnounceError::Forge`] when a logo probe fails.
pub(crate) async fn orphan_paths(
    previous_root: Option<&Value>,
    new_root: &Value,
    package_repo: &str,
    forge: &dyn Forge,
    repo: &RepoCoordinate,
    base_ref: &str,
) -> Result<Vec<String>, AnnounceError> {
    let Some(previous_root) = previous_root else {
        return Ok(Vec::new());
    };
    let live: HashSet<ocx_oci::Digest> = referenced_objects(new_root)
        .into_iter()
        .map(|(digest, _)| digest)
        .collect();
    let mut paths = Vec::new();
    for (digest, extension) in referenced_objects(previous_root) {
        if live.contains(&digest) {
            continue;
        }
        let Some(extension) = extension else {
            for candidate in ["png", "svg"] {
                let path = object_path(package_repo, &digest, candidate);
                if forge.get_file_contents(repo, &path, base_ref).await?.is_some() {
                    paths.push(path);
                    break;
                }
            }
            continue;
        };
        paths.push(object_path(package_repo, &digest, extension));
    }
    Ok(paths)
}

/// Assemble the announce's atomic file set: root, CAS objects, description
/// blobs and orphan removals. A path both written and orphaned keeps its `Put`.
pub fn build_files(
    root_path: &str,
    root_bytes: &[u8],
    package_repo: &str,
    observed: &[Observed],
    desc_blobs: &[DescBlob],
    orphans: &[String],
) -> BTreeMap<String, FileChange> {
    let mut files = BTreeMap::new();
    files.insert(root_path.to_string(), FileChange::Put(root_bytes.to_vec()));
    for entry in observed {
        files.insert(
            object_path(package_repo, &entry.content, "json"),
            FileChange::Put(entry.bytes.clone()),
        );
    }
    for blob in desc_blobs {
        files.insert(
            object_path(package_repo, &blob.digest, blob.extension),
            FileChange::Put(blob.bytes.clone()),
        );
    }
    for orphan in orphans {
        files.entry(orphan.clone()).or_insert(FileChange::Delete);
    }
    files
}

/// Write the announce file set under `dir`, returning the sorted relative paths
/// written.
///
/// A [`FileChange::Delete`] removes the path and is a no-op when it is absent.
///
/// # Errors
///
/// [`AnnounceError::OutputWrite`] on any directory-create, file-write or
/// file-remove failure.
pub async fn write_out(dir: &Path, files: &BTreeMap<String, FileChange>) -> Result<Vec<String>, AnnounceError> {
    let mut written = Vec::with_capacity(files.len());
    for (relative, change) in files {
        let path = dir.join(relative);
        let FileChange::Put(bytes) = change else {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(AnnounceError::OutputWrite {
                        path: path.display().to_string(),
                        source,
                    });
                }
            }
            continue;
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| AnnounceError::OutputWrite {
                    path: parent.display().to_string(),
                    source,
                })?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|source| AnnounceError::OutputWrite {
                path: path.display().to_string(),
                source,
            })?;
        written.push(relative.clone());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// The bytes `build_files` wrote at `path`, or `None` when it wrote none.
    ///
    /// The payload is a map of *intents* now, so every assertion on content has
    /// to name the `Put` arm; a helper keeps that one line rather than five.
    fn written<'a>(files: &'a BTreeMap<String, FileChange>, path: &str) -> Option<&'a [u8]> {
        match files.get(path) {
            Some(FileChange::Put(bytes)) => Some(bytes.as_slice()),
            Some(FileChange::Delete) | None => None,
        }
    }

    use super::*;
    use ocx_index::serialize_root;
    use ocx_oci::client::ManifestPresence;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

    // ── fixtures ─────────────────────────────────────────────────────────────

    fn digest_string(fill: char) -> String {
        format!("sha256:{}", fill.to_string().repeat(64))
    }

    /// A 64-hex `__ocx.keep.sha256-<hex>` keep tag — reserved, and the D7 case
    /// a default `ocx package push` writes into every repository.
    fn keep_tag() -> String {
        format!("__ocx.keep.sha256-{}", "a".repeat(64))
    }

    /// The physical pointer every loopback fixture below announces against —
    /// the stub transport's own address.
    const LOOPBACK_POINTER: &str = "oci://127.0.0.1/x";

    /// A committed root Value with one already-observed tag, in canonical form.
    fn committed_root(repository: &str) -> Value {
        serde_json::json!({
            "name": "ocx.sh/acme/widget",
            "repository": repository,
            "owners": [{ "github": "alice", "github_id": 1 }],
            "status": "active",
            "created": "2026-07-24",
            "desc": null,
            "tags": {
                "1.0.0": { "content": digest_string('a'), "observed": "2026-01-01T00:00:00Z" }
            }
        })
    }

    /// Build an `Observed` for `tag` from an image index carrying a single
    /// platform leaf `digest`, exactly as a registry would serve it: the bytes
    /// are the serialized index and `content` is their real digest, so digest
    /// comparisons behave as in production.
    fn observed(tag: &str, leaf: char) -> Observed {
        let bytes = serde_json::to_vec(&image_index(vec![index_entry("amd64", leaf)])).expect("index serializes");
        let content = ocx_oci::Algorithm::Sha256.hash(&bytes);
        Observed {
            tag: tag.to_string(),
            content,
            bytes,
        }
    }

    // ── resolve_curated_tags (C3/C5) ─────────────────────────────────────────

    /// [`resolve_curated_tags`] for the three caller-supplied selections, which
    /// reach no registry and so have nothing discovered. `FromRegistry` tests
    /// call the real function and pass their discovered set explicitly.
    fn resolve_no_discovery(selection: &TagSelection, committed: &[String]) -> Result<ResolvedTags, AnnounceError> {
        resolve_curated_tags(selection, committed, &[])
    }

    #[test]
    fn replace_is_the_universe_and_drops_absent_committed_tags() {
        let committed = vec!["1.0.0".to_string(), "2.0.0".to_string()];
        let curated = resolve_no_discovery(&TagSelection::Replace(vec!["2.0.0".into()]), &committed).unwrap();
        assert_eq!(
            curated.tags,
            vec!["2.0.0".to_string()],
            "a committed tag absent from --tags is dropped"
        );
    }

    /// `--tags-file` names exactly the tags it lists: a committed row the file
    /// leaves out is not re-observed, so it can neither be refreshed nor removed
    /// by this run.
    #[test]
    fn union_file_resolves_to_exactly_the_listed_tags_in_file_order() {
        let committed = vec!["1.0.0".to_string(), "2.0.0".to_string()];
        let curated = resolve_no_discovery(
            &TagSelection::UnionFile(vec!["3.0.0".into(), "1.0.0".into(), "3.0.0".into()]),
            &committed,
        )
        .unwrap();
        assert_eq!(
            curated.tags,
            vec!["3.0.0".to_string(), "1.0.0".to_string()],
            "file order, duplicates dropped, the unlisted committed 2.0.0 not given"
        );
    }

    #[test]
    fn refresh_re_observes_the_committed_set_in_order() {
        let committed = vec!["1.0.0".to_string(), "latest".to_string()];
        let curated = resolve_no_discovery(&TagSelection::Refresh, &committed).unwrap();
        assert_eq!(curated.tags, committed);
    }

    /// The empty set is a refusal for `--tags`: the invocation asked for
    /// nothing, and accepting it would retract the whole curated set on the
    /// strength of a typo. An empty `--tags-file` gives nothing and carries
    /// every row, so it resolves to nothing rather than refusing.
    #[test]
    fn only_an_empty_tags_list_is_refused() {
        assert!(matches!(
            resolve_no_discovery(&TagSelection::Replace(vec![]), &[]),
            Err(AnnounceError::NoCuratedTags { ref reserved_dropped }) if reserved_dropped.is_empty()
        ));
        let from_file = resolve_no_discovery(&TagSelection::UnionFile(vec![]), &["1.0.0".to_string()])
            .expect("an empty tags file is not a refusal");
        assert!(
            from_file.tags.is_empty(),
            "the file gives no tag, the committed row included"
        );
        assert!(from_file.reserved_dropped.is_empty());
    }

    /// #487, the other half: `--refresh` and `--tags-from-registry` derive their
    /// universe from state, and a freshly claimed root's state is `"tags": {}`.
    /// An empty result there is the legitimate "nothing curated yet", so the
    /// collapse returns it and the pipeline runs on to the description
    /// observation — which is the only thing such a run has to do.
    ///
    /// Reds on restoring the old unconditional `tags.is_empty()` refusal.
    #[test]
    fn an_empty_state_derived_selection_resolves_to_no_tags_rather_than_an_error() {
        let refreshed = resolve_no_discovery(&TagSelection::Refresh, &[])
            .expect("--refresh over an empty committed set is not a refusal");
        assert!(
            refreshed.tags.is_empty(),
            "nothing was committed, so nothing is curated"
        );
        assert!(refreshed.reserved_dropped.is_empty(), "nothing was dropped either");

        let discovered = resolve_curated_tags(&TagSelection::FromRegistry, &[], &[])
            .expect("--tags-from-registry over an empty repository is not a refusal");
        assert!(discovered.tags.is_empty());
        assert!(discovered.reserved_dropped.is_empty());
    }

    // ── the D7 reserved-tag filter, one site, all three selections ───────────

    /// Explicit curation of a reserved tag is a drop, not a refusal: a reserved
    /// tag is not a version, so there is nothing to refuse, and refusing would
    /// make announce police how a publisher tags their own repository.
    #[test]
    fn resolve_curated_tags_drops_reserved_from_replace() {
        let keep = keep_tag();
        let curated = resolve_no_discovery(
            &TagSelection::Replace(vec![
                "__ocx.desc".into(),
                "__ocx".into(),
                "__ocxfoo".into(),
                "__OCX.desc".into(),
                keep.clone(),
                "1.2.3".into(),
            ]),
            &[],
        )
        .expect("one real version survives");
        assert_eq!(curated.tags, vec!["1.2.3".to_string()]);
        assert_eq!(
            curated.reserved_dropped,
            vec![
                "__ocx.desc".to_string(),
                "__ocx".to_string(),
                "__ocxfoo".to_string(),
                "__OCX.desc".to_string(),
                keep,
            ],
            "every reserved form is reported, in selection order"
        );
    }

    /// The carrier case: `--refresh` carries no tags of its own, so a reserved
    /// tag already sitting in the committed root would be re-announced forever
    /// if the filter lived at the selection sources instead of here.
    #[test]
    fn resolve_curated_tags_drops_reserved_from_refresh_carrier() {
        let committed = vec!["1.0.0".to_string(), "__ocx.desc".to_string(), keep_tag()];
        let curated = resolve_no_discovery(&TagSelection::Refresh, &committed).unwrap();
        assert_eq!(curated.tags, vec!["1.0.0".to_string()]);
        assert_eq!(curated.reserved_dropped, vec!["__ocx.desc".to_string(), keep_tag()]);
    }

    /// `--tags-file` names its tags itself, so only what the file lists passes
    /// through the one filter; a reserved tag in the committed root is not given.
    #[test]
    fn resolve_curated_tags_drops_reserved_from_union_file() {
        let committed = vec!["1.0.0".to_string(), "__ocx.desc".to_string()];
        let curated =
            resolve_no_discovery(&TagSelection::UnionFile(vec![keep_tag(), "2.0.0".into()]), &committed).unwrap();
        assert_eq!(curated.tags, vec!["2.0.0".to_string()]);
        assert_eq!(
            curated.reserved_dropped,
            vec![keep_tag()],
            "a reserved tag the file names is reported; the committed root's is not given"
        );
    }

    /// An entirely reserved **caller-named** selection is the empty-set case —
    /// it collapses into the existing `NoCuratedTags`, so no new error variant
    /// exists to add. The dropped names ride out on the variant: this is the one
    /// D7 path with no outcome for the CLI's drop notice to read.
    #[test]
    fn resolve_curated_tags_all_reserved_is_no_curated_tags() {
        let Err(AnnounceError::NoCuratedTags { reserved_dropped }) =
            resolve_no_discovery(&TagSelection::Replace(vec!["__ocx.desc".into(), keep_tag()]), &[])
        else {
            panic!("an entirely reserved selection resolves to nothing");
        };
        assert_eq!(reserved_dropped, vec!["__ocx.desc".to_string(), keep_tag()]);
    }

    /// The `--refresh` half of the same shape (#487): a committed set that is
    /// wholly reserved carries no version to announce, but the publisher named
    /// none either — the root simply holds tags that are not versions. So the
    /// drops ride out on `ResolvedTags` for the outcome's
    /// `reserved_tags_dropped` to report, and the run proceeds.
    ///
    /// Reds on restoring the old unconditional `tags.is_empty()` refusal.
    #[test]
    fn an_all_reserved_committed_set_under_refresh_drops_without_refusing() {
        let curated = resolve_no_discovery(&TagSelection::Refresh, &["__ocx.patch".to_string()])
            .expect("a carrier selection never refuses the empty set");
        assert!(curated.tags.is_empty(), "no version survived the D7 filter");
        assert_eq!(
            curated.reserved_dropped,
            vec!["__ocx.patch".to_string()],
            "the drop is reported on the outcome instead of on an error message"
        );

        // The #436 tripwire is the reason the empty set could not simply be
        // waved through: a regenerated root that carries fewer committed tags
        // than it started with is a deletion. It cannot fire here, and this is
        // the assertion rather than the argument — a reserved name is excluded
        // by construction, so the only committed tag is not a loss.
        let regenerated = serde_json::json!({ "tags": {} });
        assert!(
            dropped_committed_tags(&["__ocx.patch".to_string()], &regenerated, &curated.reserved_dropped).is_empty(),
            "a D7 drop is not a lost tag, so the empty refresh does not trip the #436 guard"
        );
    }

    // ── require_root (unclaimed package, C10) ────────────────────────────────

    #[test]
    fn require_root_errors_on_a_missing_committed_root() {
        let error = require_root("acme/widget", "p/acme/widget.json", "main", None).unwrap_err();
        assert!(matches!(error, AnnounceError::UnclaimedPackage { .. }));
    }

    #[test]
    fn require_root_returns_present_bytes() {
        let bytes = require_root("acme/widget", "p/acme/widget.json", "main", Some(b"root".to_vec())).unwrap();
        assert_eq!(bytes, b"root");
    }

    // ── extract_physical (C3) ────────────────────────────────────────────────

    #[test]
    fn extract_physical_parses_host_and_repository() {
        let physical = extract_physical("oci://ghcr.io/ocx-contrib/widget").unwrap();
        assert_eq!(physical.host, "ghcr.io");
        assert_eq!(physical.port, 443);
        assert_eq!(physical.identifier.registry(), "ghcr.io");
        assert_eq!(physical.identifier.repository(), "ocx-contrib/widget");
    }

    #[test]
    fn extract_physical_honours_an_explicit_port() {
        let physical = extract_physical("oci://registry.corp:5000/team/tool").unwrap();
        assert_eq!(physical.host, "registry.corp");
        assert_eq!(physical.port, 5000);
    }

    #[test]
    fn extract_physical_rejects_a_missing_scheme() {
        assert!(matches!(
            extract_physical("ghcr.io/ocx-contrib/widget"),
            Err(AnnounceError::MalformedPhysicalRepository { .. })
        ));
    }

    // ── observe_one_tag — verbatim bytes, and the D4(a) refusal ──────────────

    /// The observation of a tag the test seeded as present.
    fn present(observation: TagObservation) -> Observed {
        match observation {
            TagObservation::Present(observed) => observed,
            TagObservation::Absent { tag, code } => panic!("{tag} was seeded present, observed absent: {code:?}"),
        }
    }

    fn image_index(entries: Vec<ocx_oci::ImageIndexEntry>) -> ocx_oci::Manifest {
        ocx_oci::Manifest::ImageIndex(ocx_oci::ImageIndex {
            schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
            media_type: Some(ocx_oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            artifact_type: None,
            manifests: entries,
            annotations: None,
        })
    }

    fn index_entry(architecture: &str, digest: char) -> ocx_oci::ImageIndexEntry {
        ocx_oci::ImageIndexEntry {
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: digest_string(digest),
            size: 0,
            platform: Some(ocx_oci::native::Platform {
                architecture: architecture.into(),
                os: "linux".into(),
                os_version: None,
                os_features: None,
                variant: None,
                features: None,
            }),
            artifact_type: None,
            annotations: None,
        }
    }

    /// Seed the stub with `manifest` served at `127.0.0.1/x:<tag>` and hand
    /// back the exact bytes and digest the registry would answer with.
    ///
    /// Deliberately pretty-printed: a registry serves whatever encoding the
    /// publisher pushed, not serde's canonical one. Compact bytes here would
    /// make a re-serializing implementation byte-indistinguishable from one
    /// that carries the served bytes through, and the verbatim assertions
    /// below would pass vacuously.
    fn seed_manifest(data: &StubTransportData, tag: &str, manifest: &ocx_oci::Manifest) -> (Vec<u8>, ocx_oci::Digest) {
        let bytes = serde_json::to_vec_pretty(manifest).expect("manifest serializes");
        let digest = ocx_oci::Algorithm::Sha256.hash(&bytes);
        data.write()
            .manifests
            .insert(format!("127.0.0.1/x:{tag}"), (bytes.clone(), digest.to_string()));
        (bytes, digest)
    }

    /// The two-anchor property: the CAS payload is the registry's own bytes and
    /// the `content` pointer is the digest the registry served them under. A
    /// re-serialization creeping back into the announce path breaks both.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_one_tag_keeps_the_registry_bytes_and_digest_verbatim() {
        let data = StubTransportData::new();
        let manifest = image_index(vec![index_entry("amd64", 'a'), index_entry("arm64", 'b')]);
        let (served_bytes, served_digest) = seed_manifest(&data, "1.0.0", &manifest);
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).unwrap();

        let observed = present(observe_one_tag(&publisher, &physical, "1.0.0").await.unwrap());

        assert_eq!(observed.bytes, served_bytes, "the CAS payload must be the served bytes");
        assert_eq!(observed.content, served_digest, "the pointer must be the served digest");
        let files = build_files("p/x.json", b"root", "x", std::slice::from_ref(&observed), &[], &[]);
        let (algorithm, hex) = served_digest.parts();
        assert_eq!(
            written(&files, &format!("p/x/o/{algorithm}/{hex}.json")),
            Some(served_bytes.as_slice()),
            "the CAS filename is the served digest and the file is the served bytes"
        );
    }

    /// D4(a): the index records image indices only. `ocx package push` always
    /// publishes one, so a bare image manifest was not published by ocx — a
    /// refusal, never a silent skip.
    #[tokio::test(flavor = "multi_thread")]
    async fn announce_refuses_a_bare_image_manifest_tag() {
        let data = StubTransportData::new();
        seed_manifest(
            &data,
            "1.0.0",
            &ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
        );
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).unwrap();

        let result = observe_one_tag(&publisher, &physical, "1.0.0").await;

        let Err(AnnounceError::TagIsNotAnImageIndex { tag, repository }) = result else {
            panic!("a bare image manifest must be refused");
        };
        assert_eq!(tag, "1.0.0");
        assert_eq!(repository, "oci://127.0.0.1/x");
    }

    /// A platform-less descriptor (an attestation) no longer costs the whole
    /// index its entry: the pipeline carries the index verbatim and filters
    /// nothing. Candidate selection is the index reader's concern.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_one_tag_carries_platform_less_descriptors_through() {
        let data = StubTransportData::new();
        let mut attestation = index_entry("amd64", 'a');
        attestation.platform = None;
        let manifest = image_index(vec![index_entry("arm64", 'b'), attestation]);
        let (served_bytes, _) = seed_manifest(&data, "1.0.0", &manifest);
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).unwrap();

        let observed = present(observe_one_tag(&publisher, &physical, "1.0.0").await.unwrap());

        assert_eq!(observed.bytes, served_bytes, "no descriptor is dropped on the way in");
    }

    // ── observe_desc (D6) ────────────────────────────────────────────────────

    /// Seed one `__ocx.desc` layer blob on the stub and describe it.
    fn desc_layer(data: &StubTransportData, media_type: &str, bytes: &[u8]) -> ocx_oci::Descriptor {
        let digest = ocx_oci::Algorithm::Sha256.hash(bytes);
        data.write().blobs.insert(digest.to_string(), bytes.to_vec());
        ocx_oci::Descriptor {
            media_type: media_type.to_string(),
            digest: digest.to_string(),
            size: i64::try_from(bytes.len()).expect("test blob fits i64"),
            urls: None,
            artifact_type: None,
            annotations: None,
        }
    }

    /// Seed a description artifact at `127.0.0.1/x:__ocx.desc` — the manifest
    /// and its layer blobs — and hand back the tag digest the registry serves.
    fn seed_description(
        data: &StubTransportData,
        readme: &[u8],
        logo: Option<(&str, &[u8])>,
        annotations: &[(&str, &str)],
    ) -> String {
        let mut layers = vec![desc_layer(data, ocx_oci::media_type::MEDIA_TYPE_MARKDOWN, readme)];
        if let Some((media_type, bytes)) = logo {
            layers.push(desc_layer(data, media_type, bytes));
        }
        let manifest = ocx_oci::Manifest::Image(ocx_oci::ImageManifest {
            artifact_type: Some(ocx_oci::media_type::MEDIA_TYPE_DESCRIPTION_V1.to_string()),
            layers,
            annotations: Some(
                annotations
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect(),
            ),
            ..Default::default()
        });
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        let digest = ocx_oci::Algorithm::Sha256.hash(&bytes);
        data.write().manifests.insert(
            format!("127.0.0.1/x:{}", InternalTag::DESCRIPTION_TAG),
            (bytes, digest.to_string()),
        );
        digest.to_string()
    }

    fn loopback_physical() -> Physical {
        extract_physical(LOOPBACK_POINTER).expect("root parses")
    }

    /// A committed root whose `desc` records tag digest `digest` and points at
    /// CAS readme object `readme`.
    fn root_with_desc(digest: &str, readme: &str) -> Value {
        let mut root = committed_root("oci://127.0.0.1/x");
        root["desc"] = serde_json::json!({
            "digest": digest,
            "title": "Widget",
            "description": "A widget",
            "keywords": [],
            "readme": readme,
        });
        root
    }

    /// The full wire contract of a fresh observation: the `desc` object's field
    /// order and values, and the CAS blobs it points at. `digest` is the
    /// registry's floating `__ocx.desc` tag digest — never a content hash —
    /// while `readme`/`logo` are this index's own hashes over the served bytes,
    /// which the index CI re-derives from the committed files.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_builds_the_wire_object_and_its_cas_blobs() {
        let data = StubTransportData::new();
        let readme = b"# widget\n\nDoes widget things.\n".as_slice();
        let logo = b"\x89PNG\r\n\x1a\nnot-really-a-png".as_slice();
        let tag_digest = seed_description(
            &data,
            readme,
            Some((ocx_oci::media_type::MEDIA_TYPE_PNG, logo)),
            &[
                (ocx_oci::annotations::TITLE, "Widget"),
                (ocx_oci::annotations::DESCRIPTION, "A widget"),
                (ocx_oci::annotations::KEYWORDS, " build ,, tool "),
            ],
        );
        let publisher = stub_publisher(&data);

        let observed = observe_desc(&publisher, &loopback_physical(), &committed_root("oci://127.0.0.1/x"))
            .await
            .expect("the description observes");
        let rebuilt = observed
            .desc
            .clone()
            .expect("a description where the root had none is a change");

        let readme_digest = ocx_oci::Algorithm::Sha256.hash(readme);
        let logo_digest = ocx_oci::Algorithm::Sha256.hash(logo);
        let expected = serde_json::json!({
            "digest": tag_digest,
            "title": "Widget",
            "description": "A widget",
            "keywords": ["build", "tool"],
            "readme": readme_digest.to_string(),
            "logo": logo_digest.to_string(),
        });
        assert_eq!(
            String::from_utf8(serialize_root(&rebuilt)).expect("UTF-8"),
            String::from_utf8(serialize_root(&expected)).expect("UTF-8"),
            "field order and values must match the index bot's Desc wire shape"
        );

        let files = build_files("p/acme/widget.json", b"root", "acme/widget", &[], &observed.blobs, &[]);
        assert_eq!(
            written(&files, &format!("p/acme/widget/o/sha256/{}.md", readme_digest.hex())),
            Some(readme),
            "the readme rides into CAS verbatim, under its own hash and a .md name"
        );
        assert_eq!(
            written(&files, &format!("p/acme/widget/o/sha256/{}.png", logo_digest.hex())),
            Some(logo),
            "the logo's extension comes from its layer media type"
        );
    }

    /// D6 is a floating-tag comparison: an unmoved `__ocx.desc` digest leaves the
    /// committed `desc` object alone, so a `--refresh` of a package whose
    /// description never changes keeps the C6 short-circuit intact.
    ///
    /// Its CAS blobs still ride along. The `--out` contract materializes the
    /// whole entry every run (`announce --out dir && publish dir`), and the
    /// curated tags' CAS objects are already re-written unconditionally — a
    /// `desc.readme` the run alone omitted is a dangling reference the index
    /// refuses.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unmoved_description_rewrites_no_desc_but_still_carries_its_blobs() {
        let data = StubTransportData::new();
        let readme = b"# widget\n".as_slice();
        let tag_digest = seed_description(&data, readme, None, &[]);
        let readme_digest = ocx_oci::Algorithm::Sha256.hash(readme);
        let publisher = stub_publisher(&data);
        let root = root_with_desc(&tag_digest, &readme_digest.to_string());

        let observed = observe_desc(&publisher, &loopback_physical(), &root)
            .await
            .expect("the probe succeeds");

        assert!(
            observed.desc.is_none(),
            "an unmoved description is not rewritten into the root"
        );
        let files = build_files("p/acme/widget.json", b"root", "acme/widget", &[], &observed.blobs, &[]);
        let readme_path = format!("p/acme/widget/o/sha256/{}.md", readme_digest.hex());
        assert_eq!(
            written(&files, &readme_path),
            Some(readme),
            "the root still points at {readme_path}, so the unchanged run must write it too: {:?}",
            files.keys().collect::<Vec<_>>()
        );
    }

    /// The optional half of the format: no logo layer means no `logo` key at
    /// all. The index types it as a digest string, so `null` is not a form it
    /// has.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_omits_the_logo_field_when_the_artifact_carries_none() {
        let data = StubTransportData::new();
        seed_description(&data, b"# widget\n", None, &[(ocx_oci::annotations::TITLE, "Widget")]);
        let publisher = stub_publisher(&data);

        let observed = observe_desc(&publisher, &loopback_physical(), &committed_root("oci://127.0.0.1/x"))
            .await
            .expect("the description observes");
        let rebuilt = observed.desc.expect("a new description is a change");

        assert!(rebuilt.get("logo").is_none(), "an absent logo omits the key: {rebuilt}");
        assert_eq!(observed.blobs.len(), 1, "only the readme blob is written");
        assert_eq!(observed.blobs[0].extension, "md");
    }

    /// `keywords` is required, so an absent `sh.ocx.keywords` annotation is the
    /// empty array — never a missing field.
    ///
    /// `title` is required too, but typed `minLength: 1`: the empty string is
    /// not a value it has. An `__ocx.desc` pushed without a title annotation
    /// (`ocx package description push --readme` on a readme with no frontmatter title)
    /// therefore falls back to the last segment of the root's `name` — the
    /// package's own display name, without the namespace path the card renders
    /// beside it. Emitting `""` builds a root the pull request checks pass and
    /// `schema:validate:rendered` then rejects, blocking the whole index deploy.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_defaults_absent_annotations_to_valid_values() {
        let data = StubTransportData::new();
        seed_description(&data, b"# widget\n", None, &[]);
        let publisher = stub_publisher(&data);
        let root = committed_root("oci://127.0.0.1/x");

        let observed = observe_desc(&publisher, &loopback_physical(), &root)
            .await
            .expect("the description observes");
        let rebuilt = observed.desc.expect("a new description is a change");

        assert_eq!(rebuilt.get("keywords"), Some(&serde_json::json!([])));
        assert_eq!(
            rebuilt.get("title"),
            Some(&Value::String("widget".to_string())),
            "an absent title annotation falls back to the name's last segment, never the empty string \
             and never the whole ocx.sh/<namespace>/<package> path"
        );
        assert_eq!(
            rebuilt.get("description"),
            Some(&Value::String(String::new())),
            "an absent description annotation is the empty string — the schema allows it"
        );
    }

    /// The same fallback for a title annotation the publisher set to the empty
    /// string (`ocx package description push --title ""`): present-but-empty is exactly
    /// the value the schema refuses, so absence is not the only trigger.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_replaces_an_empty_title_annotation() {
        let data = StubTransportData::new();
        seed_description(&data, b"# widget\n", None, &[(ocx_oci::annotations::TITLE, "")]);
        let publisher = stub_publisher(&data);
        let root = committed_root("oci://127.0.0.1/x");

        let observed = observe_desc(&publisher, &loopback_physical(), &root)
            .await
            .expect("the description observes");
        let rebuilt = observed.desc.expect("a new description is a change");

        assert_eq!(rebuilt.get("title"), Some(&Value::String("widget".to_string())));
        assert_ne!(
            rebuilt.get("title"),
            root.get("name"),
            "the whole logical name is a path, not a title"
        );
    }

    /// The last rung. `name` is schema-required of every real root, but the
    /// title fallback exists precisely because reading a non-empty string out of
    /// data that merely ought to carry one is the bet that produced `""` in the
    /// first place — so a root without `name` still yields a valid title.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_titles_a_root_without_a_name_from_its_repository() {
        let data = StubTransportData::new();
        seed_description(&data, b"# widget\n", None, &[]);
        let publisher = stub_publisher(&data);
        let mut root = committed_root("oci://127.0.0.1/x");
        root.as_object_mut().expect("the root is an object").remove("name");

        let observed = observe_desc(&publisher, &loopback_physical(), &root)
            .await
            .expect("the description observes");
        let rebuilt = observed.desc.expect("a new description is a change");

        assert_eq!(
            rebuilt.get("title"),
            Some(&Value::String("x".to_string())),
            "the physical repository stands in; the empty string never ships"
        );
    }

    /// The overwhelmingly common case today: no `__ocx.desc` published and a
    /// `desc: null` root. Both absent is "no change", never an error.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_of_a_package_without_a_description_is_a_no_op() {
        let data = StubTransportData::new();
        let publisher = stub_publisher(&data);

        let observed = observe_desc(&publisher, &loopback_physical(), &committed_root("oci://127.0.0.1/x"))
            .await
            .expect("an absent description must not fail the announce");

        assert!(observed.desc.is_none());
        assert!(observed.blobs.is_empty());
    }

    /// A recorded description that has since vanished stops the run: retraction
    /// semantics are unspecified, and silently clearing `desc` back to null
    /// would destroy governed content on a publisher's routine tag announce.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_desc_refuses_a_description_that_disappeared() {
        let data = StubTransportData::new();
        let publisher = stub_publisher(&data);
        let recorded = digest_string('e');

        let root = root_with_desc(&recorded, &digest_string('f'));
        let result = observe_desc(&publisher, &loopback_physical(), &root).await;

        let Err(AnnounceError::DescDisappeared { repository, digest }) = result else {
            panic!("a vanished description must be refused, not silently cleared");
        };
        assert_eq!(repository, "oci://127.0.0.1/x");
        assert_eq!(digest, recorded);
    }

    // ── carry_branch_tags (stale-branch rebuild, ADR D1) ─────────────

    /// Tag order is byte-visible: `serialize_root` does not sort, so "the
    /// base's set first, branch-only keys appended in the branch's order" is a
    /// wire contract rather than cosmetics. `tags` must also stay the root's
    /// last key (CONTRACTS §14).
    #[test]
    fn carry_appends_branch_only_tags_after_the_base_set() {
        let mut base = committed_root("oci://ghcr.io/x/y");
        base["tags"]["3.0.0"] = json!({ "content": digest_string('c'), "observed": "2026-02-01T00:00:00Z" });
        let branch_tags = json!({
            "2.0.0": { "content": digest_string('b'), "observed": "2026-03-01T00:00:00Z" },
            "1.5.0": { "content": digest_string('d'), "observed": "2026-03-02T00:00:00Z" },
        });

        carry_branch_tags(&mut base, &branch_tags);

        let order: Vec<&str> = base["tags"]
            .as_object()
            .expect("tags object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            order,
            vec!["1.0.0", "3.0.0", "2.0.0", "1.5.0"],
            "the base keeps its order and the branch-only tags append in theirs"
        );
        let fields: Vec<&str> = base
            .as_object()
            .expect("root object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            vec!["name", "repository", "owners", "status", "created", "desc", "tags"],
            "tags stays the last key — replacing it must not move it"
        );
    }

    /// The base entry wins on a shared key. The tie-break is yank governance,
    /// not freshness: `regenerate` rewrites `content`/`observed` on any digest
    /// move, so it only ever decides `yanked`, and the base's marker is merged,
    /// CI-validated state a stale branch must not revert.
    #[test]
    fn carry_keeps_the_base_entry_for_a_shared_tag() {
        let mut base = committed_root("oci://ghcr.io/x/y");
        base["tags"]["1.0.0"]["yanked"] = json!({ "reason": "CVE-2026-1", "at": "2026-02-01T00:00:00Z" });
        let branch_tags = json!({
            "1.0.0": { "content": digest_string('b'), "observed": "2026-03-01T00:00:00Z" },
        });

        carry_branch_tags(&mut base, &branch_tags);

        assert_eq!(
            base["tags"]["1.0.0"]["yanked"]["reason"].as_str(),
            Some("CVE-2026-1"),
            "the base's yank marker must survive a branch entry that lacks one"
        );
        assert_eq!(
            base["tags"]["1.0.0"]["observed"].as_str(),
            Some("2026-01-01T00:00:00Z"),
            "the branch's fresher observed must not replace the base entry"
        );
    }

    /// The append path: a root carrying no `tags` at all still ends up with
    /// one, in last position, holding the branch's entries in the branch's
    /// order. `tags` is required of every real index root, so this is the
    /// defensive branch — and the one whose failure mode is silent tag loss.
    #[test]
    fn carry_creates_a_missing_tags_object_as_the_last_key() {
        let mut base = committed_root("oci://ghcr.io/x/y");
        base.as_object_mut().expect("root object").shift_remove("tags");
        let branch_tags = json!({
            "2.0.0": { "content": digest_string('b'), "observed": "2026-03-01T00:00:00Z" },
            "1.0.0": { "content": digest_string('a'), "observed": "2026-03-02T00:00:00Z" },
        });

        carry_branch_tags(&mut base, &branch_tags);

        let fields: Vec<&str> = base
            .as_object()
            .expect("root object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            vec!["name", "repository", "owners", "status", "created", "desc", "tags"],
            "the created tags object must land last (CONTRACTS §14)"
        );
        let order: Vec<&str> = base["tags"]
            .as_object()
            .expect("tags object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            order,
            vec!["2.0.0", "1.0.0"],
            "every branch tag carries, in branch order"
        );
    }

    // ── regenerate (C6 no-churn) ─────────────────────────────────────────────

    /// `regenerate` over `observed` as a `--tags` run naming exactly those tags.
    fn regenerate_replace(committed: &Value, observed: Vec<Observed>, now: &str) -> Value {
        let selection = TagSelection::Replace(observed.iter().map(|entry| entry.tag.clone()).collect());
        let plan = TagPlan {
            observed,
            ..TagPlan::default()
        };
        regenerate(committed, &plan, &selection, false, now)
    }

    #[test]
    fn regenerate_keeps_the_observed_timestamp_for_an_unmoved_digest() {
        let root = committed_root("oci://ghcr.io/x/y");
        // Splice the committed tag's content to match what was observed so the
        // "unchanged" path fires.
        let entry = observed("1.0.0", 'z');
        let mut committed = root;
        committed["tags"]["1.0.0"]["content"] = Value::String(entry.content.to_string());
        let regenerated = regenerate_replace(&committed, vec![entry], "2099-12-31T00:00:00Z");
        assert_eq!(
            regenerated["tags"]["1.0.0"]["observed"].as_str(),
            Some("2026-01-01T00:00:00Z"),
            "an unmoved digest must keep its committed observed timestamp"
        );
    }

    #[test]
    fn regenerate_stamps_now_for_a_new_or_changed_digest() {
        let committed = committed_root("oci://ghcr.io/x/y");
        // The committed `1.0.0` content is `sha256:aaaa…`; the observed digest
        // differs, so the tag is treated as changed.
        let regenerated = regenerate_replace(&committed, vec![observed("1.0.0", 'c')], "2099-12-31T00:00:00Z");
        assert_eq!(
            regenerated["tags"]["1.0.0"]["observed"].as_str(),
            Some("2099-12-31T00:00:00Z")
        );
    }

    #[test]
    fn regenerate_drops_a_committed_tag_absent_from_the_curated_set() {
        let mut committed = committed_root("oci://ghcr.io/x/y");
        committed["tags"]["2.0.0"] =
            serde_json::json!({ "content": digest_string('b'), "observed": "2026-02-02T00:00:00Z" });
        // Only observe `1.0.0`; `2.0.0` must be dropped.
        let regenerated = regenerate_replace(&committed, vec![observed("1.0.0", 'a')], "2099-12-31T00:00:00Z");
        assert!(regenerated["tags"].get("2.0.0").is_none());
        assert!(regenerated["tags"].get("1.0.0").is_some());
    }

    /// The #436 tripwire, both ways.
    ///
    /// A predicate whose production path is meant never to fire needs its red
    /// state produced by hand, or it ships as a habit rather than a check: under
    /// `UnionFile`/`Refresh`/`FromRegistry` every committed row is kept unless
    /// confirmed gone, so nothing reachable today makes it non-empty. What it
    /// guards is the fourth route nobody has found yet.
    #[test]
    fn dropped_committed_tags_names_a_loss_and_stays_silent_otherwise() {
        let mut committed = committed_root("oci://ghcr.io/x/y");
        committed["tags"]["2.0.0"] =
            serde_json::json!({ "content": digest_string('b'), "observed": "2026-02-02T00:00:00Z" });
        let names = committed_tag_names(&committed);
        assert!(
            names.contains(&"2.0.0".to_string()),
            "the fixture must commit the tag the loss is measured on: {names:?}"
        );

        let kept = regenerate_replace(
            &committed,
            vec![observed("1.0.0", 'a'), observed("2.0.0", 'b')],
            "2099-12-31T00:00:00Z",
        );
        assert!(
            dropped_committed_tags(&names, &kept, &[]).is_empty(),
            "a run that re-observed everything committed loses nothing"
        );

        let lost = regenerate_replace(&committed, vec![observed("1.0.0", 'a')], "2099-12-31T00:00:00Z");
        assert_eq!(
            dropped_committed_tags(&names, &lost, &[]),
            vec!["2.0.0".to_string()],
            "and a run that did not re-observe `2.0.0` is naming a deletion"
        );
        assert!(
            dropped_committed_tags(&names, &lost, &["2.0.0".to_string()]).is_empty(),
            "unless D7 dropped it on purpose, which is not a loss"
        );
    }

    #[test]
    fn regenerate_carries_human_fields_verbatim() {
        let committed = committed_root("oci://ghcr.io/x/y");
        let regenerated = regenerate_replace(&committed, vec![observed("1.0.0", 'a')], "2099-12-31T00:00:00Z");
        assert_eq!(regenerated["name"], committed["name"]);
        assert_eq!(regenerated["owners"], committed["owners"]);
        assert_eq!(regenerated["status"], committed["status"]);
        assert_eq!(regenerated["created"], committed["created"]);
    }

    #[test]
    fn regenerate_of_a_no_op_run_is_byte_identical_driving_c6() {
        // A committed root whose single tag's content already equals the observed
        // digest must round-trip byte-for-byte through regenerate + serialize.
        let entry = observed("1.0.0", 'a');
        let committed = serde_json::json!({
            "name": "ocx.sh/acme/widget",
            "repository": "oci://ghcr.io/ocx-contrib/widget",
            "owners": [{ "github": "alice", "github_id": 1 }],
            "status": "active",
            "created": "2026-07-24",
            "desc": null,
            "tags": {
                "1.0.0": { "content": entry.content.to_string(), "observed": "2026-01-01T00:00:00Z" }
            }
        });
        let committed_bytes = serialize_root(&committed);
        let regenerated = regenerate_replace(&committed, vec![entry], "2099-12-31T00:00:00Z");
        let regenerated_bytes = serialize_root(&regenerated);
        assert_eq!(
            regenerated_bytes, committed_bytes,
            "a no-op regenerate must be byte-identical (C6 short-circuit)"
        );
        assert_eq!(
            new_cas_count(&committed, std::slice::from_ref(&observed("1.0.0", 'a'))),
            0
        );
    }

    // ── regenerate: the vestigial `variants` key ─────────────────────────────

    #[test]
    fn regenerate_records_no_variants_even_when_the_tags_carry_one() {
        // The index bot derives `variants` from `tags` and no longer reads a
        // stored field, so recording one would be a second source of truth
        // that can only ever drift. Not `"variants": []` either — the key is
        // absent, and its absence is what the index gate accepts.
        let committed = committed_root("oci://ghcr.io/x/y");
        let regenerated = regenerate_replace(
            &committed,
            vec![observed("1.0.0", 'a'), observed("slim-1.0.0", 'b')],
            "2099-12-31T00:00:00Z",
        );
        assert!(
            regenerated.get("variants").is_none(),
            "the variant set is the index's to derive, got {:?}",
            regenerated.get("variants")
        );
        assert!(
            !String::from_utf8(serialize_root(&regenerated))
                .expect("ASCII")
                .contains("variants")
        );
    }

    #[test]
    fn regenerate_removes_a_committed_variants_key_without_reordering_the_root() {
        // Removal, not merely "stop writing": `regenerate` clones the committed
        // root, so a stored set would ride through verbatim and, once a
        // variant's last tag left upstream, stop matching the bot's derivation
        // and be rejected. `Map::remove` is `swap_remove` under
        // `preserve_order` — it would drop `tags` into the vacated slot and
        // silently reorder the document.
        let mut committed = committed_root("oci://ghcr.io/x/y");
        let object = committed.as_object_mut().expect("root object");
        let index = object.keys().position(|key| key == "tags").expect("tags key");
        object.shift_insert(index, "variants".to_string(), serde_json::json!(["slim"]));

        let regenerated = regenerate_replace(&committed, vec![observed("1.0.0", 'a')], "2099-12-31T00:00:00Z");
        let fields: Vec<&str> = regenerated
            .as_object()
            .expect("root object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            vec!["name", "repository", "owners", "status", "created", "desc", "tags"],
            "the committed key must go and every other field stay where it was"
        );
    }

    // ── apply_yank_markers (C7) ──────────────────────────────────────────────

    fn root_with_tags(tags: &[&str]) -> Value {
        let mut map = Map::new();
        for tag in tags {
            map.insert(
                (*tag).to_string(),
                serde_json::json!({ "content": digest_string('a'), "observed": "2026-01-01T00:00:00Z" }),
            );
        }
        serde_json::json!({ "tags": Value::Object(map) })
    }

    #[test]
    fn yank_sets_a_reason_and_timestamp_marker() {
        let mut root = root_with_tags(&["1.0.0"]);
        apply_yank_markers(&mut root, &["1.0.0".into()], &[], "security", "2026-02-01T00:00:00Z").unwrap();
        assert_eq!(root["tags"]["1.0.0"]["yanked"]["reason"], "security");
        assert_eq!(root["tags"]["1.0.0"]["yanked"]["at"], "2026-02-01T00:00:00Z");
    }

    #[test]
    fn unyank_clears_the_marker() {
        let mut root = root_with_tags(&["1.0.0"]);
        root["tags"]["1.0.0"]["yanked"] = serde_json::json!({ "reason": "old", "at": "2026-01-01T00:00:00Z" });
        apply_yank_markers(&mut root, &[], &["1.0.0".into()], "unused", "now").unwrap();
        assert!(root["tags"]["1.0.0"].get("yanked").is_none());
    }

    #[test]
    fn yank_of_an_absent_tag_errors() {
        let mut root = root_with_tags(&["1.0.0"]);
        assert!(matches!(
            apply_yank_markers(&mut root, &["9.9.9".into()], &[], "r", "now"),
            Err(AnnounceError::YankTagNotCurated { .. })
        ));
    }

    #[test]
    fn yank_and_unyank_of_the_same_tag_errors() {
        let mut root = root_with_tags(&["1.0.0"]);
        assert!(matches!(
            apply_yank_markers(&mut root, &["1.0.0".into()], &["1.0.0".into()], "r", "now"),
            Err(AnnounceError::YankUnyankOverlap { .. })
        ));
    }

    #[test]
    fn empty_yank_and_unyank_leaves_markers_untouched() {
        let mut root = root_with_tags(&["1.0.0"]);
        root["tags"]["1.0.0"]["yanked"] = serde_json::json!({ "reason": "kept", "at": "2026-01-01T00:00:00Z" });
        apply_yank_markers(&mut root, &[], &[], "r", "now").unwrap();
        assert_eq!(
            root["tags"]["1.0.0"]["yanked"]["reason"], "kept",
            "--refresh must not touch yank markers"
        );
    }

    // ── build_files ──────────────────────────────────────────────────────────

    #[test]
    fn build_files_keys_root_and_cas_by_wire_path() {
        let entry = observed("1.0.0", 'a');
        let hex = entry.content.hex().to_string();
        let files = build_files(
            "p/acme/widget.json",
            b"root-bytes",
            "acme/widget",
            std::slice::from_ref(&entry),
            &[],
            &[],
        );
        assert_eq!(written(&files, "p/acme/widget.json"), Some(b"root-bytes".as_slice()));
        assert!(files.contains_key(&format!("p/acme/widget/o/sha256/{hex}.json")));
    }

    // ── write_out ────────────────────────────────────────────────────────────

    /// `--out` renders the file set a commit would produce, and since the
    /// orphan sweep that set carries removals. All three cases ride one map,
    /// because they only mean anything together: the removal lands, the removal
    /// that hits nothing is the same no-op the forge drivers owe rather than an
    /// error, and neither disturbs the write beside them. A caller pointing
    /// `--out` at the directory a previous run filled is where this is visible
    /// at all — a fresh directory makes every `Delete` the second case.
    ///
    /// The return value is the *written* paths: a deletion is not a written
    /// file, so reporting one would tell `publish` to look for a file that is
    /// deliberately absent.
    #[tokio::test(flavor = "multi_thread")]
    async fn write_out_applies_a_delete_and_tolerates_one_that_hits_nothing() {
        const ROOT: &str = "p/acme/widget.json";
        const STALE: &str = "p/acme/widget/o/sha256/aaaa.json";
        const NEVER_WRITTEN: &str = "p/acme/widget/o/sha256/bbbb.md";

        let directory = tempfile::tempdir().expect("a temporary directory");
        let stale = directory.path().join(STALE);
        tokio::fs::create_dir_all(stale.parent().expect("the object has a parent"))
            .await
            .expect("the previous run's directory");
        tokio::fs::write(&stale, b"a previous run's object")
            .await
            .expect("the previous run's object");

        let files = BTreeMap::from([
            (ROOT.to_string(), FileChange::Put(b"root-bytes".to_vec())),
            (STALE.to_string(), FileChange::Delete),
            (NEVER_WRITTEN.to_string(), FileChange::Delete),
        ]);

        let written = write_out(directory.path(), &files)
            .await
            .expect("a delete naming an absent path is a no-op, not a failure");

        assert_eq!(written, vec![ROOT.to_string()], "only the written path is reported");
        assert!(
            !tokio::fs::try_exists(&stale)
                .await
                .expect("the probe reads the directory")
        );
        assert_eq!(
            tokio::fs::read(directory.path().join(ROOT))
                .await
                .expect("the root was written"),
            b"root-bytes",
            "the write beside the removals still lands"
        );
    }

    // ── unclaimed package + SSRF ordering ────────────────────────────────────

    fn stub_publisher(data: &StubTransportData) -> Publisher {
        Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ssrf_pre_flight_refuses_a_forbidden_host() {
        // A committed root pointing at loopback must abort at the pre-flight (X3)
        // before any registry request is even constructible: `guarded_physical` is
        // the only thing that yields the `Physical` both the tag listing and the
        // observe loop need.
        assert!(
            matches!(
                guarded_physical(
                    LOOPBACK_POINTER,
                    "ocx.sh",
                    &[],
                    &[],
                    &ocx_oci::ssrf::ProxyRules::direct()
                )
                .await,
                Err(AnnounceError::Ssrf { .. })
            ),
            "forbidden host must be refused"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ssrf_pre_flight_allows_a_trusted_forbidden_host() {
        // The trusted_hosts escape hatch (X2) lets a loopback registry through;
        // the stub then answers with no manifest, surfacing an absent tag — proof
        // the observe loop ran only *after* the pre-flight passed.
        let data = StubTransportData::new();
        data.write().probe_results = vec![Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown))];
        let publisher = stub_publisher(&data);
        let physical = guarded_physical(
            LOOPBACK_POINTER,
            "ocx.sh",
            &["127.0.0.1".to_string()],
            &[],
            &ocx_oci::ssrf::ProxyRules::direct(),
        )
        .await
        .expect("a trusted loopback host passes the pre-flight");
        let result = observe_curated(&publisher, &physical, &["1.0.0".to_string()]).await;
        let observations = result.expect("a trusted host proceeds to observe");
        assert!(
            matches!(observations.as_slice(), [TagObservation::Absent { tag, .. }] if tag == "1.0.0"),
            "the empty stub yields an absent tag"
        );
    }

    // ── proxy-aware pre-flight (announce's copy of plan rows 9–10) ───────────
    //
    // Every case here builds its rules from `Matcher::builder()`, which starts
    // from `Default` and never from the environment, so an ambient developer
    // proxy cannot perturb them — and no test mutates an env var.

    /// A registry name that no resolver can ever answer for, standing in for a
    /// public registry only the corporate proxy can resolve. `.invalid` is
    /// reserved by RFC 2606, so this needs no network and no fixture.
    const UNRESOLVABLE_REGISTRY: &str = "no-such-registry.invalid:5000";

    #[tokio::test(flavor = "multi_thread")]
    async fn a_proxied_route_admits_a_registry_this_host_cannot_resolve() {
        // Mirrors plan row 9 (`guard_physical_dial_admits_an_unresolvable_target_on_a_proxied_route`)
        // at the announce guard: under a proxy the destination name is literal
        // text in the CONNECT line, so a local lookup neither can succeed on a
        // proxy-only-DNS network nor decides anything (ocx#407).
        let pointer = format!("oci://{UNRESOLVABLE_REGISTRY}/acme/widget");

        let physical = guarded_physical(
            &pointer,
            "ocx.sh",
            &[],
            &[],
            &ocx_oci::ssrf::ProxyRules::proxied_everywhere("http://proxy.corp:3128"),
        )
        .await
        .expect("a proxied destination is admitted without resolving it locally");

        assert_eq!(physical.host, "no-such-registry.invalid");
        assert_eq!(physical.port, 5000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_forbidden_ip_literal_is_refused_even_on_a_proxied_route() {
        // Mirrors plan row 10 (`…refuses_a_forbidden_literal_even_on_a_proxied_route`).
        // Guard test: green before and after the fix. A proxy must never become
        // a laundering hop for a loopback target, so a forbidden literal is
        // refused on the text alone, with no lookup and no trusted_hosts entry.
        let result = guarded_physical(
            "oci://127.0.0.1:5000/acme/widget",
            "ocx.sh",
            &[],
            &[],
            &ocx_oci::ssrf::ProxyRules::proxied_everywhere("http://proxy.corp:3128"),
        )
        .await;

        assert!(
            matches!(
                result,
                Err(AnnounceError::Ssrf {
                    source: ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. },
                    ..
                })
            ),
            "a forbidden IP literal stays refused on a proxied route, got {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_plain_http_allowance_decides_which_proxy_variable_applies() {
        // Mirrors plan row 9's route decision at the announce guard, for the
        // design's "each guard site computes `DialScheme::for_registry(
        // insecure_hosts, physical.registry())`". With only an HTTP proxy
        // configured, the same authority is proxied when it is dialed over
        // plain HTTP and direct when it is dialed over HTTPS — and a direct
        // dial to an unresolvable name still fails closed.
        let pointer = format!("oci://{UNRESOLVABLE_REGISTRY}/acme/widget");
        let rules = ocx_oci::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .http("http://proxy.corp:3128")
                .build(),
        );

        let physical = guarded_physical(&pointer, "ocx.sh", &[], &[UNRESOLVABLE_REGISTRY.to_string()], &rules)
            .await
            .expect("an insecure registry dials http, which this proxy intercepts");
        assert_eq!(physical.identifier.registry(), UNRESOLVABLE_REGISTRY);

        let refused = guarded_physical(&pointer, "ocx.sh", &[], &[], &rules).await;
        assert!(
            matches!(
                refused,
                Err(AnnounceError::Ssrf {
                    source: ocx_oci::ssrf::SsrfError::Resolution { .. },
                    ..
                })
            ),
            "without the plain-HTTP allowance the dial is https, which this proxy does not \
             intercept: the route is direct and the name does not resolve, got {refused:?}"
        );
    }

    // ── observe_curated ordering + failure determinism ───────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn observe_curated_returns_the_curated_order_not_the_completion_order() {
        // The observations run concurrently, so nothing about response timing may
        // reach the rebuilt root. The curated order is the wire order.
        let data = StubTransportData::new();
        let curated: Vec<String> = ["9.0.0", "1.0.0", "latest", "2.5.1"]
            .iter()
            .map(|t| (*t).to_string())
            .collect();
        for (index, tag) in curated.iter().enumerate() {
            let manifest = image_index(vec![index_entry("amd64", (b'a' + index as u8) as char)]);
            seed_manifest(&data, tag, &manifest);
        }
        // Answer in the exact reverse of the curated order, so an implementation
        // that yields results as they complete cannot produce the curated order
        // by accident. Without this the assertion passes against `buffer_unordered`.
        for (index, tag) in curated.iter().enumerate() {
            let remaining = (curated.len() - index) as u32;
            data.write().manifest_delays.insert(
                format!("127.0.0.1/x:{tag}"),
                Duration::from_millis(20 * u64::from(remaining)),
            );
        }
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");

        let observed: Vec<Observed> = observe_curated(&publisher, &physical, &curated)
            .await
            .expect("every seeded tag resolves")
            .into_iter()
            .map(present)
            .collect();

        let tags: Vec<&str> = observed.iter().map(|o| o.tag.as_str()).collect();
        assert_eq!(tags, vec!["9.0.0", "1.0.0", "latest", "2.5.1"]);
    }

    /// One line per observation, so an order assertion reads as a list.
    fn describe(observation: &TagObservation) -> String {
        match observation {
            TagObservation::Present(observed) => format!("present:{}", observed.tag),
            TagObservation::Absent { tag, .. } => format!("absent:{tag}"),
        }
    }

    /// A gone tag is an observation, not an error: whether it removes a row or
    /// fails the run is the plan's decision, so the observe loop must hand every
    /// answer back, in the curated order whichever request finished first.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_curated_returns_gone_tags_in_curated_order_not_completion_order() {
        let data = StubTransportData::new();
        seed_manifest(&data, "1.0.0", &image_index(vec![index_entry("amd64", 'a')]));
        data.write().probe_results = vec![
            Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown)),
            Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown)),
        ];
        let curated = ["1.0.0", "missing-first", "missing-second"]
            .iter()
            .map(|t| (*t).to_string())
            .collect::<Vec<_>>();
        // The earlier gone tag answers last, so completion order and curated
        // order are different answers here.
        data.write()
            .manifest_delays
            .insert("127.0.0.1/x:missing-first".to_string(), Duration::from_millis(60));
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");

        let observations = observe_curated(&publisher, &physical, &curated)
            .await
            .expect("a tag the registry does not serve is an answer, not a failure");

        let described: Vec<String> = observations.iter().map(describe).collect();
        assert_eq!(
            described,
            vec!["present:1.0.0", "absent:missing-first", "absent:missing-second"]
        );
    }

    /// Of two gone tags with no committed row, the run names the first in
    /// observation order, never whichever lost the race.
    #[test]
    fn plan_tags_reports_the_first_unresolved_tag_in_observation_order() {
        let committed = committed_holding(serde_json::json!({}));
        let observations = vec![
            served("1.0.0", 'a'),
            gone("missing-first", NotFoundCode::ManifestUnknown),
            gone("missing-second", NotFoundCode::ManifestUnknown),
        ];

        let Err(error) = plan_tags(observations, &committed, &TagSelection::Refresh, "127.0.0.1/x") else {
            panic!("a gone tag with no committed row is a hard error");
        };

        assert!(
            matches!(&error, AnnounceError::UnresolvedTag { tag, .. } if tag == "missing-first"),
            "expected the earlier tag to be reported, got {error:?}"
        );
    }

    // ── observe_one_tag: the not-found answer and the canonical read ─────────

    /// The code the probe read off the envelope rides out on the observation:
    /// only one of the three may later remove a row, so the loop must not
    /// flatten them into a single "absent".
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_one_tag_reports_the_not_found_code_the_probe_answered() {
        for code in [
            NotFoundCode::ManifestUnknown,
            NotFoundCode::NameUnknown,
            NotFoundCode::Unspecified,
        ] {
            let data = StubTransportData::new();
            data.write().probe_results = vec![Ok(ManifestPresence::Absent(code))];
            let publisher = stub_publisher(&data);
            let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");

            let observation = observe_one_tag(&publisher, &physical, "gone")
                .await
                .expect("a not-found answer is an observation");

            assert!(
                matches!(&observation, TagObservation::Absent { tag, code: seen } if tag == "gone" && *seen == code),
                "expected Absent carrying {code:?}, got {}",
                describe(&observation)
            );
            assert_eq!(
                data.read().probe_calls.len(),
                1,
                "one follow-up probe per not-found read"
            );
        }
    }

    /// A tag the registry serves is never probed: the probe exists to confirm
    /// an absence, and a second read per present tag would double the traffic.
    #[tokio::test(flavor = "multi_thread")]
    async fn observe_one_tag_probes_only_after_a_not_found_read() {
        let data = StubTransportData::new();
        seed_manifest(&data, "1.0.0", &image_index(vec![index_entry("amd64", 'a')]));
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");

        let observation = observe_one_tag(&publisher, &physical, "1.0.0").await.expect("seeded");

        assert!(matches!(observation, TagObservation::Present(_)));
        assert!(data.read().probe_calls.is_empty(), "a present tag costs no probe");
    }

    /// A push landing between the two reads: the GET said not found, the probe
    /// says present. The run must not treat the tag as gone.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tag_that_appears_between_the_read_and_the_probe_is_a_race_error() {
        let data = StubTransportData::new();
        data.write().probe_results = vec![Ok(ManifestPresence::Present(ocx_oci::Digest::Sha256("c".repeat(64))))];
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");

        let Err(error) = observe_one_tag(&publisher, &physical, "racy").await else {
            panic!("a tag that read absent and probed present must not resolve to either answer");
        };

        assert!(
            matches!(&error, AnnounceError::ObserveRaced { tag, repository } if tag == "racy" && *repository == physical.display),
            "expected the race error naming the tag, got {error:?}"
        );
    }

    /// Observation reads the registry the tag lives on, mirror or not: a mirror
    /// may lag behind it, and a removal decided from a lagging copy deletes a
    /// row whose tag still exists.
    ///
    /// `read_targets` and `probe_calls` are both required non-empty, or a read
    /// that recorded nothing would pass as "never reached the mirror".
    #[tokio::test(flavor = "multi_thread")]
    async fn observation_reads_the_canonical_registry_when_a_mirror_is_configured() {
        const MIRROR_HOST: &str = "mirror.invalid";
        let data = StubTransportData::new();
        seed_manifest(&data, "1.0.0", &image_index(vec![index_entry("amd64", 'a')]));
        data.write().probe_results = vec![Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown))];
        let publisher = Publisher::new(ocx_oci::client::test_transport::mirrored_stub_client(
            &data,
            "127.0.0.1",
            MIRROR_HOST,
            "mirrored",
        ));
        let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");
        let curated = vec!["1.0.0".to_string(), "gone".to_string()];

        let observations = observe_curated(&publisher, &physical, &curated)
            .await
            .expect("both tags answer on the canonical registry");

        let described: Vec<String> = observations.iter().map(describe).collect();
        assert_eq!(described, vec!["present:1.0.0", "absent:gone"]);
        let inner = data.read();
        assert!(!inner.read_targets.is_empty(), "the reads were recorded");
        for (method, registry, _) in &inner.read_targets {
            assert_eq!(registry, "127.0.0.1", "{method} must not reach the mirror");
        }
        assert!(!inner.probe_calls.is_empty(), "the probe was recorded");
        for call in &inner.probe_calls {
            assert!(
                call.starts_with("127.0.0.1/"),
                "the probe must not reach the mirror: {call}"
            );
        }
    }

    // ── list_registry_tags (--tags-from-registry source) ─────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn list_registry_tags_drops_reserved_at_the_source() {
        // `__ocx.keep.*` tags are pushed by default, so a registry
        // listing carries one per published version. They are filtered here and
        // never reported: `reserved_dropped` answers "what did the CALLER name
        // that is not a version", and a listing names nothing.
        let data = StubTransportData::new();
        data.write().tags = vec![vec![
            "1.0.0".to_string(),
            keep_tag(),
            "__ocx.desc".to_string(),
            "latest".to_string(),
        ]];
        let publisher = stub_publisher(&data);
        let physical = extract_physical(LOOPBACK_POINTER).expect("root parses");

        let tags = list_registry_tags(&publisher, &physical)
            .await
            .expect("listing succeeds");

        assert_eq!(tags, vec!["1.0.0".to_string(), "latest".to_string()]);
    }

    #[test]
    fn from_registry_unions_onto_the_committed_set() {
        let committed = vec!["1.0.0".to_string(), "latest".to_string()];
        let discovered = vec!["latest".to_string(), "2.0.0".to_string(), "1.0.0".to_string()];
        let curated = resolve_curated_tags(&TagSelection::FromRegistry, &committed, &discovered).unwrap();
        // Committed order first, then only the genuinely new registry tag.
        assert_eq!(
            curated.tags,
            vec!["1.0.0".to_string(), "latest".to_string(), "2.0.0".to_string()]
        );
    }

    /// A committed tag the listing lacks stays in the given set: whether the
    /// row is removed is decided after the registry has been asked for it
    /// directly, never by its absence from the listing.
    #[test]
    fn from_registry_never_drops_a_committed_tag_the_registry_lacks() {
        let committed = vec!["1.0.0".to_string(), "0.9.0".to_string()];
        let discovered = vec!["1.0.0".to_string()];
        let curated = resolve_curated_tags(&TagSelection::FromRegistry, &committed, &discovered).unwrap();
        assert_eq!(
            curated.tags, committed,
            "0.9.0 is still asked for after the listing lacked it"
        );
    }

    /// The registry is not consulted for any other selection, so `discovered`
    /// arrives empty and must not leak into the resolved set.
    #[test]
    fn discovered_tags_are_ignored_by_the_caller_supplied_selections() {
        let committed = vec!["1.0.0".to_string()];
        let discovered = vec!["9.9.9".to_string()];
        for selection in [
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            TagSelection::UnionFile(vec!["1.0.0".to_string()]),
            TagSelection::Refresh,
        ] {
            let curated = resolve_curated_tags(&selection, &committed, &discovered).unwrap();
            assert_eq!(
                curated.tags,
                vec!["1.0.0".to_string()],
                "{selection:?} must not adopt a discovered tag"
            );
        }
    }
    // ── plan_tags / regenerate / change_body: removal ────────────────────────

    const EARLIER: &str = "2026-01-01T00:00:00Z";
    const NOW: &str = "2026-07-25T00:00:00Z";

    /// A committed root carrying `tags` verbatim.
    fn committed_holding(tags: Value) -> Value {
        serde_json::json!({
            "name": "ocx.sh/acme/widget",
            "repository": LOOPBACK_POINTER,
            "owners": [{ "github": "alice", "github_id": 1 }],
            "status": "active",
            "created": "2026-07-24",
            "desc": null,
            "tags": tags,
        })
    }

    /// A committed row with no marker.
    fn durable_row(fill: char) -> Value {
        serde_json::json!({ "content": digest_string(fill), "observed": EARLIER })
    }

    fn ephemeral_row(fill: char) -> Value {
        serde_json::json!({ "content": digest_string(fill), "observed": EARLIER, "ephemeral": true })
    }

    fn served(tag: &str, leaf: char) -> TagObservation {
        TagObservation::Present(observed(tag, leaf))
    }

    fn gone(tag: &str, code: NotFoundCode) -> TagObservation {
        TagObservation::Absent {
            tag: tag.to_string(),
            code,
        }
    }

    fn plan_over(
        observations: Vec<TagObservation>,
        committed: &Value,
        selection: &TagSelection,
    ) -> Result<TagPlan, AnnounceError> {
        plan_tags(observations, committed, selection, "127.0.0.1/x")
    }

    fn observed_tags(plan: &TagPlan) -> Vec<&str> {
        plan.observed.iter().map(|entry| entry.tag.as_str()).collect()
    }

    /// The selections that name their tags, and the two that reach committed
    /// rows without naming them.
    fn naming(tags: &[&str]) -> [TagSelection; 2] {
        let tags: Vec<String> = tags.iter().map(|tag| (*tag).to_string()).collect();
        [TagSelection::Replace(tags.clone()), TagSelection::UnionFile(tags)]
    }

    fn reaching() -> [TagSelection; 2] {
        [TagSelection::Refresh, TagSelection::FromRegistry]
    }

    fn keys(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn a_served_tag_is_upserted_and_removes_nothing() {
        let committed = committed_holding(serde_json::json!({ "1.0.0": ephemeral_row('a') }));

        let plan = plan_over(
            vec![served("1.0.0", 'z'), served("2.0.0", 'y')],
            &committed,
            &TagSelection::Refresh,
        )
        .expect("both tags are served");

        assert_eq!(observed_tags(&plan), vec!["1.0.0", "2.0.0"]);
        assert!(plan.removed.is_empty());
        assert!(plan.durable_missing.is_empty());
    }

    /// An ephemeral row is removable without review, so a gone tag removes it
    /// whichever selection reached it.
    #[test]
    fn a_gone_tag_with_an_ephemeral_row_is_removed_under_every_selection() {
        let committed =
            committed_holding(serde_json::json!({ "1.0.0": ephemeral_row('a'), "2.0.0": durable_row('b') }));
        let selections = naming(&["1.0.0", "2.0.0"]).into_iter().chain(reaching());

        for selection in selections {
            let plan = plan_over(
                vec![gone("1.0.0", NotFoundCode::ManifestUnknown), served("2.0.0", 'y')],
                &committed,
                &selection,
            )
            .unwrap_or_else(|error| panic!("{selection:?}: {error:?}"));

            assert_eq!(plan.removed, vec!["1.0.0".to_string()], "{selection:?}");
            assert!(plan.durable_missing.is_empty(), "{selection:?}");
            assert_eq!(observed_tags(&plan), vec!["2.0.0"], "{selection:?}");
        }
    }

    /// The owner named the tag, so removing its durable row is the owner's
    /// stated intent (the index change is still reviewed on the bot side).
    #[test]
    fn a_gone_tag_with_a_durable_row_is_removed_when_the_selection_names_it() {
        let committed = committed_holding(serde_json::json!({ "1.0.0": durable_row('a') }));

        for selection in naming(&["1.0.0"]) {
            let plan = plan_over(
                vec![gone("1.0.0", NotFoundCode::ManifestUnknown)],
                &committed,
                &selection,
            )
            .unwrap_or_else(|error| panic!("{selection:?}: {error:?}"));

            assert_eq!(plan.removed, vec!["1.0.0".to_string()], "{selection:?}");
            assert!(plan.durable_missing.is_empty(), "{selection:?}");
        }
    }

    /// Nobody named the tag: a durable row that a sweep merely reached is kept
    /// and reported, and the run still succeeds.
    #[test]
    fn a_gone_tag_with_a_durable_row_is_kept_and_reported_when_only_reached() {
        let committed = committed_holding(serde_json::json!({ "1.0.0": durable_row('a') }));

        for selection in reaching() {
            let plan = plan_over(
                vec![gone("1.0.0", NotFoundCode::ManifestUnknown)],
                &committed,
                &selection,
            )
            .unwrap_or_else(|error| panic!("{selection:?}: {error:?}"));

            assert!(plan.removed.is_empty(), "{selection:?}");
            assert_eq!(plan.durable_missing, vec!["1.0.0".to_string()], "{selection:?}");
        }
    }

    /// Only `"ephemeral": true` makes a row removable; a stored `false` is the
    /// same as no marker.
    #[test]
    fn an_explicit_false_marker_is_a_durable_row() {
        let committed = committed_holding(serde_json::json!({
            "1.0.0": { "content": digest_string('a'), "observed": EARLIER, "ephemeral": false }
        }));

        let plan = plan_over(
            vec![gone("1.0.0", NotFoundCode::ManifestUnknown)],
            &committed,
            &TagSelection::Refresh,
        )
        .expect("a durable row is kept, not refused");

        assert!(plan.removed.is_empty());
        assert_eq!(plan.durable_missing, vec!["1.0.0".to_string()]);
    }

    /// A gone tag has no row to remove: the caller named something that never
    /// existed here, which is the typo case, under every selection.
    #[test]
    fn a_gone_tag_with_no_committed_row_is_unresolved_under_every_selection() {
        let committed = committed_holding(serde_json::json!({}));
        let selections = naming(&["9.9.9"]).into_iter().chain(reaching());

        for selection in selections {
            let Err(error) = plan_over(
                vec![gone("9.9.9", NotFoundCode::ManifestUnknown)],
                &committed,
                &selection,
            ) else {
                panic!("{selection:?}: a tag with no row cannot be removed, so it is unresolved");
            };

            assert!(
                matches!(&error, AnnounceError::UnresolvedTag { tag, repository } if tag == "9.9.9" && repository == "127.0.0.1/x"),
                "{selection:?}: got {error:?}"
            );
        }
    }

    /// `MANIFEST_UNKNOWN` is the one answer that says "this tag is gone".
    /// `NAME_UNKNOWN` (a repository the registry does not know) and an
    /// unenveloped 404 say nothing about the tag, so they remove nothing even
    /// from an ephemeral row the owner named.
    #[test]
    fn only_manifest_unknown_can_remove_a_row() {
        let committed = committed_holding(serde_json::json!({ "1.0.0": ephemeral_row('a') }));
        let selections = naming(&["1.0.0"]).into_iter().chain(reaching());

        for selection in selections {
            for code in [NotFoundCode::NameUnknown, NotFoundCode::Unspecified] {
                let Err(error) = plan_over(vec![gone("1.0.0", code)], &committed, &selection) else {
                    panic!("{selection:?} / {code:?}: an inconclusive 404 must not remove or keep a row silently");
                };

                assert!(
                    matches!(&error, AnnounceError::UnresolvedTag { tag, .. } if tag == "1.0.0"),
                    "{selection:?} / {code:?}: got {error:?}"
                );
            }
        }
    }

    #[test]
    fn plan_lists_removed_and_observed_tags_in_observation_order() {
        let committed = committed_holding(serde_json::json!({
            "a": ephemeral_row('a'), "b": ephemeral_row('b'), "c": durable_row('c')
        }));
        let observations = vec![
            gone("b", NotFoundCode::ManifestUnknown),
            served("c", 'y'),
            gone("a", NotFoundCode::ManifestUnknown),
            served("d", 'x'),
        ];

        let plan = plan_over(observations, &committed, &TagSelection::Refresh).expect("plans");

        assert_eq!(plan.removed, vec!["b".to_string(), "a".to_string()]);
        assert_eq!(observed_tags(&plan), vec!["c", "d"]);
    }

    // ── regenerate ───────────────────────────────────────────────────────────

    fn regenerate_over(committed: &Value, plan: &TagPlan, selection: &TagSelection, ephemeral: bool) -> Value {
        regenerate(committed, plan, selection, ephemeral, NOW)
    }

    fn plan_of(observed: Vec<Observed>, removed: &[&str]) -> TagPlan {
        TagPlan {
            observed,
            removed: removed.iter().map(|tag| (*tag).to_string()).collect(),
            durable_missing: Vec::new(),
        }
    }

    /// A row the run did not reach comes out untouched and in place, so a
    /// no-change run stays byte-identical.
    #[test]
    fn a_plan_that_touches_nothing_regenerates_the_committed_root_byte_for_byte() {
        let committed = committed_holding(serde_json::json!({
            "1.0.0": durable_row('a'),
            "2.0.0": { "content": digest_string('b'), "observed": EARLIER, "ephemeral": true, "x-future": [1, 2] },
            "3.0.0": durable_row('c'),
        }));

        for selection in [
            TagSelection::UnionFile(vec!["4.0.0".to_string()]),
            TagSelection::Refresh,
        ] {
            let regenerated = regenerate_over(&committed, &plan_of(vec![], &[]), &selection, false);

            assert_eq!(
                serialize_root(&regenerated),
                serialize_root(&committed),
                "{selection:?}: unnamed rows are carried verbatim in committed order"
            );
        }
    }

    /// A moved digest keeps everything the row already carries: the yank, the
    /// marker and a field a newer writer added all survive; only `content` and
    /// `observed` change, and the row keeps its place.
    #[test]
    fn a_moved_row_keeps_its_whole_object_and_changes_only_content_and_observed() {
        let yanked = serde_json::json!({ "reason": "cve", "at": "2026-02-01T00:00:00Z" });
        let committed = committed_holding(serde_json::json!({
            "1.0.0": durable_row('a'),
            "2.0.0": {
                "content": digest_string('b'), "observed": EARLIER, "yanked": yanked,
                "ephemeral": true, "x-future": [1, 2]
            },
            "3.0.0": durable_row('c'),
        }));
        let moved = observed("2.0.0", 'z');
        let new_content = moved.content.to_string();
        let selection = TagSelection::UnionFile(vec!["2.0.0".to_string()]);

        let regenerated = regenerate_over(&committed, &plan_of(vec![moved], &[]), &selection, false);

        assert_eq!(keys(&regenerated["tags"]), vec!["1.0.0", "2.0.0", "3.0.0"]);
        assert_eq!(regenerated["tags"]["1.0.0"], committed["tags"]["1.0.0"]);
        assert_eq!(regenerated["tags"]["3.0.0"], committed["tags"]["3.0.0"]);
        let row = &regenerated["tags"]["2.0.0"];
        assert_eq!(row["content"], Value::String(new_content));
        assert_eq!(row["observed"], Value::String(NOW.to_string()));
        assert_eq!(row["yanked"], committed["tags"]["2.0.0"]["yanked"]);
        assert_eq!(row["ephemeral"], Value::Bool(true));
        assert_eq!(row["x-future"], serde_json::json!([1, 2]));
        assert_eq!(
            keys(row),
            vec!["content", "observed", "yanked", "ephemeral", "x-future"],
            "the row's own field order is not disturbed"
        );
    }

    /// Committed rows first in committed order, then the tags the run adds in
    /// the order it observed them: the root's key order is a wire contract.
    #[test]
    fn added_tags_follow_the_committed_rows_in_observation_order() {
        let committed = committed_holding(serde_json::json!({ "1.0.0": durable_row('a'), "2.0.0": durable_row('b') }));
        let observed = vec![observed("9.0.0", 'y'), observed("1.0.0", 'z'), observed("0.1.0", 'x')];
        let selection = TagSelection::UnionFile(vec!["9.0.0".into(), "1.0.0".into(), "0.1.0".into()]);

        let regenerated = regenerate_over(&committed, &plan_of(observed, &[]), &selection, false);

        assert_eq!(keys(&regenerated["tags"]), vec!["1.0.0", "2.0.0", "9.0.0", "0.1.0"]);
    }

    #[test]
    fn a_removed_row_leaves_and_its_neighbours_stay_verbatim() {
        let committed = committed_holding(serde_json::json!({
            "1.0.0": durable_row('a'), "2.0.0": ephemeral_row('b'), "3.0.0": durable_row('c')
        }));

        let regenerated = regenerate_over(&committed, &plan_of(vec![], &["2.0.0"]), &TagSelection::Refresh, false);

        assert_eq!(keys(&regenerated["tags"]), vec!["1.0.0", "3.0.0"]);
        assert_eq!(regenerated["tags"]["1.0.0"], committed["tags"]["1.0.0"]);
        assert_eq!(regenerated["tags"]["3.0.0"], committed["tags"]["3.0.0"]);
    }

    /// The same removal applied to a root that no longer has the row changes
    /// nothing, which is what lets a retry or a rebuilt branch replay it.
    #[test]
    fn removing_a_row_that_is_already_gone_changes_nothing() {
        let committed =
            committed_holding(serde_json::json!({ "1.0.0": durable_row('a'), "2.0.0": ephemeral_row('b') }));
        let plan = plan_of(vec![], &["2.0.0"]);
        let once = regenerate_over(&committed, &plan, &TagSelection::Refresh, false);

        let twice = regenerate_over(&once, &plan, &TagSelection::Refresh, false);

        assert_eq!(serialize_root(&twice), serialize_root(&once));
    }

    /// The marker records how a row was added, so it is stamped on rows the
    /// run adds and never rewritten on one that already exists, in either
    /// direction.
    #[test]
    fn the_ephemeral_flag_marks_only_the_rows_the_run_adds() {
        let committed =
            committed_holding(serde_json::json!({ "1.0.0": durable_row('a'), "2.0.0": ephemeral_row('b') }));
        let observed = vec![observed("1.0.0", 'x'), observed("2.0.0", 'y'), observed("3.0.0", 'z')];
        let selection = TagSelection::UnionFile(vec!["1.0.0".into(), "2.0.0".into(), "3.0.0".into()]);

        let marked = regenerate_over(&committed, &plan_of(observed, &[]), &selection, true);

        assert!(
            marked["tags"]["1.0.0"].get("ephemeral").is_none(),
            "an existing durable row is not promoted to ephemeral"
        );
        assert_eq!(marked["tags"]["2.0.0"]["ephemeral"], Value::Bool(true));
        assert_eq!(
            marked["tags"]["3.0.0"]["ephemeral"],
            Value::Bool(true),
            "the added row is marked"
        );
    }

    #[test]
    fn without_the_ephemeral_flag_added_rows_carry_no_marker_and_existing_markers_stay() {
        let committed = committed_holding(serde_json::json!({ "2.0.0": ephemeral_row('b') }));
        let observed = vec![observed("2.0.0", 'y'), observed("4.0.0", 'z')];
        let selection = TagSelection::UnionFile(vec!["2.0.0".into(), "4.0.0".into()]);

        let regenerated = regenerate_over(&committed, &plan_of(observed, &[]), &selection, false);

        assert_eq!(
            regenerated["tags"]["2.0.0"]["ephemeral"],
            Value::Bool(true),
            "a run without the flag never clears an existing marker"
        );
        assert!(regenerated["tags"]["4.0.0"].get("ephemeral").is_none());
    }

    /// Key order is byte-visible: `content, observed, yanked, ephemeral`. The
    /// yank is applied after regeneration, so this holds only if the yank
    /// lands before a marker the row already carries.
    #[test]
    fn a_new_ephemeral_row_keeps_the_field_order_through_a_yank() {
        let committed = committed_holding(serde_json::json!({}));
        let selection = TagSelection::UnionFile(vec!["3.0.0".into()]);
        let mut regenerated = regenerate_over(
            &committed,
            &plan_of(vec![observed("3.0.0", 'z')], &[]),
            &selection,
            true,
        );
        assert_eq!(
            keys(&regenerated["tags"]["3.0.0"]),
            vec!["content", "observed", "ephemeral"]
        );

        apply_yank_markers(&mut regenerated, &["3.0.0".to_string()], &[], "broken", NOW).expect("3.0.0 is curated");

        assert_eq!(
            keys(&regenerated["tags"]["3.0.0"]),
            vec!["content", "observed", "yanked", "ephemeral"]
        );
    }

    // ── change_body ──────────────────────────────────────────────────────────

    fn body_for(plan: &TagPlan) -> String {
        let committed = committed_holding(serde_json::json!({
            "1.0.0": durable_row('a'), "2.0.0": ephemeral_row('b'), "3.0.0": durable_row('c')
        }));
        change_body("acme/tool", &committed, plan, &[])
    }

    /// The body is what a reviewer reads before merging a removal, so every
    /// tag the run adds, removes or keeps-while-missing must be named in it.
    #[test]
    fn the_body_names_added_removed_and_durable_missing_tags() {
        let mut plan = plan_of(vec![observed("7.7.7-added", 'y')], &["2.0.0"]);
        plan.durable_missing = vec!["3.0.0".to_string()];

        let body = body_for(&plan);

        assert_eq!(
            body,
            "Publisher-curated tag update for `acme/tool`.\n\
             \nAdded or updated: 7.7.7-added\n\
             \nRemoved: 2.0.0\n\
             \nKept, gone from the registry: 3.0.0\n",
            "each tag sits under its own label"
        );
    }

    /// A tag whose row this run did not change is not part of the change.
    #[test]
    fn the_body_does_not_name_a_tag_the_run_left_alone() {
        let committed = committed_holding(serde_json::json!({ "1.0.0": durable_row('a') }));
        let unchanged = Observed {
            tag: "1.0.0".to_string(),
            content: ocx_oci::Digest::try_from(digest_string('a').as_str()).expect("a valid digest"),
            bytes: Vec::new(),
        };
        let plan = plan_of(vec![unchanged, observed("5.0.0-added", 'y')], &[]);

        let body = change_body("acme/tool", &committed, &plan, &[]);

        assert!(body.contains("5.0.0-added"));
        assert!(!body.contains("1.0.0"), "an unchanged row is not listed: {body}");
    }

    /// A reviewer must see which package the request changes.
    #[test]
    fn the_body_names_the_package() {
        let plan = plan_of(vec![observed("7.7.7-added", 'y')], &[]);

        assert!(body_for(&plan).contains("`acme/tool`"));
    }

    /// A `--tags` run drops every committed row it does not name; the body
    /// lists those beside the confirmed-gone rows, so no removal goes unreviewed.
    #[test]
    fn the_body_lists_a_row_dropped_by_omission_as_removed() {
        let plan = plan_of(vec![observed("1.0.0", 'a')], &["2.0.0"]);
        let committed = committed_holding(serde_json::json!({
            "1.0.0": durable_row('a'), "2.0.0": ephemeral_row('b'), "3.0.0": durable_row('c')
        }));

        let body = change_body("acme/tool", &committed, &plan, &["3.0.0".to_string()]);

        let removed = body
            .lines()
            .find(|line| line.starts_with("Removed:"))
            .unwrap_or_else(|| panic!("no removed line: {body}"));
        assert!(removed.contains("2.0.0") && removed.contains("3.0.0"), "{removed}");
    }

    /// The body rides a push option that must carry no markdown or mention, and a
    /// dropped name comes from the committed root unchecked against the tag grammar.
    #[test]
    fn the_body_leaves_out_a_name_outside_the_tag_grammar() {
        let plan = plan_of(vec![observed("1.0.0", 'a')], &[]);
        let committed = committed_holding(serde_json::json!({ "1.0.0": durable_row('a') }));

        let body = change_body("acme/tool", &committed, &plan, &["__ocx](http://x) @u".to_string()]);

        assert!(!body.contains("__ocx"), "an invalid name is not listed: {body}");
        assert!(
            !body.contains("http") && !body.contains('@'),
            "no link or mention: {body}"
        );
        assert!(body.contains("1.0.0"), "a valid name is still listed: {body}");
    }

    // ── Shared clock (C-007) ─────────────────────────────────────────────────

    /// The instant the seam is pinned to. Past-dated on purpose: a renderer that
    /// ignores the pin produces today, which can never equal it.
    const PINNED_INSTANT: &str = "2001-02-03T04:05:06Z";

    /// Owns `__OCX_TESTING_ANNOUNCE_CLOCK` for the lifetime of one test.
    ///
    /// The name is a literal here, never a constant shared with production, so
    /// renaming what production reads leaves the pin inert and reds the test.
    /// The real process variable is written rather than
    /// [`ocx_util::env::overrides::EnvLock`]'s override map, because a real variable is
    /// visible to `std::env::var` and to `ocx_util::env::var` alike — this test must
    /// not dictate which of the two the shared clock reads through. `EnvLock` is
    /// held for the process-wide serialisation it provides.
    struct ClockSeam {
        _lock: ocx_util::env::overrides::EnvLock,
    }

    impl ClockSeam {
        /// Pins the seam to `instant` until the guard drops.
        fn pinned(instant: &str) -> Self {
            let lock = ocx_util::env::overrides::lock();
            // SAFETY: nextest (`taskfiles/rust.taskfile.yml:146`) gives every
            // test its own process, and `EnvLock` serialises this write against
            // every test that goes through `ocx_util::env::overrides`. Two seams opt out
            // of that serialisation instead — `ocx_oci::host_capabilities` and
            // `file_structure::shim_bin_store` — each safe only because one test
            // function owns its variable.
            unsafe { std::env::set_var("__OCX_TESTING_ANNOUNCE_CLOCK", instant) };
            Self { _lock: lock }
        }
    }

    impl Drop for ClockSeam {
        fn drop(&mut self) {
            // SAFETY: see `ClockSeam::pinned`. A struct's own `Drop` runs before
            // its fields', so the pin is gone before `_lock` releases the mutex.
            // Unconditional, so a stub that panics mid-test cannot leak it into a
            // sibling — the Specify phase runs against `unimplemented!()`.
            unsafe { std::env::remove_var("__OCX_TESTING_ANNOUNCE_CLOCK") };
        }
    }

    /// C-007 — announce and the index-root writer render the *same* instant.
    ///
    /// `ocx package claim` is WP-9's and does not exist yet, so the contract is
    /// expressed over the two renderers that exist today: this module's
    /// `current_timestamp`, [`ocx_index::current_timestamp`], and
    /// [`ocx_index::current_date`] as that same instant's date. A future claim
    /// call site rides on exactly this relation.
    ///
    /// Deliberately **behavioural**, never a source-text count of readers of the
    /// env var: such a scan would live in the file it scans and match its own
    /// needle (plan, "Two guards were deliberately not written").
    ///
    /// Its discrimination limit, stated so the guard is not read as more than it
    /// is: the pin reds a `current_timestamp` here that reacquires a bare
    /// `Utc::now()`, because that renders today. A verbatim copy of the old body
    /// — its own clock *plus* its own read of the same seam — would still return
    /// the pinned value; that shape is caught by this module holding no clock at
    /// all, at review, not by this assertion.
    #[test]
    fn claim_and_announce_render_the_same_instant() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        let announce = current_timestamp();
        let index = ocx_index::current_timestamp();
        let date = ocx_index::current_date();

        assert_eq!(announce, index, "one clock stands behind both renderers");
        assert_eq!(
            announce, PINNED_INSTANT,
            "and it is the pinned instant, not a fresh reading either side made"
        );
        assert_eq!(date, PINNED_INSTANT[..10], "the date is that same instant's date");
    }
}
