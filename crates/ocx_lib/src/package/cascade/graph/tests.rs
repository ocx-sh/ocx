// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The pure core's state space, covered from literals.
//!
//! Every fixture here is a [`TagGraphObservation`] built by hand, which is the
//! whole reason [`super`] takes one instead of a client: dropped platforms,
//! never-created aliases, bare manifests at alias tags, orphaned entries and a
//! stale public index are all one struct literal away, and none of them need a
//! registry to reproduce.
//!
//! Matrices, in the order the plan pins them: **A** alias chains and
//! leaf-versus-alias classification, **B** the fold, **C** the diff (including
//! the index layer), **D** scope resolution, **E** repair planning.
//!
//! Planning fixtures route through [`plan_scoped`] — directly or via
//! [`plan_of`] — which asserts the E8 invariant on every one of them: a plan
//! never names a digest the registry does not already serve. Four cases call
//! [`plan_repairs`] themselves and so sit outside that assertion: C26 and C26b
//! need the report alongside the writes, E14 asserts the plan is empty, and E8
//! is the case that proves the check discriminates.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::oci::index::{IndexRoot, RootTag};

// ── Fixture construction ────────────────────────────────────────

/// The size every seeded descriptor claims unless a case is about size.
const SIZE: i64 = 512;

/// Parses `os/arch[/variant][+feature[,feature]]` into a wire platform.
///
/// Deliberately not [`crate::oci::Platform`]'s grammar: the fold keys on what
/// a registry actually put in the document, including values ocx cannot model.
fn platform(spec: &str) -> native::Platform {
    let (base, os_features) = match spec.split_once('+') {
        Some((base, features)) => (base, Some(features.split(',').map(str::to_string).collect())),
        None => (spec, None),
    };
    let mut parts = base.split('/');
    let os = parts.next().expect("split always yields one part");
    let architecture = parts.next().expect("platform spec needs an architecture");
    native::Platform {
        architecture: architecture.into(),
        os: os.into(),
        os_version: None,
        os_features,
        variant: parts.next().map(str::to_string),
        features: None,
    }
}

/// The spec form of a platform, for readable assertions.
fn platform_key(platform: &native::Platform) -> String {
    let mut key = format!("{}/{}", platform.os, platform.architecture);
    if let Some(variant) = &platform.variant {
        key.push('/');
        key.push_str(variant);
    }
    if let Some(features) = &platform.os_features {
        key.push('+');
        key.push_str(&features.join(","));
    }
    key
}

/// A descriptor for `platform_spec` (`"-"` for none) pointing at `digest`.
fn entry(platform_spec: &str, digest: &str) -> oci::ImageIndexEntry {
    oci::ImageIndexEntry {
        media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
        digest: format!("sha256:{digest}"),
        size: SIZE,
        platform: (platform_spec != "-").then(|| platform(platform_spec)),
        artifact_type: None,
        annotations: None,
    }
}

fn image_index(entries: Vec<oci::ImageIndexEntry>) -> oci::ImageIndex {
    oci::ImageIndex {
        schema_version: oci::INDEX_SCHEMA_VERSION,
        media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
        artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
        manifests: entries,
        annotations: None,
    }
}

/// The digest a seeded tag resolves to — a function of the tag name, so a
/// fixture can state what an index root committed without threading values.
fn tag_digest(tag: &str) -> oci::Digest {
    oci::Digest::Sha256(format!("tag-{tag}"))
}

fn identifier() -> oci::Identifier {
    oci::Identifier::new_registry("test/pkg", "example.com")
}

/// Builds observations. Tags are classified through [`AliasTag::parse`] at
/// [`Graph::build`] time, against the variant names the whole tag set carries
/// — the same two-step gather performs — so a tag that is not part of the
/// graph lands in `ignored_tags` here exactly as it would in production.
#[derive(Default)]
struct Graph {
    seeds: Vec<(String, Option<oci::ImageIndex>)>,
    root: Option<Vec<(String, oci::Digest)>>,
}

impl Graph {
    fn new() -> Self {
        Self::default()
    }

    /// An image index at `tag` carrying `entries`.
    fn index(mut self, tag: &str, entries: Vec<oci::ImageIndexEntry>) -> Self {
        self.seeds.push((tag.to_string(), Some(image_index(entries))));
        self
    }

    /// An image index at `tag` with full control over the index-level fields.
    fn raw(mut self, tag: &str, index: oci::ImageIndex) -> Self {
        self.seeds.push((tag.to_string(), Some(index)));
        self
    }

    /// A bare image manifest at `tag` — the shape an alias must never have.
    fn bare(mut self, tag: &str) -> Self {
        self.seeds.push((tag.to_string(), None));
        self
    }

    /// A live index root committing `tag → digest` for each pair.
    fn root(mut self, commits: &[(&str, oci::Digest)]) -> Self {
        self.root = Some(
            commits
                .iter()
                .map(|(tag, digest)| ((*tag).to_string(), digest.clone()))
                .collect(),
        );
        self
    }

    /// A live index root committing every listed tag at the digest it actually
    /// resolves to — an index in agreement with the registry.
    fn root_agreeing(self, tags: &[&str]) -> Self {
        let commits: Vec<(&str, oci::Digest)> = tags.iter().map(|tag| (*tag, tag_digest(tag))).collect();
        self.root(&commits)
    }

    fn build(self) -> TagGraphObservation {
        let names: Vec<&str> = self.seeds.iter().map(|(tag, _)| tag.as_str()).collect();
        let variants = crate::package::version::variant_names(names.iter().copied());

        let mut tags = BTreeMap::new();
        let mut ignored_tags = Vec::new();
        for (tag, body) in &self.seeds {
            let Some(alias) = AliasTag::parse(tag, &variants) else {
                ignored_tags.push(tag.clone());
                continue;
            };
            let manifest = match body {
                Some(index) => oci::Manifest::ImageIndex(index.clone()),
                None => oci::Manifest::Image(oci::ImageManifest::default()),
            };
            tags.insert(
                alias,
                ObservedTag {
                    digest: tag_digest(tag),
                    size: SIZE,
                    manifest,
                },
            );
        }

        TagGraphObservation {
            identifier: identifier(),
            logical: self
                .root
                .as_ref()
                .map(|_| oci::Identifier::new_registry("test/pkg", "ocx.sh")),
            tags,
            ignored_tags,
            index_root: self.root.map(|commits| IndexRoot {
                repository: "oci://example.com/test/pkg".to_string(),
                tags: commits
                    .into_iter()
                    .map(|(tag, content)| (tag, RootTag { content, yanked: None }))
                    .collect(),
                status: None,
                deprecated_message: None,
                superseded_by: None,
            }),
        }
    }
}

// ── Assertion helpers ───────────────────────────────────────────

fn version(raw: &str) -> Version {
    Version::parse(raw).unwrap_or_else(|| panic!("not a version: {raw}"))
}

fn versions(raw: &[&str]) -> BTreeSet<Version> {
    raw.iter().map(|value| version(value)).collect()
}

