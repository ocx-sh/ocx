// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The exposed-name algebra — one function, four consumers (plan contract
//! C-021, `plan_toolchain_activation.md`).
//!
//! [`exposed_names`] replaces `prepare_lazy`'s former `interface_shim_names`,
//! which discarded which package claimed which name. `ocx env`, `ocx inspect
//! --closure`, [`super::prepare_lazy`], and the rendered `bin/` (WP-7) all
//! need the *same* answer to "what names does this closure expose, and who
//! owns each one" — one algebra, computed once, never a directory scan
//! (C-023).

use std::collections::{BTreeMap, BTreeSet};

use crate::oci;
use crate::package::metadata::{Binaries, BinaryName, Entrypoints};
use crate::package_manager::composer;
use crate::package_manager::error::PackageErrorKind;

use super::common::ClosureNode;
use super::inspect;

/// The admission surface [`exposed_names`] gates on: the **interface**
/// surface, never the private one (C-023 — a shim or a rendered launcher is a
/// consumer-facing artifact, never the package's own private view). Named
/// rather than a bare `false` at its three call sites, matching the constant
/// the base `interface_shim_names` carried before this module replaced it.
const INTERFACE_SURFACE: bool = false;

/// How [`exposed_names`] treats a node that claims neither `binaries` nor
/// entry points (C-022).
///
/// A *deferred* `prepare_lazy` call has nothing else to fall back on — an
/// unenumerable node means no shim tree can be generated, full stop — while a
/// render already knows the tree is a projection of whatever metadata is
/// declared: a node contributing nothing is ordinary, not a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotEnumerablePolicy {
    /// Refuse the whole call — [`PackageErrorKind::ShimNamesNotEnumerable`].
    Refuse,
    /// Skip the node; it contributes no names.
    ///
    /// No production caller constructs this yet — the render path (WP-7,
    /// `render_toolchain.rs`) is the one that passes `Skip`; `prepare_lazy`
    /// always passes `Refuse`. The `expect` below is scoped to non-test
    /// builds only: this crate's own tests already construct `Skip`, which
    /// would make an unscoped `expect` unfulfilled (and therefore itself a
    /// hard error) under `--all-targets`.
    Skip,
}

/// Who claims one exposed name, and what else claimed it (C-021, C-024).
///
/// Collisions never refuse, warn, or rise above debug level — the last tool
/// walked wins, matching composed-PATH order, and [`Self::shadowed`] is the
/// `ocx inspect` collision row's source, not a refusal payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameOwner {
    /// The tool whose claim won.
    pub tool: oci::PinnedIdentifier,
    /// Whether `tool` is the closure's own root, as opposed to an
    /// interface-admitted dependency.
    pub is_root: bool,
    /// Every other tool that also claimed this name and lost, oldest walked
    /// first.
    pub shadowed: Vec<oci::PinnedIdentifier>,
    /// The position of the winning claim in the merged multi-root node slice
    /// [`exposed_names`] walked (RUL-12), zero-based.
    ///
    /// A [`BTreeMap`]'s only total order is its key order, which is not walk
    /// order — and ASCII lowercase sorts above uppercase, so a fold keyed on
    /// the map's own iteration order would always pick an all-lowercase
    /// spelling, making the tool that owns `bin/make` differ from the one
    /// that wins on the composed `PATH`. `walk_index` (RUL-32) is what lets
    /// [`fold_case_insensitive`] recover "last walked" as a real total order
    /// after the map has already discarded the slice's own order.
    pub walk_index: usize,
}

/// The exposed-name set: `binaries` ∪ `entrypoints` of the default group's
/// roots and every interface-admitted dependency (C-021, C-023).
///
/// The closure is pre-filtered by
/// [`inspect::admitted_on_surface`](super::inspect::admitted_on_surface)
/// (interface surface — a shim or a rendered launcher is a **consumer**-facing
/// artifact, never the package's own private view), then unioned through
/// [`composer::carrier_crosses`](crate::package_manager::composer::carrier_crosses).
/// Never a directory scan: a node with no `binaries` claim and no entry points
/// contributes no names, per `policy` (C-022).
///
/// A collision between two nodes' claims never refuses or warns (C-024): the
/// last node walked wins, matching composed-PATH order, and the losing
/// claim(s) are recorded on the winning [`NameOwner::shadowed`] for `ocx
/// inspect` to report.
///
/// **The returned map is unfolded.** `Make` and `make` are two distinct keys
/// here, even on a case-insensitive filesystem — this function never reads
/// host filesystem case sensitivity. C-025's case-fold collapse is a separate
/// pure step, [`fold_case_insensitive`], that the renderer calls on this
/// map's output; keeping the two apart is what lets the fold be unit-tested
/// deterministically on both hosts, with no `cfg!` inside either function.
///
/// # Errors
///
/// - [`PackageErrorKind::ShimNamesNotEnumerable`] — a node claims neither
///   `binaries` nor entry points and `policy` is
///   [`NotEnumerablePolicy::Refuse`] (C-022).
/// - [`PackageErrorKind::ShimNameInvalid`] — a declared entry point name does
///   not survive conversion to a [`BinaryName`] (every Windows-reserved device
///   name is one).
pub(crate) fn exposed_names(
    nodes: &[ClosureNode],
    policy: NotEnumerablePolicy,
) -> Result<BTreeMap<BinaryName, NameOwner>, PackageErrorKind> {
    let mut exposed: BTreeMap<BinaryName, NameOwner> = BTreeMap::new();

    for (walk_index, node) in nodes.iter().enumerate() {
        // C-023: the pre-filter is the shared interface-surface admission —
        // never a re-implementation, and never the private surface (E-14).
        if !inspect::admitted_on_surface(node, INTERFACE_SURFACE) {
            continue;
        }

        // `Some(empty)` is enumerable (E-03) — the publisher asserted zero
        // interface executables — and contributes no name, which is still a
        // complete tree. Only `None` *and* no entry points leaves nothing to
        // enumerate.
        if node.binaries.is_none() && node.entrypoints.is_empty() {
            match policy {
                NotEnumerablePolicy::Refuse => {
                    return Err(PackageErrorKind::ShimNamesNotEnumerable {
                        package: node.identifier.clone(),
                    });
                }
                NotEnumerablePolicy::Skip => continue,
            }
        }

        // A name claimed on both axes by this node is one claim, not two
        // (E-08) — collected into a set before recording so a node can never
        // shadow itself across its own two axes (RUL-11).
        let mut claimed: BTreeSet<BinaryName> = BTreeSet::new();

        if let Some(binaries) = &node.binaries
            && composer::carrier_crosses(Binaries::IMPLICIT_VISIBILITY, node.is_root, INTERFACE_SURFACE)
        {
            claimed.extend(binaries.iter().cloned());
        }

        if !node.entrypoints.is_empty()
            && composer::carrier_crosses(Entrypoints::IMPLICIT_VISIBILITY, node.is_root, INTERFACE_SURFACE)
        {
            for entrypoint in &node.entrypoints {
                // Not total: every Windows-reserved device name is a valid
                // slug and none is a valid `BinaryName` (E-09). Refusing
                // beats skipping under every policy — a quietly incomplete
                // name set is the failure C-022 exists to prevent.
                claimed.insert(BinaryName::try_from(entrypoint.as_str()).map_err(PackageErrorKind::ShimNameInvalid)?);
            }
        }

        for name in claimed {
            record_claim(&mut exposed, name, node, walk_index);
        }
    }

    Ok(exposed)
}

