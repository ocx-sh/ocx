// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The pure, I/O-free core of `ocx package cascade check|repair`: functions of a
//! [`TagGraphObservation`] alone.
//!
//! [`alias_chain`] derives each chain from the push path's
//! [`decompose`](super::decompose) algebra, so check cannot drift from push.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Serialize, Serializer};

use crate::cascade::decompose_targets;
use crate::tag::Tag;
use crate::version::Version;
use ocx_oci::{self, native};
use ocx_oci::{
    media_type::MEDIA_TYPE_OCI_IMAGE_INDEX, media_type::MEDIA_TYPE_OCI_IMAGE_MANIFEST,
    media_type::MEDIA_TYPE_PACKAGE_V1,
};
use ocx_util::wire_words;

#[cfg(test)]
mod tests;

/// The expected content of the whole graph, per alias tag and platform.
pub type ExpectedGraph = BTreeMap<AliasTag, BTreeMap<native::Platform, ExpectedSlot>>;

/// A tag that carries content on behalf of a whole subtree rather than one
/// concrete build: a track root, or the [`Version`] it rolls.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AliasTag {
    /// The root of one track: `None` is the default track's `latest`, `Some`
    /// is a variant track's bare name.
    Root { variant: Option<String> },
    /// Any other alias, named by the version it rolls.
    Version(Version),
}

impl AliasTag {
    /// Classifies a registry tag as a graph node, or `None` for a reserved or
    /// non-version tag.
    ///
    /// A bare name is a track root only when `variants`
    /// ([`variant_names`](crate::version::variant_names)) lists it.
    pub fn parse(tag: &str, variants: &[String]) -> Option<Self> {
        match Tag::from(tag.to_string()) {
            Tag::Latest => Some(AliasTag::Root { variant: None }),
            Tag::Version(version) => Some(AliasTag::Version(version)),
            Tag::Other(name) if variants.iter().any(|variant| variant == &name) => {
                Some(AliasTag::Root { variant: Some(name) })
            }
            _ => None,
        }
    }

    /// `None` for the default track.
    pub fn track(&self) -> Option<&str> {
        match self {
            AliasTag::Root { variant } => variant.as_deref(),
            AliasTag::Version(version) => version.variant(),
        }
    }
}

impl fmt::Display for AliasTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AliasTag::Root { variant: None } => formatter.write_str("latest"),
            AliasTag::Root { variant: Some(variant) } => formatter.write_str(variant),
            AliasTag::Version(version) => write!(formatter, "{version}"),
        }
    }
}

/// Serializes as the registry tag itself.
impl Serialize for AliasTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// Hand-written: a derive would publish a tagged union, but `Serialize` emits the tag string.
impl schemars::JsonSchema for AliasTag {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AliasTag".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "The registry tag itself, e.g. latest, 3, 3.28, 3.28.1, or a variant name.",
        })
    }
}

/// Everything one gather pass read about one package; a tag missing from
/// [`Self::tags`] reads as never created.
#[derive(Clone, Debug)]
pub struct TagGraphObservation {
    /// The physical repository every tag below was read from.
    pub identifier: ocx_oci::OciIdentifier,
    /// The logical name the user asked for, when it differed; `Some` turns the index layer on.
    pub logical: Option<ocx_oci::PackageRef>,
    pub tags: BTreeMap<AliasTag, ObservedTag>,
    /// Tags deliberately not part of the graph.
    pub ignored_tags: Vec<String>,
    /// The live index root; `None` for a physical invocation.
    pub index_root: Option<ocx_index::IndexRoot>,
}

/// One tag exactly as the registry served it.
#[derive(Clone, Debug)]
pub struct ObservedTag {
    pub digest: ocx_oci::Digest,
    /// The served body's length, never a re-serialization's, or a wrapped
    /// bare manifest's descriptor becomes a broken pointer.
    pub size: i64,
    pub manifest: ocx_oci::Manifest,
}