/// [`alias_chain`] rendered as the tag strings it would write.
fn chain(subject: &str, others: &[&str]) -> Vec<String> {
    let mut all = versions(others);
    all.insert(version(subject));
    alias_chain(&version(subject), &all)
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Every alias the tag set implies, sorted.
fn universe(tags: &[&str]) -> Vec<String> {
    alias_universe(&versions(tags))
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn is_leaf(subject: &str, tags: &[&str]) -> bool {
    !alias_universe(&versions(tags)).contains(&AliasTag::Version(version(subject)))
}

/// The whole graph, as `scope_filter` derives it from a tagless request.
fn whole(observation: &TagGraphObservation) -> BTreeSet<AliasTag> {
    scope_filter(&observation.versions(), &[None]).expect("a tagless request never fails")
}

fn check(observation: &TagGraphObservation) -> CascadeReport {
    diff(observation, &fold_expected(observation), &whole(observation))
}

/// The digest and source version of one folded slot.
fn slot(expected: &ExpectedGraph, tag: &str, platform_spec: &str) -> Option<(String, String)> {
    let alias = AliasTag::parse(tag, &["debug".to_string(), "pgo".to_string()])?;
    let found = expected.get(&alias)?.get(&platform(platform_spec))?;
    Some((found.entry.digest.clone(), found.source.to_string()))
}

/// The platform slots one alias is expected to carry, sorted.
fn slots(expected: &ExpectedGraph, tag: &str) -> Vec<String> {
    let alias = AliasTag::parse(tag, &["debug".to_string(), "pgo".to_string()]).expect("test tag is a graph node");
    expected
        .get(&alias)
        .map(|slots| slots.keys().map(platform_key).collect())
        .unwrap_or_default()
}

fn render(row: &SlotRow) -> String {
    format!("{} {} {:?}", row.tag, platform_key(&row.platform), row.status)
}

/// Every row, in report order.
fn rendered(report: &CascadeReport) -> Vec<String> {
    report.rows.iter().map(render).collect()
}

/// Only the rows that are not `Ok`.
fn findings(report: &CascadeReport) -> Vec<String> {
    report
        .rows
        .iter()
        .filter(|row| row.status != SlotStatus::Ok)
        .map(render)
        .collect()
}

fn row_at<'a>(report: &'a CascadeReport, tag: &str, platform_spec: &str) -> &'a SlotRow {
    report
        .rows
        .iter()
        .find(|row| row.tag.to_string() == tag && row.platform == platform(platform_spec))
        .unwrap_or_else(|| panic!("no row for {tag} {platform_spec} in {:?}", rendered(report)))
}

/// Every manifest digest the registry serves in this observation — the set a
/// repair is allowed to draw from.
fn served_digests(observation: &TagGraphObservation) -> BTreeSet<String> {
    let mut digests = BTreeSet::new();
    for tag in observation.tags.values() {
        match &tag.manifest {
            oci::Manifest::ImageIndex(index) => {
                digests.extend(index.manifests.iter().map(|entry| entry.digest.clone()));
            }
            // The wrap of a bare manifest points at that manifest itself.
            oci::Manifest::Image(_) => {
                digests.insert(tag.digest.to_string());
            }
        }
    }
    digests
}

/// Every digest a plan names that the registry does not serve.
///
/// A value rather than an assertion, so E8 can state what a broken plan looks
/// like instead of trusting the checker's own verdict about itself.
fn unserved(writes: &[PlannedWrite], served: &BTreeSet<String>) -> Vec<String> {
    writes
        .iter()
        .flat_map(|write| &write.index.manifests)
        .map(|entry| entry.digest.clone())
        .filter(|digest| !served.contains(digest))
        .collect()
}

/// Plans the whole graph, under the E8 invariant [`plan_scoped`] asserts.
fn plan_of(observation: &TagGraphObservation) -> Vec<PlannedWrite> {
    plan_scoped(observation, &whole(observation))
}

/// Plans `scope` **and asserts the E8 invariant** on the result: every digest a
/// planned index names is one the registry already serves, so a repair can only
/// ever re-point tags at existing content.
fn plan_scoped(observation: &TagGraphObservation, scope: &BTreeSet<AliasTag>) -> Vec<PlannedWrite> {
    let expected = fold_expected(observation);
    let report = diff(observation, &expected, scope);
    let writes = plan_repairs(&report, observation, &expected);

    assert_eq!(
        unserved(&writes, &served_digests(observation)),
        Vec::<String>::new(),
        "E8: the plan names content the registry does not serve"
    );
    writes
}

/// The planned writes as `tag → [platform → digest]`, for readable assertions.
fn planned(writes: &[PlannedWrite]) -> Vec<(String, Vec<String>)> {
    writes
        .iter()
        .map(|write| {
            (
                write.tag.to_string(),
                write
                    .index
                    .manifests
                    .iter()
                    .map(|entry| match &entry.platform {
                        Some(platform) => format!("{}={}", platform_key(platform), entry.digest),
                        None => format!("-={}", entry.digest),
                    })
                    .collect(),
            )
        })
        .collect()
}

fn write_for<'a>(writes: &'a [PlannedWrite], tag: &str) -> &'a PlannedWrite {
    writes
        .iter()
        .find(|write| write.tag.to_string() == tag)
        .unwrap_or_else(|| panic!("no planned write for {tag} in {:?}", planned(writes)))
}

// ── A. alias_chain and leaf-versus-alias classification ─────────

#[test]
fn a1_single_version_walks_the_full_chain() {
    assert_eq!(chain("3.28.1_b1", &[]), ["3.28.1", "3.28", "3", "latest"]);
}

#[test]
fn a2_build_tag_makes_its_own_version_an_alias() {
    assert_eq!(chain("1.0.0_b1", &[]), ["1.0.0", "1.0", "1", "latest"]);
    assert_eq!(universe(&["1.0.0_b1"]), ["latest", "1.0.0", "1.0", "1"]);
}

#[test]
fn a3_two_majors_keep_separate_subtrees_under_one_root() {
    assert_eq!(
        universe(&["1.0.0", "2.0.0"]),
        ["latest", "1.0", "1", "2.0", "2"].map(String::from)
    );
}

#[test]
fn a4_a_version_flips_from_leaf_to_alias_when_a_build_appears() {
    assert!(is_leaf("1.13.2", &["1.13.2"]));
    assert!(!is_leaf("1.13.2", &["1.13.2", "1.13.2_b1"]));
    assert!(is_leaf("1.13.2_b1", &["1.13.2", "1.13.2_b1"]));
}

#[test]
fn a5_a_build_less_prerelease_is_a_detached_leaf() {
    assert!(chain("1.0.0-beta", &[]).is_empty());
    assert!(universe(&["1.0.0-beta"]).is_empty());
    assert!(is_leaf("1.0.0-beta", &["1.0.0-beta"]));
}

#[test]
fn a6_a_prerelease_with_build_reaches_only_its_parent_prerelease() {
    assert_eq!(chain("1.0.0-beta_b1", &[]), ["1.0.0-beta"]);
    assert_eq!(universe(&["1.0.0-beta_b1"]), ["1.0.0-beta"]);
}

#[test]
fn a7_a_release_and_its_release_candidate_coexist() {
    // The rc cascades nowhere, so the release's chain is the whole universe.
    assert_eq!(chain("1.0.0-rc1", &["1.0.0"]), Vec::<String>::new());
    assert_eq!(chain("1.0.0", &["1.0.0-rc1"]), ["1.0", "1", "latest"]);
    assert_eq!(universe(&["1.0.0-rc1", "1.0.0"]), ["latest", "1.0", "1"]);
}

#[test]
fn a8_a_variant_chain_terminates_at_its_own_bare_name() {
    assert_eq!(
        chain("debug-3.28.1_b1", &[]),
        ["debug-3.28.1", "debug-3.28", "debug-3", "debug"]
    );
}

#[test]
fn a9_two_variant_tracks_stay_disjoint() {
    assert_eq!(
        universe(&["debug-1.0.0", "pgo-2.0.0"]),
        ["debug", "pgo", "debug-1.0", "debug-1", "pgo-2.0", "pgo-2"].map(String::from)
    );
}

#[test]
fn a10_minor_siblings_share_only_the_major_and_the_root() {
    assert_eq!(universe(&["3.28.0", "3.29.0"]), ["latest", "3.28", "3.29", "3"]);
}

#[test]
fn a11_the_two_minor_shape_gives_each_minor_its_own_alias() {
    assert_eq!(chain("3.27.1", &["3.28.1"]), ["3.27", "3", "latest"]);
    assert_eq!(chain("3.28.1", &["3.27.1"]), ["3.28", "3", "latest"]);
}

#[test]
fn a12_two_builds_share_one_alias() {
    assert_eq!(chain("3.28.1_b1", &["3.28.1_b2"]), ["3.28.1", "3.28", "3", "latest"]);
    assert_eq!(chain("3.28.1_b2", &["3.28.1_b1"]), ["3.28.1", "3.28", "3", "latest"]);
    assert_eq!(universe(&["3.28.1_b1", "3.28.1_b2"]), ["latest", "3.28.1", "3.28", "3"]);
}

