// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Equivalence suite: a graph the push path built must read back clean.
//!
//! The oracle is the production push path itself. For a set of versions and
//! platforms, every publication order is replayed against a stub registry
//! through the same tag resolution and index merge a real push performs, and
//! the resulting graph is then gathered and diffed. A finding means check and
//! push disagree about the same algebra — either a real drift bug or a false
//! positive, and both are defects.
//!
//! The suite carries its own falsifier: one scenario deliberately damages the
//! replayed graph and asserts exactly one finding comes back, so a green run
//! here is distinguishable from a run that checked nothing.

use std::collections::{BTreeMap, BTreeSet};

use super::graph::{
    AliasState, AliasTag, CascadeReport, ObservedTag, PlannedWrite, SlotRow, SlotStatus, TagGraphObservation, diff,
    fold_expected, plan_repairs, scope_filter,
};
use super::{gather::gather, resolve_cascade_tags};
use crate::oci::client::test_transport::{StubTransport, StubTransportData};
use crate::oci::{self, native};
use crate::package::version::Version;

const REGISTRY: &str = "example.com";
const REPOSITORY: &str = "test/pkg";

/// The size every replayed platform manifest claims.
///
/// Constant on purpose: the descriptor a merge writes and the descriptor the
/// fold copies come from the same publication, so a per-publication size would
/// vary both sides together and discriminate nothing.
const MANIFEST_SIZE: i64 = 512;

// ── Fixtures ────────────────────────────────────────────────────

fn identifier() -> oci::Identifier {
    oci::Identifier::new_registry(REPOSITORY, REGISTRY)
}

/// The stub keys its manifest map on the transport reference's string form.
fn manifest_key(tag: &str) -> String {
    identifier().clone_with_tag(tag).canonical_reference().to_string()
}

fn version(raw: &str) -> Version {
    Version::parse(raw).unwrap_or_else(|| panic!("'{raw}' must be a version tag"))
}

fn platform(spec: &str) -> oci::Platform {
    spec.parse().expect("a test platform spec parses")
}

fn native_platform(spec: &str) -> native::Platform {
    platform(spec).into()
}

/// One publication: one version, built for one platform.
///
/// The unit of replay, because it is the unit of publication — a real matrix
/// build pushes each platform from its own job, and the order those jobs land
/// in is exactly what this suite quantifies over.
#[derive(Clone, Debug)]
struct Publication {
    version: Version,
    platform: oci::Platform,
}

fn publication(version_raw: &str, platform_spec: &str) -> Publication {
    Publication {
        version: version(version_raw),
        platform: platform(platform_spec),
    }
}

impl Publication {
    /// The platform manifest digest this publication puts on the wire.
    ///
    /// A function of the content it stands for, so the same publication has
    /// the same digest in every ordering and a slot can be attributed to the
    /// version it came from by value alone.
    fn digest(&self) -> String {
        oci::Algorithm::Sha256
            .hash(format!("{}|{}", self.version, self.platform).as_bytes())
            .to_string()
    }

    fn label(&self) -> String {
        format!("{}@{}", self.version, self.platform)
    }
}

fn order_label(order: &[Publication]) -> String {
    order.iter().map(Publication::label).collect::<Vec<_>>().join(" → ")
}

// ── The replay ──────────────────────────────────────────────────

/// A stub registry plus the tag list it would answer with.
///
/// [`Self::publish`] is the tag-graph half of `push_with_cascade`: the same
/// [`resolve_cascade_tags`] call over the same version set, then the same
/// [`merge_platform_into_index`](oci::Client::merge_platform_into_index) per
/// resolved tag. The blob half is left out because it cannot alter the graph —
/// it uploads content the descriptors already name.
struct Registry {
    data: StubTransportData,
    client: oci::Client,
    /// Every tag written so far — what the registry lists, and the version set
    /// the next push resolves its cascade against.
    tags: BTreeSet<String>,
}

impl Registry {
    fn new() -> Self {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let client = oci::Client::with_transport(Box::new(StubTransport::new(data.clone())));
        Self {
            data,
            client,
            tags: BTreeSet::new(),
        }
    }