impl TagGraphObservation {
    /// The concrete versions the graph is folded from.
    pub fn versions(&self) -> BTreeSet<Version> {
        self.tags
            .keys()
            .filter_map(|tag| match tag {
                AliasTag::Version(version) => Some(version.clone()),
                AliasTag::Root { .. } => None,
            })
            .collect()
    }

    fn index(&self, tag: &AliasTag) -> Option<&ocx_oci::ImageIndex> {
        match &self.tags.get(tag)?.manifest {
            ocx_oci::Manifest::ImageIndex(index) => Some(index),
            ocx_oci::Manifest::Image(_) => None,
        }
    }
}

/// What one alias should carry for one platform: a verbatim copy of `source`'s
/// own index entry, so a repair never composes a new descriptor.
#[derive(Clone, Debug)]
pub struct ExpectedSlot {
    pub entry: ocx_oci::ImageIndexEntry,
    pub source: Version,
}

wire_words! {
    /// The verdict for one (alias, platform) slot.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
    pub enum SlotStatus {
        /// The alias carries exactly the expected entry.
        Ok = "ok",
        /// The alias should carry a platform it does not carry at all.
        Missing = "missing",
        /// The alias carries this platform, but not the expected content.
        Stale = "stale",
        /// The alias carries a platform nothing folds into it — a leftover from
        /// an earlier cascade. Reported always; removed only when the entry it
        /// points at no longer exists.
        Orphan = "orphan",
        /// The alias carries more than one entry for this platform. Only the last
        /// one resolves, so a shadowed entry is invisible to every consumer while
        /// still being published — including when the surviving one is exactly what
        /// the fold expects. One row per shadowed entry, beside the row for the
        /// entry that wins.
        Duplicate = "duplicate",
    }
}

/// One row of the diff: what one alias holds for one platform versus what the
/// fold says it should hold.
///
/// Digests are the wire strings verbatim, an algorithm this build does not
/// implement included.
// Strings, not parsed digests: an index may legitimately name an algorithm this build lacks.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct SlotRow {
    /// The alias tag this slot belongs to.
    pub tag: AliasTag,
    /// The slot's platform.
    // Kept native, not `ocx_oci::Platform`: a registry may carry a platform this build does not
    // support, and both serialize as the same OCI object.
    #[schemars(with = "ocx_oci::Platform")]
    pub platform: native::Platform,
    /// The slot's verdict.
    pub status: SlotStatus,
    /// The digest the alias carries for this platform; absent when it carries none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    /// The digest the fold expects; absent when it expects none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// The version the expectation was folded from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Version>,
    /// The version the observed digest belongs to, when the observed content
    /// is recognisable as some published version's — absent when the alias
    /// points at content no observed leaf carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_source: Option<Version>,
}

/// What the registry holds at an alias tag, taken as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AliasState {
    /// An image index — the shape every alias is supposed to have.
    Present,
    /// No such tag at the registry: the alias was never created.
    Absent,
    /// The tag exists but resolves to a bare image manifest, so it has no
    /// per-platform slots at all and every expected platform reads as missing.
    NotAnIndex {
        /// The bare manifest the tag resolves to.
        digest: ocx_oci::Digest,
    },
}

/// A disagreement between the registry graph and the public index that
/// publishes it.
///
/// Only ever produced for a logical identifier: a physical repository has no
/// reverse mapping back to an index root, so the layer is skipped entirely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum IndexFinding {
    /// The index committed a different digest than the alias points at today.
    /// Repair cannot fix this — announcing the tag can.
    Stale {
        /// The alias tag.
        tag: AliasTag,
        /// The digest the index committed.
        committed: ocx_oci::Digest,
        /// The digest the alias points at today.
        live: ocx_oci::Digest,
    },
    /// The registry carries the alias and the index has never recorded it.
    NotCommitted {
        /// The alias tag.
        tag: AliasTag,
    },
}