#[test]
fn a13_a_variant_track_and_the_default_track_both_get_a_root() {
    let all = universe(&["3.28.1", "debug-3.28.1"]);
    assert!(all.contains(&"latest".to_string()), "{all:?}");
    assert!(all.contains(&"debug".to_string()), "{all:?}");
}

#[test]
fn a14_input_order_does_not_change_the_derived_graph() {
    let forward = universe(&["1.0.0", "1.0.1_b1", "2.0.0", "debug-1.0.0"]);
    let shuffled = universe(&["debug-1.0.0", "2.0.0", "1.0.0", "1.0.1_b1"]);
    assert_eq!(forward, shuffled);
}

#[test]
fn a15_an_empty_package_has_no_aliases() {
    assert!(universe(&[]).is_empty());
}

#[test]
fn a16_a_package_of_only_ignorable_tags_has_no_aliases() {
    let observation = Graph::new()
        .index("__ocx.desc", vec![entry("linux/amd64", "d1")])
        .index(
            &format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            vec![entry("linux/amd64", "d1")],
        )
        .index("nightly", vec![entry("linux/amd64", "d1")])
        .build();
    assert!(observation.tags.is_empty(), "{:?}", observation.tags.keys());
    assert_eq!(observation.ignored_tags.len(), 3);
    assert!(alias_universe(&observation.versions()).is_empty());
}

#[test]
fn a17_a_bare_major_cascades_straight_to_the_root() {
    assert_eq!(chain("3", &[]), ["latest"]);
    assert_eq!(universe(&["3"]), ["latest"]);
    assert!(is_leaf("3", &["3"]));
}

#[test]
fn a18_a_minor_is_an_alias_once_a_patch_exists_under_it() {
    assert!(!is_leaf("1.0", &["1.0", "1.0.0"]));
    assert!(is_leaf("1.0.0", &["1.0", "1.0.0"]));
}

// ── B. fold_expected ────────────────────────────────────────────

#[test]
fn b1_a_uniform_set_folds_the_newest_into_every_alias() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "old")])
        .index("1.0.1", vec![entry("linux/amd64", "new")])
        .build();
    let expected = fold_expected(&observation);
    for alias in ["latest", "1", "1.0"] {
        assert_eq!(
            slot(&expected, alias, "linux/amd64"),
            Some(("sha256:new".to_string(), "1.0.1".to_string())),
            "{alias}"
        );
    }
}

#[test]
fn b2_a_platform_the_newest_release_dropped_folds_from_the_older_one() {
    // The zig shape: 0.14.0 stopped shipping windows, so `latest` must keep
    // serving 0.13.0's windows build rather than losing the platform.
    let observation = Graph::new()
        .index(
            "0.13.0",
            vec![entry("linux/amd64", "l13"), entry("windows/amd64", "w13")],
        )
        .index("0.14.0", vec![entry("linux/amd64", "l14")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:l14".to_string(), "0.14.0".to_string()))
    );
    assert_eq!(
        slot(&expected, "latest", "windows/amd64"),
        Some(("sha256:w13".to_string(), "0.13.0".to_string()))
    );
}

#[test]
fn b3_a_platform_only_the_newest_publishes_is_still_expected() {
    let observation = Graph::new()
        .index("0.13.0", vec![entry("linux/amd64", "l13")])
        .index(
            "0.14.0",
            vec![entry("linux/amd64", "l14"), entry("darwin/arm64", "d14")],
        )
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(slots(&expected, "latest"), ["darwin/arm64", "linux/amd64"]);
    assert_eq!(
        slot(&expected, "latest", "darwin/arm64"),
        Some(("sha256:d14".to_string(), "0.14.0".to_string()))
    );
}

#[test]
fn b4_two_libc_flavours_of_one_arch_are_independent_slots() {
    let observation = Graph::new()
        .index(
            "1.0.0",
            vec![
                entry("linux/amd64+libc.glibc", "glibc1"),
                entry("linux/amd64+libc.musl", "musl1"),
            ],
        )
        .index("1.0.1", vec![entry("linux/amd64+libc.glibc", "glibc2")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64+libc.glibc"),
        Some(("sha256:glibc2".to_string(), "1.0.1".to_string()))
    );
    assert_eq!(
        slot(&expected, "latest", "linux/amd64+libc.musl"),
        Some(("sha256:musl1".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b5_an_older_minor_owns_its_own_alias() {
    let observation = Graph::new()
        .index("3.27.1", vec![entry("linux/amd64", "v327")])
        .index("3.28.1", vec![entry("linux/amd64", "v328")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "3.27", "linux/amd64"),
        Some(("sha256:v327".to_string(), "3.27.1".to_string()))
    );
}

#[test]
fn b6_the_newest_minor_owns_the_major_and_the_root() {
    let observation = Graph::new()
        .index("3.27.1", vec![entry("linux/amd64", "v327")])
        .index("3.28.1", vec![entry("linux/amd64", "v328")])
        .build();
    let expected = fold_expected(&observation);
    for alias in ["3", "latest"] {
        assert_eq!(
            slot(&expected, alias, "linux/amd64"),
            Some(("sha256:v328".to_string(), "3.28.1".to_string())),
            "{alias}"
        );
    }
}

#[test]
fn b7_a_build_less_prerelease_never_feeds_the_root() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "release")])
        .index("1.1.0-beta", vec![entry("linux/amd64", "beta")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:release".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b8_a_prerelease_with_build_feeds_only_its_parent_prerelease() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "release")])
        .index("1.1.0-beta_b1", vec![entry("linux/amd64", "beta")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "1.1.0-beta", "linux/amd64"),
        Some(("sha256:beta".to_string(), "1.1.0-beta_b1".to_string()))
    );
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:release".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b9_a_variant_track_folds_into_its_own_root() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "default")])
        .index("debug-1.0.0", vec![entry("linux/amd64", "debug")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "debug", "linux/amd64"),
        Some(("sha256:debug".to_string(), "debug-1.0.0".to_string()))
    );
}

#[test]
fn b10_a_variant_never_leaks_into_the_default_track() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "default")])
        .index("debug-9.0.0", vec![entry("linux/amd64", "debug")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:default".to_string(), "1.0.0".to_string()))
    );
    assert!(slot(&expected, "9", "linux/amd64").is_none());
}

#[test]
fn b11_one_digest_under_two_platform_keys_keeps_both_slots() {
    // Rosetta: the darwin/amd64 build is the darwin/arm64 image. Two keys,
    // one digest — and both slots must survive the fold.
    let observation = Graph::new()
        .index(
            "1.0.0",
            vec![entry("darwin/amd64", "universal"), entry("darwin/arm64", "universal")],
        )
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(slots(&expected, "latest"), ["darwin/amd64", "darwin/arm64"]);
}

#[test]
fn b12_any_any_is_a_real_slot() {
    let observation = Graph::new().index("1.0.0", vec![entry("any/any", "portable")]).build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "any/any"),
        Some(("sha256:portable".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b13_a_platform_less_attestation_is_not_folded() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image"), entry("-", "attestation")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(slots(&expected, "latest"), ["linux/amd64"]);
}

#[test]
fn b14_the_unknown_unknown_placeholder_is_not_folded() {
    let observation = Graph::new()
        .index(
            "1.0.0",
            vec![entry("linux/amd64", "image"), entry("unknown/unknown", "attestation")],
        )
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(slots(&expected, "latest"), ["linux/amd64"]);
}

#[test]
fn b15_a_platform_ocx_cannot_model_is_still_folded() {
    // `freebsd/amd64` is a real platform this build has no enum for. Reading
    // "cannot parse" as "not a platform" would plan its deletion.
    let observation = Graph::new()
        .index(
            "1.0.0",
            vec![entry("linux/amd64", "image"), entry("freebsd/amd64", "bsd")],
        )
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(slots(&expected, "latest"), ["freebsd/amd64", "linux/amd64"]);
    assert_eq!(
        slot(&expected, "latest", "freebsd/amd64"),
        Some(("sha256:bsd".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b16_the_greatest_build_wins_its_shared_alias() {
    let observation = Graph::new()
        .index("3.28.1_b1", vec![entry("linux/amd64", "b1")])
        .index("3.28.1_b2", vec![entry("linux/amd64", "b2")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "3.28.1", "linux/amd64"),
        Some(("sha256:b2".to_string(), "3.28.1_b2".to_string()))
    );
}

#[test]
fn b17_an_alias_shaped_tag_contributes_its_children_not_its_own_content() {
    // `3.28.1` is rolled by `3.28.1_b1`, so it is an alias. Its own (wrong)
    // content must not become the expectation for `3.28` — one broken alias
    // would otherwise justify the next.
    let observation = Graph::new()
        .index("3.28.1_b1", vec![entry("linux/amd64", "good")])
        .index("3.28.1", vec![entry("linux/amd64", "wrong")])
        .build();
    let expected = fold_expected(&observation);
    for alias in ["3.28.1", "3.28", "3", "latest"] {
        assert_eq!(
            slot(&expected, alias, "linux/amd64"),
            Some(("sha256:good".to_string(), "3.28.1_b1".to_string())),
            "{alias}"
        );
    }
}

#[test]
fn b18_a_platform_only_an_alias_carries_is_expected_nowhere() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index(
            "latest",
            vec![entry("linux/amd64", "image"), entry("linux/arm64", "leftover")],
        )
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(slots(&expected, "latest"), ["linux/amd64"]);
}

#[test]
fn b19_size_and_annotations_are_copied_verbatim_into_the_expectation() {
    let mut source = entry("linux/amd64", "image");
    source.size = 4242;
    source.artifact_type = Some("application/vnd.example".to_string());
    source.annotations = Some(BTreeMap::from([("org.example".to_string(), "kept".to_string())]));

    let observation = Graph::new().index("1.0.0", vec![source.clone()]).build();
    let expected = fold_expected(&observation);
    let folded = &expected[&AliasTag::Root { variant: None }][&platform("linux/amd64")].entry;
    // Field by field rather than through `same_descriptor`: that predicate is
    // what the diff trusts, so using it here would let it agree with itself.
    assert_eq!(folded.digest, source.digest);
    assert_eq!(folded.size, source.size);
    assert_eq!(folded.media_type, source.media_type);
    assert_eq!(folded.artifact_type, source.artifact_type);
    assert_eq!(folded.annotations, source.annotations);
    assert_eq!(folded.platform, source.platform);
}

#[test]
fn b20_the_fold_does_not_depend_on_tag_order() {
    let forward = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "a")])
        .index("1.0.1", vec![entry("linux/amd64", "b")])
        .index("debug-1.0.0", vec![entry("linux/amd64", "c")])
        .build();
    let shuffled = Graph::new()
        .index("debug-1.0.0", vec![entry("linux/amd64", "c")])
        .index("1.0.1", vec![entry("linux/amd64", "b")])
        .index("1.0.0", vec![entry("linux/amd64", "a")])
        .build();

    let left: Vec<_> = fold_expected(&forward)
        .into_iter()
        .map(|(alias, slots)| (alias.to_string(), slots.len()))
        .collect();
    let right: Vec<_> = fold_expected(&shuffled)
        .into_iter()
        .map(|(alias, slots)| (alias.to_string(), slots.len()))
        .collect();
    assert_eq!(left, right);
}

#[test]
fn b21_an_empty_package_folds_to_nothing() {
    assert!(fold_expected(&Graph::new().build()).is_empty());
}

#[test]
fn b22_an_empty_newest_index_leaves_the_older_version_in_charge() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "old")])
        .index("1.0.1", vec![])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:old".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b23_a_bare_manifest_leaf_contributes_no_slots() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "old")])
        .bare("1.0.1")
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:old".to_string(), "1.0.0".to_string()))
    );
}