    async fn publish(&mut self, publication: &Publication) {
        let identifier = identifier();
        // Exactly what `ocx package push --cascade` passes: every tag the
        // registry currently holds that parses as a version, rolling aliases
        // included.
        let others: BTreeSet<Version> = self.tags.iter().filter_map(|tag| Version::parse(tag)).collect();
        let (cascade_tags, _) = resolve_cascade_tags(
            &self.client,
            &identifier,
            &publication.version,
            &others,
            &publication.platform,
        )
        .await
        .expect("resolving cascade tags against the stub registry succeeds");

        // The primary tag first, then each resolved cascade tag. Nothing is
        // appended for `latest`: `resolve_cascade_tags` already carries the
        // track root when the version is latest-eligible.
        for tag in std::iter::once(publication.version.to_string()).chain(cascade_tags) {
            self.client
                .merge_platform_into_index(
                    &identifier,
                    tag.clone(),
                    &publication.platform,
                    &publication.digest(),
                    MANIFEST_SIZE,
                    &BTreeMap::new(),
                )
                .await
                .expect("the stub registry accepts the merged index");
            self.tags.insert(tag);
        }
    }

    /// Gathers the replayed graph exactly as `ocx package cascade check` would.
    async fn observe(&self) -> TagGraphObservation {
        self.data.write().tags = vec![self.tags.iter().cloned().collect()];
        assert_eq!(
            self.tags.len(),
            self.data.read().manifests.len(),
            "the tag list must name every manifest the replay wrote, and nothing else"
        );
        gather(&self.client, &identifier(), None, None)
            .await
            .expect("a replayed graph gathers")
    }

    /// Fails one tag's manifest fetch, the way a registry failing a single read
    /// mid-run does.
    fn fail_fetch(&self, tag: &str, message: &str) {
        self.data
            .write()
            .manifest_errors
            .insert(manifest_key(tag), message.to_string());
    }

    fn heal(&self) {
        self.data.write().manifest_errors.clear();
    }

    /// Deletes one platform entry from an alias index, in place at the
    /// registry — the damage the negative control needs.
    fn drop_entry(&self, tag: &str, platform_spec: &str) {
        let key = manifest_key(tag);
        let wanted = native_platform(platform_spec);
        let mut inner = self.data.write();
        let (bytes, _) = inner.manifests.get(&key).expect("the damaged tag exists").clone();
        let oci::Manifest::ImageIndex(mut index) =
            serde_json::from_slice::<oci::Manifest>(&bytes).expect("a replayed body parses")
        else {
            panic!("'{tag}' is not an index");
        };
        let before = index.manifests.len();
        index.manifests.retain(|entry| entry.platform.as_ref() != Some(&wanted));
        assert_eq!(
            index.manifests.len(),
            before - 1,
            "the damage must remove exactly one entry, or it is not the damage this test describes"
        );
        let body = serde_json::to_vec(&oci::Manifest::ImageIndex(index)).expect("an index serializes");
        let digest = oci::Algorithm::Sha256.hash(&body).to_string();
        inner.manifests.insert(key, (body, digest));
    }
}

// ── Reading the result ──────────────────────────────────────────

fn whole(observation: &TagGraphObservation) -> BTreeSet<AliasTag> {
    scope_filter(&observation.versions(), &[None]).expect("the whole graph is always a valid scope")
}

fn check(observation: &TagGraphObservation) -> CascadeReport {
    let expected = fold_expected(observation);
    diff(observation, &expected, &whole(observation))
}

fn slot_label(row: &SlotRow) -> String {
    let mut label = format!("{} {}/{}", row.tag, row.platform.os, row.platform.architecture);
    if let Some(features) = &row.platform.os_features {
        label.push('+');
        label.push_str(&features.join(","));
    }
    label
}

/// Everything a report says needs attention, rendered for a failure message.
fn findings(report: &CascadeReport) -> Vec<String> {
    let mut rendered: Vec<String> = report
        .rows
        .iter()
        .filter(|row| row.status != SlotStatus::Ok)
        .map(|row| {
            format!(
                "{} {:?} observed={:?} expected={:?} source={:?}",
                slot_label(row),
                row.status,
                row.observed,
                row.expected,
                row.source
            )
        })
        .collect();
    rendered.extend(
        report
            .aliases
            .iter()
            .filter(|(_, state)| *state != &AliasState::Present)
            .map(|(tag, state)| format!("{tag} {state:?}")),
    );
    rendered.extend(report.index_findings.iter().map(|finding| format!("{finding:?}")));
    rendered.extend(report.unrepairable.iter().map(|reason| format!("{reason:?}")));
    rendered
}