/// Something check found that no repair can fix without new content being
/// published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Unrepairable {
    /// A planned entry points at a child manifest the registry no longer
    /// holds, so writing the alias would publish a dangling pointer.
    ChildManifestMissing {
        /// The alias tag.
        tag: AliasTag,
        /// The missing child manifest.
        digest: ocx_oci::Digest,
    },
    /// A planned entry names a digest algorithm this build cannot address, so
    /// whether the child is still there could not be checked at all. Distinct
    /// from `child_manifest_missing`: nothing was observed to be gone —
    /// the alias is refused because the check could not be made.
    ChildDigestUnaddressable {
        /// The alias tag.
        tag: AliasTag,
        /// The child digest exactly as the index writes it; no `Digest`, since this build cannot parse it.
        #[serde(rename = "digest_text")]
        digest: String,
    },
    /// Repairing the alias would leave it with no entries at all. Refused:
    /// an empty index is worse than a stale one.
    WouldEmptyIndex {
        /// The alias tag.
        tag: AliasTag,
    },
}

// Also the input `plan_repairs` works from.
/// The whole finding set for one package — the value both `check` and `repair`
/// report.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct CascadeReport {
    /// The physical repository the graph was read from.
    // Same `registry/repository[:tag][@digest]` string a package reference writes.
    #[schemars(with = "ocx_oci::PackageRef")]
    pub identifier: ocx_oci::OciIdentifier,
    /// The logical name the user asked for, when it differed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical: Option<ocx_oci::PackageRef>,
    /// Per alias, what the registry holds as a whole.
    pub aliases: BTreeMap<AliasTag, AliasState>,
    /// Per-slot rows, sorted by (tag, platform). Includes `Ok` rows so a
    /// reader sees the whole graph, not only its damage.
    pub rows: Vec<SlotRow>,
    /// Registry-versus-index findings; always empty for a physical identifier.
    pub index_findings: Vec<IndexFinding>,
    /// Tags excluded from the graph, verbatim.
    pub ignored_tags: Vec<String>,
    /// Findings that need new content published before they can be fixed.
    pub unrepairable: Vec<Unrepairable>,
}

impl CascadeReport {
    /// Whether anything needs attention: a non-`Ok` row, a non-present alias, or an index finding.
    pub fn has_findings(&self) -> bool {
        self.rows.iter().any(|row| row.status != SlotStatus::Ok)
            || self.aliases.values().any(|state| state != &AliasState::Present)
            || !self.index_findings.is_empty()
    }
}

/// One alias index to write, recomputed whole.
///
/// A repair never patches an alias in place: it rebuilds the entire index from
/// content the registry already serves and PUTs it, so the write is idempotent
/// and a partially-applied run leaves every untouched alias byte-identical.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct PlannedWrite {
    /// The alias tag to write.
    pub tag: AliasTag,
    /// The complete index to PUT at `tag`.
    // `OciImageIndex` comes from `oci_client` and cannot carry our derive. The
    // OCI image index is a registry-defined document, not part of the contract
    // this schema publishes, so it is described as free-form JSON.
    #[schemars(with = "serde_json::Value")]
    pub index: ocx_oci::ImageIndex,
    /// The digest the alias points at now — absent when the alias does not
    /// exist yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_digest: Option<ocx_oci::Digest>,
    /// Every child manifest digest `index` references, deduped. Apply
    /// preflights each one so a missing child refuses the alias instead of
    /// publishing a dangling pointer.
    pub referenced_digests: Vec<String>,
    /// The rows that justify the write — what the user is being told changed.
    ///
    /// Includes the alias's `orphan` rows even though an orphan never causes a
    /// write on its own.
    // They are how apply tells a preserved leftover from a folded entry when
    // preflight finds a child gone, and so which entry it may drop instead of
    // refusing the whole alias.
    pub reasons: Vec<SlotRow>,
}

/// Why a requested scope names no node of this package's tag graph; a usage fault.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeError {
    #[error("tag '{0}' is not a version tag")]
    NotAVersionTag(String),
    #[error("a digest reference names one manifest, not a tag graph")]
    DigestReference,
    #[error("tag '{0}' is not in this package's tag graph")]
    UnknownTag(String),
}