#[test]
fn b24_a_bare_major_leaf_feeds_the_root() {
    let observation = Graph::new().index("3", vec![entry("linux/amd64", "three")]).build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:three".to_string(), "3".to_string()))
    );
}

#[test]
fn b25_a_minor_alias_is_pinned_to_its_own_subtree() {
    let observation = Graph::new()
        .index("3.27.1", vec![entry("linux/amd64", "v327")])
        .index("3.28.1", vec![entry("linux/amd64", "v328")])
        .index("4.0.0", vec![entry("linux/amd64", "v4")])
        .build();
    let expected = fold_expected(&observation);
    assert_eq!(
        slot(&expected, "3.27", "linux/amd64"),
        Some(("sha256:v327".to_string(), "3.27.1".to_string()))
    );
    assert_eq!(
        slot(&expected, "3", "linux/amd64"),
        Some(("sha256:v328".to_string(), "3.28.1".to_string()))
    );
    assert_eq!(
        slot(&expected, "latest", "linux/amd64"),
        Some(("sha256:v4".to_string(), "4.0.0".to_string()))
    );
}

// ── C. diff ─────────────────────────────────────────────────────

/// A fully cascaded single-version package — the shape a healthy push leaves.
fn healthy() -> TagGraphObservation {
    let content = || vec![entry("linux/amd64", "image")];
    Graph::new()
        .index("1.0.0", content())
        .index("1.0", content())
        .index("1", content())
        .index("latest", content())
        .build()
}

#[test]
fn c1_a_clean_graph_reports_nothing() {
    let report = check(&healthy());
    assert!(!report.has_findings(), "{:?}", findings(&report));
    assert!(findings(&report).is_empty());
    assert!(report.unrepairable.is_empty());
    assert!(report.index_findings.is_empty());
}

#[test]
fn c2_a_platform_missing_from_every_alias_is_one_row_per_alias() {
    // The ninja shape: a swallowed cascade left darwin out of every rolling
    // tag while the leaf still carries it.
    let full = || vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")];
    let observation = Graph::new()
        .index("1.12.0", full())
        .index("1.12", vec![entry("linux/amd64", "lin")])
        .index("1", vec![entry("linux/amd64", "lin")])
        .index("latest", vec![entry("linux/amd64", "lin")])
        .build();

    let report = check(&observation);
    assert_eq!(
        findings(&report),
        [
            "latest darwin/arm64 Missing",
            "1.12 darwin/arm64 Missing",
            "1 darwin/arm64 Missing",
        ]
    );
}

#[test]
fn c3_a_stale_alias_names_the_version_it_is_stuck_on() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "old")])
        .index("1.0.1", vec![entry("linux/amd64", "new")])
        .index("1.0", vec![entry("linux/amd64", "new")])
        .index("1", vec![entry("linux/amd64", "old")])
        .index("latest", vec![entry("linux/amd64", "old")])
        .build();

    let report = check(&observation);
    let row = row_at(&report, "latest", "linux/amd64");
    assert_eq!(row.status, SlotStatus::Stale);
    assert_eq!(row.observed.as_deref(), Some("sha256:old"));
    assert_eq!(row.expected.as_deref(), Some("sha256:new"));
    assert_eq!(row.source.as_ref().map(ToString::to_string), Some("1.0.1".to_string()));
    assert_eq!(
        row.observed_source.as_ref().map(ToString::to_string),
        Some("1.0.0".to_string())
    );
}

#[test]
fn c4_a_stale_alias_pointing_at_unpublished_content_has_no_provenance() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "new")])
        .index("latest", vec![entry("linux/amd64", "foreign")])
        .build();

    let row = check(&observation);
    let row = row_at(&row, "latest", "linux/amd64");
    assert_eq!(row.status, SlotStatus::Stale);
    assert_eq!(row.observed_source, None);
}

#[test]
fn c5_a_platform_nothing_folds_into_is_an_orphan() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![entry("linux/amd64", "image")])
        .index("1", vec![entry("linux/amd64", "image")])
        .index(
            "latest",
            vec![entry("linux/amd64", "image"), entry("linux/s390x", "leftover")],
        )
        .build();

    let report = check(&observation);
    assert_eq!(findings(&report), ["latest linux/s390x Orphan"]);
    assert!(report.has_findings());
}