/// The alias tag `tag` names in this graph, classified the way gather does.
fn alias(observation: &TagGraphObservation, tag: &str) -> AliasTag {
    let variants: Vec<String> = observation
        .tags
        .keys()
        .filter_map(|alias| alias.track().map(str::to_string))
        .collect();
    AliasTag::parse(tag, &variants).unwrap_or_else(|| panic!("'{tag}' names no node of this graph"))
}

/// What `tag` carries for `platform_spec`, or `None` when it carries nothing.
fn digest_at(observation: &TagGraphObservation, tag: &str, platform_spec: &str) -> Option<String> {
    let wanted = native_platform(platform_spec);
    match &observation.tags.get(&alias(observation, tag))?.manifest {
        oci::Manifest::ImageIndex(index) => index
            .manifests
            .iter()
            .find(|entry| entry.platform.as_ref() == Some(&wanted))
            .map(|entry| entry.digest.clone()),
        oci::Manifest::Image(_) => None,
    }
}

/// Applies a repair plan the way the registry would, so the fixed point can be
/// asserted without a second round trip.
fn apply_in_memory(observation: &TagGraphObservation, writes: &[PlannedWrite]) -> TagGraphObservation {
    let mut applied = observation.clone();
    for write in writes {
        let body = serde_json::to_vec(&write.index).expect("a planned index serializes");
        applied.tags.insert(
            write.tag.clone(),
            ObservedTag {
                digest: oci::Algorithm::Sha256.hash(&body),
                size: i64::try_from(body.len()).expect("a planned body fits i64"),
                manifest: oci::Manifest::ImageIndex(write.index.clone()),
            },
        );
    }
    applied
}

// ── Orderings ───────────────────────────────────────────────────

/// Every ordering of `items`, enumerated deterministically.
///
/// Hand-rolled rather than pulled from a combinatorics crate: it is five lines,
/// and a failing ordering has to be reproducible from its position alone —
/// nothing here may consult a clock or a random source.
fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut orders = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(position);
        for mut order in permutations(&rest) {
            order.insert(0, item.clone());
            orders.push(order);
        }
    }
    orders
}

/// One hand-picked ordering, named by the positions it takes from `base`.
fn reordered(base: &[Publication], positions: &[usize]) -> Vec<Publication> {
    assert_eq!(
        positions.iter().copied().collect::<BTreeSet<_>>().len(),
        base.len(),
        "an ordering must be a permutation of the base set, not a subset or a repeat"
    );
    positions.iter().map(|&position| base[position].clone()).collect()
}

/// Replays every ordering and asserts each resulting graph reads back clean.
///
/// Returns one observation per ordering so a caller can add the positive
/// assertion that keeps its scenario non-vacuous — "clean" alone is also what
/// an empty graph reports.
async fn replay_all(scenario: &str, orders: &[Vec<Publication>]) -> Vec<TagGraphObservation> {
    assert!(!orders.is_empty(), "{scenario}: no orderings to replay");
    let labels: BTreeSet<String> = orders.iter().map(|order| order_label(order)).collect();
    assert_eq!(
        labels.len(),
        orders.len(),
        "{scenario}: the orderings must be distinct — a duplicate silently shrinks the coverage"
    );

    let mut observations = Vec::with_capacity(orders.len());
    for order in orders {
        let mut registry = Registry::new();
        for publication in order {
            registry.publish(publication).await;
        }
        let observation = registry.observe().await;
        let report = check(&observation);
        assert!(
            !report.has_findings(),
            "{scenario}: the push path and the fold disagree for order [{}]: {:?}",
            order_label(order),
            findings(&report)
        );
        observations.push(observation);
    }
    observations
}

// ── H1: two versions, two platforms, every order ────────────────