/// Every rolling tag that should carry `version`'s content when nothing newer
/// blocks it, from the push path's [`decompose_targets`](super::decompose_targets).
pub fn alias_chain(version: &Version, _others: &BTreeSet<Version>) -> Vec<AliasTag> {
    let targets = decompose_targets(version);
    let mut chain: Vec<AliasTag> = targets.targets.into_iter().map(AliasTag::Version).collect();
    if targets.latest_eligible {
        chain.push(AliasTag::Root {
            variant: version.variant().map(str::to_string),
        });
    }
    chain
}

/// Every alias any observed version cascades into; a version not in it is a leaf.
fn alias_universe(versions: &BTreeSet<Version>) -> BTreeSet<AliasTag> {
    versions
        .iter()
        .flat_map(|version| alias_chain(version, versions))
        .collect()
}

/// The versions in nobody's alias chain, the only sources for a fold; ascending,
/// so the fold's last write is its max-selection.
fn leaves(versions: &BTreeSet<Version>) -> Vec<&Version> {
    let universe = alias_universe(versions);
    versions
        .iter()
        .filter(|version| !universe.contains(&AliasTag::Version((*version).clone())))
        .collect()
}

/// The platform an entry claims a slot for; `None` only for no platform or the
/// `unknown/unknown` attestation placeholder.
///
/// An unmodellable platform is still a slot, or the fold plans a real entry's deletion.
fn platform_slot(entry: &ocx_oci::ImageIndexEntry) -> Option<&native::Platform> {
    let platform = entry.platform.as_ref()?;
    let unknown = platform.os.to_string() == "unknown" && platform.architecture.to_string() == "unknown";
    (!unknown).then_some(platform)
}

/// Whether two descriptors match in every field, not just the digest: a repair
/// PUTs whole entries, so a narrower diff would silently change a "clean" one.
fn same_descriptor(left: &ocx_oci::ImageIndexEntry, right: &ocx_oci::ImageIndexEntry) -> bool {
    left.digest == right.digest
        && left.size == right.size
        && left.media_type == right.media_type
        && left.artifact_type == right.artifact_type
        && left.annotations == right.annotations
        && left.platform == right.platform
}

/// The platform-slot entries of one alias, keyed by platform, plus the entries
/// a later one shadows, which the diff must report or they hide behind a clean slot.
fn observed_slots<'a>(
    observation: &'a TagGraphObservation,
    alias: &AliasTag,
) -> (
    BTreeMap<&'a native::Platform, &'a ocx_oci::ImageIndexEntry>,
    Vec<(&'a native::Platform, &'a ocx_oci::ImageIndexEntry)>,
) {
    let mut slots = BTreeMap::new();
    let mut shadowed = Vec::new();
    let Some(index) = observation.index(alias) else {
        return (slots, shadowed);
    };
    for entry in &index.manifests {
        let Some(platform) = platform_slot(entry) else {
            continue;
        };
        if let Some(previous) = slots.insert(platform, entry) {
            shadowed.push((platform, previous));
        }
    }
    (slots, shadowed)
}

/// Which leaf version a digest observed at an alias came from; the greatest
/// leaf wins on byte-identical content.
fn leaf_provenance(observation: &TagGraphObservation) -> BTreeMap<(native::Platform, String), Version> {
    let versions = observation.versions();
    let mut provenance = BTreeMap::new();
    for leaf in leaves(&versions) {
        let Some(index) = observation.index(&AliasTag::Version(leaf.clone())) else {
            continue;
        };
        for entry in &index.manifests {
            let Some(platform) = platform_slot(entry) else {
                continue;
            };
            provenance.insert((platform.clone(), entry.digest.clone()), leaf.clone());
        }
    }
    provenance
}