#[test]
fn c6_an_alias_that_was_never_created_is_absent_with_its_platforms_missing() {
    // The pnpm shape: versions pushed without cascade, so no rolling tag was
    // ever written.
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .build();

    let report = check(&observation);
    assert_eq!(
        report.aliases[&AliasTag::Root { variant: None }],
        AliasState::Absent,
        "{:?}",
        report.aliases
    );
    assert_eq!(
        findings(&report),
        [
            "latest darwin/arm64 Missing",
            "latest linux/amd64 Missing",
            "1.0 darwin/arm64 Missing",
            "1.0 linux/amd64 Missing",
            "1 darwin/arm64 Missing",
            "1 linux/amd64 Missing",
        ]
    );
}

#[test]
fn c7_an_alias_holding_a_bare_manifest_is_not_an_index() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .bare("latest")
        .build();

    let report = check(&observation);
    assert_eq!(
        report.aliases[&AliasTag::Root { variant: None }],
        AliasState::NotAnIndex {
            digest: tag_digest("latest")
        }
    );
    assert!(findings(&report).contains(&"latest linux/amd64 Missing".to_string()));
}

#[test]
fn c8_platform_less_entries_are_never_reported_as_orphans() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index(
            "latest",
            vec![
                entry("linux/amd64", "image"),
                entry("-", "attestation"),
                entry("unknown/unknown", "provenance"),
            ],
        )
        .index("1.0", vec![entry("linux/amd64", "image")])
        .index("1", vec![entry("linux/amd64", "image")])
        .build();

    let report = check(&observation);
    assert!(findings(&report).is_empty(), "{:?}", findings(&report));
    assert!(!report.has_findings());
}

#[test]
fn c9_a_keep_tag_is_ignored() {
    let keep = format!("__ocx.keep.sha256-{}", "b".repeat(64));
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![entry("linux/amd64", "image")])
        .index("1", vec![entry("linux/amd64", "image")])
        .index("latest", vec![entry("linux/amd64", "image")])
        .index(&keep, vec![entry("linux/amd64", "image")])
        .build();

    let report = check(&observation);
    assert_eq!(report.ignored_tags, [keep]);
    assert!(!report.has_findings(), "{:?}", findings(&report));
}

#[test]
fn c10_an_internal_ocx_tag_is_ignored() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![entry("linux/amd64", "image")])
        .index("1", vec![entry("linux/amd64", "image")])
        .index("latest", vec![entry("linux/amd64", "image")])
        .index("__ocx.desc", vec![])
        .build();

    let report = check(&observation);
    assert_eq!(report.ignored_tags, ["__ocx.desc"]);
    assert!(!report.has_findings(), "{:?}", findings(&report));
}

#[test]
fn c11_a_tag_that_is_not_a_version_is_ignored() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![entry("linux/amd64", "image")])
        .index("1", vec![entry("linux/amd64", "image")])
        .index("latest", vec![entry("linux/amd64", "image")])
        .index("nightly", vec![entry("linux/amd64", "other")])
        .index("v1.0.0", vec![entry("linux/amd64", "other")])
        .build();

    let report = check(&observation);
    assert_eq!(report.ignored_tags, ["nightly", "v1.0.0"]);
    assert!(!report.has_findings(), "{:?}", findings(&report));
}

#[test]
fn c12_an_empty_package_reports_nothing() {
    let report = check(&Graph::new().build());
    assert!(!report.has_findings());
    assert!(report.rows.is_empty());
    assert!(report.aliases.is_empty());
}

#[test]
fn c13_an_empty_alias_index_reports_every_platform_missing() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .index("1.0", vec![])
        .index("1", vec![])
        .index("latest", vec![])
        .build();

    let report = check(&observation);
    assert_eq!(
        findings(&report),
        [
            "latest darwin/arm64 Missing",
            "latest linux/amd64 Missing",
            "1.0 darwin/arm64 Missing",
            "1.0 linux/amd64 Missing",
            "1 darwin/arm64 Missing",
            "1 linux/amd64 Missing",
        ]
    );
}

#[test]
fn c14_a_descriptor_that_differs_only_in_size_is_stale() {
    let mut shrunk = entry("linux/amd64", "image");
    shrunk.size = SIZE + 1;
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![shrunk.clone()])
        .index("1", vec![shrunk.clone()])
        .index("latest", vec![shrunk])
        .build();

    let report = check(&observation);
    assert_eq!(
        findings(&report),
        [
            "latest linux/amd64 Stale",
            "1.0 linux/amd64 Stale",
            "1 linux/amd64 Stale",
        ]
    );
}

/// The findings for a graph whose aliases all carry `alias_entry` while the
/// leaf `1.0.0` carries the plain descriptor the fold expects — the C14 shape,
/// for the cases that vary one descriptor field at a time.
fn findings_for_alias_entry(alias_entry: oci::ImageIndexEntry) -> Vec<String> {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![alias_entry.clone()])
        .index("1", vec![alias_entry.clone()])
        .index("latest", vec![alias_entry])
        .build();
    findings(&check(&observation))
}

/// What C14b–C14d expect: the same digest at every alias, still stale.
const STALE_EVERY_ALIAS: [&str; 3] = [
    "latest linux/amd64 Stale",
    "1.0 linux/amd64 Stale",
    "1 linux/amd64 Stale",
];

#[test]
fn c14b_a_descriptor_that_differs_only_in_annotations_is_stale() {
    let mut annotated = entry("linux/amd64", "image");
    annotated.annotations = Some(BTreeMap::from([(
        "org.opencontainers.image.created".to_string(),
        "1970-01-01T00:00:00Z".to_string(),
    )]));
    assert_eq!(findings_for_alias_entry(annotated), STALE_EVERY_ALIAS);
}

#[test]
fn c14c_a_descriptor_that_differs_only_in_media_type_is_stale() {
    let mut docker_typed = entry("linux/amd64", "image");
    docker_typed.media_type = "application/vnd.docker.distribution.manifest.v2+json".to_string();
    assert_eq!(findings_for_alias_entry(docker_typed), STALE_EVERY_ALIAS);
}

#[test]
fn c14d_a_descriptor_that_differs_only_in_artifact_type_is_stale() {
    let mut typed = entry("linux/amd64", "image");
    typed.artifact_type = Some("application/vnd.example.thing".to_string());
    assert_eq!(findings_for_alias_entry(typed), STALE_EVERY_ALIAS);
}

#[test]
fn c15_a_leafs_own_index_produces_no_rows() {
    let report = check(&healthy());
    assert!(
        !report.rows.iter().any(|row| row.tag.to_string() == "1.0.0"),
        "{:?}",
        rendered(&report)
    );
}

#[test]
fn c16_a_detached_prerelease_is_clean_on_its_own() {
    let observation = Graph::new()
        .index("1.0.0-beta", vec![entry("linux/amd64", "beta")])
        .build();
    let report = check(&observation);
    assert!(!report.has_findings(), "{:?}", findings(&report));
    assert!(report.aliases.is_empty());
}

#[test]
fn c17_rows_are_sorted_by_tag_then_platform() {
    let observation = Graph::new()
        .index(
            "1.0.0",
            vec![
                entry("linux/amd64", "lin"),
                entry("darwin/arm64", "mac"),
                entry("windows/amd64", "win"),
            ],
        )
        .build();

    let report = check(&observation);
    // Report order is `latest` < `1.0` < `1` (the alias ordering), so a plain
    // sort would differ — assert the exact emitted order instead.
    assert_eq!(
        rendered(&report),
        [
            "latest darwin/arm64 Missing",
            "latest linux/amd64 Missing",
            "latest windows/amd64 Missing",
            "1.0 darwin/arm64 Missing",
            "1.0 linux/amd64 Missing",
            "1.0 windows/amd64 Missing",
            "1 darwin/arm64 Missing",
            "1 linux/amd64 Missing",
            "1 windows/amd64 Missing",
        ]
    );
}

