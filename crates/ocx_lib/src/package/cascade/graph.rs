// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The pure compute core of `ocx package cascade check|repair`.
//!
//! Everything here is synchronous and I/O-free: [`fold_expected`], [`diff`],
//! [`plan_repairs`], [`scope_filter`] and [`announce_tags`] are functions of a
//! [`TagGraphObservation`] alone. That is the point of the split — the whole
//! state space (dropped platforms, prereleases, variant tracks, orphans,
//! never-created aliases) is reachable from a literal, so it can be covered by
//! ordinary unit tests instead of a registry fixture. Reads live in
//! [`super::gather`], writes in [`super::apply`].
//!
//! The expected state is a **fold**, not a materialized trie. Version tags form
//! a trie through [`Version::parent`]; a version's alias chain is derived on
//! demand by [`alias_chain`] from the production [`decompose`](super::decompose)
//! algebra — the same code the push path walks — so what check expects can never
//! drift from what a push would have written.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Serialize, Serializer};

use crate::oci::{self, native};
use crate::package::cascade::decompose_targets;
use crate::package::tag::Tag;
use crate::package::version::Version;
use crate::{MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_PACKAGE_V1};

#[cfg(test)]
mod tests;

/// The expected content of the whole graph: per alias tag, per platform, the
/// entry that alias should carry and the version it was folded from.
///
/// A type alias rather than a struct — it is the plain output of a fold, and
/// every consumer wants the maps, not a wrapper with accessors.
pub type ExpectedGraph = BTreeMap<AliasTag, BTreeMap<native::Platform, ExpectedSlot>>;

/// A tag that carries content on behalf of a whole subtree rather than one
/// concrete build.
///
/// Every alias but a track root is named by the [`Version`] it rolls (`3`,
/// `3.28`, `3.28.1`). A track root — `latest` for the default track, the bare
/// variant name (`debug`) for a variant track — is named by no version at all,
/// which is the only reason this enum exists rather than a bare `Version`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AliasTag {
    /// The root of one track: `None` is the default track's `latest`, `Some`
    /// is a variant track's bare name.
    Root { variant: Option<String> },
    /// Any other alias, named by the version it rolls.
    Version(Version),
}

impl AliasTag {
    /// Classifies a registry tag as a node of this package's tag graph.
    ///
    /// `variants` is the set of variant names the package's tag list carries
    /// ([`variant_names`](crate::package::version::variant_names)), because a
    /// bare tag alone cannot say whether it is a track root: `debug` is the
    /// `debug` track's root only next to a `debug-…` sibling, and is otherwise
    /// an arbitrary tag someone pushed. Reserved tags — the `__ocx` namespace
    /// and keep tags — and anything else that is not a
    /// version return `None`, which is what puts them in `ignored_tags`.
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

