// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure decision logic + registry observation for the announce pipeline.
//!
//! Everything a forge is *not* needed for lives here so it can be unit-tested
//! without a network: curated-tag resolution (C3/C5), the regenerate step that
//! preserves `observed` timestamps for unmoved digests (C6), the yank/unyank
//! rules (C7), physical-repository extraction, and the SSRF-before-observe
//! ordering (X3). The forge-touching orchestration (root read, fork/commit/PR
//! dispatch) lives in the parent [`super`] module.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::error::AnnounceError;
use super::request::TagSelection;
use crate::oci;
use crate::oci::index::{Observation, ObservationPlatform, serialize_observation};
use crate::publisher::Publisher;

/// One curated tag's freshly observed state — the serialized observation-object
/// bytes plus the `sha256:<hex>` digest of exactly those bytes (its `content`
/// pointer and CAS filename).
pub struct Observed {
    /// The curated tag name.
    pub tag: String,
    /// The observation-object digest `sha256:<hex>` (the tag's new `content`).
    pub content: String,
    /// The canonical observation-object bytes (CONTRACTS §14, the CAS payload).
    pub bytes: Vec<u8>,
}

/// The physical registry target dereferenced from a root's `repository` pointer.
pub struct Physical {
    /// The registry host (for the SSRF pre-flight).
    pub host: String,
    /// The registry port (443 unless the pointer carried an explicit `:port`).
    pub port: u16,
    /// The physical `<registry>/<repository>` identifier the observe loop fetches
    /// tags against.
    pub identifier: oci::Identifier,
    /// The verbatim `oci://…` pointer, echoed in observe error messages.
    pub display: String,
}