#[test]
fn c18_a_narrowed_scope_narrows_the_rows() {
    // Both majors are broken; scoping to `1.0` must leave the `2` subtree
    // unexamined while still covering `1.0`'s own path to the root.
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "one")])
        .index("2.0.0", vec![entry("linux/amd64", "two")])
        .build();
    let scope = scope_filter(&observation.versions(), &[Some(AliasTag::Version(version("1.0")))])
        .expect("1.0 is an alias of this graph");
    let report = diff(&observation, &fold_expected(&observation), &scope);

    assert_eq!(
        findings(&report),
        [
            "latest linux/amd64 Missing",
            "1.0 linux/amd64 Missing",
            "1 linux/amd64 Missing",
        ]
    );
}

#[test]
fn c19_a_broken_variant_track_leaves_the_default_track_clean() {
    let content = || vec![entry("linux/amd64", "image")];
    let observation = Graph::new()
        .index("1.0.0", content())
        .index("1.0", content())
        .index("1", content())
        .index("latest", content())
        .index("debug-1.0.0", vec![entry("linux/amd64", "debug")])
        .build();

    let report = check(&observation);
    assert_eq!(
        findings(&report),
        [
            "debug linux/amd64 Missing",
            "debug-1.0 linux/amd64 Missing",
            "debug-1 linux/amd64 Missing",
        ]
    );
}

#[test]
fn c20_healthy_slots_are_reported_alongside_broken_ones() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .index("1.0", vec![entry("linux/amd64", "lin")])
        .index("1", vec![entry("linux/amd64", "lin")])
        .index(
            "latest",
            vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")],
        )
        .build();

    let report = check(&observation);
    assert!(report.has_findings());
    assert_eq!(
        rendered(&report),
        [
            "latest darwin/arm64 Ok",
            "latest linux/amd64 Ok",
            "1.0 darwin/arm64 Missing",
            "1.0 linux/amd64 Ok",
            "1 darwin/arm64 Missing",
            "1 linux/amd64 Ok",
        ]
    );
}

// ── C. diff — the index layer ───────────────────────────────────

/// A healthy graph whose public index agrees with the registry.
fn healthy_with_root() -> TagGraphObservation {
    let content = || vec![entry("linux/amd64", "image")];
    Graph::new()
        .index("1.0.0", content())
        .index("1.0", content())
        .index("1", content())
        .index("latest", content())
        .root_agreeing(&["latest", "1.0", "1"])
        .build()
}

#[test]
fn c21_an_index_that_agrees_with_the_registry_reports_nothing() {
    let report = check(&healthy_with_root());
    assert!(report.index_findings.is_empty(), "{:?}", report.index_findings);
    assert!(!report.has_findings());
}

#[test]
fn c22_an_index_committing_a_different_digest_is_stale() {
    let content = || vec![entry("linux/amd64", "image")];
    let observation = Graph::new()
        .index("1.0.0", content())
        .index("1.0", content())
        .index("1", content())
        .index("latest", content())
        .root(&[
            ("latest", oci::Digest::Sha256("older".to_string())),
            ("1.0", tag_digest("1.0")),
            ("1", tag_digest("1")),
        ])
        .build();

    let report = check(&observation);
    assert_eq!(
        report.index_findings,
        [IndexFinding::Stale {
            tag: AliasTag::Root { variant: None },
            committed: oci::Digest::Sha256("older".to_string()),
            live: tag_digest("latest"),
        }]
    );
}

#[test]
fn c23_an_alias_the_index_never_recorded_is_not_committed() {
    let content = || vec![entry("linux/amd64", "image")];
    let observation = Graph::new()
        .index("1.0.0", content())
        .index("1.0", content())
        .index("1", content())
        .index("latest", content())
        .root_agreeing(&["1.0", "1"])
        .build();

    let report = check(&observation);
    assert_eq!(
        report.index_findings,
        [IndexFinding::NotCommitted {
            tag: AliasTag::Root { variant: None },
        }]
    );
}

#[test]
fn c24_a_physical_invocation_skips_the_index_layer_entirely() {
    let observation = healthy();
    assert!(observation.index_root.is_none());
    assert!(check(&observation).index_findings.is_empty());
}

#[test]
fn c25_a_healthy_registry_with_a_stale_index_still_has_findings() {
    let content = || vec![entry("linux/amd64", "image")];
    let observation = Graph::new()
        .index("1.0.0", content())
        .index("1.0", content())
        .index("1", content())
        .index("latest", content())
        .root(&[
            ("latest", oci::Digest::Sha256("older".to_string())),
            ("1.0", tag_digest("1.0")),
            ("1", tag_digest("1")),
        ])
        .build();

    let report = check(&observation);
    assert!(findings(&report).is_empty(), "the registry graph itself is clean");
    assert!(report.has_findings(), "the index disagreement is still a finding");
}

#[test]
fn c26_announce_tags_names_every_alias_that_needs_an_index_hop() {
    // A graph that is both broken (`latest` was never created) and behind in
    // the index (`1` committed at the wrong digest) — announce needs both.
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "image")])
        .index("1.0", vec![entry("linux/amd64", "image")])
        .index("1", vec![entry("linux/amd64", "image")])
        .root(&[("1.0", tag_digest("1.0")), ("1", oci::Digest::Sha256("old".into()))])
        .build();

    let expected = fold_expected(&observation);
    let report = diff(&observation, &expected, &whole(&observation));
    let writes = plan_repairs(&report, &observation, &expected);

    assert_eq!(announce_tags(&report, &writes), ["1", "latest"]);
}

#[test]
fn c26b_announce_tags_is_empty_for_a_clean_graph() {
    let observation = healthy_with_root();
    let expected = fold_expected(&observation);
    let report = diff(&observation, &expected, &whole(&observation));
    let writes = plan_repairs(&report, &observation, &expected);
    assert!(announce_tags(&report, &writes).is_empty());
}

// ── D. scope resolution ─────────────────────────────────────────