/// Folds every observed version into the state each alias should be in: per
/// platform, the entry of the greatest version that cascades there and ships it.
pub fn fold_expected(observation: &TagGraphObservation) -> ExpectedGraph {
    let versions = observation.versions();
    let mut expected = ExpectedGraph::new();

    // Only leaves: folding an alias's own copy would let one broken alias justify the next.
    for leaf in leaves(&versions) {
        let Some(index) = observation.index(&AliasTag::Version(leaf.clone())) else {
            continue;
        };
        let chain = alias_chain(leaf, &versions);
        if chain.is_empty() {
            continue;
        }
        for entry in &index.manifests {
            let Some(platform) = platform_slot(entry) else {
                continue;
            };
            for alias in &chain {
                expected.entry(alias.clone()).or_default().insert(
                    platform.clone(),
                    ExpectedSlot {
                        entry: entry.clone(),
                        source: leaf.clone(),
                    },
                );
            }
        }
    }
    expected
}

/// Diffs what the registry holds against what the fold expects, restricted to `scope`.
pub fn diff(observation: &TagGraphObservation, expected: &ExpectedGraph, scope: &BTreeSet<AliasTag>) -> CascadeReport {
    let provenance = leaf_provenance(observation);
    let no_slots = BTreeMap::new();

    let mut aliases = BTreeMap::new();
    let mut rows = Vec::new();
    let mut unrepairable = Vec::new();

    for alias in scope {
        let observed = observation.tags.get(alias);
        let state = match observed {
            None => AliasState::Absent,
            Some(tag) => match &tag.manifest {
                ocx_oci::Manifest::ImageIndex(_) => AliasState::Present,
                ocx_oci::Manifest::Image(_) => AliasState::NotAnIndex {
                    digest: tag.digest.clone(),
                },
            },
        };

        let wanted = expected.get(alias).unwrap_or(&no_slots);
        let (held, shadowed) = observed_slots(observation, alias);
        let platforms: BTreeSet<&native::Platform> = wanted.keys().chain(held.keys().copied()).collect();

        let mut alias_rows: Vec<SlotRow> = platforms
            .into_iter()
            .filter_map(|platform| {
                let want = wanted.get(platform);
                let have = held.get(platform).copied();
                let status = match (want, have) {
                    (Some(want), Some(have)) if same_descriptor(&want.entry, have) => SlotStatus::Ok,
                    (Some(_), Some(_)) => SlotStatus::Stale,
                    (Some(_), None) => SlotStatus::Missing,
                    (None, Some(_)) => SlotStatus::Orphan,
                    (None, None) => return None,
                };
                Some(SlotRow {
                    tag: alias.clone(),
                    platform: platform.clone(),
                    status,
                    observed: have.map(|entry| entry.digest.clone()),
                    expected: want.map(|slot| slot.entry.digest.clone()),
                    source: want.map(|slot| slot.source.clone()),
                    observed_source: have
                        .and_then(|entry| provenance.get(&(platform.clone(), entry.digest.clone())))
                        .cloned(),
                })
            })
            .collect();

        alias_rows.extend(shadowed.iter().map(|(platform, entry)| SlotRow {
            tag: alias.clone(),
            platform: (*platform).clone(),
            status: SlotStatus::Duplicate,
            observed: Some(entry.digest.clone()),
            expected: wanted.get(*platform).map(|slot| slot.entry.digest.clone()),
            source: wanted.get(*platform).map(|slot| slot.source.clone()),
            observed_source: provenance.get(&((*platform).clone(), entry.digest.clone())).cloned(),
        }));
        alias_rows.sort_by(|left, right| {
            (&left.platform, left.status, &left.observed).cmp(&(&right.platform, right.status, &right.observed))
        });

        if needs_write(&state, &alias_rows) && planned_entries(alias, observation, expected).is_empty() {
            unrepairable.push(Unrepairable::WouldEmptyIndex { tag: alias.clone() });
        }

        aliases.insert(alias.clone(), state);
        rows.extend(alias_rows);
    }

    CascadeReport {
        identifier: observation.identifier.clone(),
        logical: observation.logical.clone(),
        index_findings: index_findings(observation, scope),
        aliases,
        rows,
        ignored_tags: observation.ignored_tags.clone(),
        unrepairable,
    }
}