/// Records one node's claim on `name`, resolving a collision by last-walked
/// wins (C-024, RUL-11, RUL-12).
///
/// `node` claiming a name it already owns (its own second axis is filtered
/// before this is called; a shared dependency reached twice through a merged
/// multi-root slice, E-34, is not) is not a collision: the identifier is
/// updated in place — `walk_index` and `is_root` travel with the freshest
/// sighting — and nothing is pushed onto `shadowed`, or the node would shadow
/// itself.
///
/// Otherwise the incoming node wins (it is walked later): every prior
/// owner's own identifier, plus whatever it had already accumulated in its
/// own `shadowed`, moves onto the new owner's `shadowed`, oldest first — with
/// `node.identifier` itself dropped from that inherited list first. Without
/// that filter, a name claimed by an **interleaved** repeat of the same
/// identifier (`[A, B, A]`, all claiming one name — reachable the moment
/// RUL-12's merged multi-root slice exists, WP-7) would make `A` shadow
/// itself: `B` shadows `A` at the middle step, and `A` winning again at the
/// end would otherwise carry that `A` entry straight back onto its own
/// winning `shadowed`.
///
/// A collision is never a refusal, a warning, or silence (C-024) — it emits
/// exactly one debug line naming the name, the winner and the loser.
fn record_claim(
    exposed: &mut BTreeMap<BinaryName, NameOwner>,
    name: BinaryName,
    node: &ClosureNode,
    walk_index: usize,
) {
    let logged_name = name.clone();
    exposed
        .entry(name)
        .and_modify(|owner| {
            if owner.tool == node.identifier {
                owner.is_root = node.is_root;
                owner.walk_index = walk_index;
                return;
            }
            crate::log::debug!(
                "name '{logged_name}' claimed by '{}', shadowing '{}' — last walked wins (C-024)",
                node.identifier,
                owner.tool
            );
            let mut shadowed = std::mem::take(&mut owner.shadowed);
            shadowed.retain(|id| *id != node.identifier);
            shadowed.push(owner.tool.clone());
            *owner = NameOwner {
                tool: node.identifier.clone(),
                is_root: node.is_root,
                shadowed,
                walk_index,
            };
        })
        .or_insert_with(|| NameOwner {
            tool: node.identifier.clone(),
            is_root: node.is_root,
            shadowed: Vec::new(),
            walk_index,
        });
}