fn scope(tags: &[&str], requests: &[Option<AliasTag>]) -> Vec<String> {
    scope_filter(&versions(tags), requests)
        .expect("scope resolves")
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn tag_request(tag: &str) -> Option<AliasTag> {
    let identifier = identifier().clone_with_tag(tag);
    scope_request(&identifier).unwrap_or_else(|error| panic!("{tag}: {error}"))
}

#[test]
fn d1_a_tagless_request_covers_every_track() {
    assert_eq!(
        scope(&["1.0.0", "debug-1.0.0"], &[None]),
        ["latest", "debug", "debug-1.0", "debug-1", "1.0", "1"]
    );
}

#[test]
fn d2_an_explicit_latest_covers_the_default_track_only() {
    assert_eq!(
        scope(&["1.0.0", "debug-1.0.0"], &[tag_request("latest")]),
        ["latest", "1.0", "1"]
    );
}

#[test]
fn d3_an_alias_covers_its_subtree_and_its_path_to_the_root() {
    assert_eq!(
        scope(&["3.28.1_b1", "3.29.0"], &[tag_request("3.28")]),
        ["latest", "3.28.1", "3.28", "3"]
    );
}

#[test]
fn d4_a_leaf_covers_its_path_to_the_root_and_not_itself() {
    let resolved = scope(&["3.28.1_b1"], &[tag_request("3.28.1_b1")]);
    assert_eq!(resolved, ["latest", "3.28.1", "3.28", "3"]);
    assert!(!resolved.contains(&"3.28.1_b1".to_string()));
}

#[test]
fn d5_a_version_with_builds_under_it_is_scoped_as_an_alias() {
    assert_eq!(
        scope(&["3.28.1", "3.28.1_b1"], &[tag_request("3.28.1")]),
        ["latest", "3.28.1", "3.28", "3"]
    );
}

#[test]
fn d6_two_requests_union() {
    assert_eq!(
        scope(&["1.0.0", "2.0.0"], &[tag_request("1.0.0"), tag_request("2.0.0")]),
        ["latest", "1.0", "1", "2.0", "2"]
    );
}

#[test]
fn d7_a_repeated_request_is_deduplicated() {
    assert_eq!(
        scope(&["1.0.0"], &[tag_request("1.0.0"), tag_request("1.0.0")]),
        ["latest", "1.0", "1"]
    );
}

#[test]
fn d8_a_version_the_package_never_published_is_an_unknown_tag() {
    let error = scope_filter(&versions(&["1.0.0"]), &[tag_request("9.9.9")]).expect_err("must refuse");
    assert!(
        matches!(&error, ScopeError::UnknownTag(tag) if tag == "9.9.9"),
        "{error:?}"
    );
}

#[test]
fn d9_a_digest_reference_names_one_manifest_not_a_graph() {
    let identifier = identifier().clone_with_digest(oci::Digest::Sha256("a".repeat(64)));
    let error = scope_request(&identifier).expect_err("must refuse");
    assert!(matches!(error, ScopeError::DigestReference), "{error:?}");
}

#[test]
fn d10_a_tag_that_is_not_a_version_is_refused() {
    for tag in ["nightly", "debug", "custom-tag", "__ocx.desc"] {
        let error = scope_request(&identifier().clone_with_tag(tag)).expect_err("must refuse");
        assert!(
            matches!(&error, ScopeError::NotAVersionTag(refused) if refused == tag),
            "{tag}: {error:?}"
        );
    }
}

#[test]
fn d11_a_keep_tag_is_refused() {
    let keep = format!("__ocx.keep.sha256-{}", "a".repeat(64));
    let error = scope_request(&identifier().clone_with_tag(&keep)).expect_err("must refuse");
    assert!(
        matches!(&error, ScopeError::NotAVersionTag(refused) if refused == &keep),
        "{error:?}"
    );
}

#[test]
fn d12_a_variant_alias_covers_its_own_track_only() {
    assert_eq!(
        scope(&["1.0.0", "debug-1.0.0"], &[tag_request("debug-1")]),
        ["debug", "debug-1.0", "debug-1"]
    );
}

#[test]
fn d13_the_only_leaf_of_a_package_covers_the_whole_graph() {
    assert_eq!(scope(&["1.0.0"], &[tag_request("1.0.0")]), ["latest", "1.0", "1"]);
}

#[test]
fn d14_a_prerelease_only_package_scopes_to_nothing_without_erroring() {
    assert!(scope(&["1.0.0-beta"], &[None]).is_empty());
    assert!(scope(&["1.0.0-beta"], &[tag_request("1.0.0-beta")]).is_empty());
}

#[test]
fn d15_the_resolved_scope_does_not_depend_on_request_order() {
    let forward = scope(
        &["1.0.0", "2.0.0", "debug-1.0.0"],
        &[tag_request("1.0.0"), tag_request("debug-1.0.0"), tag_request("2.0.0")],
    );
    let reversed = scope(
        &["1.0.0", "2.0.0", "debug-1.0.0"],
        &[tag_request("2.0.0"), tag_request("debug-1.0.0"), tag_request("1.0.0")],
    );
    assert_eq!(forward, reversed);
    assert_eq!(
        forward,
        ["latest", "debug", "debug-1.0", "debug-1", "1.0", "1", "2.0", "2"]
    );
}

// ── E. plan_repairs ─────────────────────────────────────────────

#[test]
fn e1_a_healthy_graph_plans_no_writes() {
    assert!(plan_of(&healthy()).is_empty());
}

#[test]
fn e2_one_missing_platform_rewrites_the_whole_alias() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .index("1.0", vec![entry("linux/amd64", "lin")])
        .index("1", vec![entry("linux/amd64", "lin")])
        .index(
            "latest",
            vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")],
        )
        .build();

    let writes = plan_of(&observation);
    assert_eq!(
        planned(&writes),
        [
            (
                "1.0".to_string(),
                vec![
                    "darwin/arm64=sha256:mac".to_string(),
                    "linux/amd64=sha256:lin".to_string()
                ]
            ),
            (
                "1".to_string(),
                vec![
                    "darwin/arm64=sha256:mac".to_string(),
                    "linux/amd64=sha256:lin".to_string()
                ]
            ),
        ]
    );
}

#[test]
fn e3a_an_orphan_whose_child_still_exists_is_preserved_and_only_reported() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .index(
            "latest",
            vec![entry("linux/amd64", "lin"), entry("linux/s390x", "orphan")],
        )
        .build();

    let writes = plan_of(&observation);
    let write = write_for(&writes, "latest");
    assert_eq!(
        planned(std::slice::from_ref(write))[0].1,
        [
            "darwin/arm64=sha256:mac",
            "linux/amd64=sha256:lin",
            "linux/s390x=sha256:orphan",
        ]
    );
}

#[test]
fn e3b_an_orphan_is_carried_into_the_plan_where_preflight_can_see_it() {
    // Removing a dead pointer is apply's half of the policy; the plan's job is
    // to hand it both the digest to probe and the row that says it is droppable.
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin")])
        .index(
            "latest",
            vec![entry("linux/amd64", "stale"), entry("linux/s390x", "dead")],
        )
        .build();

    let write = plan_of(&observation);
    let write = write_for(&write, "latest");
    assert!(write.referenced_digests.contains(&"sha256:dead".to_string()));
    assert!(
        write
            .reasons
            .iter()
            .any(|row| row.status == SlotStatus::Orphan && row.observed.as_deref() == Some("sha256:dead")),
        "{:?}",
        write.reasons.iter().map(render).collect::<Vec<_>>()
    );
}

#[test]
fn e3c_an_alias_whose_only_fault_is_an_orphan_is_left_untouched() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin")])
        .index("1.0", vec![entry("linux/amd64", "lin")])
        .index("1", vec![entry("linux/amd64", "lin")])
        .index(
            "latest",
            vec![entry("linux/amd64", "lin"), entry("linux/s390x", "orphan")],
        )
        .build();

    let report = check(&observation);
    assert_eq!(findings(&report), ["latest linux/s390x Orphan"]);
    assert!(plan_of(&observation).is_empty(), "an orphan alone is report-only");
}

#[test]
fn e4_attestations_are_preserved_verbatim_after_the_platform_entries() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .index(
            "latest",
            vec![
                entry("linux/amd64", "lin"),
                entry("-", "attestation"),
                entry("unknown/unknown", "provenance"),
            ],
        )
        .build();

    let writes = plan_of(&observation);
    assert_eq!(
        planned(&writes)
            .into_iter()
            .find(|(tag, _)| tag == "latest")
            .expect("latest is planned")
            .1,
        [
            "darwin/arm64=sha256:mac",
            "linux/amd64=sha256:lin",
            "-=sha256:attestation",
            "unknown/unknown=sha256:provenance",
        ]
    );
}

#[test]
fn e5_index_level_annotations_survive_the_rewrite() {
    let annotations = BTreeMap::from([("org.opencontainers.image.source".to_string(), "https://x".to_string())]);
    let mut broken = image_index(vec![entry("linux/amd64", "lin")]);
    broken.annotations = Some(annotations.clone());

    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .raw("latest", broken)
        .build();

    let writes = plan_of(&observation);
    assert_eq!(write_for(&writes, "latest").index.annotations, Some(annotations));
}

#[test]
fn e6_an_absent_alias_is_created_from_content_that_already_exists() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .build();

    let writes = plan_of(&observation);
    assert_eq!(
        writes.iter().map(|write| write.tag.to_string()).collect::<Vec<_>>(),
        ["latest", "1.0", "1"]
    );
    for write in &writes {
        assert_eq!(write.observed_digest, None, "{}", write.tag);
        assert_eq!(
            write.index.media_type.as_deref(),
            Some(MEDIA_TYPE_OCI_IMAGE_INDEX),
            "a fresh index states its own type"
        );
        assert_eq!(write.index.artifact_type.as_deref(), Some(MEDIA_TYPE_PACKAGE_V1));
    }
}