    /// The variant track this tag belongs to — `None` for the default track.
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

/// Serializes as the registry tag itself, so the report's JSON keys are the
/// tags a reader would type at the registry, not a tagged union.
impl Serialize for AliasTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// The `#[derive(schemars::JsonSchema)]` a plain enum would carry publishes a
// tagged union (`Root`/`Version` variant shapes) — a lie once `Serialize`
// above collapses every variant to the tag string. The schema must describe
// the wire form, not the Rust shape behind it.
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

/// Everything one gather pass read about one package.
///
/// Absence is never recorded here, only derived: gather fetches the tags the
/// registry listed, so an alias that was never created is simply missing from
/// [`Self::tags`], and the pure core reads that as `Absent`. A listed tag that
/// could not be fetched aborts the run instead of landing here as a gap — a
/// degraded read would manufacture findings that look exactly like real drift.
#[derive(Clone, Debug)]
pub struct TagGraphObservation {
    /// The physical repository every tag below was read from.
    pub identifier: oci::Identifier,
    /// The logical name the user asked for, when it differed from
    /// [`Self::identifier`]. `Some` is what turns the index layer on.
    pub logical: Option<oci::Identifier>,
    /// Every version-ish tag the registry holds, with the body behind it.
    pub tags: BTreeMap<AliasTag, ObservedTag>,
    /// Tags deliberately not part of the graph — keep tags
    /// tags, `__ocx.*` internals, and anything that is not a version.
    /// Reported so a user can see nothing was silently dropped.
    pub ignored_tags: Vec<String>,
    /// The live index root for a logical package, when one was read. `None`
    /// for a physical invocation, where no root maps back to the repository.
    pub index_root: Option<crate::oci::index::IndexRoot>,
}

/// One tag exactly as the registry served it.
///
/// `size` is the byte length of the manifest body `digest` was computed over,
/// not of a re-serialization: an alias holding a bare image manifest is
/// repaired by wrapping that manifest in an index entry, and a descriptor
/// whose `size` disagreed with the bytes on the registry would be a broken
/// pointer even though the digest matched.
#[derive(Clone, Debug)]
pub struct ObservedTag {
    /// The digest the tag resolved to.
    pub digest: oci::Digest,
    /// The length in bytes of the manifest body served at `digest`.
    pub size: i64,
    /// The parsed body.
    pub manifest: oci::Manifest,
}

impl TagGraphObservation {
    /// The concrete versions the graph is folded from — every observed tag
    /// that names a version, alias-shaped or not.
    pub fn versions(&self) -> BTreeSet<Version> {
        self.tags
            .keys()
            .filter_map(|tag| match tag {
                AliasTag::Version(version) => Some(version.clone()),
                AliasTag::Root { .. } => None,
            })
            .collect()
    }

    /// The index at `tag`, when the tag exists and is one.
    fn index(&self, tag: &AliasTag) -> Option<&oci::ImageIndex> {
        match &self.tags.get(tag)?.manifest {
            oci::Manifest::ImageIndex(index) => Some(index),
            oci::Manifest::Image(_) => None,
        }
    }
}

/// What one alias should carry for one platform, and where that came from.
///
/// `entry` is a verbatim copy of the leaf's own index entry, so a repair
/// re-points an alias at content that already exists instead of composing a
/// new descriptor; `source` is the version it was copied from, which is what
/// makes a finding explainable ("`3` should carry what `3.28.1` carries").
#[derive(Clone, Debug)]
pub struct ExpectedSlot {
    pub entry: oci::ImageIndexEntry,
    pub source: Version,
}

/// The verdict for one (alias, platform) slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SlotStatus {
    /// The alias carries exactly the expected entry.
    Ok,
    /// The alias should carry a platform it does not carry at all.
    Missing,
    /// The alias carries this platform, but not the expected content.
    Stale,
    /// The alias carries a platform nothing folds into it — a leftover from
    /// an earlier cascade. Reported always; removed only when the entry it
    /// points at no longer exists.
    Orphan,
    /// The alias carries more than one entry for this platform. Only the last
    /// one resolves, so a shadowed entry is invisible to every consumer while
    /// still being published — including when the surviving one is exactly what
    /// the fold expects. One row per shadowed entry, beside the row for the
    /// entry that wins.
    Duplicate,
}

/// One row of the diff: what one alias holds for one platform versus what the
/// fold says it should hold.
///
/// Digests are the wire strings verbatim rather than parsed values: an index
/// may legitimately name a digest algorithm this build does not implement, and
/// a report must be able to show it.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct SlotRow {
    pub tag: AliasTag,
    // `oci_client::manifest::Platform` is registry-defined and cannot carry our
    // derive; it is published as free-form JSON rather than mirrored here.
    #[schemars(with = "serde_json::Value")]
    pub platform: native::Platform,
    pub status: SlotStatus,
    /// The digest the alias carries for this platform, if any.
    pub observed: Option<String>,
    /// The digest the fold expects, if any.
    pub expected: Option<String>,
    /// The version the expectation was folded from.
    pub source: Option<Version>,
    /// The version the observed digest belongs to, when the observed content
    /// is recognisable as some published version's — `None` when the alias
    /// points at content no observed leaf carries.
    pub observed_source: Option<Version>,
}