/// What the public index committed for each alias against what it points at
/// now; empty without a root.
fn index_findings(observation: &TagGraphObservation, scope: &BTreeSet<AliasTag>) -> Vec<IndexFinding> {
    let Some(root) = observation.index_root.as_ref() else {
        return Vec::new();
    };
    scope
        .iter()
        .filter_map(|alias| {
            let live = observation.tags.get(alias)?.digest.clone();
            match root.tags.get(&alias.to_string()) {
                None => Some(IndexFinding::NotCommitted { tag: alias.clone() }),
                Some(committed) if committed.content != live => Some(IndexFinding::Stale {
                    tag: alias.clone(),
                    committed: committed.content.clone(),
                    live,
                }),
                Some(_) => None,
            }
        })
        .collect()
}

/// Whether an alias needs its index rewritten; an orphan alone is reported and left alone.
fn needs_write(state: &AliasState, rows: &[SlotRow]) -> bool {
    state != &AliasState::Present
        || rows.iter().any(|row| {
            matches!(
                row.status,
                SlotStatus::Missing | SlotStatus::Stale | SlotStatus::Duplicate
            )
        })
}

/// The complete `manifests` list one alias should carry: platform entries in
/// platform order, then platform-less ones as served, each a verbatim copy.
fn planned_entries(
    alias: &AliasTag,
    observation: &TagGraphObservation,
    expected: &ExpectedGraph,
) -> Vec<ocx_oci::ImageIndexEntry> {
    let mut slots: BTreeMap<native::Platform, ocx_oci::ImageIndexEntry> = expected
        .get(alias)
        .into_iter()
        .flatten()
        .map(|(platform, slot)| (platform.clone(), slot.entry.clone()))
        .collect();

    let observed = observation.tags.get(alias);
    let mut unplatformed: Vec<ocx_oci::ImageIndexEntry> = Vec::new();
    match observed.map(|tag| &tag.manifest) {
        Some(ocx_oci::Manifest::ImageIndex(index)) => {
            for entry in &index.manifests {
                match platform_slot(entry) {
                    // Orphans are preserved, so a truncated tag listing never becomes a deletion.
                    Some(platform) => {
                        slots.entry(platform.clone()).or_insert_with(|| entry.clone());
                    }
                    None => unplatformed.push(entry.clone()),
                }
            }
        }
        // As the push path's merge does: a bare manifest becomes a platform-less entry, never dropped.
        Some(ocx_oci::Manifest::Image(_)) => {
            if let Some(tag) = observed {
                unplatformed.push(ocx_oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: tag.digest.to_string(),
                    size: tag.size,
                    platform: None,
                    artifact_type: None,
                    annotations: None,
                });
            }
        }
        None => {}
    }

    let mut entries: Vec<ocx_oci::ImageIndexEntry> = slots.into_values().collect();
    entries.extend(unplatformed);
    entries
}

