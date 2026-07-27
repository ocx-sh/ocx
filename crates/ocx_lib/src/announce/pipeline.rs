// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure decision logic + the registry observe loop for the announce pipeline.
//!
//! Everything a forge is *not* needed for lives here so it can be unit-tested
//! without a network: curated-tag resolution (C3/C5/D7), the regenerate step
//! that preserves `observed` timestamps for unmoved digests (C6), the
//! yank/unyank rules (C7), physical-repository extraction, and the
//! SSRF-before-any-registry-request ordering (X3). The forge-touching
//! orchestration (root read, fork/commit/PR dispatch) lives in the parent
//! [`super`] module.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::error::AnnounceError;
use super::request::TagSelection;
use crate::oci;
use crate::package::tag::Tag;
use crate::publisher::Publisher;

/// One curated tag's freshly observed state — the registry's own image-index
/// bytes and the digest the registry served them under (the tag's `content`
/// pointer and its CAS filename).
///
/// Both fields are verbatim registry output: announce stores what it fetched
/// and never re-encodes it, so the CAS payload is byte-identical to the
/// artifact the publisher pushed.
pub struct Observed {
    /// The curated tag name.
    pub tag: String,
    /// The image-index digest the registry served (the tag's new `content`).
    pub content: oci::Digest,
    /// The registry's image-index bytes, unmodified (the CAS payload).
    pub bytes: Vec<u8>,
}