#[tokio::test]
async fn h1_two_versions_on_two_platforms_read_back_clean_in_every_order() {
    let base = vec![
        publication("3.28.0", "linux/amd64"),
        publication("3.28.0", "linux/arm64"),
        publication("3.28.1", "linux/amd64"),
        publication("3.28.1", "linux/arm64"),
    ];
    let orders = permutations(&base);
    assert_eq!(orders.len(), 24, "4 publications have 24 orderings");

    for observation in replay_all("H1", &orders).await {
        // Non-vacuity: a graph where `latest` never advanced would also report
        // clean if the fold were reading the alias instead of the leaves.
        assert_eq!(
            digest_at(&observation, "latest", "linux/amd64"),
            Some(publication("3.28.1", "linux/amd64").digest()),
            "latest must carry the newest version's amd64 content"
        );
        assert_eq!(
            digest_at(&observation, "3", "linux/arm64"),
            Some(publication("3.28.1", "linux/arm64").digest())
        );
    }
}

// ── H2: three versions across two minors, adversarial orders ────

#[tokio::test]
async fn h2_a_three_version_two_minor_shape_reads_back_clean_in_adversarial_orders() {
    let base = vec![
        publication("3.27.0", "linux/amd64"), // 0
        publication("3.27.0", "linux/arm64"), // 1
        publication("3.28.0", "linux/amd64"), // 2
        publication("3.28.0", "linux/arm64"), // 3
        publication("3.28.1", "linux/amd64"), // 4
        publication("3.28.1", "linux/arm64"), // 5
    ];
    // Twelve of the 720, chosen for the shapes that actually break a cascade:
    // a whole platform track landing before the other, the newest version
    // arriving first, and the middle version arriving last.
    let positions: [[usize; 6]; 12] = [
        [0, 1, 2, 3, 4, 5], // oldest first, platforms paired
        [5, 4, 3, 2, 1, 0], // reversed
        [0, 2, 4, 1, 3, 5], // the whole amd64 track, then the whole arm64 track
        [1, 3, 5, 0, 2, 4], // arm64 track first
        [4, 5, 2, 3, 0, 1], // newest version first
        [1, 0, 3, 2, 5, 4], // each version's arm64 before its amd64
        [4, 2, 0, 1, 3, 5], // amd64 descending, then arm64 ascending
        [0, 2, 4, 5, 3, 1], // amd64 ascending, then arm64 descending
        [2, 3, 0, 1, 4, 5], // the middle version first
        [0, 1, 4, 5, 2, 3], // the middle version last
        [0, 3, 4, 1, 2, 5], // interleaved across versions
        [5, 2, 1, 4, 3, 0], // interleaved the other way
    ];
    let orders: Vec<Vec<Publication>> = positions.iter().map(|positions| reordered(&base, positions)).collect();

    for observation in replay_all("H2", &orders).await {
        // 3.27 owns its own subtree while 3 and latest have moved on — the
        // case a global-max fold would get wrong.
        assert_eq!(
            digest_at(&observation, "3.27", "linux/amd64"),
            Some(publication("3.27.0", "linux/amd64").digest()),
            "3.27 must stay pinned to its own subtree's newest build"
        );
        assert_eq!(
            digest_at(&observation, "3", "linux/amd64"),
            Some(publication("3.28.1", "linux/amd64").digest())
        );
        assert_eq!(
            digest_at(&observation, "3.28", "linux/arm64"),
            Some(publication("3.28.1", "linux/arm64").digest())
        );
    }
}

// ── H3: the zig shape — a platform the newest version dropped ───

#[tokio::test]
async fn h3_a_platform_the_newest_version_dropped_is_not_a_finding_in_any_order() {
    // zig's shape: 0.15.0 shipped three platforms, 0.16.0 only two. The rolling
    // tags must keep serving darwin/arm64 from 0.15.0 — a fold that took the
    // newest version wholesale would call that entry an orphan and delete it.
    let base = vec![
        publication("0.15.0", "linux/amd64"),
        publication("0.15.0", "linux/arm64"),
        publication("0.15.0", "darwin/arm64"),
        publication("0.16.0", "linux/amd64"),
        publication("0.16.0", "linux/arm64"),
    ];
    let orders = permutations(&base);
    assert_eq!(orders.len(), 120, "5 publications have 120 orderings");

    for observation in replay_all("H3", &orders).await {
        assert_eq!(
            digest_at(&observation, "latest", "darwin/arm64"),
            Some(publication("0.15.0", "darwin/arm64").digest()),
            "the dropped platform must still be served from the older version"
        );
        assert_eq!(
            digest_at(&observation, "latest", "linux/amd64"),
            Some(publication("0.16.0", "linux/amd64").digest()),
            "and the platforms the newest version does ship must have advanced"
        );
        assert_eq!(
            digest_at(&observation, "0.16", "darwin/arm64"),
            None,
            "0.16 never shipped it"
        );
    }
}