/// Collapses [`exposed_names`]'s unfolded map onto the case-folded key
/// (C-025), for the renderer only — `exposed_names` itself never does this
/// (see its doc).
///
/// The fold key is **ASCII** case (C-015's own charset rule folds the same
/// way), never Unicode `to_lowercase`: a Unicode fold would collapse
/// characters ASCII case-insensitivity does not (e.g. Turkish dotted/dotless
/// İ/i), which is not what a case-insensitive *filesystem* does.
///
/// Same collision rule as [`exposed_names`], one level up: the winner on a
/// shared folded key is the entry with the greatest recorded
/// [`NameOwner::walk_index`] among the colliding entries — **never** the
/// greatest key (RUL-32; a `BTreeMap`'s only total order is key order, and
/// ASCII lowercase sorts above uppercase, which would make an all-lowercase
/// spelling always win). Every losing owner's identifier — plus its own prior
/// [`NameOwner::shadowed`] — is appended to the winner's `shadowed`,
/// oldest-first (RUL-11), **excluding any entry equal to the winner's own
/// tool** — reachable when one node claims both case twins on its own two
/// axes (`binaries = ["Make"]`, `entrypoints = ["make"]`), which would
/// otherwise make the winner shadow itself. Pure: no I/O, no
/// `cfg!(target_os)` — the host question ("is this filesystem
/// case-insensitive") is answered by the caller, not here, which is what
/// makes this fold unit-testable deterministically on every host.
pub(crate) fn fold_case_insensitive(names: BTreeMap<BinaryName, NameOwner>) -> BTreeMap<BinaryName, NameOwner> {
    // Group by the ASCII-folded key first — a `BTreeMap`'s only total order
    // is its key order, which the module doc above and RUL-32 both name as
    // the wrong order to resolve a collision on.
    let mut groups: BTreeMap<String, Vec<(BinaryName, NameOwner)>> = BTreeMap::new();
    for (key, owner) in names {
        groups
            .entry(key.as_str().to_ascii_lowercase())
            .or_default()
            .push((key, owner));
    }

    let mut folded = BTreeMap::new();
    for (_, mut group) in groups {
        if group.len() == 1 {
            let (key, owner) = group.pop().expect("a group of length one has one entry");
            folded.insert(key, owner);
            continue;
        }

        // RUL-32: the winner is the entry with the greatest `walk_index`,
        // never the greatest key. On a genuine TIE — reachable only when one
        // node claims both case twins on its own two axes, so both entries
        // carry the same `walk_index` — the key is an explicit, stated
        // tie-break rather than an accidental fallthrough to however this
        // group's vec happened to be built. Which spelling *should* survive
        // in that one-node case is an open question (D-1, deferred to the
        // owner); this only makes the fallback deterministic and documented
        // instead of silent.
        group.sort_by_key(|(key, owner)| (owner.walk_index, key.clone()));
        let (winning_key, mut winner) = group.pop().expect("a colliding group has at least two entries");

        // Every loser, oldest walked first (RUL-11): its own identifier plus
        // whatever it had already accumulated in its own `shadowed` moves
        // onto the winner's — EXCEPT an entry equal to the winner's own tool.
        // That is reachable whenever one node claims both case twins (its two
        // keys collapse to one folded group with the *same* owner): without
        // this guard the winner would shadow itself, either directly (the
        // other entry is literally the same owner) or through an inherited
        // `shadowed` list from a raw collision `exposed_names` already
        // resolved in the winner's favour under a different unfolded key. The
        // winner's own prior `shadowed` is appended last — the relative order
        // between an inherited entry and its owning loser's own identifier is
        // not otherwise fixed.
        let mut shadowed = Vec::new();
        for (_, loser) in group {
            if loser.tool == winner.tool {
                continue;
            }
            shadowed.extend(loser.shadowed.into_iter().filter(|id| *id != winner.tool));
            shadowed.push(loser.tool);
        }
        shadowed.extend(
            std::mem::take(&mut winner.shadowed)
                .into_iter()
                .filter(|id| *id != winner.tool),
        );
        winner.shadowed = shadowed;

        // RUL-19: the surviving key is the winner's OWN spelling
        // (`winning_key`), never the folded one — the fold stops a second
        // write, it never renames a tool.
        folded.insert(winning_key, winner);
    }

    folded
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::package::metadata::visibility::Visibility;
    use crate::package::metadata::{Binaries, EntrypointName};

    // ── Fixtures ────────────────────────────────────────────────────────────
    //
    // Moved here from `prepare_lazy.rs`'s test module together with the nine
    // `exposed_names_*` tests below (S-11): validation item 16 and the plan's
    // *Contract coverage* row both name `tasks/toolchain_names.rs` as the home
    // of the C-021/C-022/C-023 unit cases, and the `fold_case_insensitive`
    // cases need the same node/owner fixtures.

    /// An arbitrary valid SHA-256 hex, built from a one-byte seed so each
    /// fixture node can carry a digest distinguishable from its neighbours'.
    fn digest_from(seed: &str) -> oci::Digest {
        oci::Digest::Sha256(seed.repeat(32))
    }

    fn pinned(repository: &str, seed: &str) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, "example.com").clone_with_digest(digest_from(seed)),
        )
        .expect("digest-bearing identifier is pinned")
    }

    fn name(value: &str) -> BinaryName {
        BinaryName::try_from(value).expect("fixture binary name is valid")
    }

    fn binaries(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names.iter().map(|n| name(n)).collect();
        Binaries::try_from(set).expect("fixture binaries claim is valid")
    }

    fn entrypoints(names: &[&str]) -> Vec<EntrypointName> {
        names
            .iter()
            .map(|n| EntrypointName::try_from((*n).to_string()).expect("fixture entrypoint name is valid"))
            .collect()
    }

    /// A closure node carrying only what the interface-surface name set is
    /// derived from; every other field is the inert value for this axis.
    fn node(
        identifier: oci::PinnedIdentifier,
        claimed: Option<&[&str]>,
        entries: &[&str],
        is_root: bool,
    ) -> ClosureNode {
        ClosureNode {
            config_digest: identifier.digest(),
            identifier,
            effective_visibility: None,
            binaries: claimed.map(binaries),
            entrypoints: entrypoints(entries),
            env: Vec::new(),
            integrations: Vec::new(),
            dependencies: Vec::new(),
            is_root,
        }
    }

    /// A **dependency** node (`is_root = false`) carrying an explicit
    /// effective visibility — the field `admitted_on_surface` gates on
    /// (C-023). A root has none, which is why [`node`] leaves it `None`.
    fn dependency(
        identifier: oci::PinnedIdentifier,
        claimed: Option<&[&str]>,
        entries: &[&str],
        effective: Visibility,
    ) -> ClosureNode {
        let mut node = node(identifier, claimed, entries, false);
        node.effective_visibility = Some(effective);
        node
    }

    /// The name keys of an [`exposed_names`] result, for assertions that only
    /// care about the set, not ownership.
    fn owned_names(map: &BTreeMap<BinaryName, NameOwner>) -> Vec<&str> {
        map.keys().map(BinaryName::as_str).collect()
    }

    /// The owner of one name, failing with the whole key set when absent so a
    /// red names what *was* exposed instead of just "None".
    fn owner_at<'a>(map: &'a BTreeMap<BinaryName, NameOwner>, key: &str) -> &'a NameOwner {
        map.get(&name(key))
            .unwrap_or_else(|| panic!("'{key}' must be exposed; the set was {:?}", owned_names(map)))
    }

    /// A [`NameOwner`] with no shadowed rivals, for building
    /// [`fold_case_insensitive`] inputs directly. `walk_index` is a
    /// placeholder — [`map_of`] overwrites it from the fixture's own
    /// construction order, which is what "last walked" means in these tests.
    fn owner_of(tool: oci::PinnedIdentifier, is_root: bool) -> NameOwner {
        NameOwner {
            tool,
            is_root,
            shadowed: Vec::new(),
            walk_index: 0,
        }
    }

    /// The publisher asserting **zero** executables, distinct from `None`
    /// ("no claim at all"). Named because `Some(&[])` has no element type to
    /// infer at the call site.
    const ASSERTED_EMPTY: &[&str] = &[];

    /// Builds a [`fold_case_insensitive`] input map from `entries`, assigning
    /// each owner's `walk_index` (RUL-32) from its position in `entries` —
    /// the vec's own order stands in for "walked order" in every fold
    /// fixture, so listing an owner earlier in `entries` means it was walked
    /// earlier, regardless of where its key then sorts in the `BTreeMap`.
    fn map_of(entries: Vec<(&str, NameOwner)>) -> BTreeMap<BinaryName, NameOwner> {
        entries
            .into_iter()
            .enumerate()
            .map(|(walk_index, (key, mut owner))| {
                owner.walk_index = walk_index;
                (name(key), owner)
            })
            .collect()
    }

    // ── C-021 / C-023: the claim axes ───────────────────────────────────────

    /// C-021/C-023 (E-06): the name set is `binaries ∪ entrypoints`.
    #[test]
    fn exposed_names_unions_binaries_and_entrypoint_names() {
        let nodes = vec![node(pinned("ns/cmake", "a"), Some(&["cmake"]), &["ctest"], true)];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        assert_eq!(owned_names(&set), vec!["cmake", "ctest"]);
    }

    /// C-021 (E-06, single axis): a `binaries` claim alone yields exactly
    /// those names — the union's other operand contributes nothing rather
    /// than, say, defaulting to the entry point set.
    #[test]
    fn exposed_names_admits_a_node_claiming_only_binaries() {
        let nodes = vec![node(pinned("ns/cmake", "a"), Some(&["cmake", "cpack"]), &[], true)];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("a binaries claim alone is enumerable");

        assert_eq!(owned_names(&set), vec!["cmake", "cpack"]);
    }

    /// C-021 (E-08, key half): "a name claimed on both axes yields exactly one
    /// launcher" — the set is flat, so `bin/` never holds two entries for one
    /// name.
    #[test]
    fn exposed_names_yields_one_name_when_both_axes_claim_it() {
        let nodes = vec![node(pinned("ns/cmake", "a"), Some(&["cmake"]), &["cmake"], true)];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        assert_eq!(
            owned_names(&set),
            vec!["cmake"],
            "a doubly-claimed name must appear once"
        );
    }

    /// RUL-11 (E-08, `shadowed` half): a node's two axes are one claimant, so
    /// the name it claims on both must NOT record the node as its own
    /// shadowed rival. Discriminates a fold that pushes every observed claim
    /// onto `shadowed` before checking whether the winner is the same tool.
    #[test]
    fn exposed_names_never_shadows_a_node_with_its_own_second_axis() {
        let claiming = pinned("ns/cmake", "a");
        let nodes = vec![node(claiming.clone(), Some(&["cmake"]), &["cmake"], true)];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        let owner = owner_at(&set, "cmake");
        assert_eq!(owner.tool, claiming);
        assert!(
            owner.shadowed.is_empty(),
            "a node must not shadow itself across its own two axes, got {:?}",
            owner.shadowed
        );
    }

    // ── C-022: the two `NotEnumerablePolicy` arms ───────────────────────────

    /// C-022 (E-01): a node claiming neither `binaries` nor entry points makes
    /// the name set non-enumerable under `Refuse` — `prepare_lazy`'s policy —
    /// and the error names *that node*, which may be a dependency, not the
    /// tool the user asked for.
    #[test]
    fn exposed_names_refuses_a_node_claiming_neither_binaries_nor_entrypoints_under_refuse() {
        let silent = pinned("ns/zlib", "b");
        let nodes = vec![
            dependency(silent.clone(), None, &[], Visibility::INTERFACE),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let error =
            exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect_err("a non-enumerable closure is refused");

        match error {
            PackageErrorKind::ShimNamesNotEnumerable { package } => {
                assert_eq!(package, silent, "the refusal must name the node that claims nothing");
            }
            other => panic!("expected ShimNamesNotEnumerable, got {other:?}"),
        }
    }

    /// C-022 (E-02): the identical closure under `Skip` — a render's policy —
    /// contributes no name for the silent node instead of refusing the whole
    /// call.
    #[test]
    fn exposed_names_skips_a_node_claiming_neither_binaries_nor_entrypoints_under_skip() {
        let nodes = vec![
            dependency(pinned("ns/zlib", "b"), None, &[], Visibility::INTERFACE),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Skip).expect("Skip never refuses");

        assert_eq!(owned_names(&set), vec!["cmake"]);
    }

    /// C-022 (E-05): the silent node is the **root**, not a dependency. Same
    /// variant, and `package` names the root — an implementation that only
    /// examined dependencies (the arm the sibling test exercises) would return
    /// `Ok` here.
    #[test]
    fn exposed_names_refuses_a_silent_root_under_refuse() {
        let silent_root = pinned("ns/meta", "a");
        let nodes = vec![
            dependency(
                pinned("ns/zlib", "b"),
                Some(&["zlib-flate"]),
                &[],
                Visibility::INTERFACE,
            ),
            node(silent_root.clone(), None, &[], true),
        ];

        let error = exposed_names(&nodes, NotEnumerablePolicy::Refuse)
            .expect_err("a silent root refuses just like a silent dependency");

        match error {
            PackageErrorKind::ShimNamesNotEnumerable { package } => {
                assert_eq!(package, silent_root, "the refusal must name the root");
            }
            other => panic!("expected ShimNamesNotEnumerable, got {other:?}"),
        }
    }

    /// C-022 (E-03): `binaries = Some([])` is the publisher **asserting zero
    /// executables**, which is enumerable. It contributes no name and is never
    /// a refusal, under either policy — the distinction `Option::is_none`
    /// carries and `Binaries::is_empty` does not.
    #[test]
    fn exposed_names_treats_an_empty_binaries_claim_as_enumerable_under_both_policies() {
        let asserted_empty = || {
            vec![
                dependency(pinned("ns/zlib", "b"), Some(ASSERTED_EMPTY), &[], Visibility::INTERFACE),
                node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
            ]
        };

        for policy in [NotEnumerablePolicy::Refuse, NotEnumerablePolicy::Skip] {
            let set = exposed_names(&asserted_empty(), policy)
                .unwrap_or_else(|e| panic!("an asserted-empty claim is enumerable under {policy:?}, got {e:?}"));
            assert_eq!(owned_names(&set), vec!["cmake"], "under {policy:?}");
        }
    }

    /// C-022 / C-044 (E-04): every node silent, under `Skip` → an empty map
    /// and no error. The render still writes a complete (empty) `bin/`, so
    /// "nothing to expose" must be a value, never a refusal.
    #[test]
    fn exposed_names_returns_an_empty_map_when_every_node_is_silent_under_skip() {
        let nodes = vec![
            dependency(pinned("ns/zlib", "b"), None, &[], Visibility::INTERFACE),
            node(pinned("ns/meta", "a"), None, &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Skip).expect("Skip never refuses");

        assert!(set.is_empty(), "expected no names, got {:?}", owned_names(&set));
    }

    /// C-022 / F-8 (E-07): the refusal fires only when a node has **no**
    /// `binaries` **and** no entry points. A node declaring entry points and no
    /// `binaries` claim is perfectly enumerable — keying the refusal on
    /// `Surface::binaries_complete` would over-refuse it.
    #[test]
    fn exposed_names_admits_a_node_with_entrypoints_and_no_binaries_claim() {
        let nodes = vec![node(pinned("ns/cmake", "a"), None, &["cmake"], true)];

        let set =
            exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("entry points alone make the set enumerable");

        assert_eq!(owned_names(&set), vec!["cmake"]);
    }

    /// C-021 F-5 (E-09, `Refuse` arm): every Windows-reserved device name is a
    /// valid slug — hence a valid `EntrypointName` — and none is a valid
    /// `BinaryName`.
    #[test]
    fn exposed_names_refuses_an_entrypoint_name_that_is_not_a_valid_binary_name() {
        // Guards the premise: `nul` really is a legal entry point name today.
        EntrypointName::try_from("nul".to_string()).expect("'nul' is a valid slug, hence a valid EntrypointName");
        assert!(BinaryName::try_from("nul").is_err(), "'nul' must not be a BinaryName");

        let nodes = vec![node(pinned("ns/tool", "a"), Some(&["tool"]), &["nul"], true)];

        let error = exposed_names(&nodes, NotEnumerablePolicy::Refuse)
            .expect_err("an unconvertible entry point name refuses the package");

        assert!(
            matches!(error, PackageErrorKind::ShimNameInvalid(_)),
            "expected ShimNameInvalid, got {error:?}"
        );
    }

    /// C-021 F-5 (E-09, `Skip` arm — the discriminating one). The refusal is
    /// "regardless of `policy`": `Skip` skips a node that claims *nothing*, not
    /// a node whose claim cannot be rendered. Folding the two into one
    /// "tolerant" arm would publish a quietly incomplete `bin/`.
    #[test]
    fn exposed_names_refuses_an_invalid_entrypoint_name_under_skip_too() {
        let nodes = vec![node(pinned("ns/tool", "a"), Some(&["tool"]), &["nul"], true)];

        let error = exposed_names(&nodes, NotEnumerablePolicy::Skip)
            .expect_err("an unconvertible entry point name refuses under every policy");

        assert!(
            matches!(error, PackageErrorKind::ShimNameInvalid(_)),
            "expected ShimNameInvalid, got {error:?}"
        );
    }

    // ── C-023: the admission surface ────────────────────────────────────────

    /// C-023 (E-10): the set is the *closure's* interface surface, so an
    /// interface-admitted dependency's claims are in it too — not just the
    /// root's.
    #[test]
    fn exposed_names_unions_across_every_interface_admitted_node() {
        let nodes = vec![
            dependency(
                pinned("ns/zlib", "b"),
                Some(&["zlib-flate"]),
                &[],
                Visibility::INTERFACE,
            ),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        assert_eq!(owned_names(&set), vec!["cmake", "zlib-flate"]);
    }

    /// C-021/C-023 (E-12, E-13, E-20): `is_root` discriminates the two
    /// claimants. The root carries no `effective_visibility` at all and is
    /// admitted at depth 0 unconditionally; the dependency is admitted only
    /// through its edge. Both axes cross for both — `binaries` under
    /// `Binaries::IMPLICIT_VISIBILITY` (PUBLIC), entry points under
    /// `Entrypoints::IMPLICIT_VISIBILITY` (INTERFACE) — so all four cells are
    /// asserted in one place.
    #[test]
    fn exposed_names_marks_the_root_as_root_and_an_admitted_dependency_as_not() {
        let root = pinned("ns/cmake", "a");
        let dep = pinned("ns/zlib", "b");
        let nodes = vec![
            dependency(
                dep.clone(),
                Some(&["zlib-flate"]),
                &["zlib-inspect"],
                Visibility::INTERFACE,
            ),
            node(root.clone(), Some(&["cmake"]), &["ctest"], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        for key in ["cmake", "ctest"] {
            let owner = owner_at(&set, key);
            assert_eq!(owner.tool, root, "'{key}' is the root's claim");
            assert!(owner.is_root, "'{key}' is claimed by the root, so is_root must be true");
        }
        for key in ["zlib-flate", "zlib-inspect"] {
            let owner = owner_at(&set, key);
            assert_eq!(owner.tool, dep, "'{key}' is the dependency's claim");
            assert!(
                !owner.is_root,
                "'{key}' is claimed by a dependency, so is_root must be false"
            );
        }
    }

    /// C-023 (E-11, first half): a dependency the interface surface does not
    /// admit contributes nothing, even though its `binaries` claim is
    /// perfectly valid. `SEALED` propagates on neither axis.
    #[test]
    fn exposed_names_drops_a_dependency_that_is_not_interface_admitted() {
        let nodes = vec![
            dependency(pinned("ns/zlib", "b"), Some(&["zlib-flate"]), &[], Visibility::SEALED),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        assert_eq!(
            owned_names(&set),
            vec!["cmake"],
            "a sealed dependency's claim never reaches the consumer surface"
        );
    }

    /// C-022 + C-023 (E-11, second half — and it fixes the order of two
    /// operations). `exposed_names` now owns the admission filter that used to
    /// live at its caller, so *filter then enumerate* versus *enumerate then
    /// filter* is this function's own choice. A sealed dependency claiming
    /// nothing must be dropped silently: refusing the whole tool because an
    /// invisible node is silent would make `prepare_lazy` fail on closures
    /// eager composition handles fine.
    #[test]
    fn exposed_names_does_not_refuse_for_a_silent_dependency_it_would_drop_anyway() {
        let nodes = vec![
            dependency(pinned("ns/zlib", "b"), None, &[], Visibility::SEALED),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse)
            .expect("a node that is filtered out cannot make the set non-enumerable");

        assert_eq!(owned_names(&set), vec!["cmake"]);
    }

    /// C-023 (E-14): the pre-filter surface is `admitted_on_surface(node,
    /// /* self_view = */ false)`, never `true`. A `PRIVATE` dependency is
    /// admitted on the *private* surface and refused on the interface one, so
    /// this row is exactly what reds an implementation that passed
    /// `self_view = true` — `SEALED` alone cannot discriminate, since it is
    /// refused on both.
    #[test]
    fn exposed_names_filters_on_the_interface_surface_never_the_private_one() {
        let nodes = vec![
            dependency(pinned("ns/zlib", "b"), Some(&["zlib-flate"]), &[], Visibility::PRIVATE),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an enumerable closure yields a name set");

        assert_eq!(
            owned_names(&set),
            vec!["cmake"],
            "a private-only dependency crosses the self surface, never the shim tree's"
        );
    }

    // ── C-024 / RUL-11 / RUL-12: collisions ─────────────────────────────────

    /// C-024 + RUL-11 (E-15): two tools, one name — the **later**-walked wins,
    /// matching composed-PATH order, and the earlier is recorded rather than
    /// discarded. Never an `Err`.
    #[test]
    fn exposed_names_lets_the_last_walked_of_two_claimants_win() {
        let first = pinned("ns/first", "b");
        let last = pinned("ns/last", "c");
        let nodes = vec![
            dependency(first.clone(), Some(&["make"]), &[], Visibility::INTERFACE),
            dependency(last.clone(), Some(&["make"]), &[], Visibility::INTERFACE),
            node(pinned("ns/root", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("a collision is never a refusal");

        let owner = owner_at(&set, "make");
        assert_eq!(owner.tool, last, "the last node walked wins the name");
        assert_eq!(owner.shadowed, vec![first], "the loser is recorded, not dropped");
    }

    /// RUL-11 verbatim (E-16): three claimants — `tool` is the last, and
    /// `shadowed` carries the other two **oldest walked first**. Reds a
    /// `Vec::push`-onto-the-front or a `BTreeSet`-shaped accumulator.
    #[test]
    fn exposed_names_records_three_claimants_shadowed_oldest_first() {
        let first = pinned("ns/first", "b");
        let second = pinned("ns/second", "c");
        let third = pinned("ns/third", "d");
        let nodes = vec![
            dependency(first.clone(), Some(&["make"]), &[], Visibility::INTERFACE),
            dependency(second.clone(), Some(&["make"]), &[], Visibility::INTERFACE),
            dependency(third.clone(), Some(&["make"]), &[], Visibility::INTERFACE),
            node(pinned("ns/root", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("a collision is never a refusal");

        let owner = owner_at(&set, "make");
        assert_eq!(owner.tool, third);
        assert_eq!(owner.shadowed, vec![first, second], "oldest walked first (RUL-11)");
    }

    /// C-024 + RUL-12 (E-17): "last walked" is the **slice** order, and RUL-12
    /// fixes that slice as the one merged multi-root node list. Reversing the
    /// input reverses the winner; without this row the phrase is untested
    /// vocabulary that a `BTreeMap`-key tie-break would also satisfy.
    #[test]
    fn exposed_names_takes_its_winner_from_slice_order_not_identifier_order() {
        let alpha = pinned("ns/alpha", "b");
        let omega = pinned("ns/omega", "c");
        let claim = |id: &oci::PinnedIdentifier| dependency(id.clone(), Some(&["make"]), &[], Visibility::INTERFACE);

        let forward = exposed_names(&[claim(&alpha), claim(&omega)], NotEnumerablePolicy::Refuse)
            .expect("a collision is never a refusal");
        let reversed = exposed_names(&[claim(&omega), claim(&alpha)], NotEnumerablePolicy::Refuse)
            .expect("a collision is never a refusal");

        assert_eq!(owner_at(&forward, "make").tool, omega);
        assert_eq!(
            owner_at(&reversed, "make").tool,
            alpha,
            "reversing the slice reverses the winner"
        );
    }

    /// C-024 + C-021 (E-19): the closure is walked deps-before-dependents with
    /// the root last, so a root that claims a dependency's name wins it —
    /// `shadowed` carries the dependency and `is_root` follows the **winner**,
    /// not the name.
    #[test]
    fn exposed_names_lets_a_root_walked_last_win_over_a_dependency() {
        let dep = pinned("ns/zlib", "b");
        let root = pinned("ns/cmake", "a");
        let nodes = vec![
            dependency(dep.clone(), Some(&["make"]), &[], Visibility::INTERFACE),
            node(root.clone(), Some(&["make"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("a collision is never a refusal");

        let owner = owner_at(&set, "make");
        assert_eq!(owner.tool, root);
        assert!(owner.is_root, "is_root follows the winning claim, not the name");
        assert_eq!(owner.shadowed, vec![dep]);
    }

    /// RUL-11 + RUL-12 (E-34): the same pinned identifier twice in the slice —
    /// reachable the moment two merged roots share a dependency. The node must
    /// not shadow itself, so `shadowed` gains no self-entry.
    #[test]
    fn exposed_names_does_not_self_shadow_an_identifier_repeated_in_the_slice() {
        let shared = pinned("ns/zlib", "b");
        let nodes = vec![
            dependency(shared.clone(), Some(&["zlib-flate"]), &[], Visibility::INTERFACE),
            dependency(shared.clone(), Some(&["zlib-flate"]), &[], Visibility::INTERFACE),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("a shared dependency is never a conflict");

        let owner = owner_at(&set, "zlib-flate");
        assert_eq!(owner.tool, shared);
        assert!(
            owner.shadowed.is_empty(),
            "a node reached twice must not shadow itself, got {:?}",
            owner.shadowed
        );
    }

    /// RUL-11 + RUL-12 (F-3): the same pinned identifier claiming a name
    /// **twice with a different claimant interleaved** (`[A, B, A]`) —
    /// reachable the moment two merged roots share a dependency AND a third
    /// node also claims the name in between. `A`'s second win must not
    /// self-shadow through the `shadowed` list it inherits from `B`'s own
    /// win in the middle: `B`'s `shadowed` already carries `A` (from the
    /// first collision), and pushing `B` on top without filtering `A` back
    /// out would leave `A`'s own identifier on its own final `shadowed`.
    #[test]
    fn exposed_names_does_not_self_shadow_across_an_interleaved_repeat() {
        let shared = pinned("ns/zlib", "b");
        let other = pinned("ns/openssl", "c");
        let nodes = vec![
            dependency(shared.clone(), Some(&["zlib-flate"]), &[], Visibility::INTERFACE),
            dependency(other.clone(), Some(&["zlib-flate"]), &[], Visibility::INTERFACE),
            dependency(shared.clone(), Some(&["zlib-flate"]), &[], Visibility::INTERFACE),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an interleaved repeat is never a refusal");

        let owner = owner_at(&set, "zlib-flate");
        assert_eq!(owner.tool, shared, "the identifier walked last wins");
        assert!(
            !owner.shadowed.contains(&shared),
            "the winner must not shadow itself via an inherited list, got {:?}",
            owner.shadowed
        );
        assert_eq!(owner.shadowed, vec![other], "only the interleaved claimant is shadowed");
    }

    // ── S-012 / C-024: no denylist, no reserved name ────────────────────────

    /// C-024 / S-012 (E-21): a claimed `ocx` renders like any other name.
    ///
    /// Inverted from `interface_shim_names_refuses_the_literal_ocx_name`,
    /// which this replaces: that test asserted exactly the refusal D-4
    /// deletes. The assertion that carries the inversion is the **admission** —
    /// `is_ok()` alone would be satisfied by an implementation that quietly
    /// dropped `ocx` from the map, which is the same refusal one register down.
    #[test]
    fn exposed_names_admits_a_claimed_ocx_name() {
        let claiming = pinned("ns/tool", "a");
        let nodes = vec![node(claiming.clone(), Some(&["ocx"]), &[], true)];

        let set =
            exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("a claimed 'ocx' is admitted, never refused");

        assert_eq!(owned_names(&set), vec!["ocx"], "the name must be exposed, not dropped");
        assert_eq!(
            owner_at(&set, "ocx").tool,
            claiming,
            "the winning owner is the node that claimed it"
        );
    }

    /// C-025 / RUL-10 / S-012 (E-22): `Ocx` is admitted **and is a distinct
    /// key from `ocx`** in the map `exposed_names` returns. The unfolded map is
    /// the contract (D-V18); only `fold_case_insensitive` collapses the two.
    #[test]
    fn exposed_names_keeps_ocx_and_its_case_twin_as_two_unfolded_keys() {
        let lower = pinned("ns/lower", "b");
        let mixed = pinned("ns/mixed", "c");
        let nodes = vec![
            dependency(lower.clone(), Some(&["ocx"]), &[], Visibility::INTERFACE),
            dependency(mixed.clone(), Some(&["Ocx"]), &[], Visibility::INTERFACE),
            node(pinned("ns/root", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("neither casing is refused");

        assert_eq!(owned_names(&set), vec!["Ocx", "cmake", "ocx"]);
        assert_eq!(owner_at(&set, "Ocx").tool, mixed);
        assert_eq!(owner_at(&set, "ocx").tool, lower);
    }

    /// ADR D-4 / S-012 (E-23): there is **no denylist**. The privilege-boundary
    /// names are the ones a denylist would reach for first; each is admitted
    /// like any other claim. This row reds the day one is reintroduced.
    #[test]
    fn exposed_names_admits_privilege_boundary_names_like_any_other() {
        let claiming = pinned("ns/tool", "a");
        let nodes = vec![node(
            claiming.clone(),
            Some(&["sudo", "pkexec", "su", "doas"]),
            &[],
            true,
        )];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("no name is reserved (ADR D-4)");

        assert_eq!(owned_names(&set), vec!["doas", "pkexec", "su", "sudo"]);
        for key in ["sudo", "pkexec", "su", "doas"] {
            assert_eq!(owner_at(&set, key).tool, claiming, "'{key}' is owned by its claimant");
        }
    }

    /// C-024 (E-24): the comparison target the deleted refusal used was the
    /// literal `ocx`, never `current_exe()`'s stem — this test binary is not
    /// named `ocx`, and its own name is admitted exactly like any other claim.
    #[test]
    fn exposed_names_admits_a_name_equal_to_this_binarys_own_stem() {
        let current = std::env::current_exe().expect("the test binary has a path");
        let stem = current
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("the test binary's stem is UTF-8")
            .to_string();
        let stem = BinaryName::try_from(stem).expect("a cargo test binary's stem is a valid BinaryName");
        assert_ne!(stem.as_str(), "ocx", "this test binary must not itself be named 'ocx'");

        let nodes = vec![node(pinned("ns/tool", "a"), Some(&[stem.as_str()]), &[], true)];

        let set = exposed_names(&nodes, NotEnumerablePolicy::Refuse).expect("an ordinary name is admitted");

        assert_eq!(owned_names(&set), vec![stem.as_str()]);
    }

    // ── Degenerate inputs ───────────────────────────────────────────────────

    /// C-021/C-022 (E-32): an empty node slice is `Ok(empty)` under **both**
    /// policies — `Refuse` refuses a node that claims nothing, and there is no
    /// node.
    #[test]
    fn exposed_names_returns_an_empty_map_for_an_empty_node_slice() {
        for policy in [NotEnumerablePolicy::Refuse, NotEnumerablePolicy::Skip] {
            let set = exposed_names(&[], policy)
                .unwrap_or_else(|e| panic!("no nodes is not a refusal ({policy:?}), got {e:?}"));
            assert!(set.is_empty(), "under {policy:?}, got {:?}", owned_names(&set));
        }
    }

    /// C-022 (E-33): the one-node boundary — a single root claiming nothing.
    /// `Skip` yields the empty map, `Refuse` names that one node.
    #[test]
    fn exposed_names_handles_a_single_node_claiming_nothing_under_both_policies() {
        let only = pinned("ns/meta", "a");

        let skipped = exposed_names(&[node(only.clone(), None, &[], true)], NotEnumerablePolicy::Skip)
            .expect("Skip never refuses");
        assert!(skipped.is_empty(), "got {:?}", owned_names(&skipped));

        let error = exposed_names(&[node(only.clone(), None, &[], true)], NotEnumerablePolicy::Refuse)
            .expect_err("Refuse refuses the only node there is");
        match error {
            PackageErrorKind::ShimNamesNotEnumerable { package } => assert_eq!(package, only),
            other => panic!("expected ShimNamesNotEnumerable, got {other:?}"),
        }
    }

    // ── C-025 / RUL-10 / RUL-19 / RUL-32: the case fold ─────────────────────
    //
    // The fold's total order is `NameOwner::walk_index` (RUL-32), not the
    // `BTreeMap`'s own key order — a fold keyed on key order would always
    // pick an all-lowercase spelling, since ASCII lowercase sorts above
    // uppercase. "Last walked wins" therefore reads here as "the greatest
    // `walk_index` wins", never "the greatest colliding key wins". Every row
    // below is deterministic on every host by construction: no `cfg!`, no
    // filesystem probe, no `current_exe`.

    /// C-025 / RUL-10 (E-26): three spellings of one name collapse to one key,
    /// and the losers land on the winner's `shadowed` oldest-first (RUL-11).
    #[test]
    fn fold_case_insensitive_collapses_three_spellings_onto_one_key() {
        let upper = pinned("ns/upper", "b");
        let mixed = pinned("ns/mixed", "c");
        let lower = pinned("ns/lower", "d");
        let map = map_of(vec![
            ("MAKE", owner_of(upper.clone(), false)),
            ("Make", owner_of(mixed.clone(), false)),
            ("make", owner_of(lower.clone(), false)),
        ]);

        let folded = fold_case_insensitive(map);

        assert_eq!(owned_names(&folded), vec!["make"], "one file, one key");
        let owner = owner_at(&folded, "make");
        assert_eq!(owner.tool, lower);
        assert_eq!(owner.shadowed, vec![upper, mixed], "oldest colliding key first");
    }

    /// RUL-19 (E-27, the binding half): **the winner keeps its ORIGINAL
    /// spelling.** With no all-lowercase spelling among the claims, the
    /// surviving key must be the winner's own bytes — `Make`, never the folded
    /// `make`. This is the row that reds an implementation that re-keys the map
    /// on `to_ascii_lowercase()`, which every other fold row would pass.
    #[test]
    fn fold_case_insensitive_keeps_the_winners_original_spelling() {
        let upper = pinned("ns/upper", "b");
        let mixed = pinned("ns/mixed", "c");
        let map = map_of(vec![
            ("MAKE", owner_of(upper.clone(), false)),
            ("Make", owner_of(mixed.clone(), false)),
        ]);

        let folded = fold_case_insensitive(map);

        let surviving: Vec<&str> = owned_names(&folded);
        assert_eq!(
            surviving,
            vec!["Make"],
            "RUL-19: the fold stops a second write, it never renames a tool"
        );
        assert_eq!(owner_at(&folded, "Make").tool, mixed);
        assert_eq!(owner_at(&folded, "Make").shadowed, vec![upper]);
    }

    /// RUL-32 (E-28, updated from the round-1 key-order draft to the binding
    /// walk-order rule): the folded winner is whichever colliding entry has
    /// the greatest `walk_index`, **never** whichever key sorts last in the
    /// `BTreeMap`. Here the winner (`Make`, walked second) sorts BEFORE its
    /// rival (`make`, walked first) in key order, so an implementation that
    /// picked the greatest *key* would answer `make`/`lower` — this row reds
    /// that implementation. This is the sole test RUL-32 names as an
    /// exception to "do not edit a test": the original assertion pinned the
    /// key-order rule the round-2 ruling replaced.
    #[test]
    fn fold_case_insensitive_winner_is_the_greatest_walk_index_not_the_greatest_key() {
        let lower = pinned("ns/lower", "b");
        let mixed = pinned("ns/mixed", "c");
        let map = map_of(vec![
            ("make", owner_of(lower.clone(), false)),
            ("Make", owner_of(mixed.clone(), true)),
        ]);

        let folded = fold_case_insensitive(map);

        let owner = owner_at(&folded, "Make");
        assert_eq!(
            owner.tool, mixed,
            "the greatest walk_index wins, even though its key sorts before its rival's"
        );
        assert!(owner.is_root, "the surviving owner's own fields travel with it");
        assert_eq!(owner.shadowed, vec![lower]);
    }

    /// RUL-10 (E-29): the empty map folds to the empty map, no panic.
    #[test]
    fn fold_case_insensitive_returns_an_empty_map_unchanged() {
        assert!(fold_case_insensitive(BTreeMap::new()).is_empty());
    }

    /// C-025 (E-26, the non-colliding boundary): a map with no case twins is
    /// returned entry-for-entry, spellings and owners intact. Reds a fold that
    /// lowercases unconditionally.
    #[test]
    fn fold_case_insensitive_leaves_a_map_without_case_twins_alone() {
        let cmake = pinned("ns/cmake", "a");
        let msbuild = pinned("ns/msbuild", "b");
        let map = map_of(vec![
            ("MSBuild", owner_of(msbuild.clone(), false)),
            ("cmake", owner_of(cmake.clone(), true)),
        ]);

        let folded = fold_case_insensitive(map.clone());

        assert_eq!(folded, map, "nothing collides, so nothing changes");
    }

    /// RUL-11 through the fold: a loser's own `shadowed` list is not lost when
    /// its key is collapsed — every identifier that lost the name survives on
    /// the winner exactly once, and the winner never shadows itself.
    ///
    /// The **relative order** of a loser's inherited `shadowed` entries against
    /// the winner's own is not fixed by any authority (see the report's
    /// underspecification list), so this row asserts membership, uniqueness and
    /// the self-exclusion — the three properties `ocx inspect`'s collision row
    /// actually depends on — and leaves the ordering to the row above, where
    /// no prior lists exist to interleave.
    #[test]
    fn fold_case_insensitive_carries_every_losers_prior_shadowed_entry() {
        let upper = pinned("ns/upper", "b");
        let upper_loser = pinned("ns/upper-loser", "c");
        let lower = pinned("ns/lower", "d");
        let lower_loser = pinned("ns/lower-loser", "e");
        let map = map_of(vec![
            (
                "MAKE",
                NameOwner {
                    tool: upper.clone(),
                    is_root: false,
                    shadowed: vec![upper_loser.clone()],
                    walk_index: 0,
                },
            ),
            (
                "make",
                NameOwner {
                    tool: lower.clone(),
                    is_root: false,
                    shadowed: vec![lower_loser.clone()],
                    walk_index: 0,
                },
            ),
        ]);

        let folded = fold_case_insensitive(map);

        assert_eq!(owned_names(&folded), vec!["make"]);
        let owner = owner_at(&folded, "make");
        assert_eq!(owner.tool, lower);
        for lost in [&upper, &upper_loser, &lower_loser] {
            assert_eq!(
                owner.shadowed.iter().filter(|id| *id == lost).count(),
                1,
                "{lost} must appear exactly once among {:?}",
                owner.shadowed
            );
        }
        assert!(
            !owner.shadowed.contains(&lower),
            "the winner must not shadow itself, got {:?}",
            owner.shadowed
        );
        assert_eq!(owner.shadowed.len(), 3, "no entry invented, none lost");
    }

    /// C-025 (F-2): the self-shadow fixture — **one node claims both case
    /// twins**, so both unfolded entries carry the identical `tool` AND the
    /// identical `walk_index` (a single node is processed at one position in
    /// `exposed_names`'s walk). Without the `loser.tool == winner.tool` guard,
    /// the fold would push the node onto its own `shadowed` the moment the
    /// group's other entry is popped as a "loser" — it never is a loser, it
    /// is the same claimant. The genuine walk_index TIE this fixture
    /// produces also exercises the documented key tie-break (RUL-32): with
    /// both walk_index equal, `"make"` sorts after `"Make"` (ASCII lowercase
    /// sorts above uppercase), so `"make"` survives.
    #[test]
    fn fold_case_insensitive_does_not_shadow_itself_when_one_node_claims_both_case_twins() {
        let claiming = pinned("ns/tool", "a");
        let map: BTreeMap<BinaryName, NameOwner> = [
            (
                name("Make"),
                NameOwner {
                    tool: claiming.clone(),
                    is_root: false,
                    shadowed: Vec::new(),
                    walk_index: 0,
                },
            ),
            (
                name("make"),
                NameOwner {
                    tool: claiming.clone(),
                    is_root: false,
                    shadowed: Vec::new(),
                    walk_index: 0,
                },
            ),
        ]
        .into_iter()
        .collect();

        let folded = fold_case_insensitive(map);

        assert_eq!(
            owned_names(&folded),
            vec!["make"],
            "the tie breaks on key order, per RUL-32's documented fallback"
        );
        let owner = owner_at(&folded, "make");
        assert_eq!(owner.tool, claiming);
        assert!(
            owner.shadowed.is_empty(),
            "a node that claims both its own case twins must not shadow itself, got {:?}",
            owner.shadowed
        );
    }

    /// C-025 (round-2 review WARN): the **inherited**-`shadowed` filter — the
    /// `.filter(|id| *id != winner.tool)` on a LOSER's own prior `shadowed`,
    /// distinct from the self-claim guard above. Reachable the moment a name's
    /// unfolded collision history already names the eventual fold winner: `C`
    /// claims `MAKE` first, `B` displaces it (so `B`'s `shadowed` already
    /// holds `C`), then `C` claims the case twin `make` and wins the fold.
    /// Without this filter, `C` would inherit its own identifier straight back
    /// off `B`'s `shadowed` list — a second, independent self-shadow route
    /// from the one the sibling test above covers (that one guards the
    /// LOSER-equals-winner case; this one guards a loser whose own `shadowed`
    /// *contains* the winner).
    #[test]
    fn fold_case_insensitive_filters_the_winner_out_of_an_inherited_shadowed_list() {
        let winner_tool = pinned("ns/root", "c");
        let loser_tool = pinned("ns/other", "b");
        let map: BTreeMap<BinaryName, NameOwner> = [
            (
                name("MAKE"),
                NameOwner {
                    tool: loser_tool.clone(),
                    is_root: false,
                    shadowed: vec![winner_tool.clone()],
                    walk_index: 1,
                },
            ),
            (
                name("make"),
                NameOwner {
                    tool: winner_tool.clone(),
                    is_root: false,
                    shadowed: Vec::new(),
                    walk_index: 2,
                },
            ),
        ]
        .into_iter()
        .collect();

        let folded = fold_case_insensitive(map);

        let owner = owner_at(&folded, "make");
        assert_eq!(owner.tool, winner_tool);
        assert_eq!(
            owner.shadowed,
            vec![loser_tool],
            "the winner's own identifier must be filtered out of a loser's inherited shadowed list"
        );
        assert!(
            !owner.shadowed.contains(&winner_tool),
            "the winner must not shadow itself via an inherited list, got {:?}",
            owner.shadowed
        );
    }

    /// C-024 (F-7): the collision branch [`record_claim`] takes emits exactly
    /// one debug line naming the collision — the module doc's own claim
    /// ("never a refusal, a warning, or silence") is otherwise unobserved by
    /// any behavioural test, since a debug log has no return-value effect. A
    /// source-shape guard because the log crate's bare facade
    /// (`crate::log = pub use tracing_log::log::*;`) has no capture facility
    /// in this codebase (precedent: `project/registry.rs`'s
    /// `live_projects_departed_other_project_is_debug_only`). Scoped to
    /// `record_claim`'s own body (up to the next function) rather than the
    /// whole file, so a debug line anywhere else could not satisfy this row
    /// by accident.
    #[test]
    fn record_claim_emits_a_debug_line_on_its_collision_branch() {
        const SOURCE: &str = include_str!("toolchain_names.rs");
        let start = SOURCE
            .find("fn record_claim(")
            .expect("record_claim must exist in this file");
        let after_start = &SOURCE[start..];
        let end = after_start
            .find("\npub(crate) fn fold_case_insensitive")
            .expect("fold_case_insensitive must follow record_claim, or this guard is scoped wrong");
        let body = &after_start[..end];

        let debug_at = body
            .find("log::debug!")
            .expect("record_claim's collision branch must emit a debug line (C-024), scanned body:\n{body}");
        // Presence alone is satisfied by a debug line anywhere in the
        // function, including the same-owner arm's early `return` above the
        // collision branch, where a re-sighting is not a collision. Pin the
        // ordering too: the debug line must come AFTER that arm's `return;`,
        // proving it sits in the collision branch and not the re-sighting one.
        let return_at = body
            .find("return;")
            .expect("the same-owner arm's early return must exist, or this guard proves nothing about placement");
        assert!(
            debug_at > return_at,
            "the debug line must fire only on the collision branch, after the same-owner arm's early return, scanned body:\n{body}"
        );
    }

    /// C-015 / C-025 (E-31): the fold key is ASCII case **by construction of
    /// the key type**, not by a rule the fold has to remember. `BinaryName`
    /// admits only `is_ascii_graphic` bytes, so the Unicode/ASCII divergence a
    /// `to_lowercase()` would introduce (Turkish dotted `İ` folding onto `i`)
    /// is unreachable through this map.
    ///
    /// A premise guard, not a behaviour test: it reds the day `BinaryName`'s
    /// charset widens, which is the day the fold has to state its own rule.
    #[test]
    fn binary_name_admits_no_character_where_unicode_and_ascii_folding_differ() {
        for non_ascii in ["İ", "Ｍake", "мake"] {
            assert!(
                BinaryName::try_from(non_ascii).is_err(),
                "'{non_ascii}' must not be constructible as a BinaryName"
            );
        }
    }

    /// RUL-10 (E-30): the module is **pure with respect to the host** — no
    /// `cfg!` macro anywhere in its code. That is the entire reason D-V18 moved
    /// the case-sensitivity question to the renderer's call site: a
    /// `cfg!(target_os = "windows")` branch inside either function is green on
    /// whichever host runs it and never observed on the other, which is
    /// `quality-core.md`'s unreachable-red class.
    ///
    /// A source-shape guard because no runtime assertion can see a branch that
    /// is compiled out. The needle is assembled at compile time from two
    /// fragments so this test's own source cannot satisfy the search it
    /// performs, and the comment filter is pinned below so a filter that
    /// removed everything could not read as a pass.
    #[test]
    fn toolchain_names_carries_no_cfg_macro_outside_its_comments() {
        const SOURCE: &str = include_str!("toolchain_names.rs");
        const NEEDLE: &str = concat!("cfg", "!(");

        let code: String = SOURCE
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("pub(crate) fn fold_case_insensitive"),
            "the comment filter must leave the module's code behind, or this guard proves nothing"
        );
        assert!(
            !code.contains(NEEDLE),
            "RUL-10: the host question belongs at the renderer's call site, not in a {NEEDLE} branch here"
        );
    }
}