/// The outcome of collapsing a [`TagSelection`] against the committed tags.
pub struct ResolvedTags {
    /// The curated tags to observe, in resolution order.
    pub tags: Vec<String>,
    /// Reserved tags dropped from the selection (D7), in resolution order.
    pub reserved_dropped: Vec<String>,
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
/// re-observes the committed set; `FromRegistry` unions the committed set with
/// `discovered` (the tags the physical repository currently holds, already
/// filtered — see below). Duplicates are dropped, first occurrence wins.
///
/// `discovered` is empty for every selection other than
/// [`TagSelection::FromRegistry`], which is the only one that reaches the
/// registry to decide *which* tags exist.
///
/// This is where the reserved-tag rule applies (D7), and it applies **here**
/// rather than at any of the caller-supplied selection sources: only after the
/// collapse is the tag universe concrete. `--refresh` and `--tags-from-file`
/// start from the committed root, so they are carriers — neither can introduce a
/// reserved tag, but either would re-announce one forever once it landed. A
/// reserved tag is not a version, so it is dropped and reported, never refused:
/// refusing would make announce police how a publisher tags their own
/// repository. A selection that is *entirely* reserved collapses into the
/// existing [`AnnounceError::NoCuratedTags`] — the empty-set case, no separate
/// variant — carrying the dropped names, because that path returns no outcome
/// for the CLI's drop notice to read and the names would otherwise vanish.
///
/// `discovered` is the one source filtered *before* the collapse, by
/// [`list_registry_tags`], and it is filtered silently. `reserved_dropped`
/// reports what the **caller** named that turned out not to be a version; a
/// registry listing names nothing — canonical `sha256.<hex>` tags are pushed by
/// default, so reporting them would drown a real drop under one line per
/// published version. A reserved tag that is nonetheless *committed* still
/// reaches the collapse through `committed` and is still dropped and reported.
///
/// # Errors
///
/// [`AnnounceError::NoCuratedTags`] when nothing survives resolution.
pub fn resolve_curated_tags(
    selection: &TagSelection,
    committed: &[String],
    discovered: &[String],
) -> Result<ResolvedTags, AnnounceError> {
    let resolved = match selection {
        TagSelection::Replace(tags) => dedup_in_order(tags),
        TagSelection::UnionFile(file_tags) => union_onto_committed(committed, file_tags),
        TagSelection::Refresh => dedup_in_order(committed),
        TagSelection::FromRegistry => union_onto_committed(committed, discovered),
    };
    let (reserved_dropped, tags): (Vec<String>, Vec<String>) =
        resolved.into_iter().partition(|tag| Tag::is_reserved_str(tag));
    if tags.is_empty() {
        return Err(AnnounceError::NoCuratedTags { reserved_dropped });
    }
    Ok(ResolvedTags { tags, reserved_dropped })
}

/// The additive merge shared by `--tags-from-file` and `--tags-from-registry`: the
/// committed set in its on-disk order, then whatever `additions` contributes
/// that is not already there. A committed tag is never dropped by either — only
/// [`TagSelection::Replace`] removes.
fn union_onto_committed(committed: &[String], additions: &[String]) -> Vec<String> {
    let mut union = dedup_in_order(committed);
    for tag in additions {
        if !union.contains(tag) {
            union.push(tag.clone());
        }
    }
    union
}

/// List the tags the physical repository currently holds, dropping the reserved
/// ones (D7) at the source.
///
/// The caller must have run the SSRF pre-flight for `physical` already — this is
/// the first registry request of a `--tags-from-registry` run, so validating
/// after it would validate nothing.
///
/// # Errors
///
/// [`AnnounceError::ListTags`] when the registry listing fails. An empty
/// repository is not an error here: it collapses into
/// [`AnnounceError::NoCuratedTags`] alongside an empty committed set, the same
/// as any other selection that resolves to nothing.
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

/// Resolve and validate the physical host (X3) before any registry request.
///
/// Split out of [`observe_curated`] because the observe loop is no longer the
/// only thing that talks to the registry: `--tags-from-registry` lists tags
/// first, so a pre-flight living inside the observe loop would leave that
/// listing unguarded. One `Physical` is resolved once and threaded to both, so
/// the guarded target and the requested target cannot diverge.
///
/// # Errors
///
/// [`AnnounceError::RootMissingField`] / [`AnnounceError::MalformedPhysicalRepository`]
/// if the root's `repository` pointer is absent or unparseable;
/// [`AnnounceError::Ssrf`] if the host is forbidden or unresolvable.
pub async fn guarded_physical(root: &Value, trusted_hosts: &[String]) -> Result<Physical, AnnounceError> {
    let physical = extract_physical(root)?;
    oci::ssrf::resolve_and_validate(&physical.host, physical.port, trusted_hosts).await?;
    Ok(physical)
}

/// Observe every curated tag against the physical repository.
///
/// `physical` must come from [`guarded_physical`] — this function makes registry
/// requests and runs no pre-flight of its own.
///
/// # Errors
///
/// [`AnnounceError::UnresolvedTag`] if a curated tag does not resolve (a
/// publisher typo); or an [`AnnounceError::Observe`] transport failure.
pub async fn observe_curated(
    publisher: &Publisher,
    physical: &Physical,
    curated: &[String],
) -> Result<Vec<Observed>, AnnounceError> {
    let mut observed = Vec::with_capacity(curated.len());
    for tag in curated {
        observed.push(observe_one_tag(publisher, physical, tag).await?);
    }
    Ok(observed)
}

/// Observe a single curated tag: fetch what the registry serves and keep it.
///
/// The bytes and the digest ride out of here untouched — the index stores the
/// publisher's own image index, it does not derive a second document from it.
/// The one judgement made here is document kind: the index records image
/// indices only, so a bare image manifest is refused (D4(a)).
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
    let Some((bytes, content, manifest)) = fetched else {
        // A curated tag that does not resolve is a publisher typo — hard error,
        // never a silent drop (reference parity).
        return Err(AnnounceError::UnresolvedTag {
            tag: tag.to_string(),
            repository: physical.display.clone(),
        });
    };
    if !matches!(manifest, oci::Manifest::ImageIndex(_)) {
        return Err(AnnounceError::TagIsNotAnImageIndex {
            tag: tag.to_string(),
            repository: physical.display.clone(),
        });
    }
    Ok(Observed {
        tag: tag.to_string(),
        content,
        bytes,
    })
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
        let content = entry.content.to_string();
        let committed_entry = committed_tags.and_then(|tags| tags.get(&entry.tag));
        let committed_content = committed_entry
            .and_then(|committed| committed.get("content"))
            .and_then(Value::as_str);
        let regenerated = if committed_content == Some(content.as_str()) {
            // Unmoved digest — carry the committed entry verbatim (no churn).
            committed_entry
                .cloned()
                .unwrap_or_else(|| new_tag_entry(&content, now, None))
        } else {
            // New or changed digest — fresh timestamp, keep any yank marker.
            let yanked = committed_entry.and_then(|committed| committed.get("yanked")).cloned();
            new_tag_entry(&content, now, yanked)
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
        .filter(|entry| !committed_contents.contains(entry.content.to_string().as_str()))
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
        let (algorithm, hex) = entry.content.parts();
        files.insert(
            format!("p/{package_repo}/o/{algorithm}/{hex}.json"),
            entry.bytes.clone(),
        );
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
    use crate::oci::index::serialize_root;

    // ── fixtures ─────────────────────────────────────────────────────────────

    fn digest_string(fill: char) -> String {
        format!("sha256:{}", fill.to_string().repeat(64))
    }

    /// A 64-hex `sha256.<hex>` canonical tag — reserved, and the D7 case a
    /// default `ocx package push` writes into every repository.
    fn canonical_tag() -> String {
        format!("sha256.{}", "a".repeat(64))
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

    /// Build an `Observed` for `tag` from an image index carrying a single
    /// platform leaf `digest`, exactly as a registry would serve it: the bytes
    /// are the serialized index and `content` is their real digest, so digest
    /// comparisons behave as in production.
    fn observed(tag: &str, leaf: char) -> Observed {
        let bytes = serde_json::to_vec(&image_index(vec![index_entry("amd64", leaf)])).expect("index serializes");
        let content = oci::Algorithm::Sha256.hash(&bytes);
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

    #[test]
    fn union_file_adds_to_the_committed_set_preserving_order() {
        let committed = vec!["1.0.0".to_string(), "2.0.0".to_string()];
        let curated = resolve_no_discovery(
            &TagSelection::UnionFile(vec!["3.0.0".into(), "1.0.0".into()]),
            &committed,
        )
        .unwrap();
        // Committed order first, then the genuinely new file tag; the duplicate
        // `1.0.0` is not re-added.
        assert_eq!(
            curated.tags,
            vec!["1.0.0".to_string(), "2.0.0".to_string(), "3.0.0".to_string()]
        );
    }

    #[test]
    fn refresh_re_observes_the_committed_set_in_order() {
        let committed = vec!["1.0.0".to_string(), "latest".to_string()];
        let curated = resolve_no_discovery(&TagSelection::Refresh, &committed).unwrap();
        assert_eq!(curated.tags, committed);
    }

    #[test]
    fn empty_curated_set_is_an_error() {
        assert!(matches!(
            resolve_no_discovery(&TagSelection::Replace(vec![]), &[]),
            Err(AnnounceError::NoCuratedTags { ref reserved_dropped }) if reserved_dropped.is_empty()
        ));
        assert!(matches!(
            resolve_no_discovery(&TagSelection::Refresh, &[]),
            Err(AnnounceError::NoCuratedTags { ref reserved_dropped }) if reserved_dropped.is_empty()
        ));
    }

    // ── the D7 reserved-tag filter, one site, all three selections ───────────

    /// Explicit curation of a reserved tag is a drop, not a refusal: a reserved
    /// tag is not a version, so there is nothing to refuse, and refusing would
    /// make announce police how a publisher tags their own repository.
    #[test]
    fn resolve_curated_tags_drops_reserved_from_replace() {
        let canonical = canonical_tag();
        let curated = resolve_no_discovery(
            &TagSelection::Replace(vec![
                "__ocx.desc".into(),
                "__ocx".into(),
                "__ocxfoo".into(),
                "__OCX.desc".into(),
                canonical.clone(),
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
                canonical,
            ],
            "every reserved form is reported, in selection order"
        );
    }

    /// The carrier case: `--refresh` carries no tags of its own, so a reserved
    /// tag already sitting in the committed root would be re-announced forever
    /// if the filter lived at the selection sources instead of here.
    #[test]
    fn resolve_curated_tags_drops_reserved_from_refresh_carrier() {
        let committed = vec!["1.0.0".to_string(), "__ocx.desc".to_string(), canonical_tag()];
        let curated = resolve_no_discovery(&TagSelection::Refresh, &committed).unwrap();
        assert_eq!(curated.tags, vec!["1.0.0".to_string()]);
        assert_eq!(
            curated.reserved_dropped,
            vec!["__ocx.desc".to_string(), canonical_tag()]
        );
    }

    /// `--tags-from-file` contributes additions only; the committed base arrives
    /// separately. Both halves pass through the one filter.
    #[test]
    fn resolve_curated_tags_drops_reserved_from_union_file() {
        let committed = vec!["1.0.0".to_string(), "__ocx.desc".to_string()];
        let curated = resolve_no_discovery(
            &TagSelection::UnionFile(vec![canonical_tag(), "2.0.0".into()]),
            &committed,
        )
        .unwrap();
        assert_eq!(curated.tags, vec!["1.0.0".to_string(), "2.0.0".to_string()]);
        assert_eq!(
            curated.reserved_dropped,
            vec!["__ocx.desc".to_string(), canonical_tag()],
            "a reserved tag is dropped whether it came from the root or the file"
        );
    }

    /// An entirely reserved selection is the empty-set case — it collapses into
    /// the existing `NoCuratedTags`, so no new error variant exists to add.
    /// The dropped names ride out on the variant: this is the one D7 path with
    /// no outcome for the CLI's drop notice to read.
    #[test]
    fn resolve_curated_tags_all_reserved_is_no_curated_tags() {
        let Err(AnnounceError::NoCuratedTags { reserved_dropped }) =
            resolve_no_discovery(&TagSelection::Replace(vec!["__ocx.desc".into(), canonical_tag()]), &[])
        else {
            panic!("an entirely reserved selection resolves to nothing");
        };
        assert_eq!(reserved_dropped, vec!["__ocx.desc".to_string(), canonical_tag()]);

        let Err(AnnounceError::NoCuratedTags { reserved_dropped }) =
            resolve_no_discovery(&TagSelection::Refresh, &["__ocx.patch".to_string()])
        else {
            panic!("an entirely reserved committed set resolves to nothing");
        };
        assert_eq!(reserved_dropped, vec!["__ocx.patch".to_string()]);
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

    // ── observe_one_tag — verbatim bytes, and the D4(a) refusal ──────────────

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

    /// Seed the stub with `manifest` served at `127.0.0.1/x:<tag>` and hand
    /// back the exact bytes and digest the registry would answer with.
    ///
    /// Deliberately pretty-printed: a registry serves whatever encoding the
    /// publisher pushed, not serde's canonical one. Compact bytes here would
    /// make a re-serializing implementation byte-indistinguishable from one
    /// that carries the served bytes through, and the verbatim assertions
    /// below would pass vacuously.
    fn seed_manifest(data: &StubTransportData, tag: &str, manifest: &oci::Manifest) -> (Vec<u8>, oci::Digest) {
        let bytes = serde_json::to_vec_pretty(manifest).expect("manifest serializes");
        let digest = oci::Algorithm::Sha256.hash(&bytes);
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
        let physical = extract_physical(&committed_root("oci://127.0.0.1/x")).unwrap();

        let observed = observe_one_tag(&publisher, &physical, "1.0.0").await.unwrap();

        assert_eq!(observed.bytes, served_bytes, "the CAS payload must be the served bytes");
        assert_eq!(observed.content, served_digest, "the pointer must be the served digest");
        let files = build_files("p/x.json", b"root", "x", std::slice::from_ref(&observed));
        let (algorithm, hex) = served_digest.parts();
        assert_eq!(
            files.get(&format!("p/x/o/{algorithm}/{hex}.json")).map(Vec::as_slice),
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
        seed_manifest(&data, "1.0.0", &oci::Manifest::Image(oci::ImageManifest::default()));
        let publisher = stub_publisher(&data);
        let physical = extract_physical(&committed_root("oci://127.0.0.1/x")).unwrap();

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
        let physical = extract_physical(&committed_root("oci://127.0.0.1/x")).unwrap();

        let observed = observe_one_tag(&publisher, &physical, "1.0.0").await.unwrap();

        assert_eq!(observed.bytes, served_bytes, "no descriptor is dropped on the way in");
    }

    // ── regenerate (C6 no-churn) ─────────────────────────────────────────────

    #[test]
    fn regenerate_keeps_the_observed_timestamp_for_an_unmoved_digest() {
        let root = committed_root("oci://ghcr.io/x/y");
        // Splice the committed tag's content to match what was observed so the
        // "unchanged" path fires.
        let entry = observed("1.0.0", 'z');
        let mut committed = root;
        committed["tags"]["1.0.0"]["content"] = Value::String(entry.content.to_string());
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
        // The committed `1.0.0` content is `sha256:aaaa…`; the observed digest
        // differs, so the tag is treated as changed.
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
                "1.0.0": { "content": entry.content.to_string(), "observed": "2026-01-01T00:00:00Z" }
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
        let hex = entry.content.hex().to_string();
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
    async fn ssrf_pre_flight_refuses_a_forbidden_host() {
        // A committed root pointing at loopback must abort at the pre-flight (X3)
        // before any registry request is even constructible: `guarded_physical` is
        // the only thing that yields the `Physical` both the tag listing and the
        // observe loop need.
        let root = committed_root("oci://127.0.0.1/x");
        assert!(
            matches!(guarded_physical(&root, &[]).await, Err(AnnounceError::Ssrf(_))),
            "forbidden host must be refused"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ssrf_pre_flight_allows_a_trusted_forbidden_host() {
        // The trusted_hosts escape hatch (X2) lets a loopback registry through;
        // the stub then answers with no manifest, surfacing UnresolvedTag — proof
        // the observe loop ran only *after* the pre-flight passed.
        let data = StubTransportData::new();
        let publisher = stub_publisher(&data);
        let root = committed_root("oci://127.0.0.1/x");
        let physical = guarded_physical(&root, &["127.0.0.1".to_string()])
            .await
            .expect("a trusted loopback host passes the pre-flight");
        let result = observe_curated(&publisher, &physical, &["1.0.0".to_string()]).await;
        assert!(
            matches!(result, Err(AnnounceError::UnresolvedTag { .. })),
            "a trusted host proceeds to observe; the empty stub yields UnresolvedTag"
        );
    }

    // ── list_registry_tags (--tags-from-registry source) ─────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn list_registry_tags_drops_reserved_at_the_source() {
        // Canonical `sha256.<hex>` tags are pushed by default, so a registry
        // listing carries one per published version. They are filtered here and
        // never reported: `reserved_dropped` answers "what did the CALLER name
        // that is not a version", and a listing names nothing.
        let data = StubTransportData::new();
        data.write().tags = vec![vec![
            "1.0.0".to_string(),
            canonical_tag(),
            "__ocx.desc".to_string(),
            "latest".to_string(),
        ]];
        let publisher = stub_publisher(&data);
        let physical = extract_physical(&committed_root("oci://127.0.0.1/x")).expect("root parses");

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

    /// D1: additive only. A committed tag the registry no longer serves is
    /// **kept** — the index's own reconcile treats a vanished non-yanked tag as
    /// an anomaly for a human, so dropping it here would silently pre-empt that.
    #[test]
    fn from_registry_never_drops_a_committed_tag_the_registry_lacks() {
        let committed = vec!["1.0.0".to_string(), "0.9.0".to_string()];
        let discovered = vec!["1.0.0".to_string()];
        let curated = resolve_curated_tags(&TagSelection::FromRegistry, &committed, &discovered).unwrap();
        assert_eq!(curated.tags, committed, "0.9.0 survives its absence from the registry");
    }

    /// The registry is not consulted for any other selection, so `discovered`
    /// arrives empty and must not leak into the resolved set.
    #[test]
    fn discovered_tags_are_ignored_by_the_caller_supplied_selections() {
        let committed = vec!["1.0.0".to_string()];
        let discovered = vec!["9.9.9".to_string()];
        for selection in [
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            TagSelection::UnionFile(vec![]),
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
}