/// The `observed` timestamp used for new/changed tags in this announce run.
///
/// Computed once per run and threaded so a tag map is internally consistent. The
/// `__OCX_TESTING_ANNOUNCE_CLOCK` env seam (test / `__testing` only) pins it so
/// acceptance tests get byte-deterministic output; production reads the wall
/// clock in the index bot's `%Y-%m-%dT%H:%M:%SZ` seconds-Z form.
pub fn current_timestamp() -> String {
    #[cfg(any(test, feature = "__testing"))]
    {
        if let Ok(fixed) = std::env::var("__OCX_TESTING_ANNOUNCE_CLOCK") {
            return fixed;
        }
    }
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Require a committed root: a `None` read means the package has no root at
/// `base_ref`, an unclaimed namespace that must go through the human lane
/// (design register C10, reference parity).
///
/// # Errors
///
/// [`AnnounceError::UnclaimedNamespace`] when `bytes` is `None`.
pub fn require_root(
    package: &str,
    path: &str,
    base_ref: &str,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<u8>, AnnounceError> {
    bytes.ok_or_else(|| AnnounceError::UnclaimedNamespace {
        package: package.to_string(),
        path: path.to_string(),
        base_ref: base_ref.to_string(),
    })
}

/// The tag names present in the committed root, in on-disk (insertion) order.
pub fn committed_tag_names(root: &Value) -> Vec<String> {
    root.get("tags")
        .and_then(Value::as_object)
        .map(|tags| tags.keys().cloned().collect())
        .unwrap_or_default()
}

/// Resolve the curated tag set from the selection and the committed tags
/// (design register C3/C5). `Replace` is the universe as given; `UnionFile`
/// unions the committed set (order-preserving) with file additions; `Refresh`
/// re-observes the committed set. Duplicates are dropped, first occurrence wins.
pub fn resolve_curated_tags(selection: &TagSelection, committed: &[String]) -> Result<Vec<String>, AnnounceError> {
    let resolved = match selection {
        TagSelection::Replace(tags) => dedup_in_order(tags),
        TagSelection::UnionFile(file_tags) => {
            let mut union = dedup_in_order(committed);
            for tag in file_tags {
                if !union.contains(tag) {
                    union.push(tag.clone());
                }
            }
            union
        }
        TagSelection::Refresh => dedup_in_order(committed),
    };
    if resolved.is_empty() {
        return Err(AnnounceError::NoCuratedTags);
    }
    Ok(resolved)
}

fn dedup_in_order(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.iter().filter(|tag| seen.insert((*tag).clone())).cloned().collect()
}

/// Dereference a root's `repository` pointer into its physical registry target,
/// applying the strict `oci://host/path` parse (design register C3).
pub fn extract_physical(root: &Value) -> Result<Physical, AnnounceError> {
    let repository = root
        .get("repository")
        .and_then(Value::as_str)
        .ok_or(AnnounceError::RootMissingField { field: "repository" })?;
    let (registry, path) = crate::oci::index::parse_physical_repository(repository).map_err(|_| {
        AnnounceError::MalformedPhysicalRepository {
            value: repository.to_string(),
        }
    })?;
    let (host, port) = oci::ssrf::split_host_port(&registry);
    Ok(Physical {
        host: host.to_string(),
        port,
        identifier: oci::Identifier::new_registry(path, registry.clone()),
        display: repository.to_string(),
    })
}

/// SSRF pre-flight (X3) then observe every curated tag against the physical
/// repository. The pre-flight resolves + validates the physical host **before**
/// the first registry request, so a forbidden host aborts with zero observes.
///
/// # Errors
///
/// [`AnnounceError::Ssrf`] if the physical host is forbidden or unresolvable;
/// [`AnnounceError::UnresolvedTag`] if a curated tag does not resolve (a
/// publisher typo); or an [`AnnounceError::Observe`] transport failure.
pub async fn observe_curated(
    publisher: &Publisher,
    root: &Value,
    curated: &[String],
    trusted_hosts: &[String],
) -> Result<Vec<Observed>, AnnounceError> {
    let physical = extract_physical(root)?;
    // X3: validate the remote-controlled host before the first registry request.
    oci::ssrf::resolve_and_validate(&physical.host, physical.port, trusted_hosts).await?;
    let mut observed = Vec::with_capacity(curated.len());
    for tag in curated {
        observed.push(observe_one_tag(publisher, &physical, tag).await?);
    }
    Ok(observed)
}

/// Observe a single curated tag: fetch its manifest, project it into an
/// observation object, and hash the canonical bytes into its `content` pointer.
async fn observe_one_tag(publisher: &Publisher, physical: &Physical, tag: &str) -> Result<Observed, AnnounceError> {
    let tagged = physical.identifier.clone_with_tag(tag);
    let fetched = publisher
        .client()
        .fetch_manifest_raw_bytes(&tagged)
        .await
        .map_err(|source| AnnounceError::Observe {
            tag: tag.to_string(),
            repository: physical.display.clone(),
            source: Box::new(source),
        })?;
    let Some((_bytes, _digest, manifest)) = fetched else {
        // A curated tag that does not resolve is a publisher typo — hard error,
        // never a silent drop (reference parity).
        return Err(AnnounceError::UnresolvedTag {
            tag: tag.to_string(),
            repository: physical.display.clone(),
        });
    };
    let observation = manifest_to_observation(&manifest, tag, &physical.display)?;
    let bytes = serialize_observation(&observation);
    let content = oci::Algorithm::Sha256.hash(&bytes).to_string();
    Ok(Observed {
        tag: tag.to_string(),
        content,
        bytes,
    })
}

/// Project a fetched manifest into an observation object (its `platforms[]`).
///
/// ocx packages always publish an image index (even single-platform pushes are
/// wrapped in one), so a bare image manifest is out of the v1 observe scope.
/// Platform-less index entries (attestation manifests) are skipped.
fn manifest_to_observation(
    manifest: &oci::Manifest,
    tag: &str,
    repository: &str,
) -> Result<Observation, AnnounceError> {
    let index = match manifest {
        oci::Manifest::ImageIndex(index) => index,
        oci::Manifest::Image(_) => {
            return Err(AnnounceError::SinglePlatformManifest {
                tag: tag.to_string(),
                repository: repository.to_string(),
            });
        }
    };
    let mut platforms = Vec::with_capacity(index.manifests.len());
    for entry in &index.manifests {
        let Some(platform) = entry.platform.clone() else {
            continue;
        };
        let digest =
            oci::Digest::try_from(entry.digest.as_str()).map_err(|_| AnnounceError::MalformedPlatformDigest {
                tag: tag.to_string(),
                repository: repository.to_string(),
                digest: entry.digest.clone(),
            })?;
        platforms.push(ObservationPlatform { platform, digest });
    }
    if platforms.is_empty() {
        return Err(AnnounceError::EmptyObservation {
            tag: tag.to_string(),
            repository: repository.to_string(),
        });
    }
    Ok(Observation { platforms })
}

/// Rebuild the root's `tags` map from the observed curated set (design register
/// C3/C6). A tag whose observed digest equals its committed `content` keeps its
/// entry verbatim — same `observed` timestamp, same yank marker — so a no-op
/// re-observe is byte-identical (drives the C6 short-circuit). A new or
/// changed-digest tag gets `observed = now`, preserving any existing yank
/// marker (human-governed, survives a content change). A committed tag absent
/// from the curated set is dropped. Every non-`tags` field rides through
/// verbatim.
pub fn regenerate(committed: &Value, observed: &[Observed], now: &str) -> Value {
    let committed_tags = committed.get("tags").and_then(Value::as_object);
    let mut new_tags = Map::new();
    for entry in observed {
        let committed_entry = committed_tags.and_then(|tags| tags.get(&entry.tag));
        let committed_content = committed_entry
            .and_then(|committed| committed.get("content"))
            .and_then(Value::as_str);
        let regenerated = if committed_content == Some(entry.content.as_str()) {
            // Unmoved digest — carry the committed entry verbatim (no churn).
            committed_entry
                .cloned()
                .unwrap_or_else(|| new_tag_entry(&entry.content, now, None))
        } else {
            // New or changed digest — fresh timestamp, keep any yank marker.
            let yanked = committed_entry.and_then(|committed| committed.get("yanked")).cloned();
            new_tag_entry(&entry.content, now, yanked)
        };
        new_tags.insert(entry.tag.clone(), regenerated);
    }
    let mut new_root = committed.clone();
    if let Some(root) = new_root.as_object_mut() {
        // Replacing an existing key keeps its position (preserve_order), so
        // `tags` stays the last field per CONTRACTS §14.
        root.insert("tags".to_string(), Value::Object(new_tags));
    }
    new_root
}

/// A fresh `{content, observed}` tag entry, carrying a preserved yank marker
/// when one existed. Field order matches the index bot's `TagEntry` (CONTRACTS
/// §14): `content`, `observed`, then `yanked`.
fn new_tag_entry(content: &str, now: &str, yanked: Option<Value>) -> Value {
    let mut entry = Map::new();
    entry.insert("content".to_string(), Value::String(content.to_string()));
    entry.insert("observed".to_string(), Value::String(now.to_string()));
    if let Some(yanked) = yanked {
        entry.insert("yanked".to_string(), yanked);
    }
    Value::Object(entry)
}

/// Apply yank/unyank markers to the regenerated root's curated tags (design
/// register C7). Owner action only: a tag named to both lists, or a tag outside
/// the curated set, is a hard input error — never a silent no-op. `--refresh`
/// callers pass empty lists, so existing markers are untouched.
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
        entry.insert("yanked".to_string(), json!({ "reason": reason, "at": now }));
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

/// Count observed CAS objects not already referenced by a committed tag's
/// `content` — a component of the C6 "no new CAS objects" short-circuit.
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
        .filter(|entry| !committed_contents.contains(entry.content.as_str()))
        .count()
}

/// Assemble the atomic file set for an announce: the root plus one CAS object
/// per observed tag, keyed by wire path (design register C15 — one commit).
pub fn build_files(
    root_path: &str,
    root_bytes: &[u8],
    package_repo: &str,
    observed: &[Observed],
) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    files.insert(root_path.to_string(), root_bytes.to_vec());
    for entry in observed {
        let hex = entry.content.strip_prefix("sha256:").unwrap_or(&entry.content);
        files.insert(format!("p/{package_repo}/o/sha256/{hex}.json"), entry.bytes.clone());
    }
    files
}