/// What the registry holds at an alias tag, taken as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum AliasState {
    /// An image index — the shape every alias is supposed to have.
    Present,
    /// No such tag at the registry: the alias was never created.
    Absent,
    /// The tag exists but resolves to a bare image manifest, so it has no
    /// per-platform slots at all and every expected platform reads as missing.
    NotAnIndex { digest: oci::Digest },
}

/// A disagreement between the registry graph and the public index that
/// publishes it.
///
/// Only ever produced for a logical identifier: a physical repository has no
/// reverse mapping back to an index root, so the layer is skipped entirely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "finding")]
pub enum IndexFinding {
    /// The index committed a different digest than the alias points at today.
    /// Repair cannot fix this — announcing the tag can.
    Stale {
        tag: AliasTag,
        committed: oci::Digest,
        live: oci::Digest,
    },
    /// The registry carries the alias and the index has never recorded it.
    NotCommitted { tag: AliasTag },
}

/// Something check found that no repair can fix without new content being
/// published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "reason")]
pub enum Unrepairable {
    /// A planned entry points at a child manifest the registry no longer
    /// holds, so writing the alias would publish a dangling pointer.
    ChildManifestMissing { tag: AliasTag, digest: String },
    /// A planned entry names a digest algorithm this build cannot address, so
    /// whether the child is still there could not be checked at all. Distinct
    /// from [`Self::ChildManifestMissing`]: nothing was observed to be gone —
    /// the alias is refused because the check could not be made.
    ChildDigestUnaddressable { tag: AliasTag, digest: String },
    /// Repairing the alias would leave it with no entries at all. Refused:
    /// an empty index is worse than a stale one.
    WouldEmptyIndex { tag: AliasTag },
}

/// The whole finding set for one package — the value both `check` and `repair`
/// report, and the input `plan_repairs` works from.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct CascadeReport {
    /// The physical repository the graph was read from.
    pub identifier: oci::Identifier,
    /// The logical name the user asked for, when it differed.
    pub logical: Option<oci::Identifier>,
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
    /// Whether anything at all needs attention — the exit-code question.
    ///
    /// True when any row is not [`SlotStatus::Ok`], any alias is absent or not
    /// an index, or the index layer disagrees with the registry.
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
    pub tag: AliasTag,
    /// The complete index to PUT at `tag`.
    // `OciImageIndex` comes from `oci_client` and cannot carry our derive. The
    // OCI image index is a registry-defined document, not part of the contract
    // this schema publishes, so it is described as free-form JSON.
    #[schemars(with = "serde_json::Value")]
    pub index: oci::ImageIndex,
    /// The digest the alias points at now — `None` when the alias does not
    /// exist yet.
    pub observed_digest: Option<oci::Digest>,
    /// Every child manifest digest `index` references, deduped. Apply
    /// preflights each one so a missing child refuses the alias instead of
    /// publishing a dangling pointer.
    pub referenced_digests: Vec<String>,
    /// The rows that justify the write — what the user is being told changed.
    ///
    /// Includes the alias's [`SlotStatus::Orphan`] rows even though an orphan
    /// never causes a write on its own: they are how apply tells a preserved
    /// leftover from a folded entry when preflight finds a child gone, and so
    /// which entry it may drop instead of refusing the whole alias.
    pub reasons: Vec<SlotRow>,
}

/// Why a requested scope cannot be turned into a set of alias tags.
///
/// A usage fault in every variant: the argument names something that is not a
/// node of this package's tag graph.
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

/// The alias chain a version cascades into: every rolling tag that should
/// carry this version's content when nothing newer blocks it.
///
/// Derived from [`decompose_targets`](super::decompose_targets) — the half of
/// the push path's [`decompose`](super::decompose) that answers exactly this
/// question — so every carve-out the push path implements (a build-less
/// prerelease cascading nowhere, a prerelease-with-build reaching only its
/// parent prerelease, a variant track terminating at its own bare name) holds
/// here by construction rather than by a second implementation that could
/// disagree.
///
/// `others` is not read: which tags a version cascades *into* is a property of
/// the version, and the set only decides which of them it is blocked from
/// reaching — a question the fold asks separately, one alias at a time. Taking
/// it here is what let the fold pay for a blocker scan per version per level.
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