// ── H4: two libc flavours of one os/arch ────────────────────────

#[tokio::test]
async fn h4_two_libc_flavours_of_one_arch_hold_independent_slots_in_every_order() {
    let base = vec![
        publication("1.0.0", "linux/amd64+libc.glibc"),
        publication("1.0.0", "linux/amd64+libc.musl"),
        publication("1.0.1", "linux/amd64+libc.glibc"),
    ];
    let orders = permutations(&base);
    assert_eq!(orders.len(), 6, "3 publications have 6 orderings");

    for observation in replay_all("H4", &orders).await {
        // Same os/arch, different libc: the newer version advances one slot and
        // leaves the other exactly where the older version put it.
        assert_eq!(
            digest_at(&observation, "latest", "linux/amd64+libc.glibc"),
            Some(publication("1.0.1", "linux/amd64+libc.glibc").digest())
        );
        assert_eq!(
            digest_at(&observation, "latest", "linux/amd64+libc.musl"),
            Some(publication("1.0.0", "linux/amd64+libc.musl").digest()),
            "the musl slot must not follow the glibc-only release"
        );
    }
}

// ── H5: variant tracks ──────────────────────────────────────────

#[tokio::test]
async fn h5_a_variant_track_and_the_default_track_stay_independent() {
    let base = vec![
        publication("1.0.0", "linux/amd64"),
        publication("debug-1.0.0", "linux/amd64"),
    ];
    let orders = permutations(&base);
    assert_eq!(orders.len(), 2);

    for observation in replay_all("H5", &orders).await {
        let default = digest_at(&observation, "latest", "linux/amd64");
        let debug = digest_at(&observation, "debug", "linux/amd64");
        assert_eq!(default, Some(publication("1.0.0", "linux/amd64").digest()));
        assert_eq!(debug, Some(publication("debug-1.0.0", "linux/amd64").digest()));
        assert_ne!(
            default, debug,
            "the two track roots must carry different content, or this scenario proves nothing"
        );
    }
}

// ── H6: a prerelease beside its release ─────────────────────────

#[tokio::test]
async fn h6_a_prerelease_never_reaches_latest_and_never_causes_a_finding() {
    let base = vec![
        publication("1.0.0-rc1", "linux/amd64"),
        publication("1.0.0", "linux/amd64"),
    ];
    let orders = permutations(&base);
    assert_eq!(orders.len(), 2);

    for observation in replay_all("H6", &orders).await {
        assert_eq!(
            digest_at(&observation, "latest", "linux/amd64"),
            Some(publication("1.0.0", "linux/amd64").digest()),
            "latest must carry the release, not the release candidate"
        );
        assert_ne!(
            digest_at(&observation, "latest", "linux/amd64"),
            Some(publication("1.0.0-rc1", "linux/amd64").digest())
        );
    }
}

// ── H7: two builds of one patch ─────────────────────────────────

#[tokio::test]
async fn h7_the_newer_build_owns_every_alias_whichever_build_landed_first() {
    let base = vec![
        publication("1.0.0_b1", "linux/amd64"),
        publication("1.0.0_b2", "linux/amd64"),
    ];
    let orders = permutations(&base);
    assert_eq!(orders.len(), 2);

    for observation in replay_all("H7", &orders).await {
        for tag in ["1.0.0", "1.0", "1", "latest"] {
            assert_eq!(
                digest_at(&observation, tag, "linux/amd64"),
                Some(publication("1.0.0_b2", "linux/amd64").digest()),
                "{tag} must carry the newer build"
            );
        }
    }
}

// ── H8: the negative control ────────────────────────────────────