/// Turns a report into the index writes that would resolve it; an alias that
/// would end up empty is left out.
pub fn plan_repairs(
    report: &CascadeReport,
    observation: &TagGraphObservation,
    expected: &ExpectedGraph,
) -> Vec<PlannedWrite> {
    let mut writes = Vec::new();

    let mut by_alias: BTreeMap<&AliasTag, Vec<SlotRow>> = BTreeMap::new();
    for row in report.rows.iter().filter(|row| row.status != SlotStatus::Ok) {
        by_alias.entry(&row.tag).or_default().push(row.clone());
    }

    for (alias, state) in &report.aliases {
        let reasons = by_alias.remove(alias).unwrap_or_default();
        if !needs_write(state, &reasons) {
            continue;
        }

        // `diff` already recorded this as `WouldEmptyIndex`.
        let manifests = planned_entries(alias, observation, expected);
        if manifests.is_empty() {
            continue;
        }

        let observed = observation.tags.get(alias);
        let observed_index = observation.index(alias);
        let referenced_digests: BTreeSet<String> = manifests.iter().map(|entry| entry.digest.clone()).collect();

        writes.push(PlannedWrite {
            tag: alias.clone(),
            index: ocx_oci::ImageIndex {
                schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
                // Preserved verbatim, even when absent; only a fresh index states its own.
                media_type: match observed_index {
                    Some(index) => index.media_type.clone(),
                    None => Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                },
                // Filled if absent, never overwritten: never relabel someone else's artifact.
                artifact_type: observed_index
                    .and_then(|index| index.artifact_type.clone())
                    .or_else(|| Some(MEDIA_TYPE_PACKAGE_V1.to_string())),
                manifests,
                annotations: observed_index.and_then(|index| index.annotations.clone()),
            },
            observed_digest: observed.map(|tag| tag.digest.clone()),
            referenced_digests: referenced_digests.into_iter().collect(),
            reasons,
        });
    }

    writes
}

/// Reads the scope request out of an identifier; `None` scopes the whole graph.
///
/// # Errors
///
/// [`ScopeError::DigestReference`] for a digest-pinned identifier and
/// [`ScopeError::NotAVersionTag`] for a tag that names no version.
pub fn scope_request(identifier: &ocx_oci::PackageRef) -> Result<Option<AliasTag>, ScopeError> {
    if identifier.digest().is_some() {
        return Err(ScopeError::DigestReference);
    }
    let Some(tag) = identifier.tag() else {
        return Ok(None);
    };
    match Tag::from(tag.to_string()) {
        Tag::Latest => Ok(Some(AliasTag::Root { variant: None })),
        Tag::Version(version) => Ok(Some(AliasTag::Version(version))),
        // A bare variant name is refused: whether it is a root depends on tags not read here.
        _ => Err(ScopeError::NotAVersionTag(tag.to_string())),
    }
}

/// Expands requests into the alias tags to act on: an alias's subtree plus its
/// path to the track root, a leaf's path alone; `None` means the whole graph.
///
/// # Errors
///
/// [`ScopeError::UnknownTag`] when a requested version is not in `versions`.
pub fn scope_filter(
    versions: &BTreeSet<Version>,
    requests: &[Option<AliasTag>],
) -> Result<BTreeSet<AliasTag>, ScopeError> {
    let universe = alias_universe(versions);
    let mut scope = BTreeSet::new();

    for request in requests {
        match request {
            None => scope.extend(universe.iter().cloned()),
            Some(AliasTag::Root { variant }) => scope.extend(
                universe
                    .iter()
                    .filter(|alias| alias.track() == variant.as_deref())
                    .cloned(),
            ),
            Some(AliasTag::Version(version)) => {
                let requested = AliasTag::Version(version.clone());
                if universe.contains(&requested) {
                    scope.insert(requested);
                    scope.extend(universe.iter().filter(|alias| is_below(alias, version)).cloned());
                } else if !versions.contains(version) {
                    return Err(ScopeError::UnknownTag(version.to_string()));
                }
                scope.extend(path_to_root(version, &universe));
            }
        }
    }

    Ok(scope)
}

/// Whether `alias` sits under `ancestor` in the version trie.
fn is_below(alias: &AliasTag, ancestor: &Version) -> bool {
    let AliasTag::Version(version) = alias else {
        return false;
    };
    std::iter::successors(version.parent(), Version::parent).any(|parent| &parent == ancestor)
}

/// The aliases between a version and its track root, the root included.
fn path_to_root(version: &Version, universe: &BTreeSet<AliasTag>) -> BTreeSet<AliasTag> {
    std::iter::successors(version.parent(), Version::parent)
        .map(AliasTag::Version)
        .chain(std::iter::once(AliasTag::Root {
            variant: version.variant().map(str::to_string),
        }))
        .filter(|alias| universe.contains(alias))
        .collect()
}