/// Every alias any observed version cascades into — the whole set of tags the
/// graph is supposed to carry.
///
/// This is also the leaf test: a version *not* in here is in nobody's alias
/// chain, so nothing rolls it and it carries content of its own. `3.28.1` is a
/// leaf until `3.28.1_b1` is published and starts rolling it.
fn alias_universe(versions: &BTreeSet<Version>) -> BTreeSet<AliasTag> {
    versions
        .iter()
        .flat_map(|version| alias_chain(version, versions))
        .collect()
}

/// The versions that carry content of their own — the ones in nobody's alias
/// chain, and so the only legitimate sources for a fold.
///
/// Ascending, which is what makes "the last write wins" the max-selection the
/// fold is defined by.
fn leaves(versions: &BTreeSet<Version>) -> Vec<&Version> {
    let universe = alias_universe(versions);
    versions
        .iter()
        .filter(|version| !universe.contains(&AliasTag::Version((*version).clone())))
        .collect()
}

/// The platform an entry claims a slot for, or `None` when it claims none.
///
/// Two shapes carry no platform slot: a descriptor with no `platform` at all
/// (an attestation or a bare-manifest wrap) and the `unknown/unknown`
/// placeholder attestations are conventionally tagged with. Everything else is
/// a slot, including `any/any` and platforms this build cannot model — reading
/// "we failed to parse it" as "it has no platform" would blind the fold to a
/// real `freebsd/amd64` entry and plan its deletion.
fn platform_slot(entry: &oci::ImageIndexEntry) -> Option<&native::Platform> {
    let platform = entry.platform.as_ref()?;
    let unknown = platform.os.to_string() == "unknown" && platform.architecture.to_string() == "unknown";
    (!unknown).then_some(platform)
}

/// Whether two descriptors are the same in every field a repair would rewrite.
///
/// Full-descriptor rather than digest-only: a repair PUTs whole entries, so a
/// diff that ignored `size`, `mediaType`, `artifactType` or `annotations`
/// would call an entry clean and then silently change it.
fn same_descriptor(left: &oci::ImageIndexEntry, right: &oci::ImageIndexEntry) -> bool {
    left.digest == right.digest
        && left.size == right.size
        && left.media_type == right.media_type
        && left.artifact_type == right.artifact_type
        && left.annotations == right.annotations
        && left.platform == right.platform
}