#[tokio::test]
async fn h8_dropping_one_entry_from_a_clean_replay_reports_exactly_one_missing_row() {
    let mut registry = Registry::new();
    for publication in [
        publication("3.28.0", "linux/amd64"),
        publication("3.28.0", "linux/arm64"),
        publication("3.28.1", "linux/amd64"),
        publication("3.28.1", "linux/arm64"),
    ] {
        registry.publish(&publication).await;
    }

    let clean = check(&registry.observe().await);
    assert!(
        !clean.has_findings(),
        "the undamaged replay is the baseline this control is measured against: {:?}",
        findings(&clean)
    );

    registry.drop_entry("3", "linux/arm64");

    let damaged = check(&registry.observe().await);
    let broken: Vec<&SlotRow> = damaged.rows.iter().filter(|row| row.status != SlotStatus::Ok).collect();
    assert_eq!(
        broken.len(),
        1,
        "one deleted entry must produce one finding and nothing else: {:?}",
        findings(&damaged)
    );
    assert_eq!(broken[0].tag.to_string(), "3");
    assert_eq!(broken[0].platform, native_platform("linux/arm64"));
    assert_eq!(broken[0].status, SlotStatus::Missing);
    assert_eq!(
        broken[0].expected,
        Some(publication("3.28.1", "linux/arm64").digest()),
        "and it must name the content the alias should have carried"
    );
}

// ── H9: the swallowed blocker fetch, end to end ─────────────────

#[tokio::test]
async fn h9_a_swallowed_blocker_fetch_leaves_drift_that_check_finds_and_repair_closes() {
    let newest = publication("1.13.2", "linux/amd64");
    let older_amd64 = publication("1.13.1", "linux/amd64");
    let older_arm64 = publication("1.13.1", "linux/arm64");

    let mut registry = Registry::new();
    registry.publish(&newest).await;
    registry.publish(&older_amd64).await;
    // The ninja shape. 1.13.1's arm64 cascade has to check whether 1.13.2
    // blocks it — 1.13.2 ships no arm64, so it does not. That read failing
    // transiently is swallowed by `resolve_cascade_tags` by design: the push
    // reports success and the rolling tags never learn about arm64 at all.
    registry.fail_fetch("1.13.2", "503 service unavailable");
    registry.publish(&older_arm64).await;
    registry.heal();

    let observation = registry.observe().await;
    assert_eq!(
        digest_at(&observation, "1.13", "linux/arm64"),
        None,
        "the swallowed error must actually have stopped the cascade — without that this test asserts nothing"
    );
    assert_eq!(
        digest_at(&observation, "1.13.1", "linux/arm64"),
        Some(older_arm64.digest()),
        "the version tag itself still received the content, which is what makes the drift repairable"
    );

    let expected = fold_expected(&observation);
    let report = diff(&observation, &expected, &whole(&observation));
    assert!(report.has_findings());
    let mut broken: Vec<String> = report
        .rows
        .iter()
        .filter(|row| row.status != SlotStatus::Ok)
        .map(|row| format!("{} {:?}", slot_label(row), row.status))
        .collect();
    broken.sort();
    assert_eq!(
        broken,
        vec![
            "1 linux/arm64 Missing".to_string(),
            "1.13 linux/arm64 Missing".to_string(),
            "latest linux/arm64 Missing".to_string(),
        ],
        "every alias the cascade never reached must be reported, and nothing else: {:?}",
        findings(&report)
    );

    let writes = plan_repairs(&report, &observation, &expected);
    let served: BTreeSet<String> = observation
        .tags
        .values()
        .filter_map(|tag| match &tag.manifest {
            oci::Manifest::ImageIndex(index) => Some(index.manifests.iter().map(|entry| entry.digest.clone())),
            oci::Manifest::Image(_) => None,
        })
        .flatten()
        .collect();
    for write in &writes {
        for entry in &write.index.manifests {
            assert!(
                served.contains(&entry.digest),
                "a repair re-points at content the registry already serves; {} is not one: {served:?}",
                entry.digest
            );
        }
    }

    let repaired = apply_in_memory(&observation, &writes);
    let after = check(&repaired);
    assert!(
        !after.has_findings(),
        "applying the plan must close every finding: {:?}",
        findings(&after)
    );
    assert!(
        plan_repairs(&after, &repaired, &fold_expected(&repaired)).is_empty(),
        "and the repaired graph must be a fixed point"
    );
    assert_eq!(
        digest_at(&repaired, "1.13", "linux/arm64"),
        Some(older_arm64.digest()),
        "the restored slot must be the older version's content, not something invented"
    );
}