#[test]
fn e7_a_foreign_artifact_type_is_preserved() {
    let mut foreign = image_index(vec![entry("linux/amd64", "lin")]);
    foreign.artifact_type = Some("application/vnd.example.thing".to_string());
    foreign.media_type = None;

    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .raw("latest", foreign)
        .build();

    let write = plan_of(&observation);
    let write = write_for(&write, "latest");
    assert_eq!(
        write.index.artifact_type.as_deref(),
        Some("application/vnd.example.thing")
    );
    assert_eq!(write.index.media_type, None, "an existing index is not relabelled");
}

#[test]
fn e8_the_unserved_check_names_content_the_registry_does_not_serve() {
    // The invariant itself lives in `plan_scoped`, which every planning case
    // routes through. This case proves the check discriminates: a real plan
    // reports nothing unserved, and a write naming absent content reports that
    // digest — so a green `plan_scoped` cannot be a check that never ran.
    let observation = Graph::new().index("1.0.0", vec![entry("linux/amd64", "lin")]).build();
    let served = served_digests(&observation);

    let honest = plan_of(&observation);
    assert!(!honest.is_empty());
    assert_eq!(unserved(&honest, &served), Vec::<String>::new());

    let forged = PlannedWrite {
        tag: AliasTag::Root { variant: None },
        index: image_index(vec![entry("linux/amd64", "never-pushed")]),
        observed_digest: None,
        referenced_digests: vec!["sha256:never-pushed".to_string()],
        reasons: Vec::new(),
    };
    assert_eq!(unserved(&[forged], &served), ["sha256:never-pushed"]);
}

#[test]
fn e9_a_bare_manifest_at_an_alias_is_wrapped_rather_than_dropped() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin")])
        .bare("latest")
        .build();

    let writes = plan_of(&observation);
    let write = write_for(&writes, "latest");
    assert_eq!(
        planned(std::slice::from_ref(write))[0].1,
        [
            "linux/amd64=sha256:lin".to_string(),
            format!("-={}", tag_digest("latest")),
        ]
    );
    let wrapped = write.index.manifests.last().expect("the wrap is the last entry");
    assert_eq!(wrapped.media_type, MEDIA_TYPE_OCI_IMAGE_MANIFEST);
    assert_eq!(wrapped.size, SIZE, "the wrap claims the body's real length");
}

#[test]
fn e10_a_narrowed_scope_narrows_the_plan() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "one")])
        .index("2.0.0", vec![entry("linux/amd64", "two")])
        .build();
    let scope = scope_filter(&observation.versions(), &[Some(AliasTag::Version(version("1.0")))])
        .expect("1.0 is an alias of this graph");

    let writes = plan_scoped(&observation, &scope);
    assert_eq!(
        writes.iter().map(|write| write.tag.to_string()).collect::<Vec<_>>(),
        ["latest", "1.0", "1"],
        "the 2.x subtree is out of scope and stays unplanned"
    );
}

#[test]
fn e11_the_plan_does_not_depend_on_tag_order() {
    let build = |reversed: bool| {
        let mut graph = Graph::new();
        let seeds: Vec<(&str, Vec<oci::ImageIndexEntry>)> = vec![
            ("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")]),
            ("1.0", vec![entry("linux/amd64", "lin")]),
            ("1", vec![entry("darwin/arm64", "mac")]),
        ];
        let seeds: Vec<_> = if reversed {
            seeds.into_iter().rev().collect()
        } else {
            seeds
        };
        for (tag, entries) in seeds {
            graph = graph.index(tag, entries);
        }
        graph.build()
    };

    assert_eq!(planned(&plan_of(&build(false))), planned(&plan_of(&build(true))));
}

#[test]
fn e12_referenced_digests_are_exact_and_deduplicated() {
    let observation = Graph::new()
        .index(
            "1.0.0",
            vec![entry("darwin/amd64", "universal"), entry("darwin/arm64", "universal")],
        )
        .build();

    let writes = plan_of(&observation);
    for write in &writes {
        assert_eq!(write.referenced_digests, ["sha256:universal"], "{}", write.tag);
    }
}

#[test]
fn e13_every_planned_index_pins_the_schema_version() {
    let observation = Graph::new().index("1.0.0", vec![entry("linux/amd64", "lin")]).build();
    for write in plan_of(&observation) {
        assert_eq!(write.index.schema_version, oci::INDEX_SCHEMA_VERSION);
        assert_eq!(write.index.schema_version, 2);
    }
}

#[test]
fn e14_an_alias_that_would_end_up_empty_is_refused() {
    // The only leaf publishes no platform at all, so there is nothing to point
    // the aliases at — an empty index is worse than an absent one.
    let observation = Graph::new().index("1.0.0", vec![]).build();

    let expected = fold_expected(&observation);
    let report = diff(&observation, &expected, &whole(&observation));
    assert_eq!(
        report.unrepairable,
        [
            Unrepairable::WouldEmptyIndex {
                tag: AliasTag::Root { variant: None }
            },
            Unrepairable::WouldEmptyIndex {
                tag: AliasTag::Version(version("1.0"))
            },
            Unrepairable::WouldEmptyIndex {
                tag: AliasTag::Version(version("1"))
            },
        ]
    );
    assert!(plan_repairs(&report, &observation, &expected).is_empty());
}

#[test]
fn e15_applying_a_plan_makes_the_graph_clean_and_the_next_plan_empty() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin"), entry("darwin/arm64", "mac")])
        .index("1.0", vec![entry("linux/amd64", "stale")])
        .bare("1")
        .build();

    let writes = plan_of(&observation);
    assert!(!writes.is_empty());

    // Apply in memory exactly as the registry would: the tag now resolves to
    // the pushed index.
    let mut applied = observation.clone();
    for write in &writes {
        let body = serde_json::to_vec(&write.index).expect("an index serializes");
        applied.tags.insert(
            write.tag.clone(),
            ObservedTag {
                digest: crate::oci::Algorithm::Sha256.hash(&body),
                size: i64::try_from(body.len()).expect("test body fits in i64"),
                manifest: oci::Manifest::ImageIndex(write.index.clone()),
            },
        );
    }

    let report = check(&applied);
    assert!(!report.has_findings(), "{:?}", findings(&report));
    assert!(plan_of(&applied).is_empty());
}

// ── C27: two entries for one platform key ────────────────────────────────────
//
// Only the last entry for a platform resolves, so an earlier one is published
// but unreachable. Both cases below are the same malformation; they differ only
// in whether the *surviving* entry happens to be what the fold expects, which
// is exactly the difference between the damage being visible and being silent.

#[test]
fn c27a_a_duplicate_platform_entry_is_reported_even_when_the_last_one_matches() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin")])
        .index("1.0", vec![entry("linux/amd64", "lin")])
        .index("1", vec![entry("linux/amd64", "lin")])
        .index(
            "latest",
            vec![entry("linux/amd64", "ghost"), entry("linux/amd64", "lin")],
        )
        .build();

    let report = check(&observation);
    assert_eq!(
        findings(&report),
        ["latest linux/amd64 Duplicate"],
        "the shadowed entry is the whole finding — the surviving one is correct"
    );
    // And the rewrite is the fix: the plan carries one entry per platform.
    assert_eq!(
        planned(&plan_of(&observation)),
        [("latest".to_string(), vec!["linux/amd64=sha256:lin".to_string()])]
    );
}

#[test]
fn c27b_a_duplicate_platform_entry_is_reported_beside_the_staleness_it_hides() {
    let observation = Graph::new()
        .index("1.0.0", vec![entry("linux/amd64", "lin")])
        .index("1.0", vec![entry("linux/amd64", "lin")])
        .index("1", vec![entry("linux/amd64", "lin")])
        .index(
            "latest",
            vec![entry("linux/amd64", "ghost"), entry("linux/amd64", "stale")],
        )
        .build();

    let report = check(&observation);
    assert_eq!(
        findings(&report),
        ["latest linux/amd64 Stale", "latest linux/amd64 Duplicate"],
        "the stale surviving entry and the shadowed one are separate facts"
    );
    let write = plan_of(&observation);
    assert_eq!(
        planned(&write),
        [("latest".to_string(), vec!["linux/amd64=sha256:lin".to_string()])]
    );
}