/// The platform-slot entries of one alias, keyed by platform, plus the entries
/// a later one shadows.
///
/// A well-formed index has one entry per platform, and every consumer resolves
/// the last one — so a shadowed entry is published but unreachable. Collecting
/// straight into the map would hide that: the alias would diff as clean
/// whenever the *surviving* entry happens to match the fold, and the repair
/// would plan nothing. The shadowed entries come back so the diff can report
/// them; the rewrite drops them for free, since it rebuilds the index from one
/// entry per platform.
fn observed_slots<'a>(
    observation: &'a TagGraphObservation,
    alias: &AliasTag,
) -> (
    BTreeMap<&'a native::Platform, &'a oci::ImageIndexEntry>,
    Vec<(&'a native::Platform, &'a oci::ImageIndexEntry)>,
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

/// Which published version a digest observed at an alias came from.
///
/// Built over leaves only: an alias's own content is a copy, so attributing a
/// digest to the alias that also happens to carry it would explain a finding
/// with another finding. The greatest matching leaf wins when two leaves
/// publish byte-identical content.
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

/// Folds every observed version into the state each alias should be in.
///
/// An alias's slot for a platform is the entry of the greatest version that
/// cascades into that alias and publishes that platform — which is why a
/// platform dropped by the newest build keeps folding from the older version
/// that still ships it, instead of vanishing from the rolling tag.
pub fn fold_expected(observation: &TagGraphObservation) -> ExpectedGraph {
    let versions = observation.versions();
    let mut expected = ExpectedGraph::new();

    // Only leaves are folded: an alias tag's own content is a cascade copy, so
    // folding it upward would let one broken alias justify the next.
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

/// Diffs what the registry holds against what the fold expects, restricted to
/// `scope`.
///
/// Pure and total: every alias in scope produces an [`AliasState`], every
/// (alias, platform) slot in either map produces a [`SlotRow`], and the index
/// layer contributes findings only when the observation carries a root.
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
                oci::Manifest::ImageIndex(_) => AliasState::Present,
                oci::Manifest::Image(_) => AliasState::NotAnIndex {
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

        // One row per shadowed entry, carrying the digest that is published but
        // unreachable. Appended and then re-sorted rather than woven into the
        // pass above, because the pass is keyed by platform and this is the one
        // case where a platform owns more than one row.
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

/// The registry-versus-index layer: what the public index committed for each
/// alias against what the alias points at right now.
///
/// Empty for a physical invocation — with no root there is nothing to compare
/// against, and inventing one from the repository name would be a guess.
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

/// Whether an alias needs its index rewritten.
///
/// An orphan alone does not: the policy is to preserve an orphan whose child
/// still exists, so an alias whose only fault is a leftover entry is reported
/// and left byte-identical. A duplicate does, and is the one case where the
/// rewrite is the whole fix: the plan carries one entry per platform, so
/// republishing the alias is what drops the shadowed copy.
fn needs_write(state: &AliasState, rows: &[SlotRow]) -> bool {
    state != &AliasState::Present
        || rows.iter().any(|row| {
            matches!(
                row.status,
                SlotStatus::Missing | SlotStatus::Stale | SlotStatus::Duplicate
            )
        })
}

/// The complete `manifests` list one alias should carry.
///
/// The shape mirrors what the push path leaves behind: platform entries in
/// platform order, then whatever carries no platform (attestations, and the
/// wrap of a bare manifest found at the tag) in the order the registry served
/// them. Every entry is either a verbatim copy of a leaf's descriptor or a
/// verbatim copy of something already at this tag — a repair re-points, it
/// never composes new content.
fn planned_entries(
    alias: &AliasTag,
    observation: &TagGraphObservation,
    expected: &ExpectedGraph,
) -> Vec<oci::ImageIndexEntry> {
    let mut slots: BTreeMap<native::Platform, oci::ImageIndexEntry> = expected
        .get(alias)
        .into_iter()
        .flatten()
        .map(|(platform, slot)| (platform.clone(), slot.entry.clone()))
        .collect();

    let observed = observation.tags.get(alias);
    let mut unplatformed: Vec<oci::ImageIndexEntry> = Vec::new();
    match observed.map(|tag| &tag.manifest) {
        Some(oci::Manifest::ImageIndex(index)) => {
            for entry in &index.manifests {
                match platform_slot(entry) {
                    // An entry nothing folds into is an orphan: preserved, so
                    // a truncated tag listing can never turn into a deletion.
                    Some(platform) => {
                        slots.entry(platform.clone()).or_insert_with(|| entry.clone());
                    }
                    None => unplatformed.push(entry.clone()),
                }
            }
        }
        // Parity with the push path's merge: a bare manifest at an alias tag
        // becomes a platform-less entry of the index that replaces it, rather
        // than content this run drops on the floor.
        Some(oci::Manifest::Image(_)) => {
            if let Some(tag) = observed {
                unplatformed.push(oci::ImageIndexEntry {
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

    let mut entries: Vec<oci::ImageIndexEntry> = slots.into_values().collect();
    entries.extend(unplatformed);
    entries
}

/// Turns a report into the exact set of index writes that would resolve it.
///
/// Only aliases with a non-`Ok` row are planned, each rebuilt from observed
/// content alone — a repair publishes nothing new, it re-points. An alias that
/// would end up empty is refused rather than written.
pub fn plan_repairs(
    report: &CascadeReport,
    observation: &TagGraphObservation,
    expected: &ExpectedGraph,
) -> Vec<PlannedWrite> {
    let mut writes = Vec::new();

    // Grouped once rather than re-filtered per alias: the scan is over every
    // row of the whole report, so doing it inside the loop made a package's
    // aliases and its platforms multiply.
    let mut by_alias: BTreeMap<&AliasTag, Vec<SlotRow>> = BTreeMap::new();
    for row in report.rows.iter().filter(|row| row.status != SlotStatus::Ok) {
        by_alias.entry(&row.tag).or_default().push(row.clone());
    }

    for (alias, state) in &report.aliases {
        let reasons = by_alias.remove(alias).unwrap_or_default();
        if !needs_write(state, &reasons) {
            continue;
        }

        // Refused rather than written: `diff` has already recorded the alias
        // as `WouldEmptyIndex`, and an empty index is worse than a stale one.
        let manifests = planned_entries(alias, observation, expected);
        if manifests.is_empty() {
            continue;
        }

        let observed = observation.tags.get(alias);
        let observed_index = observation.index(alias);
        let referenced_digests: BTreeSet<String> = manifests.iter().map(|entry| entry.digest.clone()).collect();

        writes.push(PlannedWrite {
            tag: alias.clone(),
            index: oci::ImageIndex {
                schema_version: oci::INDEX_SCHEMA_VERSION,
                // Preserved verbatim where the tag already held an index —
                // including an absent one; only a freshly built index states
                // its own type.
                media_type: match observed_index {
                    Some(index) => index.media_type.clone(),
                    None => Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                },
                // Filled if absent, never overwritten: stating what ocx wrote
                // is not the same as relabelling someone else's artifact.
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

/// Reads the scope request out of an identifier.
///
/// `None` means the identifier named no tag and the whole graph is in scope.
/// A digest reference names one manifest rather than a tag graph, and a tag
/// that is not a version names nothing the graph contains — both are usage
/// faults, refused here rather than silently scoped to nothing.
///
/// # Errors
///
/// [`ScopeError::DigestReference`] for a digest-pinned identifier and
/// [`ScopeError::NotAVersionTag`] for a tag that names no version.
pub fn scope_request(identifier: &oci::Identifier) -> Result<Option<AliasTag>, ScopeError> {
    if identifier.digest().is_some() {
        return Err(ScopeError::DigestReference);
    }
    let Some(tag) = identifier.tag() else {
        return Ok(None);
    };
    match Tag::from(tag.to_string()) {
        Tag::Latest => Ok(Some(AliasTag::Root { variant: None })),
        Tag::Version(version) => Ok(Some(AliasTag::Version(version))),
        // A bare variant name is deliberately not accepted here: whether
        // `debug` names a track root depends on the package's other tags,
        // which this call has not read. `debug-3` scopes the same track,
        // since a major's path to the root is that root.
        _ => Err(ScopeError::NotAVersionTag(tag.to_string())),
    }
}

/// Expands per-identifier requests into the set of alias tags to act on.
///
/// A request for an alias covers its whole subtree plus its path to the track
/// root — repairing `3.28` without also considering `3` and `latest` would
/// leave the graph half-fixed. A request for a leaf covers its path to the
/// root and excludes the leaf itself, since a leaf carries its own content and
/// is never rewritten. Requests union; `None` means the whole graph.
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
                // A leaf is never rewritten, so it contributes only the path
                // it rolls into; an alias contributes that path too.
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

/// The alias tags that need an index hop, as the lines of the file
/// `--announce-tags` writes.
///
/// This is the handshake with `ocx package announce`: repair fixes the
/// registry graph, and the index hop is a separate command that needs to know
/// exactly which tags moved. That is every tag this run re-pointed or created,
/// plus every tag the index committed wrong or never committed at all — the
/// one class of drift a repair cannot close itself, and the reason the report
/// is read here and not just the writes. Announce re-observes each tag live
/// and never removes one, so a tag listed twice over costs nothing.
///
/// Sorted and deduped; empty when nothing needs the hop, which announce reads
/// as a safe no-op.
pub fn announce_tags(report: &CascadeReport, writes: &[PlannedWrite]) -> Vec<String> {
    writes
        .iter()
        .map(|write| write.tag.to_string())
        // A tag the index committed wrong (or never committed) needs the same
        // hop even when the registry graph was already correct, which is the
        // only case a repair itself cannot close.
        .chain(report.index_findings.iter().map(|finding| match finding {
            IndexFinding::Stale { tag, .. } | IndexFinding::NotCommitted { tag } => tag.to_string(),
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