/// Write the announce file set under `dir`, returning the written relative
/// paths (sorted — the `BTreeMap` iterates in key order).
///
/// # Errors
///
/// [`AnnounceError::OutputWrite`] on any directory-create or file-write failure.
pub async fn write_out(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<String>, AnnounceError> {
    let mut written = Vec::with_capacity(files.len());
    for (relative, bytes) in files {
        let path = dir.join(relative);
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
    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{serialize_observation, serialize_root};

    // ── fixtures ─────────────────────────────────────────────────────────────

    fn digest_string(fill: char) -> String {
        format!("sha256:{}", fill.to_string().repeat(64))
    }

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

    /// Build an `Observed` for `tag` whose observation carries a single platform
    /// leaf `digest`. The `content`/`bytes` are the real canonical serialization
    /// so digest comparisons behave exactly as in production.
    fn observed(tag: &str, leaf: char) -> Observed {
        let observation = Observation {
            platforms: vec![ObservationPlatform {
                platform: oci::native::Platform {
                    architecture: "amd64".into(),
                    os: "linux".into(),
                    os_version: None,
                    os_features: None,
                    variant: None,
                    features: None,
                },
                digest: oci::Digest::Sha256(leaf.to_string().repeat(64)),
            }],
        };
        let bytes = serialize_observation(&observation);
        let content = oci::Algorithm::Sha256.hash(&bytes).to_string();
        Observed {
            tag: tag.to_string(),
            content,
            bytes,
        }
    }

    // ── resolve_curated_tags (C3/C5) ─────────────────────────────────────────

    #[test]
    fn replace_is_the_universe_and_drops_absent_committed_tags() {
        let committed = vec!["1.0.0".to_string(), "2.0.0".to_string()];
        let curated = resolve_curated_tags(&TagSelection::Replace(vec!["2.0.0".into()]), &committed).unwrap();
        assert_eq!(
            curated,
            vec!["2.0.0".to_string()],
            "a committed tag absent from --tags is dropped"
        );
    }

    #[test]
    fn union_file_adds_to_the_committed_set_preserving_order() {
        let committed = vec!["1.0.0".to_string(), "2.0.0".to_string()];
        let curated = resolve_curated_tags(
            &TagSelection::UnionFile(vec!["3.0.0".into(), "1.0.0".into()]),
            &committed,
        )
        .unwrap();
        // Committed order first, then the genuinely new file tag; the duplicate
        // `1.0.0` is not re-added.
        assert_eq!(
            curated,
            vec!["1.0.0".to_string(), "2.0.0".to_string(), "3.0.0".to_string()]
        );
    }

    #[test]
    fn refresh_re_observes_the_committed_set_in_order() {
        let committed = vec!["1.0.0".to_string(), "latest".to_string()];
        let curated = resolve_curated_tags(&TagSelection::Refresh, &committed).unwrap();
        assert_eq!(curated, committed);
    }

    #[test]
    fn empty_curated_set_is_an_error() {
        assert!(matches!(
            resolve_curated_tags(&TagSelection::Replace(vec![]), &[]),
            Err(AnnounceError::NoCuratedTags)
        ));
        assert!(matches!(
            resolve_curated_tags(&TagSelection::Refresh, &[]),
            Err(AnnounceError::NoCuratedTags)
        ));
    }

    // ── require_root (unclaimed namespace, C10) ──────────────────────────────

    #[test]
    fn require_root_errors_on_a_missing_committed_root() {
        let error = require_root("acme/widget", "p/acme/widget.json", "main", None).unwrap_err();
        assert!(matches!(error, AnnounceError::UnclaimedNamespace { .. }));
    }

    #[test]
    fn require_root_returns_present_bytes() {
        let bytes = require_root("acme/widget", "p/acme/widget.json", "main", Some(b"root".to_vec())).unwrap();
        assert_eq!(bytes, b"root");
    }

    // ── extract_physical (C3) ────────────────────────────────────────────────

    #[test]
    fn extract_physical_parses_host_and_repository() {
        let root = committed_root("oci://ghcr.io/ocx-contrib/widget");
        let physical = extract_physical(&root).unwrap();
        assert_eq!(physical.host, "ghcr.io");
        assert_eq!(physical.port, 443);
        assert_eq!(physical.identifier.registry(), "ghcr.io");
        assert_eq!(physical.identifier.repository(), "ocx-contrib/widget");
    }

    #[test]
    fn extract_physical_honours_an_explicit_port() {
        let root = committed_root("oci://registry.corp:5000/team/tool");
        let physical = extract_physical(&root).unwrap();
        assert_eq!(physical.host, "registry.corp");
        assert_eq!(physical.port, 5000);
    }

    #[test]
    fn extract_physical_rejects_a_missing_scheme() {
        let root = committed_root("ghcr.io/ocx-contrib/widget");
        assert!(matches!(
            extract_physical(&root),
            Err(AnnounceError::MalformedPhysicalRepository { .. })
        ));
    }

    #[test]
    fn extract_physical_errors_when_repository_is_absent() {
        let root = serde_json::json!({ "tags": {} });
        assert!(matches!(
            extract_physical(&root),
            Err(AnnounceError::RootMissingField { field: "repository" })
        ));
    }

    // ── manifest_to_observation ──────────────────────────────────────────────

    fn image_index(entries: Vec<oci::ImageIndexEntry>) -> oci::Manifest {
        oci::Manifest::ImageIndex(oci::ImageIndex {
            schema_version: oci::INDEX_SCHEMA_VERSION,
            media_type: Some(oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            artifact_type: None,
            manifests: entries,
            annotations: None,
        })
    }

    fn index_entry(architecture: &str, digest: char) -> oci::ImageIndexEntry {
        oci::ImageIndexEntry {
            media_type: oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: digest_string(digest),
            size: 0,
            platform: Some(oci::native::Platform {
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

    #[test]
    fn manifest_to_observation_projects_index_platform_entries() {
        let manifest = image_index(vec![index_entry("amd64", 'a'), index_entry("arm64", 'b')]);
        let observation = manifest_to_observation(&manifest, "1.0.0", "oci://ghcr.io/x/y").unwrap();
        assert_eq!(observation.platforms.len(), 2);
        assert_eq!(observation.platforms[0].digest, oci::Digest::Sha256("a".repeat(64)));
    }

    #[test]
    fn manifest_to_observation_skips_platform_less_entries() {
        let mut attestation = index_entry("amd64", 'a');
        attestation.platform = None;
        let manifest = image_index(vec![index_entry("arm64", 'b'), attestation]);
        let observation = manifest_to_observation(&manifest, "1.0.0", "oci://ghcr.io/x/y").unwrap();
        assert_eq!(observation.platforms.len(), 1, "the platform-less entry is skipped");
    }

    #[test]
    fn manifest_to_observation_rejects_a_single_platform_manifest() {
        let manifest = oci::Manifest::Image(oci::ImageManifest::default());
        assert!(matches!(
            manifest_to_observation(&manifest, "1.0.0", "oci://ghcr.io/x/y"),
            Err(AnnounceError::SinglePlatformManifest { .. })
        ));
    }

    #[test]
    fn manifest_to_observation_errors_on_no_platform_entries() {
        let manifest = image_index(vec![]);
        assert!(matches!(
            manifest_to_observation(&manifest, "1.0.0", "oci://ghcr.io/x/y"),
            Err(AnnounceError::EmptyObservation { .. })
        ));
    }

    // ── regenerate (C6 no-churn) ─────────────────────────────────────────────

    #[test]
    fn regenerate_keeps_the_observed_timestamp_for_an_unmoved_digest() {
        let root = committed_root("oci://ghcr.io/x/y");
        // Splice the committed tag's content to match the observation so the
        // "unchanged" path fires.
        let entry = observed("1.0.0", 'z');
        let mut committed = root;
        committed["tags"]["1.0.0"]["content"] = Value::String(entry.content.clone());
        let regenerated = regenerate(&committed, &[entry], "2099-12-31T00:00:00Z");
        assert_eq!(
            regenerated["tags"]["1.0.0"]["observed"].as_str(),
            Some("2026-01-01T00:00:00Z"),
            "an unmoved digest must keep its committed observed timestamp"
        );
    }

    #[test]
    fn regenerate_stamps_now_for_a_new_or_changed_digest() {
        let committed = committed_root("oci://ghcr.io/x/y");
        // The committed `1.0.0` content is `sha256:aaaa…`; the observation's real
        // content differs, so the tag is treated as changed.
        let regenerated = regenerate(&committed, &[observed("1.0.0", 'c')], "2099-12-31T00:00:00Z");
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
        let regenerated = regenerate(&committed, &[observed("1.0.0", 'a')], "2099-12-31T00:00:00Z");
        assert!(regenerated["tags"].get("2.0.0").is_none());
        assert!(regenerated["tags"].get("1.0.0").is_some());
    }

    #[test]
    fn regenerate_carries_human_fields_verbatim() {
        let committed = committed_root("oci://ghcr.io/x/y");
        let regenerated = regenerate(&committed, &[observed("1.0.0", 'a')], "2099-12-31T00:00:00Z");
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
                "1.0.0": { "content": entry.content.clone(), "observed": "2026-01-01T00:00:00Z" }
            }
        });
        let committed_bytes = serialize_root(&committed);
        let regenerated = regenerate(&committed, &[entry], "2099-12-31T00:00:00Z");
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
        let hex = entry.content.strip_prefix("sha256:").unwrap().to_string();
        let files = build_files(
            "p/acme/widget.json",
            b"root-bytes",
            "acme/widget",
            std::slice::from_ref(&entry),
        );
        assert_eq!(
            files.get("p/acme/widget.json").map(Vec::as_slice),
            Some(b"root-bytes".as_slice())
        );
        assert!(files.contains_key(&format!("p/acme/widget/o/sha256/{hex}.json")));
    }

    // ── unclaimed namespace + SSRF ordering ──────────────────────────────────

    fn stub_publisher(data: &StubTransportData) -> Publisher {
        Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ssrf_pre_flight_refuses_a_forbidden_host_with_zero_observes() {
        // A committed root pointing at loopback must abort at the SSRF pre-flight
        // (X3) before any registry request reaches the stub transport.
        let data = StubTransportData::new();
        let publisher = stub_publisher(&data);
        let root = committed_root("oci://127.0.0.1/x");
        let result = observe_curated(&publisher, &root, &["1.0.0".to_string()], &[]).await;
        assert!(
            matches!(result, Err(AnnounceError::Ssrf(_))),
            "forbidden host must be refused"
        );
        let inner = data.read();
        assert!(
            inner.calls.is_empty(),
            "no registry call may precede the SSRF refusal: {:?}",
            inner.calls
        );
        assert!(inner.auth_calls.is_empty(), "no auth call may precede the SSRF refusal");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ssrf_pre_flight_allows_a_trusted_forbidden_host() {
        // The trusted_hosts escape hatch (X2) lets a loopback registry through;
        // the stub then answers with no manifest, surfacing UnresolvedTag — proof
        // the observe loop ran only *after* the pre-flight passed.
        let data = StubTransportData::new();
        let publisher = stub_publisher(&data);
        let root = committed_root("oci://127.0.0.1/x");
        let result = observe_curated(&publisher, &root, &["1.0.0".to_string()], &["127.0.0.1".to_string()]).await;
        assert!(
            matches!(result, Err(AnnounceError::UnresolvedTag { .. })),
            "a trusted host proceeds to observe; the empty stub yields UnresolvedTag"
        );
    }
}
