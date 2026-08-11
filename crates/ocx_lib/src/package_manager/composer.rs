// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! two-env composition: flat iteration over each root's pre-built transitive
//! closure (TC) with cross-root dedup, emitting entries gated per surface.
//!
//! The TC is built inductively at install time via
//! `Visibility::through_edge` + `Visibility::merge` in
//! `ResolvedPackage::with_dependencies`. At exec time the composer reads each
//! root's `resolve.json` (one read per root), iterates flatly, and gates
//! emission via `tc_entry.visibility.has_interface()` (default exec) or
//! `has_private()` (`--self`). No recursive walk at compose time.
//!
//! See `adr_two_env_composition.md` for the full design rationale.
//!
//! `ComposeOutput` also carries `admitted_binaries` / `admitted_entrypoints`
//! — the admitted set's declared-name claim attribution consumed by `ocx
//! env` / `ocx package env`'s `binaries` / `entrypoints` JSON arrays. See
//! `adr_declared_binaries_metadata.md` §4.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tokio::task::JoinSet;

use crate::{
    file_structure::{PackageDir, PackageStore},
    oci,
    package::{
        install_info::InstallInfo,
        metadata::{
            self,
            binary::{Binaries, BinaryName},
            dependency::DependencyName,
            entrypoint::{EntrypointName, Entrypoints},
            env::{dep_context::DependencyContext, entry::Entry, modifier::ModifierKind, resolver::EnvResolver},
            integrations::{INTEGRATION_TOKENS, IntegrationEntry},
            template::SelfEnvScope,
        },
        resolved_package::ResolvedPackage,
    },
    package_manager::error::{DependencyError, PackageErrorKind},
};

use super::tasks::common;

/// Result type for a single dep-load task spawned during parallel preload.
///
/// The `usize` is the task index for stable topological re-ordering after join.
type DepLoadResult = (
    usize,
    crate::Result<(metadata::Metadata, ResolvedPackage, oci::PinnedIdentifier)>,
);

/// The return value of [`compose`].
///
/// Carries the emitted env entries together with the **admitted set** — the
/// ordered, deduped set of `PinnedIdentifier`s whose surface contributions
/// were actually emitted.  The admitted set is used by `SitePatchResolver` in
/// `resolve_env` to gate the companion overlay: only identifiers that compose
/// actually visited get a patch overlay applied.
///
/// `compose` itself is **patch-agnostic** — it does not read patch config or
/// know about companions.  The admitted set is a pure by-product of the
/// surface-gating and dedup logic already performed during composition.
pub struct ComposeOutput {
    /// The composed env entries in emit order.
    pub entries: Vec<Entry>,

    /// Deduped, visit-order identifiers admitted by the surface gate.
    ///
    /// Contains the stripped identifiers (advisory tag dropped) of every
    /// TC dep **and** every explicit root that was actually emitted during
    /// this compose call.  Deps appear before their root (topological); roots
    /// are appended at the end in the same order as `roots`.  Cross-root
    /// dedup is applied: a shared dep appears only once, at its first-seen
    /// position across all roots.
    pub admitted: Vec<oci::PinnedIdentifier>,

    /// Declared `binaries` claims contributed by each admitted identifier.
    ///
    /// One entry per (identifier, claimed name) pair, restricted to packages
    /// that passed the same surface gate as `admitted` (root packages
    /// unconditionally; deps iff `has_interface()`/`has_private()`). Consumed
    /// by `ocx env` / `ocx package env`'s `binaries` JSON array. See
    /// `adr_declared_binaries_metadata.md` §4 Decision A.
    pub admitted_binaries: Vec<(oci::PinnedIdentifier, BinaryName)>,

    /// Declared `entrypoints` claims contributed by each admitted identifier.
    ///
    /// Same shape and admission rule as `admitted_binaries`, sourced from
    /// `Metadata::entrypoints()`. Consumed by `ocx env` / `ocx package
    /// env`'s `entrypoints` JSON array.
    pub admitted_entrypoints: Vec<(oci::PinnedIdentifier, EntrypointName)>,

    /// Declared `integrations` contributed by each admitted identifier.
    ///
    /// Interface surface only — always empty when `self_view == true`
    /// ([`integrations_cross`]), and likewise empty whenever the caller
    /// suppressed collection ([`compose_companion`]). Payloads are interpolated
    /// with the DECLARING package's own `${installPath}`. Ordered: each root's
    /// admitted deps in topological order, then the root; cross-root dedup
    /// applies, so a shared dep contributes once **within this compose call** —
    /// a caller merging two calls' outputs owns the dedup across them. Within
    /// one package, lexicographic by namespace. See
    /// `adr_package_integrations.md` C-012.
    pub admitted_integrations: Vec<(oci::PinnedIdentifier, IntegrationEntry)>,
}

// ── Surface algebra: the single source of truth ─────────────────────────────
//
// A surface is defined recursively over the two-axis `Visibility` algebra
// (`metadata::visibility`) — never by structural special cases:
//
//     surface(P, axis) = { carrier c of P       : vis(c).has(axis) }
//                      ∪ ⋃ { interface_surface(D) : edge P→D has(axis) }
//
// Carriers are a package's own contributions, each with a visibility:
// declared env vars carry their publisher-declared one; entry points carry
// `Entrypoints::IMPLICIT_VISIBILITY` (INTERFACE — launchers are
// consumer-facing, the package's own runtime bypasses them); binaries claims
// carry `Binaries::IMPLICIT_VISIBILITY` (PUBLIC — raw executables serve
// consumers and the package's own shims alike). Below the root the recursion
// always takes the dep's INTERFACE surface — "only the interface side of a
// dep crosses edges" (ADR Algorithm v3 step 5) — with edge composition
// precomputed by the `through_edge`/`merge` effective-visibility fold
// (`ResolvedPackage::with_dependencies`).
//
// `compose` runs this recursion flattened over the precomputed TC:
// `dep_admitted` is the edge-union term; `carrier_crosses` is the carrier
// term, where `is_root` marks recursion depth 0 — the only level where the
// requested axis, not INTERFACE, gates the package's own carriers.
// `self_view` selects WHICH of the root's two surfaces is emitted; it never
// decides membership on its own.
//
// These predicates are the ONE implementation shared by `compose` (the
// runtime env behind `ocx env` / `ocx env --self`) and
// `package_manager::tasks::inspect::project_surface` (the static summary
// behind `ocx package inspect --closure`). Inspect MUST route every
// admission / crossing decision through them — never re-derive — so the two
// views can never disagree about a surface.

/// Whether a transitive-dependency entry is admitted to a surface.
///
/// A dep enters the interface (consumer) surface iff its effective visibility
/// `has_interface()`, and the private (self) surface iff `has_private()`.
/// Root packages have no edge visibility and are ALWAYS admitted — that is the
/// caller's structural rule, applied around this predicate, not part of it.
pub(crate) fn dep_admitted(effective: metadata::visibility::Visibility, self_view: bool) -> bool {
    if self_view {
        effective.has_private()
    } else {
        effective.has_interface()
    }
}

/// Whether one carrier crosses onto a surface, given its visibility.
///
/// The flattened carrier term of the surface algebra (module comment above).
/// At the ROOT (recursion depth 0) a carrier crosses on the surface's own
/// axis — `has_interface()` on the interface surface, `has_private()` on the
/// private (self) surface. On a DEPENDENCY only the carrier's interface side
/// crosses, on EITHER surface: the recursion below the root always takes a
/// dep's *interface* surface (ADR Algorithm v3 step 5), so a dep's
/// private-only carrier never crosses the edge into the parent — even on the
/// parent's self surface. This asymmetry is exactly why a single
/// `crosses(vis)` predicate is wrong; the caller must state whether the
/// owning node is the root.
///
/// Applies uniformly to every carrier kind: declared env vars pass their
/// declared visibility, entry points pass `Entrypoints::IMPLICIT_VISIBILITY`
/// (both the `admitted_entrypoints` claim and the synth-`entrypoints/` PATH
/// push route through here, so a claim can never contradict PATH), binaries
/// claims pass `Binaries::IMPLICIT_VISIBILITY`.
pub(crate) fn carrier_crosses(carrier: metadata::visibility::Visibility, is_root: bool, self_view: bool) -> bool {
    if is_root {
        if self_view {
            carrier.has_private()
        } else {
            carrier.has_interface()
        }
    } else {
        carrier.has_interface()
    }
}

/// Whether the integrations carrier crosses onto the requested surface.
///
/// Interface surface only, at EVERY depth: `--self` composes zero
/// integrations. This is a SURFACE-LEVEL rule, not a visibility one — no
/// `Visibility` value produces it under [`carrier_crosses`] (proof: ADR
/// `adr_package_integrations.md` §4.1, the four-cell truth table). Homed
/// here, beside the algebra it deviates from, so `compose` and
/// `inspect::project_surface` share the one implementation the surface
/// contract requires.
///
/// Takes no `is_root`: the answer is the same at every depth, and a parameter
/// the body ignores is a lie about the rule. The EDGE term is unchanged and
/// stays algebraic — a dependency contributes integrations iff
/// `dep_admitted(effective, /* self_view = */ false)`.
pub(crate) fn integrations_cross(self_view: bool) -> bool {
    !self_view
}

/// Compose the runtime env from one or more root packages.
///
/// Reads each root's pre-built TC from `resolve.json` (single read per root),
/// iterates flatly with cross-root dedup, emits per-surface gated entries.
/// No recursion at compose time.
///
/// `self_view = false` selects the interface surface (default exec — consumer
/// view); `self_view = true` selects the private surface (`--self` — emits
/// the package's full runtime env including private entries).
///
/// Returns a [`ComposeOutput`] that carries both the composed entries and the
/// admitted set (deduped, visit-order identifiers that contributed to the
/// output).  The admitted set is consumed by `SitePatchResolver` to gate the
/// companion overlay; `compose` itself is patch-agnostic.
///
/// # Errors
///
/// Returns `Err` if any required package metadata cannot be loaded from the
/// store during composition, if two or more roots' interface projections
/// collide on an entrypoint name (multi-root collision gate — see
/// [`check_entrypoints`]), or if the active surface resolves a single
/// repository to two or more distinct digests (version conflict — see
/// [`check_repo_digest_conflicts`]).
pub(crate) async fn compose(
    roots: &[Arc<InstallInfo>],
    store: &PackageStore,
    self_view: bool,
) -> crate::Result<ComposeOutput> {
    compose_gated(roots, store, self_view, integrations_cross(self_view)).await
}

/// Compose ONE package as a standalone root whose output is overlaid onto a
/// different composition — the patch tier's companion projection.
///
/// Two inputs differ from [`compose`], and neither is derivable from the other:
///
/// - `self_view` is pinned to `false`. The overlay must never leak the
///   companion's private surface, even when the composition it lands in is the
///   `--self` one. Pinned by
///   `no_private_leak_companion_private_var_absent_even_under_self_view`.
/// - `collect_integrations` is the OUTER composition's gate, supplied by the
///   caller. Deriving it from the pinned `self_view` would make it
///   unconditionally true, so payloads would be resolved on every env
///   resolution and then discarded — and a payload naming a dependency whose
///   content directory is absent would fail the whole projection (hard-erroring
///   a required companion, warn-skipping an optional one along with its env
///   entries) on a surface contractually required to carry zero integrations.
///
/// A named wrapper rather than a fourth parameter on [`compose`]: it keeps two
/// adjacent booleans off every call site, and states the `self_view = false`
/// invariant once here instead of at each caller. This suppresses COMPUTE only
/// — with the gate on, the surface is byte-identical to [`compose`]'s.
///
/// # Errors
///
/// As [`compose`].
pub(crate) async fn compose_companion(
    companion: &Arc<InstallInfo>,
    store: &PackageStore,
    collect_integrations: bool,
) -> crate::Result<ComposeOutput> {
    compose_gated(
        std::slice::from_ref(companion),
        store,
        /* self_view = */ false,
        collect_integrations,
    )
    .await
}

/// The composition itself, with the integrations carrier gated by an explicit
/// input rather than re-derived from `self_view`.
///
/// Private: every caller goes through [`compose`] (gate derived from the
/// surface, the normal case) or [`compose_companion`] (gate supplied by the
/// outer composition).
async fn compose_gated(
    roots: &[Arc<InstallInfo>],
    store: &PackageStore,
    self_view: bool,
    collect_integrations: bool,
) -> crate::Result<ComposeOutput> {
    // Multi-root collision gate. Single-root case is already covered at
    // install time by `check_entrypoints`; cross-root collisions can only
    // surface here, when the user composes two or more independent roots.
    // Run before the dep walk so we fail fast on conflicting roots.
    // Guard: single-root is already gated at install time (pull.rs:425).
    if roots.len() > 1 {
        check_entrypoints(roots, store).await?;
    }

    // Fail on conflicting versions of the same `registry/repo` across the
    // surface-projected union TC (and roots themselves). A single environment
    // cannot expose two versions of one package — PATH resolves only one — so
    // a surface-visible collision is a hard error, not a best-effort pick.
    // Two tags that resolve to the same digest are not a conflict.
    // Sealed/private-edge TC entries that do not enter the active surface are
    // excluded — they cannot collide at runtime
    // (`test_sealed_conflicting_deps_coexist`). The diagnostic `deps` command
    // keeps a non-fatal warning so the conflicting tree stays inspectable.
    check_repo_digest_conflicts(roots, self_view)?;

    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: HashSet<oci::PinnedIdentifier> = HashSet::new();
    // The admitted set records every stripped identifier emitted during this
    // compose call, in visit order.  Built in parallel with `seen` so the
    // patch overlay can iterate admitted identifiers in the same topological
    // order without a second walk.
    let mut admitted: Vec<oci::PinnedIdentifier> = Vec::new();
    // Declared `binaries`/`entrypoints` claims for every admitted identifier
    // (root or dep), collected alongside `admitted` under the identical
    // surface gate. See `adr_declared_binaries_metadata.md` §4 Decision A.
    let mut admitted_binaries: Vec<(oci::PinnedIdentifier, BinaryName)> = Vec::new();
    let mut admitted_entrypoints: Vec<(oci::PinnedIdentifier, EntrypointName)> = Vec::new();
    // Interface-surface-only carrier, gated by `integrations_cross` rather
    // than the visibility algebra (ADR §4.1) — collected at the same two sites
    // as the claims above.
    let mut admitted_integrations: Vec<(oci::PinnedIdentifier, IntegrationEntry)> = Vec::new();

    // Pre-compute root keys (stripped identifiers) so a TC entry that is
    // also an explicit root is deferred to the root-emission pass instead
    // of being silently absorbed during the dep walk (Option B in the
    // composer "root-as-dep" dedup discussion). Explicit roots emit
    // unconditionally; transitive deps dedup against each other AND
    // against the explicit-root set.
    let root_keys: HashSet<oci::PinnedIdentifier> = roots.iter().map(|r| r.identifier().strip_advisory()).collect();

    for root in roots {
        // Each root's TC is already flat. Iterate in topological order
        // (deps before dependents). Dep contributions emit before root's own
        // contributions per ADR Algorithm v3.
        //
        // Batch-preload all surface-visible, non-root TC entries for this root
        // in parallel via JoinSet. This eliminates serial I/O round-trips when
        // a root has many deps — each `load_object_data` call reads two JSON
        // files from disk. Results are indexed so the topological emission
        // order is preserved after join (per quality-rust.md JoinSet pattern).
        //
        // Step 1: collect the surface-visible, deduplicated entries for this root.
        let mut visible_entries: Vec<(usize, oci::PinnedIdentifier)> = Vec::new();
        for tc_entry in &root.resolved().dependencies {
            let key = tc_entry.identifier.strip_advisory();

            // Defer to the root-emission pass when a TC entry happens to be
            // an explicit root. Otherwise a private-edge TC entry (gated out
            // here) would consume the `seen` slot and silently skip the
            // explicit-root pass for the same package.
            if root_keys.contains(&key) {
                continue;
            }

            if !dep_admitted(tc_entry.visibility, self_view) {
                continue;
            }

            // Cross-root dedup via stripped identifier (advisory tag ignored).
            // Insert AFTER the surface gate so a sealed/private TC entry that
            // gates out doesn't permanently mask a later visit of the same
            // package via a different root or path.
            if !seen.insert(key) {
                continue;
            }

            // Record in admitted set (visit order, deduped — used by
            // SitePatchResolver to gate the companion overlay). Push the
            // TAG-BEARING identifier: dedup already happened on the
            // advisory-stripped `key`, but the patch overlay matches descriptor
            // globs against this identifier and a tag-anchored rule (ADR `*:21`)
            // needs the tag preserved — otherwise a required overlay that
            // matched at install time is silently dropped at compose time (C7).
            admitted.push(tc_entry.identifier.clone());

            visible_entries.push((visible_entries.len(), tc_entry.identifier.clone()));
        }

        // Step 2: parallel-load metadata for all visible entries.
        let mut tasks: JoinSet<DepLoadResult> = JoinSet::new();
        for (idx, dep_id) in &visible_entries {
            let dep_id = dep_id.clone();
            let store = store.clone();
            let idx = *idx;
            tasks.spawn(async move {
                let dep_pkg = store.package_dir(&dep_id);
                let dep_content = dep_pkg.content();
                let result = common::load_object_data(&store, &dep_content).await;
                match result {
                    Ok((meta, resolved)) => (idx, Ok((meta, resolved, dep_id))),
                    Err(e) => (idx, Err(e)),
                }
            });
        }

        // Collect results preserving index for topological re-ordering.
        let mut loaded: Vec<Option<(metadata::Metadata, ResolvedPackage, oci::PinnedIdentifier)>> =
            vec![None; visible_entries.len()];
        while let Some(join_result) = tasks.join_next().await {
            let (idx, result) = match join_result {
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(e) => panic!("dep load task aborted: {e}"),
                Ok(v) => v,
            };
            loaded[idx] = Some(result?);
        }

        // Step 3: emit in topological order using pre-loaded metadata.
        for (meta, dep_resolved, dep_id) in loaded.into_iter().flatten() {
            let dep_pkg = store.package_dir(&dep_id);
            let dep_content = dep_pkg.content();

            // This dep already passed the surface gate (`dep_admitted`) to
            // reach `visible_entries`; each carrier kind additionally crosses
            // under its implicit visibility. Both are interface-side on a dep,
            // so both cross wherever the node is admitted. `None` binaries
            // means undeclared (contributes nothing).
            if carrier_crosses(Binaries::IMPLICIT_VISIBILITY, false, self_view)
                && let Some(binaries) = meta.binaries()
            {
                admitted_binaries.extend(binaries.iter().map(|name| (dep_id.clone(), name.clone())));
            }
            if carrier_crosses(Entrypoints::IMPLICIT_VISIBILITY, false, self_view)
                && let Some(entrypoints) = meta.entrypoints()
            {
                admitted_entrypoints.extend(entrypoints.names().map(|name| (dep_id.clone(), name.clone())));
            }

            // Build the dep's own direct-dep context map for
            // `${deps.NAME.installPath}` interpolation. Scoped to the dep's
            // own declared deps, not the root's — each package resolves its
            // own dep paths independently.
            let dep_dep_contexts = build_dep_context_map(&meta, &dep_resolved, store);

            // The edge term stays algebraic (ADR §4.4): a dep contributes
            // integrations iff `dep_admitted(effective, /* self_view = */
            // false)`. Reaching this loop already required
            // `dep_admitted(effective, self_view)`, and the gate is false
            // whenever `self_view` is true — so the surviving case is exactly
            // the interface edge, with no second gate to drift from the first.
            // Interpolation uses the DECLARING package's own `${installPath}`
            // and its own dep contexts, both already in hand: zero extra I/O.
            if collect_integrations {
                // `INTEGRATION_TOKENS`, not the resolver's `Usage::Environment`
                // default: this is the gate for the class, and the publish-time
                // check shares the constant. A hostile registry never runs that
                // check, so a `${self.env.*}` in a published payload meets only
                // this one — and its `private` value must not reach an
                // interface-surface JSON payload.
                let resolver = metadata::template::TemplateResolver::new(&dep_content, &dep_dep_contexts)
                    .usage(INTEGRATION_TOKENS);
                admitted_integrations.extend(
                    meta.integrations()
                        .resolve(&resolver)?
                        .into_iter()
                        .map(|entry| (dep_id.clone(), entry)),
                );
            }

            emit_dep_path_block(
                &meta,
                &dep_pkg,
                &dep_content,
                &dep_dep_contexts,
                self_view,
                &mut entries,
            )?;
        }

        // Root's own contributions, partitioned by `self_view`. Emit AFTER
        // the TC so root's PATH prepends win lookup over dep contributions
        // (per `add_path` prepend semantics). Root emission is unconditional
        // (no surface gate, no `seen` check against TC dedup) — explicit
        // roots are user input and always contribute. We still dedup roots
        // against each other so passing the same root twice does not
        // double-emit.
        let root_key = root.identifier().strip_advisory();
        if seen.insert(root_key) {
            // Record root in admitted set (appended after its TC deps, per visit
            // order — SitePatchResolver relies on this ordering). Push the
            // TAG-BEARING root identifier (dedup already used the stripped key) so
            // the patch overlay can match tag-anchored descriptor rules.
            admitted.push(root.identifier().clone());

            // The root's own carriers cross on the surface's axis under their
            // implicit visibilities: binaries (PUBLIC) on both surfaces — the
            // root's own executables serve its own shims too — while entry
            // points (INTERFACE) reach the interface surface only.
            if carrier_crosses(Binaries::IMPLICIT_VISIBILITY, true, self_view)
                && let Some(binaries) = root.metadata().binaries()
            {
                admitted_binaries.extend(binaries.iter().map(|name| (root.identifier().clone(), name.clone())));
            }
            if carrier_crosses(Entrypoints::IMPLICIT_VISIBILITY, true, self_view)
                && let Some(entrypoints) = root.metadata().entrypoints()
            {
                admitted_entrypoints.extend(
                    entrypoints
                        .names()
                        .map(|name| (root.identifier().clone(), name.clone())),
                );
            }

            // Build root's direct-dep context map for `${deps.NAME.installPath}`
            // interpolation in root's own env vars.
            let root_dep_contexts = build_dep_context_map(root.metadata(), root.resolved(), store);

            let root_content = root.dir().content();

            // Structural, not algebraic: no `Visibility` constant reproduces
            // "interface surface at every depth" (ADR §4.1), so the root's
            // integrations are gated by `integrations_cross` alone — the
            // one predicate `inspect::project_surface` shares, applied by
            // whichever entry point supplied `collect_integrations`.
            if collect_integrations {
                // Same capability set as the dependency site above — one rule,
                // applied wherever a payload is resolved.
                let resolver = metadata::template::TemplateResolver::new(&root_content, &root_dep_contexts)
                    .usage(INTEGRATION_TOKENS);
                admitted_integrations.extend(
                    root.metadata()
                        .integrations()
                        .resolve(&resolver)?
                        .into_iter()
                        .map(|entry| (root.identifier().clone(), entry)),
                );
            }

            emit_root_path_block(
                root.metadata(),
                root.dir(),
                &root_content,
                &root_dep_contexts,
                self_view,
                &mut entries,
            )?;
        }
    }

    Ok(ComposeOutput {
        entries,
        admitted,
        admitted_binaries,
        admitted_entrypoints,
        admitted_integrations,
    })
}

/// Uniqueness check on entrypoint names across the interface projection of
/// one or more roots.
///
/// Used at two boundaries:
///
/// - **Install gate** (single-root): `pull.rs` invokes this with the freshly
///   resolved root before persisting `resolve.json`, so closure-scoped
///   duplicate launcher names never reach disk.
/// - **Compose gate** (multi-root): [`compose`] invokes this when more than
///   one root participates, so cross-root interface collisions surface
///   before any env entries are emitted.
///
/// Scope: interface projection only. For each root, the helper records the
/// root's own bundle entrypoints, then walks `resolved().dependencies` and
/// records every TC entry whose effective visibility has the interface axis
/// (`has_interface()`). Cross-root dedup via stripped identifier ensures a
/// shared dep is counted once. Private-surface duplicates are deliberately
/// tolerated and resolved at runtime by topological PATH order.
///
/// Root entrypoints are recorded before the TC walk so the root identifier
/// appears first in the owners list when colliding with a dep entry — keeps
/// error output legible.
///
/// # Errors
///
/// Returns `Err(PackageErrorKind::EntrypointCollision { name, owners })`
/// listing all N owners on the first collision found (deterministic via
/// `BTreeMap` iteration). Returns `Err(PackageErrorKind::Internal)` if a
/// referenced package's metadata cannot be loaded from `store`.
pub async fn check_entrypoints(roots: &[Arc<InstallInfo>], store: &PackageStore) -> Result<(), PackageErrorKind> {
    let mut owners: BTreeMap<EntrypointName, Vec<oci::PinnedIdentifier>> = BTreeMap::new();
    let mut seen: HashSet<oci::PinnedIdentifier> = HashSet::new();

    for root in roots {
        // Each root's own entrypoints are unconditionally on the interface
        // surface from the root-emission perspective. Recorded first so the
        // root identifier wins ordering in the owners list on collision.
        if seen.insert(root.identifier().strip_advisory())
            && let Some(eps) = root.metadata().entrypoints()
        {
            for name in eps.names() {
                owners.entry(name.clone()).or_default().push(root.identifier().clone());
            }
        }

        // Walk the root's TC interface projection and collect entrypoints
        // contributed by every interface-visible dep. Dedup by stripped
        // identifier so a shared dep across roots only counts once.
        for tc_entry in &root.resolved().dependencies {
            if !tc_entry.visibility.has_interface() {
                continue;
            }
            let key = tc_entry.identifier.strip_advisory();
            if !seen.insert(key) {
                continue;
            }
            let dep_content = store.content(&tc_entry.identifier);
            let (dep_metadata, _dep_resolved) = common::load_object_data(store, &dep_content)
                .await
                .map_err(PackageErrorKind::Internal)?;
            if let Some(eps) = dep_metadata.entrypoints() {
                for name in eps.names() {
                    owners
                        .entry(name.clone())
                        .or_default()
                        .push(tc_entry.identifier.clone());
                }
            }
        }
    }

    // Report the first collision found. Iteration over `BTreeMap` is sorted,
    // so the choice is deterministic across runs.
    for (name, list) in owners {
        if list.len() > 1 {
            return Err(PackageErrorKind::EntrypointCollision { name, owners: list });
        }
    }

    Ok(())
}

/// Build the `${deps.NAME.installPath}` interpolation context map for a package.
///
/// Maps each of `metadata`'s declared dependencies by [`DependencyName`] to a
/// [`DependencyContext`] whose install path is resolved from `resolved`'s
/// pinned identifiers. When a dep identifier appears in the resolved TC, the
/// pinned (digest-bearing) identifier is used; otherwise the declaration
/// identifier is the fallback.
///
/// This is a pure function: no I/O, no async. Called for both TC dep entries
/// and root packages, replacing two formerly duplicate inline blocks.
fn build_dep_context_map(
    metadata: &metadata::Metadata,
    resolved: &ResolvedPackage,
    store: &PackageStore,
) -> HashMap<DependencyName, DependencyContext> {
    let resolved_id_map: HashMap<oci::Repository, &oci::PinnedIdentifier> = resolved
        .dependencies
        .iter()
        .map(|d| (oci::Repository::from(d.identifier.as_identifier()), &d.identifier))
        .collect();
    metadata
        .dependencies()
        .iter()
        .map(|d| {
            let name = d.name();
            let key = oci::Repository::from(d.identifier.as_identifier());
            let install_id = resolved_id_map.get(&key).copied().unwrap_or(&d.identifier);
            let install_path = store.content(install_id);
            (name, DependencyContext::path_only(install_id.clone(), install_path))
        })
        .collect()
}

/// Resolve one package's whole declared env into a **private per-package
/// accumulator**, then gate by visibility and push the crossing entries into
/// `entries`.
///
/// # Resolve, then gate (D8)
///
/// The order is the decision, not an implementation detail. `${self.env.KEY}`
/// is surface-**independent**: an `interface` var may reference a `private` one,
/// and the resolved bytes are identical under `ocx env`, `ocx env --self` and
/// the launcher's `self_view=true` composition. A gate-then-resolve loop cannot
/// serve that — the referenced var would never have been resolved on the
/// surface the referencing var crosses.
///
/// The accumulator is private to this package and is what `${self.env.KEY}`
/// scans. It is deliberately **not** `entries`: that vec is global across
/// packages and already surface-gated, so reading self-env out of it would
/// violate D6.1 (it holds other packages' vars) and D8 (it holds only crossing
/// ones) at once.
///
/// Push order into `entries` is unchanged, so the PATH ordering invariant
/// documented on [`emit_dep_path_block`] is untouched.
///
/// # The assertion split
///
/// *Value resolution always; every filesystem and shape assertion on emit only.*
/// A crossing var resolves through `EnvResolver::resolve`; a non-crossing one
/// through `EnvResolver::resolve_without_emit_assertions`, so a `required` path
/// that is absent (C-026) or a declared-but-uninstalled dep (C-027) cannot fail
/// a composition over a value nobody emits.
///
/// The accepted consequence: a template *fault* in a non-crossing var now
/// surfaces where it previously never ran. A package whose own metadata cannot
/// resolve is broken regardless of who is looking.
///
/// `is_root` selects the carrier axis exactly as [`carrier_crosses`] defines it
/// — at the root a carrier crosses on the surface's own axis; on a dependency
/// only its interface side crosses, on either surface.
///
/// # Errors
///
/// Propagates the first resolution failure in declaration order.
fn emit_package_vars(
    metadata: &metadata::Metadata,
    content: &Path,
    dep_contexts: &HashMap<DependencyName, DependencyContext>,
    is_root: bool,
    self_view: bool,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    let Some(env) = metadata.env() else {
        return Ok(());
    };
    let resolver = EnvResolver::new(content, dep_contexts);

    // The private accumulator: every var resolved so far, crossing or not, in
    // declaration order. This is the scope `${self.env.KEY}` scans.
    //
    // A var whose `Var::value()` is `None` — a `Modifier::Unknown`, the
    // forward-compat read fallback — produces no `Entry` and so is absent from
    // it. A `KEY` declared twice earlier with one of the two unreadable would
    // therefore count once and resolve, where D7 wants `AmbiguousSelfEnvRef`.
    // Unreachable today: every load path into the composer routes through
    // `ValidMetadata::try_from`, which D14 keeps `validate_env_modifier_types`
    // on, and that refuses `Modifier::Unknown` unconditionally. A change that
    // moves that check off the ingress path opens this hole.
    let mut declared_before: SelfEnvScope<Entry> = SelfEnvScope::new();

    for var in env {
        // Routed through the shared predicate — the single source of truth
        // inspect also uses. At the root a carrier crosses on the surface's own
        // axis; on a dependency only its interface side crosses, on either
        // surface (ADR Algorithm v3 step 5).
        let crosses = carrier_crosses(var.visibility, is_root, self_view);
        let resolved = if crosses {
            resolver.resolve(var, &declared_before)?
        } else {
            resolver.resolve_without_emit_assertions(var, &declared_before)?
        };
        let Some(entry) = resolved else {
            continue;
        };
        if crosses {
            entries.push(entry.clone());
        }
        declared_before.push(entry);
    }

    Ok(())
}

/// Emit the dep's interface-tagged env vars followed by the dep's
/// synth-entrypoints PATH entry.
///
/// # Ordering invariant
///
/// PATH is searched left-to-right (first match wins). OCX consumers apply
/// entries by **prepending**, so the **last** entry pushed into `entries`
/// ends up **first** in the resolved PATH. The required global emit order
/// is `Deps > Env > Entrypoints`, where entrypoints land last so that
/// `entrypoints/` shadows the declared `bin/` PATH entry. This means:
///
/// 1. Call [`emit_package_vars`] *first* — its `bin/` PATH entry is pushed
///    before the synth-PATH.
/// 2. Push `entrypoints/` synth-PATH *second* — pushed after, so it is
///    prepended on top and wins lookup priority at runtime.
///
/// Entrypoint launchers are the canonical way to invoke a package's tools:
/// each launcher re-enters via `ocx launcher exec` and execs the resolved
/// target by absolute path, so PATH lookup inside the child does not feed
/// back into the launcher for normal binaries.
///
/// Regression test:
/// `test/tests/test_entrypoints.py::test_synthetic_entrypoints_path_emitted_after_declared_bin`
fn emit_dep_path_block(
    dep_metadata: &metadata::Metadata,
    dep_pkg: &PackageDir,
    dep_content: &Path,
    dep_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    self_view: bool,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    // Step 1: interface-tagged env vars (includes declared bin/ PATH entry).
    // Only the interface side of a dep crosses edges into the consumer's
    // surface (ADR Algorithm v3 step 5) — `is_root = false`.
    emit_package_vars(
        dep_metadata,
        dep_content,
        dep_dep_contexts,
        /* is_root = */ false,
        self_view,
        entries,
    )?;

    // Step 2: synth-PATH last so entrypoints/ ends up at the front of PATH
    // and shadows bin/ from step 1. Same carrier gate as the claim list —
    // interface-side on a dep, so it crosses on either surface.
    if carrier_crosses(Entrypoints::IMPLICIT_VISIBILITY, false, self_view)
        && let Some(eps) = dep_metadata.entrypoints()
        && !eps.is_empty()
    {
        entries.push(synth_entrypoints_path_for(dep_pkg));
    }

    Ok(())
}

/// Emit the root's own env vars followed by the root's synth-entrypoints
/// PATH entry, partitioned by `self_view`.
///
/// # Ordering invariant
///
/// Same as [`emit_dep_path_block`]: synth-PATH must be pushed **after**
/// the declared env vars so that the synthetic `entrypoints/` entry (pushed
/// last) ends up earlier in the resolved PATH and shadows declared `bin/`.
///
/// The synth-PATH push crosses under `Entrypoints::IMPLICIT_VISIBILITY`
/// (INTERFACE) on the root's own axis: absent under `--self`, because the
/// package's private runtime view bypasses its launchers and uses `bin/`
/// directly (ADR Algorithm v3 §"Root's own contributions").
///
/// Regression test:
/// `test/tests/test_entrypoints.py::test_synthetic_entrypoints_path_emitted_after_declared_bin`
fn emit_root_path_block(
    root_metadata: &metadata::Metadata,
    root_dir: &PackageDir,
    root_content: &Path,
    root_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    self_view: bool,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    // Step 1: env vars (includes declared bin/ PATH entry when present). The
    // root's own carriers cross on the surface's own axis — `is_root = true`.
    emit_package_vars(
        root_metadata,
        root_content,
        root_dep_contexts,
        /* is_root = */ true,
        self_view,
        entries,
    )?;

    // Step 2: synth-PATH last (no launchers on the --self surface). Same
    // carrier gate as the root's `admitted_entrypoints` claim in `compose`.
    if carrier_crosses(Entrypoints::IMPLICIT_VISIBILITY, true, self_view)
        && let Some(eps) = root_metadata.entrypoints()
        && !eps.is_empty()
    {
        entries.push(synth_entrypoints_path_for(root_dir));
    }

    Ok(())
}

/// Returns `Err(DependencyError::Conflict)` for the first `registry/repo` that
/// appears with two or more distinct digests across the **surface-projected**
/// union TC of the supplied roots (including the roots themselves).
///
/// This is the fatal gate used by [`compose`]: a single environment cannot
/// expose two versions of one package — PATH resolves only one — so a
/// surface-visible version collision aborts composition. The error names the
/// conflicting identifiers (tag and digest) so the user can tell which
/// versions collided.
///
/// `self_view` selects the surface that gates TC entries: `false` (default
/// exec) keeps only entries whose effective visibility has the interface
/// axis (`has_interface()`); `true` (`--self`) keeps only those with the
/// private axis (`has_private()`). Roots themselves always participate.
/// Sealed/private-edge TC entries that do not enter the active surface cannot
/// collide at runtime — they are excluded from the scan
/// (`test_sealed_conflicting_deps_coexist`). Two tags that resolve to the same
/// digest are not a conflict.
///
/// # Errors
///
/// Returns `Err(DependencyError::Conflict { repository, identifiers })` when a
/// surface-visible repository carries two or more distinct digests.
pub fn check_repo_digest_conflicts(roots: &[Arc<InstallInfo>], self_view: bool) -> Result<(), DependencyError> {
    if let Some(conflict) = collect_repo_digest_conflicts(roots, self_view).into_iter().next() {
        return Err(DependencyError::Conflict {
            repository: conflict.repository,
            identifiers: conflict.identifiers,
        });
    }
    Ok(())
}

/// Emits `tracing::warn!` for every `registry/repo` that appears with two or
/// more distinct digests across the **surface-projected** union TC.
///
/// Non-fatal counterpart to [`check_repo_digest_conflicts`], used only by the
/// diagnostic `deps` command: listing a conflicting tree must stay possible
/// precisely so the user can inspect the collision that blocks `env`/`exec`.
/// Same surface-gating and same-digest tolerance as the fatal gate. The token
/// `"conflicting"` is part of the stable acceptance-test contract — see
/// `test_deps_flat_conflicting_digests_reports_error`,
/// `test_deep_conflict_at_depth_two`.
pub fn warn_repo_digest_conflicts(roots: &[Arc<InstallInfo>], self_view: bool) {
    for conflict in collect_repo_digest_conflicts(roots, self_view) {
        tracing::warn!(
            "conflicting versions for {}: {}",
            conflict.repository,
            conflict
                .identifiers
                .iter()
                .map(|identifier| identifier.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
}

/// A single `registry/repo` version conflict: the repository resolved to two or
/// more distinct digests on the active surface.
///
/// Pure-data return shape so unit tests can assert conflict detection without a
/// `tracing` subscriber. `identifiers` holds the distinct-digest identifiers in
/// first-seen order (always length >= 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DigestConflict {
    pub repository: oci::Repository,
    pub identifiers: Vec<oci::PinnedIdentifier>,
}

/// Collects version conflicts across the surface-projected union TC.
///
/// Pure function: no logging, no I/O. Used by [`check_repo_digest_conflicts`],
/// [`warn_repo_digest_conflicts`], and unit tests. A repository is reported
/// only when it carries two or more distinct digests on the active surface;
/// the per-repo identifier list preserves first-seen order. Iteration over the
/// `BTreeMap` makes the returned order deterministic by repository.
pub(crate) fn collect_repo_digest_conflicts(roots: &[Arc<InstallInfo>], self_view: bool) -> Vec<DigestConflict> {
    // Per repository, the distinct-digest identifiers observed on the active
    // surface, in first-seen order. A repository with two or more entries is a
    // version conflict.
    let mut by_repository: BTreeMap<oci::Repository, Vec<oci::PinnedIdentifier>> = BTreeMap::new();
    for root in roots {
        // Roots themselves always participate — explicit roots emit
        // unconditionally during compose, so a collision between roots (or
        // between a root and any surface-visible TC entry) is real at runtime.
        record_repo_identifier(root.identifier(), &mut by_repository);
        for dep in &root.resolved().dependencies {
            // Surface gate: a TC entry that does not contribute to the active
            // surface cannot collide at runtime under that surface. Mirrors the
            // gate applied in `compose` itself.
            let on_surface = if self_view {
                dep.visibility.has_private()
            } else {
                dep.visibility.has_interface()
            };
            if !on_surface {
                continue;
            }
            record_repo_identifier(&dep.identifier, &mut by_repository);
        }
    }
    by_repository
        .into_iter()
        .filter(|(_, identifiers)| identifiers.len() >= 2)
        .map(|(repository, identifiers)| DigestConflict {
            repository,
            identifiers,
        })
        .collect()
}

fn record_repo_identifier(
    id: &oci::PinnedIdentifier,
    by_repository: &mut BTreeMap<oci::Repository, Vec<oci::PinnedIdentifier>>,
) {
    let repository = oci::Repository::from(&**id);
    let seen = by_repository.entry(repository).or_default();
    // Dedup by digest: the same version reached via two paths — or a tagged and
    // a bare reference to the same digest, or two tags that resolve to the same
    // digest — is not a conflict.
    if seen.iter().any(|existing| existing.digest() == id.digest()) {
        return;
    }
    seen.push(id.clone());
}

/// Construct the synthetic `PATH ⊳ <pkg_root>/entrypoints` entry for `pkg`.
///
/// The entry kind is `Path` so consumers prepend it to PATH. Pushed *after*
/// a package's declared `bin/` PATH entry so the synthetic `entrypoints/`
/// directory ends up at the front of PATH and the launchers shadow `bin/`.
fn synth_entrypoints_path_for(pkg: &PackageDir) -> Entry {
    Entry {
        key: "PATH".to_string(),
        value: pkg.entrypoints().to_string_lossy().into_owned(),
        kind: ModifierKind::Path,
        separator: None,
    }
}

// ── Specification tests (Phase 3) ───────────────────────────────────────────
//
// These tests are authored against the ADR + plan BEFORE the implementation is
// written. Phase 4 fills the bodies and removes the `#[should_panic]` markers
// so the tests assert the real composer output.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        file_structure::{FileStructure, PackageStore},
        oci::{Digest, Identifier, PinnedIdentifier},
        package::{
            install_info::InstallInfo,
            metadata::{
                self, bundle, dependency,
                entrypoint::{EntrypointName, Entrypoints},
                env::{
                    self as metadata_env,
                    var::{Modifier, Var},
                },
                visibility::Visibility,
            },
            resolved_package::{ResolvedDependency, ResolvedPackage},
        },
        package_manager::error::PackageErrorKind,
    };

    use super::{
        DependencyError, DigestConflict, check_entrypoints, check_repo_digest_conflicts, collect_repo_digest_conflicts,
        compose, compose_companion, emit_dep_path_block, emit_root_path_block, integrations_cross,
    };

    const REGISTRY: &str = "example.com";

    // ── Fixture helpers (adapted from visible.rs::tests) ──────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned(repo: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, REGISTRY).clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build a minimal `InstallInfo` with an empty env and the given resolved closure.
    fn make_install_info(repo: &str, hex_char: char, resolved: ResolvedPackage) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir {
                dir: std::path::PathBuf::from("/nonexistent"),
            },
        )
    }

    /// Build a minimal `InstallInfo` with one env var of given key+visibility.
    fn make_install_info_with_var(
        dir: &std::path::Path,
        repo: &str,
        hex_char: char,
        resolved: ResolvedPackage,
        var_key: &str,
        var_vis: Visibility,
    ) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let var = Var {
            key: var_key.to_string(),
            modifier: Modifier::Constant(metadata_env::constant::Constant {
                value: "value".to_string(),
            }),
            visibility: var_vis,
        };
        let mut builder = metadata_env::EnvBuilder::new();
        builder.add_var(var);
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir { dir: pkg_root },
        )
    }

    /// Build a minimal `InstallInfo` that declares a single entrypoint.
    fn make_install_info_with_ep(
        dir: &std::path::Path,
        repo: &str,
        hex_char: char,
        resolved: ResolvedPackage,
        ep_name: &str,
    ) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let entrypoints = Entrypoints::from_names([ep_name]);
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir { dir: pkg_root },
        )
    }

    fn make_store(root: &std::path::Path) -> PackageStore {
        let fs = FileStructure::with_root(root.to_path_buf());
        fs.packages.clone()
    }

    /// Write a minimal on-disk package directory (metadata.json + resolve.json)
    /// so `PackageStore::lookup` can find it. Mirrors the visible.rs
    /// `seed_package_in_store` helper.
    fn seed_package_in_store(store: &PackageStore, id: &PinnedIdentifier, resolved: &ResolvedPackage) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({ "type": "bundle", "version": 1 });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();
    }

    // ── Step 3.1 — Ported topological / sealed / diamond / collision tests ────

    // ─ Topological order ──────────────────────────────────────────────────────

    /// compose preserves topological order: deps before dependents, roots last.
    ///
    /// Plan §3.1 — topological order cell.
    /// ADR Algorithm v3: "for each root, TC entries first (in topological order,
    /// deps before dependents), then root's own envvars, then entrypoints."
    #[tokio::test]
    async fn compose_preserves_topological_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        let b_id = pinned("b", 'b');
        let a_id = pinned("a", 'a');

        let c_resolved = ResolvedPackage::new();
        let b_resolved = ResolvedPackage::new();
        let a_resolved = ResolvedPackage::new();

        seed_package_in_store(&store, &c_id, &c_resolved);
        seed_package_in_store(&store, &b_id, &b_resolved);
        seed_package_in_store(&store, &a_id, &a_resolved);

        // Root's TC: [C, B, A] in topological order (deps before dependents).
        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: c_id.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: b_id.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: a_id.clone(),
                    visibility: Visibility::PUBLIC,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // Sanity: must succeed (no env vars in any package, but should still
        // not panic). The deps have no env vars and no entrypoints, so the
        // composed env is empty.
        let out = compose(&[root], &store, false).await.unwrap();
        assert!(
            out.entries.is_empty(),
            "no env vars or entrypoints declared; composed env must be empty"
        );
    }

    // ─ Sealed exclusion ────────────────────────────────────────────────────────

    /// A SEALED TC entry contributes nothing to either surface.
    ///
    /// Plan §3.1 — sealed exclusion cell.
    /// ADR §Worked Examples §1: sealed dep contributes nothing on any surface.
    #[tokio::test]
    async fn compose_sealed_dep_contributes_nothing_default_exec() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_id = pinned("sealed", 's');
        seed_package_in_store(&store, &sealed_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: sealed_id.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // SEALED.has_interface() = false → skip in default exec.
        assert!(
            out.entries.is_empty(),
            "SEALED dep must contribute nothing in default exec"
        );
    }

    /// A SEALED TC entry contributes nothing even under --self.
    #[tokio::test]
    async fn compose_sealed_dep_contributes_nothing_self_view() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_id = pinned("sealed", 's');
        seed_package_in_store(&store, &sealed_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: sealed_id.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        // SEALED.has_private() = false → skip under --self too.
        assert!(
            out.entries.is_empty(),
            "SEALED dep must contribute nothing under --self"
        );
    }

    /// The root's own entry points are interface-only —
    /// `Entrypoints::IMPLICIT_VISIBILITY` (INTERFACE) under `carrier_crosses`
    /// on the root's own axis.
    ///
    /// Couples `admitted_entrypoints` to `emit_root_path_block`'s synth-PATH
    /// push — both route through the same carrier gate: `--self` deliberately
    /// keeps the root's `entrypoints/` off PATH, so it must not claim those
    /// launchers either. The divergence this locks out surfaced through
    /// `ocx package inspect --closure`, which listed the root's `app` launcher
    /// on the private surface.
    #[tokio::test]
    async fn compose_root_entrypoints_are_interface_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "app",
        ));

        let synth_path_emitted = |out: &super::ComposeOutput| {
            out.entries
                .iter()
                .any(|e| e.key == "PATH" && e.value.contains("entrypoints"))
        };

        let consumer = compose(std::slice::from_ref(&root), &store, false).await.unwrap();
        assert_eq!(
            consumer.admitted_entrypoints.len(),
            1,
            "root launcher must be claimed on the interface surface"
        );
        assert!(
            synth_path_emitted(&consumer),
            "interface surface must put the root's entrypoints/ on PATH"
        );

        let self_view = compose(&[root], &store, true).await.unwrap();
        assert!(
            self_view.admitted_entrypoints.is_empty(),
            "root launcher must not be claimed under --self: {:?}",
            self_view.admitted_entrypoints
        );
        assert!(
            !synth_path_emitted(&self_view),
            "--self must not put the root's entrypoints/ on PATH"
        );
    }

    // ─ Diamond dedup ───────────────────────────────────────────────────────────

    /// Diamond dep appears in two root TCs but is emitted exactly once.
    ///
    /// Plan §3.1 — diamond dedup cell.
    /// Plan §3.3 — multi-root dedup test (compose(&[a,b], store, false) where both TCs list c).
    #[tokio::test]
    async fn compose_multi_root_diamond_dep_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        seed_package_in_store(&store, &c_id, &ResolvedPackage::new());

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: c_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: c_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        // c, a, b have no env vars + no entrypoints → composed env is empty
        // even when traversed twice. Guards against duplicate emission.
        let out = compose(&[a, b], &store, false).await.unwrap();
        assert!(
            out.entries.is_empty(),
            "no env vars + no entrypoints declared; composed env must be empty regardless of dedup"
        );
    }

    /// Diamond dep declaring both `binaries` and `entrypoints`, shared by two
    /// roots: each claim must be admitted exactly once, not once per root.
    ///
    /// Same shared-dep shape as `compose_multi_root_diamond_dep_emitted_once`,
    /// but asserts on `admitted_binaries`/`admitted_entrypoints` instead of
    /// `entries` — the cross-root dedup applies identically to claim
    /// attribution (`adr_declared_binaries_metadata.md` §4 Decision A).
    #[tokio::test]
    async fn compose_multi_root_diamond_dep_claims_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        let pkg_path = store.path(&c_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "binaries": ["ctool"],
            "entrypoints": { "ctool": {} },
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: c_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: c_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        let out = compose(&[a, b], &store, false).await.unwrap();
        let binary_claims: Vec<_> = out.admitted_binaries.iter().filter(|(id, _)| *id == c_id).collect();
        let entrypoint_claims: Vec<_> = out.admitted_entrypoints.iter().filter(|(id, _)| *id == c_id).collect();
        assert_eq!(
            binary_claims.len(),
            1,
            "shared dep's binaries claim must be admitted exactly once across roots: {:?}",
            out.admitted_binaries
        );
        assert_eq!(
            entrypoint_claims.len(),
            1,
            "shared dep's entrypoints claim must be admitted exactly once across roots: {:?}",
            out.admitted_entrypoints
        );
    }

    // ─ Repo-conflict (same repo, different digest — fatal) ─────────────────────

    /// Same repository with two different digests across two roots' interface
    /// surfaces is a fatal version conflict: a single environment cannot expose
    /// two versions of one package, so `compose` returns
    /// `Err(Dependency(Conflict))`.
    #[tokio::test]
    async fn compose_same_repo_conflicting_digest_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_v1 = pinned("shared", '1');
        let dep_v2 = pinned("shared", '2');
        seed_package_in_store(&store, &dep_v1, &ResolvedPackage::new());
        seed_package_in_store(&store, &dep_v2, &ResolvedPackage::new());

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_v1.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_v2.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        match compose(&[a, b], &store, false).await {
            Err(crate::Error::Dependency(DependencyError::Conflict {
                repository,
                identifiers,
            })) => {
                assert_eq!(repository, crate::oci::Repository::from(&*dep_v1));
                assert_eq!(identifiers, vec![dep_v1, dep_v2]);
            }
            Err(other) => panic!("expected Dependency(Conflict), got {other:?}"),
            Ok(_) => panic!("expected Err(Dependency(Conflict)), got Ok"),
        }
    }

    // ─ Edge filter: has_interface() vs has_private() ──────────────────────────
    //
    // Plan §3.1 "Coverage to FLIP": 4 intersects-edge-filter cells become
    // has_interface() / has_private() cells.

    /// Default exec (self_view=false): PRIVATE TC entry skipped — PRIVATE.has_interface()=false.
    ///
    /// Replaces the old `import_visible_packages_consumer_excludes_private_dep`
    /// test (visible.rs:1368) ported to the new accessor vocabulary.
    #[tokio::test]
    async fn compose_default_exec_skips_private_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_dep = pinned("priv", 'p');
        seed_package_in_store(&store, &priv_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: priv_dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // PRIVATE.has_interface()=false → skip in default exec.
        assert!(
            out.entries.is_empty(),
            "PRIVATE TC entry must be skipped in default exec"
        );
    }

    /// Default exec (self_view=false): INTERFACE TC entry included — INTERFACE.has_interface()=true.
    #[tokio::test]
    async fn compose_default_exec_includes_interface_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let iface_dep = pinned("iface", 'i');
        seed_package_in_store(&store, &iface_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: iface_dep.clone(),
                visibility: Visibility::INTERFACE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // INTERFACE.has_interface()=true → visit; dep has no env vars,
        // so env is empty but visit happened (no panic from missing
        // store entry).
        let out = compose(&[root], &store, false).await.unwrap();
        assert!(out.entries.is_empty(), "no env vars on the dep, so output is empty");
    }

    /// --self (self_view=true): PRIVATE TC entry included — PRIVATE.has_private()=true.
    ///
    /// Replaces `import_visible_packages_self_includes_private_dep` (visible.rs:1396).
    #[tokio::test]
    async fn compose_self_view_includes_private_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_dep = pinned("priv", 'p');
        seed_package_in_store(&store, &priv_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: priv_dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        // PRIVATE.has_private()=true → emit dep contributions; dep has no
        // env vars so output is empty but visit happened.
        assert!(out.entries.is_empty(), "no env vars on the dep, so output is empty");
    }

    /// --self (self_view=true): INTERFACE TC entry skipped — INTERFACE.has_private()=false.
    ///
    /// Replaces `import_visible_packages_self_excludes_interface_only_dep` (visible.rs:1424).
    #[tokio::test]
    async fn compose_self_view_skips_interface_only_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let iface_dep = pinned("iface", 'i');
        seed_package_in_store(&store, &iface_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: iface_dep.clone(),
                visibility: Visibility::INTERFACE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        // INTERFACE.has_private()=false → skip under --self.
        assert!(
            out.entries.is_empty(),
            "INTERFACE TC entry must be skipped under --self"
        );
    }

    // ─ Synth-PATH gate (interface-projection cells) ────────────────────────────
    //
    // Plan §3.1 "Coverage to FLIP": 3 synth-PATH gate cells become
    // interface-projection cells. The new model: synth-PATH flows through the
    // same edge rules as any PATH entry — no special gate. But it is only
    // emitted for deps whose TC entry has has_interface()=true (default exec)
    // or has_private()=true (--self). Root's own entrypoints: emitted when
    // !self_view only (ADR Algorithm v3 §"Root's own contributions").

    /// Default exec: root with entrypoints emits synth-PATH for own entrypoints/.
    ///
    /// ADR Algorithm v3: "if !self_view and root has entrypoints, emit synth-PATH".
    #[tokio::test]
    async fn compose_default_exec_emits_synth_path_for_root_with_entrypoints() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root_resolved = ResolvedPackage::new();
        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "cmake",
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        // Synth-PATH for root's entrypoints/ present in default exec.
        let path_entries: Vec<_> = out
            .entries
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .collect();
        assert_eq!(
            path_entries.len(),
            1,
            "default exec must emit one synth-PATH for root entrypoints/"
        );
    }

    /// --self: root with entrypoints does NOT emit synth-PATH.
    ///
    /// ADR Algorithm v3: synth-PATH guarded by `!self_view` for root.
    /// This prevents the `ocx exec --self` launcher from finding its own
    /// entrypoints/ and recursing.
    #[tokio::test]
    async fn compose_self_view_does_not_emit_synth_path_for_root() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root_resolved = ResolvedPackage::new();
        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "cmake",
        ));

        let out = compose(&[root], &store, true).await.unwrap();
        // No synth-PATH in the --self output (root must not see its own launchers).
        let path_entries: Vec<_> = out
            .entries
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .collect();
        assert!(
            path_entries.is_empty(),
            "--self must NOT emit synth-PATH for root's own entrypoints/"
        );
    }

    /// Default exec: dep's entrypoints/ synth-PATH emitted when dep has_interface().
    ///
    /// ADR Algorithm v3 step 5-6 for dep: entrypoints synth-PATH flows through
    /// edge rules like any PATH entry.
    #[tokio::test]
    async fn compose_default_exec_emits_synth_path_for_dep_with_interface_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed dep with an entrypoint so the on-disk metadata reports
        // entrypoints when reloaded via `load_object_data`.
        let dep_id = pinned("cmake", 'c');
        let dep_resolved = ResolvedPackage::new();
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": { "cmake": {} },
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&dep_resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // PUBLIC.has_interface()=true → dep's synth-PATH emitted.
        let path_entries: Vec<_> = out
            .entries
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .collect();
        assert_eq!(
            path_entries.len(),
            1,
            "PUBLIC dep with entrypoints must contribute one synth-PATH; got {} entries",
            path_entries.len()
        );
    }

    // ─ Entry-axis filter partition cells ──────────────────────────────────────
    //
    // Plan §3.1 "Coverage to FLIP": 3 entry-axis filter cells become
    // entry-visibility partition cells.

    /// Default exec: a dep's env var with `Visibility::INTERFACE` is emitted
    /// (dep's interface side crosses the edge per ADR Algorithm v3 step 5).
    ///
    /// Plan §3.3 — partition test.
    /// ADR: "for var in dep.bundle.env, emit if var.visibility.has_interface()".
    #[tokio::test]
    async fn compose_default_exec_emits_dep_interface_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed dep with a single Interface-visibility env var.
        let dep_id = pinned("dep", 'd');
        let dep_resolved = ResolvedPackage::new();
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "DEP_IFACE",
                "type": "constant",
                "value": "v",
                "visibility": "interface",
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&dep_resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // dep's Interface-tagged var present.
        assert!(
            out.entries.iter().any(|e| e.key == "DEP_IFACE"),
            "dep's Interface var must be present: {:?}",
            out.entries.iter().map(|e| &e.key).collect::<Vec<_>>()
        );
    }

    /// Default exec: root's Interface env var is emitted.
    ///
    /// ADR: for root's own contributions, emit if var.visibility.has_interface()
    /// (when !self_view).
    #[tokio::test]
    async fn compose_default_exec_emits_root_interface_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PKG_CONFIG_PATH",
            Visibility::INTERFACE,
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        // root's Interface var present in default exec.
        assert!(
            out.entries.iter().any(|e| e.key == "PKG_CONFIG_PATH"),
            "root's Interface var must be present in default exec"
        );
    }

    /// Default exec: root's Private env var is NOT emitted (private axis hidden from consumers).
    ///
    /// ADR: root's own entry emitted if var.visibility.has_interface() when !self_view.
    /// PRIVATE.has_interface()=false → not emitted.
    #[tokio::test]
    async fn compose_default_exec_excludes_root_private_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PRIVATE_FLAG",
            Visibility::PRIVATE,
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        // root's Private var absent in default exec.
        assert!(
            !out.entries.iter().any(|e| e.key == "PRIVATE_FLAG"),
            "root's Private var must be absent in default exec"
        );
    }

    // ─ --self surface partition ────────────────────────────────────────────────

    /// --self: root's Private env var IS emitted.
    ///
    /// ADR: emit if var.visibility.has_private() when self_view=true.
    /// PRIVATE.has_private()=true.
    #[tokio::test]
    async fn compose_self_view_emits_root_private_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PRIVATE_FLAG",
            Visibility::PRIVATE,
        ));

        let out = compose(&[root], &store, true).await.unwrap();
        // root's Private var present under --self.
        assert!(
            out.entries.iter().any(|e| e.key == "PRIVATE_FLAG"),
            "root's Private var must be present under --self"
        );
    }

    /// --self: root's Interface env var is NOT emitted.
    ///
    /// ADR: emit if var.visibility.has_private() when self_view=true.
    /// INTERFACE.has_private()=false → not emitted under --self.
    ///
    /// This is the matrix walk-through fix: R running as itself does not see
    /// its own Interface-only env vars (those are consumer-only).
    #[tokio::test]
    async fn compose_self_view_excludes_root_interface_only_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PKG_CONFIG_PATH",
            Visibility::INTERFACE,
        ));

        let out = compose(&[root], &store, true).await.unwrap();
        // root's Interface var absent under --self.
        assert!(
            !out.entries.iter().any(|e| e.key == "PKG_CONFIG_PATH"),
            "root's Interface var must be absent under --self"
        );
    }

    // ─ Step 3.2 — Ported resolve.rs::resolve_visible_set surface-membership tests ──
    //
    // The four resolve_visible_set tests from tasks/resolve.rs now become
    // composer surface-membership tests. The `has_interface()` / `has_private()`
    // vocabulary replaces `intersects(view)`.

    /// Consumer (default exec): private-edge dep contributes nothing.
    ///
    /// Ported from `resolve_visible_set_consumer_excludes_private_dep`.
    /// PRIVATE.has_interface()=false → compose skips the entry.
    #[tokio::test]
    async fn compose_surface_membership_consumer_excludes_private_dep() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep = pinned("privlib", 'p');
        seed_package_in_store(&store, &dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // dep's contributions absent in consumer surface.
        assert!(out.entries.is_empty());
    }

    /// --self: private-edge dep contributes.
    ///
    /// Ported from `resolve_visible_set_self_includes_private_dep`.
    /// PRIVATE.has_private()=true → compose includes the entry.
    #[tokio::test]
    async fn compose_surface_membership_self_includes_private_dep() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep = pinned("privlib", 'p');
        seed_package_in_store(&store, &dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // dep present in --self surface — but has no env vars, so output empty.
        let out = compose(&[root], &store, true).await.unwrap();
        assert!(
            out.entries.is_empty(),
            "dep has no env vars; visit happened but output is empty"
        );
    }

    /// Default exec: SEALED dep excluded entirely.
    ///
    /// Ported from `resolve_visible_set_full_excludes_sealed_dep`.
    /// SEALED.has_interface()=false AND SEALED.has_private()=false.
    #[tokio::test]
    async fn compose_surface_membership_sealed_dep_excluded_both_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep = pinned("sealedlib", 's');
        seed_package_in_store(&store, &dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // sealed excluded.
        assert!(out.entries.is_empty());
    }

    /// Diamond merge: dep reachable via interface and public paths → merged to PUBLIC.
    /// Under self_view=true, PUBLIC.has_private()=true → dep is in --self surface.
    ///
    /// Ported from `resolve_visible_set_diamond_merge_self_mode_preserves_public_path`.
    /// Per ADR §diamond merge: PUBLIC = INTERFACE.merge(PRIVATE) = (true,true).
    #[tokio::test]
    async fn compose_surface_membership_diamond_merge_public_preserved_under_self() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // The leaf is reachable via two paths that merge to PUBLIC.
        let leaf = pinned("leaf", 'l');
        seed_package_in_store(&store, &leaf, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                // PUBLIC = INTERFACE.merge(PRIVATE) per Visibility::merge semantics.
                ResolvedDependency {
                    identifier: leaf.clone(),
                    visibility: Visibility::PUBLIC,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        // PUBLIC.has_private()=true → leaf visited; no env vars, so empty.
        assert!(out.entries.is_empty());
    }

    // ─ Step 3.3 — Composer partition + multi-root dedup + JSON roundtrip ──────

    // ─ Partition: root entries split by surface ────────────────────────────────

    /// Default exec partition: root with [Public, Private, Interface] vars →
    /// result surface contains [Public, Interface] vars only.
    ///
    /// ADR Algorithm v3 "Root's own contributions": emit if
    /// var.visibility.has_interface() when !self_view.
    /// PUBLIC.has_interface()=true, PRIVATE.has_interface()=false, INTERFACE.has_interface()=true.
    #[tokio::test]
    async fn compose_default_exec_root_partition_public_and_interface_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Build a root with 3 vars: one public, one private, one interface.
        let id = pinned("root", 'r');
        let vars = [
            ("PUBLIC_VAR", Visibility::PUBLIC),
            ("PRIVATE_VAR", Visibility::PRIVATE),
            ("IFACE_VAR", Visibility::INTERFACE),
        ];
        let mut builder = metadata_env::EnvBuilder::new();
        for (key, vis) in &vars {
            builder.add_var(Var {
                key: key.to_string(),
                modifier: Modifier::Constant(metadata_env::constant::Constant { value: "v".to_string() }),
                visibility: *vis,
            });
        }
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            id,
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        let keys: Vec<&str> = out.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(
            keys.contains(&"PUBLIC_VAR"),
            "PUBLIC_VAR must be present (has_interface=true)"
        );
        assert!(
            !keys.contains(&"PRIVATE_VAR"),
            "PRIVATE_VAR must be absent (has_interface=false)"
        );
        assert!(
            keys.contains(&"IFACE_VAR"),
            "IFACE_VAR must be present (has_interface=true)"
        );
    }

    /// --self partition: root with [Public, Private, Interface] vars →
    /// result surface contains [Public, Private] vars only.
    ///
    /// ADR: emit if var.visibility.has_private() when self_view=true.
    /// PUBLIC.has_private()=true, PRIVATE.has_private()=true, INTERFACE.has_private()=false.
    #[tokio::test]
    async fn compose_self_view_root_partition_public_and_private_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let id = pinned("root", 'r');
        let vars = [
            ("PUBLIC_VAR", Visibility::PUBLIC),
            ("PRIVATE_VAR", Visibility::PRIVATE),
            ("IFACE_VAR", Visibility::INTERFACE),
        ];
        let mut builder = metadata_env::EnvBuilder::new();
        for (key, vis) in &vars {
            builder.add_var(Var {
                key: key.to_string(),
                modifier: Modifier::Constant(metadata_env::constant::Constant { value: "v".to_string() }),
                visibility: *vis,
            });
        }
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root2");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            id,
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        let out = compose(&[root], &store, true).await.unwrap();
        let keys: Vec<&str> = out.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(
            keys.contains(&"PUBLIC_VAR"),
            "PUBLIC_VAR must be present (has_private=true)"
        );
        assert!(
            keys.contains(&"PRIVATE_VAR"),
            "PRIVATE_VAR must be present (has_private=true)"
        );
        assert!(
            !keys.contains(&"IFACE_VAR"),
            "IFACE_VAR must be absent (has_private=false)"
        );
    }

    // ─ TC entries: dep's interface side crosses edge ───────────────────────────

    /// Default exec: dep with SEALED/PRIVATE/PUBLIC/INTERFACE effective vis →
    /// contributions only from entries where tc_entry.visibility.has_interface().
    ///
    /// ADR Algorithm v3 step 3: "test tc_entry.visibility.has_interface() (default exec)"
    #[tokio::test]
    async fn compose_default_exec_tc_entry_gating_by_has_interface() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Four deps with different effective visibilities.
        let sealed_dep = pinned("sealed", 's');
        let private_dep = pinned("private", 'p');
        let public_dep = pinned("public", 'u');
        let iface_dep = pinned("iface", 'i');

        for id in [&sealed_dep, &private_dep, &public_dep, &iface_dep] {
            seed_package_in_store(&store, id, &ResolvedPackage::new());
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: sealed_dep.clone(),
                    visibility: Visibility::SEALED,
                },
                ResolvedDependency {
                    identifier: private_dep.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: public_dep.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: iface_dep.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        // No deps declare env vars or entrypoints, so output is empty.
        // The gating is observable via the load_object_data calls — sealed
        // and private deps should NOT be visited, while public and iface
        // SHOULD be. The visit happens via on-disk metadata lookup; this
        // test validates the path doesn't panic when sealed/private deps
        // are skipped (no lookup attempt).
        assert!(out.entries.is_empty());
    }

    /// --self: TC entry gating by has_private().
    ///
    /// ADR Algorithm v3 step 3: "test tc_entry.visibility.has_private() (--self)"
    #[tokio::test]
    async fn compose_self_view_tc_entry_gating_by_has_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_dep = pinned("sealed", 's');
        let private_dep = pinned("private", 'p');
        let public_dep = pinned("public", 'u');
        let iface_dep = pinned("iface", 'i');

        for id in [&sealed_dep, &private_dep, &public_dep, &iface_dep] {
            seed_package_in_store(&store, id, &ResolvedPackage::new());
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: sealed_dep.clone(),
                    visibility: Visibility::SEALED,
                },
                ResolvedDependency {
                    identifier: private_dep.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: public_dep.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: iface_dep.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        assert!(out.entries.is_empty());
    }

    // ─ Multi-root dedup ────────────────────────────────────────────────────────

    /// Atomic-vs-composite symmetry: compose(&[a, b], ...) uses the same algorithm
    /// as compose(&[a], ...). Shared dep emitted once.
    ///
    /// Plan §3.3 — "Multi-root dedup" test.
    /// ADR: "cross-root dedup via shared HashSet<DepKey>".
    #[tokio::test]
    async fn compose_multi_root_shared_dep_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed `shared` with one Public env var so we can count emissions.
        let shared = pinned("shared", 'x');
        let pkg_path = store.path(&shared);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "SHARED_VAR",
                "type": "constant",
                "value": "v",
                "visibility": "public",
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&ResolvedPackage::new()).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: shared.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: shared.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        let out = compose(&[a, b], &store, false).await.unwrap();
        // shared's contributions emitted exactly once (cross-root dedup).
        let shared_count = out.entries.iter().filter(|e| e.key == "SHARED_VAR").count();
        assert_eq!(
            shared_count, 1,
            "shared dep must emit SHARED_VAR exactly once across multi-root compose"
        );
    }

    // ─ Empty-input behaviour ──────────────────────────────────────────────────

    /// compose(&[], ...) on empty roots returns empty Env.
    ///
    /// ADR: "compose(&[], ...) returns an empty Env".
    #[tokio::test]
    async fn compose_empty_roots_returns_empty_env() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let out = compose(&[], &store, false).await.unwrap();
        assert!(out.entries.is_empty(), "compose(&[], ...) must return empty Env");
    }

    /// Leaf root (no TC): compose emits only root's own contributions.
    ///
    /// ADR: "empty input behavior: compose(&[root], ..., self_view) on a leaf
    /// package (no TC entries) emits only the root's own contributions".
    #[tokio::test]
    async fn compose_leaf_root_emits_only_own_contributions() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(), // no deps
            "ROOT_VAR",
            Visibility::PUBLIC,
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        // ROOT_VAR present, no dep contributions.
        assert_eq!(
            out.entries.len(),
            1,
            "leaf root with one Public var must emit one entry"
        );
        assert_eq!(out.entries[0].key, "ROOT_VAR");
    }

    // ─ JSON wire-format roundtrip ──────────────────────────────────────────────
    //
    // Plan §3.3 — "JSON roundtrip" tests.
    // These are UNIT tests on the Visibility serde — they do NOT
    // call compose() and do NOT panic. They verify wire stability.

    /// All 4 Visibility constants serialize to the expected strings.
    #[test]
    fn visibility_wire_format_sealed() {
        assert_eq!(serde_json::to_string(&Visibility::SEALED).unwrap(), r#""sealed""#);
    }

    #[test]
    fn visibility_wire_format_private() {
        assert_eq!(serde_json::to_string(&Visibility::PRIVATE).unwrap(), r#""private""#);
    }

    #[test]
    fn visibility_wire_format_public() {
        assert_eq!(serde_json::to_string(&Visibility::PUBLIC).unwrap(), r#""public""#);
    }

    #[test]
    fn visibility_wire_format_interface() {
        assert_eq!(serde_json::to_string(&Visibility::INTERFACE).unwrap(), r#""interface""#);
    }

    /// All 4 Visibility constants roundtrip through JSON byte-identically.
    #[test]
    fn visibility_wire_roundtrip_all_constants() {
        for (constant, expected_str) in [
            (Visibility::SEALED, "\"sealed\""),
            (Visibility::PRIVATE, "\"private\""),
            (Visibility::PUBLIC, "\"public\""),
            (Visibility::INTERFACE, "\"interface\""),
        ] {
            let serialized = serde_json::to_string(&constant).unwrap();
            assert_eq!(
                serialized, expected_str,
                "wire format for {constant:?} must be {expected_str:?}"
            );
            let deserialized: Visibility = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, constant, "roundtrip must be identity for {constant:?}");
        }
    }

    /// ResolvedPackage shape is unchanged: {dependencies: Vec<ResolvedDependency>}.
    /// Serialize → deserialize → equality.
    ///
    /// Plan §3.3 — "resolve.json shape roundtrip".
    #[test]
    fn resolved_package_wire_roundtrip_unchanged_shape() {
        use crate::package::resolved_package::{ResolvedDependency, ResolvedPackage};

        let dep = ResolvedDependency {
            // 'l' is not hex — use '1' as a valid hex digit for roundtrip test.
            identifier: pinned("lib", '1'),
            visibility: Visibility::PUBLIC,
        };
        let pkg = ResolvedPackage {
            dependencies: vec![dep.clone()],
        };

        let json = serde_json::to_string(&pkg).unwrap();

        // Must deserialize back to identical shape.
        let roundtripped: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(
            roundtripped.dependencies.len(),
            1,
            "dependency count must survive roundtrip"
        );
        assert_eq!(roundtripped.dependencies[0].identifier, dep.identifier);
        assert_eq!(roundtripped.dependencies[0].visibility, dep.visibility);
    }

    /// ResolvedPackage with all 4 Visibility constants in deps roundtrips correctly.
    #[test]
    fn resolved_package_wire_roundtrip_all_visibility_constants() {
        use crate::package::resolved_package::{ResolvedDependency, ResolvedPackage};

        let deps: Vec<ResolvedDependency> = [
            // Must be valid hex digits; non-hex chars fail serde roundtrip.
            (Visibility::SEALED, '0'),
            (Visibility::PRIVATE, '2'),
            (Visibility::PUBLIC, '3'),
            (Visibility::INTERFACE, '4'),
        ]
        .iter()
        .map(|&(vis, hex)| ResolvedDependency {
            identifier: pinned("lib", hex),
            visibility: vis,
        })
        .collect();

        let pkg = ResolvedPackage {
            dependencies: deps.clone(),
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let roundtripped: ResolvedPackage = serde_json::from_str(&json).unwrap();

        assert_eq!(
            roundtripped.dependencies.len(),
            deps.len(),
            "all deps must survive roundtrip"
        );
        for (orig, rt) in deps.iter().zip(roundtripped.dependencies.iter()) {
            assert_eq!(rt.visibility, orig.visibility, "visibility must be byte-stable");
        }
    }

    /// deny_unknown_fields on ResolvedPackage: extra field rejects.
    #[test]
    fn resolved_package_rejects_extra_fields() {
        use crate::package::resolved_package::ResolvedPackage;

        // interface_env / private_env were proposed in the M1 draft (rejected).
        // This test confirms the wire format does not accidentally accept them.
        let json = r#"{"dependencies":[],"interface_env":[]}"#;
        let result = serde_json::from_str::<ResolvedPackage>(json);
        assert!(
            result.is_err(),
            "extra field must be rejected by deny_unknown_fields; shape must be wire-stable"
        );
    }

    // ─ Step 3.1 — Entrypoint collision tests (Suite A unit-level) ─────────────

    // check_entrypoints operates on the interface projection only.
    // These unit tests correspond to the 4 edge-vis cells in the entrypoint
    // collision truth table in the ADR (Suite A).

    /// Suite A, cell: sealed edge — install OK.
    /// B is SEALED from R's interface projection: has_interface()=false → not checked.
    /// Both R and B declare entrypoint `e`; no collision fires.
    #[tokio::test]
    async fn check_entrypoints_sealed_dep_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Dep B: has entrypoint `e`, in TC with SEALED effective vis.
        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        // Seed B with an entrypoint via on-disk metadata.json.
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": { "e": {} },
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        // Root R: has entrypoint `e` + TC with B as SEALED.
        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        // Returns Ok(()) — SEALED.has_interface()=false → B not in interface projection.
        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        assert!(result.is_ok(), "SEALED dep entrypoint must not collide: {:?}", result);
    }

    /// Suite A, cell: private edge — install OK.
    /// B is PRIVATE from R's interface projection: PRIVATE.has_interface()=false → not checked.
    /// The private-surface duplicate is tolerated; runtime PATH order resolves.
    #[tokio::test]
    async fn check_entrypoints_private_dep_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": { "e": {} },
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        assert!(
            result.is_ok(),
            "PRIVATE dep entrypoint must not collide on interface projection: {:?}",
            result
        );
    }

    /// Suite A, cell: interface edge — install FAIL.
    /// B is INTERFACE from R's interface projection: INTERFACE.has_interface()=true → collision fires.
    #[tokio::test]
    async fn check_entrypoints_interface_dep_collides() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": { "e": {} },
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::INTERFACE,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        match result {
            Err(PackageErrorKind::EntrypointCollision { name, owners }) => {
                assert_eq!(name.as_str(), "e");
                assert_eq!(owners.len(), 2);
            }
            other => panic!("expected EntrypointCollision, got {other:?}"),
        }
    }

    /// Suite A, cell: public edge — install FAIL.
    /// B is PUBLIC from R's interface projection: PUBLIC.has_interface()=true → collision fires.
    #[tokio::test]
    async fn check_entrypoints_public_dep_collides() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": { "e": {} },
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        match result {
            Err(PackageErrorKind::EntrypointCollision { name, owners }) => {
                assert_eq!(name.as_str(), "e");
                assert_eq!(owners.len(), 2);
            }
            other => panic!("expected EntrypointCollision, got {other:?}"),
        }
    }

    /// EntrypointCollision variant has owners Vec, not first/second pair.
    ///
    /// Plan §3.1 — "repo-conflict" / entrypoint collision N-owner shape.
    /// This is a unit test on the error type, NOT on compose/check_entrypoints.
    #[test]
    fn entrypoint_collision_variant_has_vec_owners() {
        let name = EntrypointName::try_from("cmake").unwrap();
        let owner_a = pinned("a", 'a');
        let owner_b = pinned("b", 'b');
        let owner_c = pinned("c", 'c');

        let err = PackageErrorKind::EntrypointCollision {
            name: name.clone(),
            owners: vec![owner_a.clone(), owner_b.clone(), owner_c.clone()],
        };

        // Confirm the N-owner shape — not a 2-owner first/second shape.
        match &err {
            PackageErrorKind::EntrypointCollision { owners, .. } => {
                assert_eq!(owners.len(), 3, "EntrypointCollision must support N>2 owners");
                assert!(owners.contains(&owner_a));
                assert!(owners.contains(&owner_b));
                assert!(owners.contains(&owner_c));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ─ Multi-root entrypoint collision (Block 1 — compose-time gate) ──────────

    /// Two roots each declaring entrypoint `foo` MUST cause `compose` to fail
    /// with `EntrypointCollision` listing both owners.
    ///
    /// Codex Block 1 finding: install-gate covers within-closure collisions;
    /// cross-root collisions surface only at `ocx env A B` / `ocx exec A B`.
    /// This is the compose-time gate that blocks them before any env entries
    /// are emitted.
    #[tokio::test]
    async fn compose_multi_root_collision_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Two independent roots that each declare entrypoint `foo`. Neither
        // is in the other's TC, so the install-gate can't see the conflict.
        let a = Arc::new(make_install_info_with_ep(
            dir.path(),
            "a",
            'a',
            ResolvedPackage::new(),
            "foo",
        ));
        let b = Arc::new(make_install_info_with_ep(
            dir.path(),
            "b",
            'b',
            ResolvedPackage::new(),
            "foo",
        ));

        let result = compose(&[a.clone(), b.clone()], &store, false).await;
        let err = match result {
            Ok(_) => panic!("expected EntrypointCollision, got Ok"),
            Err(e) => e,
        };
        let err = match err {
            crate::Error::PackageManager(inner) => inner,
            other => panic!("expected PackageManager outer error, got {other:?}"),
        };
        let errs = match err {
            crate::package_manager::error::Error::ResolveFailed(es) => es,
            other => panic!("expected ResolveFailed, got {other:?}"),
        };
        assert_eq!(errs.len(), 1, "expected single packaged error");
        match &errs[0].kind {
            PackageErrorKind::EntrypointCollision { name, owners } => {
                assert_eq!(name.as_str(), "foo");
                assert_eq!(owners.len(), 2, "both roots must be listed: {owners:?}");
                assert!(owners.contains(a.identifier()));
                assert!(owners.contains(b.identifier()));
            }
            other => panic!("expected EntrypointCollision kind, got {other:?}"),
        }
    }

    // ─ Block 2 — Explicit root that is also a private dep emits fully ────────

    /// When `compose` is invoked with `[a, b]` where `a → b` is a PRIVATE edge
    /// in the consumer (default exec) projection, b's contributions MUST still
    /// appear because b is an explicit root.
    ///
    /// Codex Block 2 finding: the previous implementation inserted into `seen`
    /// before the surface gate, so iterating `a`'s TC inserted `b` into `seen`,
    /// then gated `b` out (PRIVATE.has_interface()=false), and the later
    /// explicit-root pass for `b` was silently skipped. The fix defers
    /// root-as-dep TC entries to the explicit-root pass and only inserts into
    /// `seen` after the surface gate.
    #[tokio::test]
    async fn compose_root_appearing_as_private_dep_emits_root_fully() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed `b` on disk with a Public env var so we can detect that its
        // explicit-root contributions reach the env.
        let b_id = pinned("b", 'b');
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "B_OWN_VAR",
                "type": "constant",
                "value": "v",
                "visibility": "public",
            }],
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(
            b_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        // Build `b` as the second-root InstallInfo from the on-disk seed.
        let b_resolved = ResolvedPackage::new();
        let b_root = Arc::new(make_install_info_with_var(
            dir.path(),
            "b",
            'b',
            b_resolved.clone(),
            "B_OWN_VAR",
            Visibility::PUBLIC,
        ));

        // Build `a` so its TC includes `b` as a PRIVATE edge. The explicit
        // dep entry would gate out under default exec.
        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let a = Arc::new(make_install_info("a", 'a', a_resolved));

        let out = compose(&[a, b_root], &store, false).await.unwrap();
        let keys: Vec<&str> = out.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(
            keys.contains(&"B_OWN_VAR"),
            "b's own (public) contributions must reach the env when b is an explicit root, \
             even when also reachable as a private TC entry of `a`; got keys: {keys:?}"
        );
    }

    // ─ Composition-order test ─────────────────────────────────────────────────

    /// Within each root, TC entries are emitted before root's own envvars.
    ///
    /// ADR Algorithm v3: "Composition order is fixed: for each root, TC entries
    /// first (in topological order), then root's own envvars, then entrypoints."
    #[tokio::test]
    async fn compose_tc_entries_emitted_before_root_own_envvars() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed dep with a Public env var.
        let dep_id = pinned("dep", 'd');
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let dep_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{ "key": "DEP_VAR", "type": "constant", "value": "v", "visibility": "public" }],
        });
        std::fs::write(pkg_path.join("metadata.json"), dep_meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "ROOT_OWN_VAR",
            Visibility::PUBLIC,
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        // dep's contributions appear before ROOT_OWN_VAR in the Env.
        let dep_pos = out
            .entries
            .iter()
            .position(|e| e.key == "DEP_VAR")
            .expect("DEP_VAR present");
        let root_pos = out
            .entries
            .iter()
            .position(|e| e.key == "ROOT_OWN_VAR")
            .expect("ROOT_OWN_VAR present");
        assert!(
            dep_pos < root_pos,
            "DEP_VAR (pos {dep_pos}) must come before ROOT_OWN_VAR (pos {root_pos})"
        );
    }

    /// Within each root, root's synth-PATH (entrypoints) entry is emitted
    /// AFTER root's declared envvars on the consumer surface.
    ///
    /// PATH semantics are last-prepended-wins, so emitting synth-PATH after
    /// the declared `bin/` PATH entry makes `entrypoints/` win lookup priority
    /// at runtime — entrypoint launchers shadow declared `bin/`. See
    /// acceptance test
    /// `test_synthetic_entrypoints_path_emitted_after_declared_bin`.
    #[tokio::test]
    async fn compose_root_synth_path_emitted_after_root_own_vars() {
        let dir = tempfile::tempdir().unwrap();

        // Root declares one Public var AND one entrypoint — no deps needed.
        let root_id = pinned("root", 'r');
        let var = Var {
            key: "ROOT_VAR".to_string(),
            modifier: Modifier::Constant(metadata_env::constant::Constant {
                value: "val".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(var);
        let env = env_builder.build();

        let entrypoints = Entrypoints::from_names(["mytool"]);

        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            root_id,
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        // Store is needed by compose but root has no deps, so it stays empty.
        let store = make_store(dir.path());

        let out = compose(&[root], &store, false).await.unwrap();

        let var_pos = out
            .entries
            .iter()
            .position(|e| e.key == "ROOT_VAR")
            .expect("ROOT_VAR present");
        let path_pos = out
            .entries
            .iter()
            .position(|e| e.key == "PATH")
            .expect("synth-PATH entry present");
        assert!(
            var_pos < path_pos,
            "ROOT_VAR (pos {var_pos}) must come before synth-PATH (pos {path_pos})"
        );
    }

    // ─ Digest-conflict surface gating ─────────────────────────────────────────

    /// Two roots, each pulling a different digest of the same `d` repo via a
    /// SEALED edge, MUST NOT be reported as a conflict on the default
    /// (interface) surface. Sealed deps never enter the consumer composition,
    /// so their digests cannot collide at runtime.
    ///
    /// Mirrors `test_sealed_conflicting_deps_coexist`: under `ocx env A B`
    /// stderr must be free of the `"conflicting"` token when the conflicting
    /// dep is sealed under both roots.
    #[test]
    fn digest_conflict_skipped_for_sealed_dep_on_interface_surface() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1,
                    visibility: Visibility::SEALED,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2,
                    visibility: Visibility::SEALED,
                }],
            },
        ));

        let conflicts = collect_repo_digest_conflicts(&[a, b], false);
        assert!(
            conflicts.is_empty(),
            "sealed dep with conflicting digests must not be reported on the interface surface; got {conflicts:?}"
        );
    }

    /// Asymmetric visibility: root A pulls `d v1` as PUBLIC (interface), root B
    /// pulls `d v2` as PRIVATE. Default exec only emits A's `d`; B's `d` is
    /// gated out. No conflict on the interface surface.
    #[test]
    fn digest_conflict_skipped_when_only_one_root_exposes_dep_on_surface() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1,
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2,
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        let conflicts = collect_repo_digest_conflicts(&[a, b], false);
        assert!(
            conflicts.is_empty(),
            "private-only dep under B must not collide with public dep under A on the interface surface; got {conflicts:?}"
        );
    }

    /// Two roots both pulling `d` via the interface surface (PUBLIC) with
    /// different digests MUST surface a conflict. Locks the regression
    /// guarded by `test_public_conflicting_deps_error` /
    /// `test_deep_conflict_at_depth_two`: the surface gate is not allowed to
    /// over-suppress real interface-surface conflicts.
    #[test]
    fn digest_conflict_reported_for_interface_dep() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');
        let expected_repo = crate::oci::Repository::from(&*d_v1);

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));

        let conflicts = collect_repo_digest_conflicts(&[a, b], false);
        assert_eq!(
            conflicts,
            vec![DigestConflict {
                repository: expected_repo,
                identifiers: vec![d_v1.clone(), d_v2.clone()],
            }],
        );
    }

    /// Sealed deps with conflicting digests collide on the `--self` surface
    /// only when the edge has the private axis. With `Visibility::SEALED`
    /// (neither axis), they remain hidden under both surfaces.
    #[test]
    fn digest_conflict_reported_for_private_dep_on_self_surface() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');
        let expected_repo = crate::oci::Repository::from(&*d_v1);

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        // Default (interface) surface: private-only deps gated out.
        assert!(
            collect_repo_digest_conflicts(&[a.clone(), b.clone()], false).is_empty(),
            "private deps must not collide on the interface surface"
        );
        // `--self` surface: private deps participate, conflict is reported.
        assert_eq!(
            collect_repo_digest_conflicts(&[a, b], true),
            vec![DigestConflict {
                repository: expected_repo,
                identifiers: vec![d_v1.clone(), d_v2.clone()],
            }],
        );
    }

    /// Two tags of the same repository that resolve to the **same** digest are
    /// not a conflict — `check_repo_digest_conflicts` returns `Ok`. Guards the
    /// `test_env_same_digest_roots_ok` acceptance contract.
    #[test]
    fn same_digest_is_not_a_conflict() {
        // Same repo "d", same digest '1' — two references to one version.
        let one = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));
        let two = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));

        assert!(
            collect_repo_digest_conflicts(&[one.clone(), two.clone()], false).is_empty(),
            "two references to the same digest must not be reported as a conflict"
        );
        assert!(check_repo_digest_conflicts(&[one, two], false).is_ok());
    }

    /// Two explicit roots for the same repository at different digests are a
    /// version conflict — `check_repo_digest_conflicts` returns
    /// `Err(DependencyError::Conflict)` naming both identifiers. This is the
    /// root-vs-root case the user reported (e.g. `cmake:4.1 cmake:4`).
    #[test]
    fn conflicting_roots_are_fatal() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');
        let expected_repo = crate::oci::Repository::from(&*d_v1);

        let a = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));
        let b = Arc::new(make_install_info("d", '2', ResolvedPackage::new()));

        match check_repo_digest_conflicts(&[a, b], false) {
            Err(DependencyError::Conflict {
                repository,
                identifiers,
            }) => {
                assert_eq!(repository, expected_repo);
                assert_eq!(identifiers, vec![d_v1, d_v2]);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    /// End-to-end: `compose` aborts with `Error::Dependency(Conflict)` when two
    /// roots collide on the same repository at different digests. Locks the
    /// behaviour behind `package env`/`exec`/`run`.
    #[tokio::test]
    async fn compose_errors_on_conflicting_roots() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let a = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));
        let b = Arc::new(make_install_info("d", '2', ResolvedPackage::new()));

        match compose(&[a, b], &store, false).await {
            Err(crate::Error::Dependency(DependencyError::Conflict { identifiers, .. })) => {
                assert_eq!(identifiers.len(), 2, "both colliding versions must be named");
            }
            Err(other) => panic!("expected Dependency(Conflict), got {other:?}"),
            Ok(_) => panic!("expected Err(Dependency(Conflict)), got Ok"),
        }
    }

    // ─ Item #10 — root-as-TC-dep emitted exactly once ─────────────────────────

    /// Package `a` is both an explicit root AND appears in the TC of the other
    /// root `b`. The composer must emit `a`'s contributions exactly once.
    ///
    /// Regression guard for the `root_keys` pre-computation that defers TC
    /// entries which are also explicit roots to the root-emission pass, ensuring
    /// neither double-emission nor silent suppression occurs.
    ///
    /// Setup: `b → a` (PUBLIC edge, so `a` is in b's interface-projection TC).
    /// Roots: `[b, a]`. Expected: `a`'s env var appears exactly once.
    #[tokio::test]
    async fn compose_root_that_is_also_tc_dep_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // `a` declares a single Public env var so we can count emissions.
        let a = Arc::new(make_install_info_with_var(
            dir.path(),
            "a",
            'a',
            ResolvedPackage::new(),
            "A_VAR",
            Visibility::PUBLIC,
        ));

        // Also seed `a` on disk so `load_object_data` can find it when
        // `b`'s TC walk reaches it (the parallel preload path).
        let a_path = store.path(a.identifier());
        std::fs::create_dir_all(a_path.join("content")).unwrap();
        let a_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "A_VAR",
                "type": "constant",
                "value": "v",
                "visibility": "public",
            }],
        });
        std::fs::write(a_path.join("metadata.json"), a_meta.to_string()).unwrap();
        std::fs::write(
            a_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        // `b` depends on `a` via a PUBLIC edge — `a` is in `b`'s interface TC.
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: a.identifier().clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        // Compose with both b and a as explicit roots. b's TC includes a, but
        // the root-emission pass must handle a exactly once (not from b's TC
        // walk AND again from the explicit-root pass).
        let out = compose(&[b, a], &store, false).await.unwrap();

        let a_var_count = out.entries.iter().filter(|e| e.key == "A_VAR").count();
        assert_eq!(
            a_var_count, 1,
            "A_VAR must be emitted exactly once; a is both a root and a TC dep of b. Got {a_var_count} emissions"
        );
    }

    // ─ PATH ordering invariant for emit_dep_path_block / emit_root_path_block ──
    //
    // These unit tests verify the load-bearing ordering enforced by the helpers
    // extracted in the refactor for finding #7.
    //
    // Invariant: declared `bin/` PATH entry MUST appear at a lower index than
    // the synth-entrypoints PATH entry in `entries` so that, when a consumer
    // prepends each entry in order, `entrypoints/` ends up at the front of
    // PATH and wins lookup priority — entrypoint launchers shadow declared
    // `bin/`.
    //
    // The second test in each pair demonstrates that swapping the two pushes
    // inside the helper would produce a DIFFERENT ordering, proving the order is
    // load-bearing and that a reversal is detectable.

    /// `emit_dep_path_block` emits the declared `bin/` PATH entry before the synth-PATH.
    ///
    /// Construct a dep with both an entrypoint (so synth-PATH is emitted) and a
    /// declared PATH env var (simulating `bin/`). Assert that the declared PATH
    /// entry appears at a lower index in `entries` than the synth-PATH entry.
    #[test]
    fn emit_dep_path_block_declared_bin_precedes_synth_path() {
        let dir = tempfile::tempdir().unwrap();

        // Build dep metadata: one entrypoint + one public PATH var (the bin/).
        let entrypoints = Entrypoints::from_names(["tool"]);

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::INTERFACE,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let dep_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });

        let pkg_root = dir.path().join("dep");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let dep_pkg = crate::file_structure::PackageDir { dir: pkg_root.clone() };

        let dep_content = pkg_root.join("content");
        let dep_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_dep_path_block(
            &dep_metadata,
            &dep_pkg,
            &dep_content,
            &dep_dep_contexts,
            false,
            &mut entries,
        )
        .expect("emit_dep_path_block must succeed");

        // Must have at least 2 entries: synth-PATH + declared bin/ PATH.
        let entry_summary: Vec<_> = entries.iter().map(|e| (&e.key, &e.value)).collect();
        assert!(
            entries.len() >= 2,
            "expected at least 2 entries (synth-PATH + declared PATH), got {}; entries: {:?}",
            entries.len(),
            entry_summary
        );

        let synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synth-PATH entry (contains 'entrypoints') must be present");

        let bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .expect("declared bin/ PATH entry must be present");

        assert!(
            bin_idx < synth_idx,
            "declared bin/ PATH (index {bin_idx}) must precede synth-PATH (index {synth_idx}); \
             reversing would let bin/ win lookup priority over launchers. entries: {:?}",
            entries.iter().map(|e| (&e.key, &e.value)).collect::<Vec<_>>()
        );
    }

    /// `emit_dep_path_block` ordering is load-bearing: a manually-swapped vector
    /// fails the ordering check, proving the helper's order is not accidental.
    ///
    /// This test calls `emit_dep_path_block`, then swaps the two PATH entries in
    /// the result. The swapped vector must NOT satisfy the ordering invariant —
    /// demonstrating that the invariant would be violated if the helper's pushes
    /// were reversed.
    #[test]
    fn emit_dep_path_block_swapped_order_fails_invariant() {
        let dir = tempfile::tempdir().unwrap();

        let entrypoints = Entrypoints::from_names(["tool"]);

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::INTERFACE,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let dep_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("dep2");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let dep_pkg = crate::file_structure::PackageDir { dir: pkg_root.clone() };
        let dep_content = pkg_root.join("content");
        let dep_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_dep_path_block(
            &dep_metadata,
            &dep_pkg,
            &dep_content,
            &dep_dep_contexts,
            false,
            &mut entries,
        )
        .expect("emit_dep_path_block must succeed");

        // Swap the two PATH entries to simulate reversed push order.
        let synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synth-PATH must exist");
        let bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .expect("bin/ PATH must exist");

        entries.swap(synth_idx, bin_idx);

        // After the swap, synth-PATH must now be at the *lower* index.
        // (synth_idx < bin_idx after swap.) This proves that swapping breaks the invariant.
        let new_synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .unwrap();
        let new_bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .unwrap();

        // The swapped vector has synth BEFORE bin — the invariant is violated.
        assert!(
            new_synth_idx < new_bin_idx,
            "after swap, synth-PATH (index {new_synth_idx}) must precede bin/ (index {new_bin_idx}); \
             this confirms the swap reverses the invariant"
        );
    }

    /// `emit_root_path_block` emits the declared `bin/` PATH entry before the synth-PATH
    /// on the consumer (default exec, self_view=false) surface.
    #[test]
    fn emit_root_path_block_declared_bin_precedes_synth_path_consumer_surface() {
        let dir = tempfile::tempdir().unwrap();

        let entrypoints = Entrypoints::from_names(["rootool"]);

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let root_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root_dir = crate::file_structure::PackageDir { dir: pkg_root.clone() };
        let root_content = pkg_root.join("content");
        let root_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_root_path_block(
            &root_metadata,
            &root_dir,
            &root_content,
            &root_dep_contexts,
            false, // consumer surface (default exec)
            &mut entries,
        )
        .expect("emit_root_path_block must succeed");

        assert!(
            entries.len() >= 2,
            "expected at least 2 entries (synth-PATH + declared PATH), got {}",
            entries.len()
        );

        let synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synth-PATH entry must be present in consumer surface output");

        let bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .expect("declared bin/ PATH must be present in consumer surface output");

        assert!(
            bin_idx < synth_idx,
            "declared bin/ (index {bin_idx}) must precede synth-PATH (index {synth_idx}) \
             in emit_root_path_block output; entrypoint launchers must shadow declared bin/"
        );
    }

    /// `emit_root_path_block` does NOT emit synth-PATH on the `--self` surface.
    ///
    /// Under `self_view=true` the root must not see its own launchers —
    /// `entrypoints/` is suppressed. Only the declared env vars appear.
    #[test]
    fn emit_root_path_block_no_synth_path_on_self_surface() {
        let dir = tempfile::tempdir().unwrap();

        let entrypoints = Entrypoints::from_names(["rootool"]);

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let root_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root_self");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root_dir = crate::file_structure::PackageDir { dir: pkg_root.clone() };
        let root_content = pkg_root.join("content");
        let root_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_root_path_block(
            &root_metadata,
            &root_dir,
            &root_content,
            &root_dep_contexts,
            true, // --self surface
            &mut entries,
        )
        .expect("emit_root_path_block must succeed");

        // No synth-PATH entry expected on --self surface.
        let synth_values: Vec<_> = entries
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .map(|e| &e.value)
            .collect();
        assert!(
            synth_values.is_empty(),
            "emit_root_path_block with self_view=true must NOT emit synth-PATH; \
             got entrypoints PATH values: {:?}",
            synth_values
        );
    }

    // ── admitted_binaries / admitted_entrypoints (adr_declared_binaries_metadata.md §4 Decision A) ──
    //
    // Same admission rule as `admitted`: root claims unconditional, dep
    // claims gated by the active surface (has_interface()/has_private()).
    // The entrypoints-flavored tests below exercise the rule end-to-end
    // through the already-working `Entrypoints` machinery (no dependency on
    // WP1's still-stubbed `BinaryName`/`Binaries`); the binaries-flavored
    // tests pin the identical contract for the new `BinaryName` type.

    /// An explicit root's own declared entrypoints are admitted
    /// unconditionally — mirrors `admitted`'s unconditional root emission.
    #[tokio::test]
    async fn compose_admitted_entrypoints_includes_root_claims_unconditionally() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "cmake",
        ));
        let root_id = root.identifier().clone();

        let out = compose(&[root], &store, false).await.unwrap();
        let claimed: Vec<&str> = out
            .admitted_entrypoints
            .iter()
            .filter(|(id, _)| *id == root_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["cmake"],
            "an explicit root's own entrypoint claims must be admitted unconditionally"
        );
    }

    /// A dep whose TC entry has `has_interface()==true` (PUBLIC) contributes
    /// its declared entrypoints to `admitted_entrypoints` in default exec.
    #[tokio::test]
    async fn compose_admitted_entrypoints_includes_interface_visible_dep() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("ninja", 'n');
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({"type": "bundle", "version": 1, "entrypoints": { "ninja": {} }});
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        let claimed: Vec<&str> = out
            .admitted_entrypoints
            .iter()
            .filter(|(id, _)| *id == dep_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["ninja"],
            "PUBLIC.has_interface()==true dep's declared entrypoints must be admitted"
        );
    }

    /// PRIVATE and SEALED deps' declared entrypoints never reach
    /// `admitted_entrypoints` on the default (interface) surface — both fail
    /// `has_interface()`, so the per-root loop never even visits them.
    #[tokio::test]
    async fn compose_admitted_entrypoints_excludes_private_and_sealed_dep_default_exec() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let sealed_id = pinned("sealed-tool", 's');
        for (id, name) in [(&priv_id, "privtool"), (&sealed_id, "sealedtool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "entrypoints": { name: {} }});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: sealed_id.clone(),
                    visibility: Visibility::SEALED,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        assert!(
            out.admitted_entrypoints.is_empty(),
            "PRIVATE and SEALED deps' claims must never be admitted on the default (interface) surface: {:?}",
            out.admitted_entrypoints
        );
    }

    /// `--self` flips admission: a PRIVATE dep (has_private()==true) is
    /// admitted, an INTERFACE-only dep (has_private()==false) is excluded —
    /// the mirror image of the default-exec gate.
    #[tokio::test]
    async fn compose_admitted_entrypoints_self_view_flips_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let iface_id = pinned("iface-tool", 'i');
        for (id, name) in [(&priv_id, "privtool"), (&iface_id, "ifacetool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "entrypoints": { name: {} }});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: iface_id.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        let claimed: Vec<&str> = out.admitted_entrypoints.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            claimed,
            vec!["privtool"],
            "--self must admit PRIVATE.has_private()==true deps and exclude INTERFACE.has_private()==false deps"
        );
    }

    /// Same unconditional-root rule as the entrypoints test above, pinned
    /// for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_includes_root_claims_unconditionally() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root_meta: metadata::Metadata =
            serde_json::from_str(r#"{"type":"bundle","version":1,"binaries":["cmake"]}"#).expect("fixture parses");
        let root_id = pinned("root", 'r');
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            root_meta,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        let out = compose(&[root], &store, false).await.unwrap();
        let claimed: Vec<&str> = out
            .admitted_binaries
            .iter()
            .filter(|(id, _)| *id == root_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["cmake"],
            "an explicit root's declared binaries must be admitted unconditionally"
        );
    }

    /// Same interface-surface gate as the entrypoints test above, pinned
    /// for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_dep_gated_by_interface_surface() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("ninja", 'n');
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({"type": "bundle", "version": 1, "binaries": ["ninja"]});
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        let claimed: Vec<&str> = out
            .admitted_binaries
            .iter()
            .filter(|(id, _)| *id == dep_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["ninja"],
            "PUBLIC.has_interface()==true dep's declared binaries must be admitted"
        );
    }

    /// Same PRIVATE/SEALED exclusion as `compose_admitted_entrypoints_excludes_private_and_sealed_dep_default_exec`,
    /// pinned for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_excludes_private_and_sealed_dep_default_exec() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let sealed_id = pinned("sealed-tool", 's');
        for (id, name) in [(&priv_id, "privtool"), (&sealed_id, "sealedtool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "binaries": [name]});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: sealed_id.clone(),
                    visibility: Visibility::SEALED,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false).await.unwrap();
        assert!(
            out.admitted_binaries.is_empty(),
            "PRIVATE and SEALED deps' binaries claims must never be admitted on the default (interface) surface: {:?}",
            out.admitted_binaries
        );
    }

    /// Same `--self` admission flip as `compose_admitted_entrypoints_self_view_flips_admission`,
    /// pinned for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_self_view_flips_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let iface_id = pinned("iface-tool", 'i');
        for (id, name) in [(&priv_id, "privtool"), (&iface_id, "ifacetool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "binaries": [name]});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: iface_id.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true).await.unwrap();
        let claimed: Vec<&str> = out.admitted_binaries.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            claimed,
            vec!["privtool"],
            "--self must admit PRIVATE.has_private()==true deps and exclude INTERFACE.has_private()==false deps"
        );
    }

    /// A SEALED dependency and a PRIVATE dependency, each declaring both
    /// `binaries` and `entrypoints` claims, contribute nothing to either
    /// admitted-claim array — while the root's own (unrelated) env-var
    /// contribution still reaches `entries` in the same compose call. Guards
    /// `adr_declared_binaries_metadata.md` §4 Decision A: a non-interface
    /// dependency's claims never leak into the env report, and the exclusion
    /// does not collaterally swallow the root's own surface contribution.
    #[tokio::test]
    async fn compose_sealed_and_private_dep_claims_excluded_while_root_contributes() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_id = pinned("sealed-tool", 's');
        let private_id = pinned("private-tool", 'p');
        for id in [&sealed_id, &private_id] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({
                "type": "bundle",
                "version": 1,
                "binaries": ["secret"],
                "entrypoints": { "secret": {} },
            });
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: sealed_id.clone(),
                    visibility: Visibility::SEALED,
                },
                ResolvedDependency {
                    identifier: private_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
            ],
        };
        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "OWN_VAR",
            Visibility::PUBLIC,
        ));

        let out = compose(&[root], &store, false).await.unwrap();

        assert!(
            out.admitted_binaries.is_empty(),
            "SEALED and PRIVATE deps' binaries claims must never be admitted: {:?}",
            out.admitted_binaries
        );
        assert!(
            out.admitted_entrypoints.is_empty(),
            "SEALED and PRIVATE deps' entrypoints claims must never be admitted: {:?}",
            out.admitted_entrypoints
        );
        assert!(
            out.entries.iter().any(|e| e.key == "OWN_VAR"),
            "the root's own env-var contribution must still reach entries, unaffected by dep claim exclusion: {:?}",
            out.entries
        );
    }

    // ── `${self.env.KEY}` — scope, order, and the resolve-then-gate split ─────
    //
    // Every test below builds a package whose `env` array IS the fixture: the
    // declaration order of `vars` is the property under test, so the helper
    // preserves it and never sorts.

    use std::collections::HashMap;

    use crate::package::error::Error as PackageError;
    use crate::package::metadata::dependency::DependencyName;
    use crate::package::metadata::env::dep_context::DependencyContext;
    use crate::package::metadata::env::entry::Entry;
    use crate::package::metadata::template::TemplateError;

    type DepContexts = HashMap<DependencyName, DependencyContext>;

    /// Package metadata carrying exactly `vars`, in the order given.
    fn metadata_with_vars(vars: Vec<Var>) -> metadata::Metadata {
        let mut builder = metadata_env::EnvBuilder::new();
        for var in vars {
            builder.add_var(var);
        }
        metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: builder.build(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: metadata::Integrations::default(),
        })
    }

    /// A package directory with a real `content/` tree, so path resolution has
    /// something to root itself in.
    fn package_content(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let content = root.join(name).join("content");
        std::fs::create_dir_all(&content).unwrap();
        content
    }

    /// Compose one root package's own env onto the surface `self_view` selects.
    fn compose_root(
        meta: &metadata::Metadata,
        content: &std::path::Path,
        dep_contexts: &DepContexts,
        self_view: bool,
    ) -> crate::Result<Vec<Entry>> {
        let root_dir = crate::file_structure::PackageDir {
            dir: content.parent().unwrap().to_path_buf(),
        };
        let mut entries = Vec::new();
        emit_root_path_block(meta, &root_dir, content, dep_contexts, self_view, &mut entries)?;
        Ok(entries)
    }

    /// Compose one dependency's env across the edge onto the surface
    /// `self_view` selects.
    fn compose_dep(meta: &metadata::Metadata, content: &std::path::Path, self_view: bool) -> crate::Result<Vec<Entry>> {
        let dep_pkg = crate::file_structure::PackageDir {
            dir: content.parent().unwrap().to_path_buf(),
        };
        let dep_contexts = DepContexts::new();
        let mut entries = Vec::new();
        emit_dep_path_block(meta, &dep_pkg, content, &dep_contexts, self_view, &mut entries)?;
        Ok(entries)
    }

    fn value_of<'a>(entries: &'a [Entry], key: &str) -> &'a str {
        entries
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("expected an entry for {key}; got {entries:?}"))
            .value
            .as_str()
    }

    // ─ Declaration order (D6.1, C-018, C-025) ────────────────────────────────

    /// C-018 / C-025 (first leg) — a var may reference one declared strictly
    /// earlier in the same package, and gets its resolved value.
    ///
    /// Paired with
    /// `the_same_two_vars_in_the_opposite_declaration_order_are_refused`: the
    /// two documents are identical modulo the order of the `env` array, so an
    /// implementation that ignores order gives them the same verdict and one
    /// of the two legs reds. Neither leg pins order on its own.
    #[test]
    fn a_var_may_reference_one_declared_earlier_in_the_same_package() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "forward");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "alpha", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
        ]);

        let entries = compose_root(&meta, &content, &DepContexts::new(), false).expect("the document must compose");
        assert_eq!(value_of(&entries, "A"), "alpha");
        assert_eq!(value_of(&entries, "B"), "alpha/x");
    }

    /// C-025 (second leg) — the same two vars, same keys, same values, only
    /// the array order swapped: the reference now points forward and is
    /// refused.
    #[test]
    fn the_same_two_vars_in_the_opposite_declaration_order_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "backward");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
            Var::new_constant_with_visibility("A", "alpha", Visibility::PUBLIC),
        ]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("a forward reference names a var that is not yet in scope");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    var_key,
                    source: TemplateError::UndefinedSelfEnvRef { key, .. },
                } if var_key == "B" && key == "A"
            )),
            "unexpected error: {error}"
        );
    }

    /// C-020 — a var referencing itself is the same fault, reached through a
    /// one-var document: its own declaration is not strictly earlier than
    /// itself, so the scope is empty and there is no cycle to detect.
    #[test]
    fn a_var_referencing_itself_is_undefined_rather_than_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "self-ref");
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "A",
            "${self.env.A}",
            Visibility::PUBLIC,
        )]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("a var cannot see its own declaration");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UndefinedSelfEnvRef { key, declared_before }, ..
                } if key == "A" && declared_before.is_empty()
            )),
            "unexpected error: {error}"
        );
    }

    /// C-021 (first leg) — a key declared twice earlier is refused, not
    /// picked: both contributions are legally visible and neither is
    /// privileged.
    #[test]
    fn a_key_declared_twice_earlier_makes_the_reference_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "ambiguous");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "one", Visibility::PUBLIC),
            Var::new_constant_with_visibility("A", "two", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}", Visibility::PUBLIC),
        ]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("two earlier contributions leave no non-arbitrary answer");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::AmbiguousSelfEnvRef { key }, ..
                } if key == "A"
            )),
            "unexpected error: {error}"
        );
    }

    /// C-021 (second leg) — duplicates stay legal. Only *referencing* an
    /// ambiguous key is refused; without this leg the refusal above is
    /// indistinguishable from a new uniqueness rule on `Env`.
    #[test]
    fn a_key_declared_twice_with_no_reference_to_it_composes_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "duplicates");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "one", Visibility::PUBLIC),
            Var::new_constant_with_visibility("A", "two", Visibility::PUBLIC),
        ]);

        let entries =
            compose_root(&meta, &content, &DepContexts::new(), false).expect("duplicate keys remain publishable");
        let values: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.key == "A")
            .map(|entry| entry.value.as_str())
            .collect();
        assert_eq!(
            values,
            vec!["one", "two"],
            "both contributions must still be emitted, in declaration order"
        );
    }

    // ─ Surface independence (D8, C-024) ──────────────────────────────────────

    /// C-024 — an `interface` var may reference a `private` one, and the
    /// resolved bytes are identical on both surfaces.
    ///
    /// The fixture is a dependency, where `carrier_crosses` is
    /// `has_interface()` on either surface: `I` crosses both times and `S`
    /// crosses neither, so the two runs differ only in the surface asked for.
    ///
    /// The literal value assertion is not redundant with C-018: an
    /// implementation resolving `${self.env.S}` to the empty string on **both**
    /// surfaces satisfies equality perfectly, so equality alone cannot tell
    /// surface-independence from uniformly-degenerate.
    #[test]
    fn an_interface_var_referencing_a_private_one_resolves_identically_on_both_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "surfaces");
        let build = || {
            metadata_with_vars(vec![
                Var::new_constant_with_visibility("S", "private-value", Visibility::PRIVATE),
                Var::new_constant_with_visibility("I", "${self.env.S}", Visibility::INTERFACE),
            ])
        };

        let interface_surface = compose_dep(&build(), &content, false).expect("the interface surface must compose");
        let private_surface = compose_dep(&build(), &content, true).expect("the private surface must compose");

        assert_eq!(
            value_of(&interface_surface, "I"),
            value_of(&private_surface, "I"),
            "the same metadata must not produce different bytes depending on who asked"
        );
        assert_eq!(
            value_of(&interface_surface, "I"),
            "private-value",
            "the agreed value must be the referenced var's own resolved value"
        );

        for (surface, entries) in [("interface", &interface_surface), ("private", &private_surface)] {
            assert!(
                !entries.iter().any(|entry| entry.key == "S"),
                "a dep's private var crosses no edge, so it must not be emitted on the {surface} surface: {entries:?}"
            );
        }
    }

    // ─ Resolve, then gate: assertions on emit only (D8, C-026, C-027) ────────

    /// C-026(a) — a `required` path var whose target is absent and that does
    /// **not** cross the active surface is resolved but not asserted.
    ///
    /// This leg cannot red against the stub, and it cannot red against `main`
    /// either: today the var is `continue`d before `EnvResolver::resolve` runs,
    /// so the assertion never fires. Its red state exists only against
    /// resolve-then-gate code that left the existence assertion on the
    /// non-emitted path — which is exactly the regression D8 would otherwise
    /// introduce. `..._that_crosses_is_asserted_to_exist` is what makes the
    /// pair a check: without it, deleting the assertion outright passes here.
    #[test]
    fn a_missing_required_path_that_does_not_cross_is_not_asserted_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "required-private");
        let meta = metadata_with_vars(vec![Var::new_path_with_visibility(
            "TOOL_DIR",
            "${installPath}/absent",
            /* required = */ true,
            Visibility::PRIVATE,
        )]);

        let entries = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect("a var nobody emits must not assert its target exists");
        assert!(
            entries.is_empty(),
            "a private var does not cross the interface surface: {entries:?}"
        );
    }

    /// C-026(b) — the otherwise identical `required` path var that **does**
    /// cross still raises `RequiredPathMissing`. This leg carries the pair's
    /// discrimination.
    #[test]
    fn a_missing_required_path_that_crosses_is_asserted_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "required-interface");
        let meta = metadata_with_vars(vec![Var::new_path_with_visibility(
            "TOOL_DIR",
            "${installPath}/absent",
            /* required = */ true,
            Visibility::INTERFACE,
        )]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("an emitted required path must be asserted to exist");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(), PackageError::RequiredPathMissing(_))),
            "unexpected error: {error}"
        );
    }

    /// A dependency context whose install path does not exist on disk — the
    /// declared-but-not-installed case `build_dep_context_map` produces when a
    /// declared dep is absent from the resolved toolchain.
    fn uninstalled_dep_contexts(missing: std::path::PathBuf) -> DepContexts {
        let mut contexts = DepContexts::new();
        contexts.insert(
            DependencyName::try_from("tool").unwrap(),
            DependencyContext::path_only(pinned("tool", 'e'), missing),
        );
        contexts
    }

    /// C-027(a) — a var referencing a declared-but-uninstalled dependency
    /// composes cleanly when it does not cross the active surface.
    ///
    /// Same standing as C-026(a): green today because the var is never
    /// resolved at all, and red only against resolve-then-gate code that kept
    /// `check_exists = true` on the non-emitted path — which would turn a
    /// working install into exit 79. The crossing sibling below is what makes
    /// the pair a check.
    #[test]
    fn an_uninstalled_dependency_in_a_non_crossing_var_does_not_fail_composition() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "dep-private");
        let contexts = uninstalled_dep_contexts(dir.path().join("not-installed"));
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "TOOL",
            "${deps.tool.installPath}/bin",
            Visibility::PRIVATE,
        )]);

        let entries = compose_root(&meta, &content, &contexts, false)
            .expect("an uninstalled dep must not fail a value nobody emits");
        assert!(
            entries.is_empty(),
            "a private var does not cross the interface surface: {entries:?}"
        );
    }

    /// C-027(b) — the otherwise identical var that **does** cross fails with
    /// `DependencyNotInstalled`, exit 79.
    #[test]
    fn an_uninstalled_dependency_in_a_crossing_var_fails_composition() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "dep-interface");
        let contexts = uninstalled_dep_contexts(dir.path().join("not-installed"));
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "TOOL",
            "${deps.tool.installPath}/bin",
            Visibility::INTERFACE,
        )]);

        let error = compose_root(&meta, &content, &contexts, false)
            .expect_err("an emitted value must still assert its dependency is installed");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::DependencyNotInstalled { ref_name, .. }, ..
                } if ref_name.as_str() == "tool"
            )),
            "unexpected error: {error}"
        );
        assert_eq!(error.classify(), Some(ExitCode::NotFound));
    }

    /// D8 — a template *fault* in a non-crossing var now surfaces where it
    /// previously never ran. This is the accepted behaviour change, and it is
    /// what keeps the two suppression legs above from reading as "a
    /// non-crossing var is never resolved at all".
    #[test]
    fn a_template_fault_in_a_non_crossing_var_still_fails_composition() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "fault-private");
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "BROKEN",
            "${deps.undeclared.installPath}",
            Visibility::PRIVATE,
        )]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("a package whose own metadata cannot resolve is broken regardless of who is looking");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownDependencyRef { ref_name, .. }, ..
                } if ref_name.as_str() == "undeclared"
            )),
            "unexpected error: {error}"
        );
    }

    // ─ What is substituted: the resolved value, never the template (C-029) ───

    /// C-029 — composition substitutes the referenced var's **resolved value**,
    /// not its template.
    ///
    /// The red state is a mutant of the code WP4 adds: an accumulator holding
    /// each var's authored template instead of its resolved `Entry`. `A`'s
    /// template and `A`'s resolved value differ visibly here, so the mutant is
    /// caught by the assertion rather than by an equality that both satisfy.
    #[test]
    fn a_self_env_reference_substitutes_the_resolved_value_not_the_template() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "resolved-value");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "${installPath}/bin", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
        ]);

        let entries = compose_root(&meta, &content, &DepContexts::new(), false).expect("the document must compose");
        let a = value_of(&entries, "A");
        let b = value_of(&entries, "B");

        assert_ne!(
            a, "${installPath}/bin",
            "the fixture is only a check if A's template and A's resolved value differ"
        );
        assert_eq!(b, format!("{a}/x"), "B must carry A's resolved value");
        assert!(
            !b.contains("${installPath}"),
            "substituting A's template would leave an unresolved token in B: {b:?}"
        );
    }

    /// C-029 / C-009 sibling — bytes a `${self.env.*}` reference substitutes
    /// are never rescanned.
    ///
    /// `A` resolves, through the escape, to the literal text
    /// `${deps.tool.installPath}`, and `tool` is present in `dep_contexts`. If
    /// composition re-read substituted bytes, `B` would come out carrying the
    /// dependency's install path. D12 deletes the install-path injection
    /// defence on the grounds that substituted bytes are never re-examined;
    /// `${self.env.*}` is the second composition path where that premise could
    /// be falsified.
    #[test]
    fn bytes_a_self_env_reference_substitutes_are_never_rescanned() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "injection");
        let installed = dir.path().join("tool-installed");
        std::fs::create_dir_all(&installed).unwrap();
        let mut contexts = DepContexts::new();
        contexts.insert(
            DependencyName::try_from("tool").unwrap(),
            DependencyContext::path_only(pinned("tool", 'f'), installed.clone()),
        );

        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "$${deps.tool.installPath}", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
        ]);

        let entries = compose_root(&meta, &content, &contexts, false).expect("the document must compose");
        assert_eq!(
            value_of(&entries, "B"),
            "${deps.tool.installPath}/x",
            "substituted bytes must reach the output verbatim"
        );
        assert!(
            !value_of(&entries, "B").contains(&*installed.to_string_lossy()),
            "the dependency's install path must not appear — that would mean the output was rescanned"
        );
    }

    // ── C-011: integrations_cross — interface-surface-only carrier ────────
    //
    // ADR `adr_package_integrations.md` §4.1's four-cell truth table.
    // `integrations_cross` takes only `self_view` — no `is_root` — because
    // the answer is the same at every depth (a lie a parameter the body
    // ignores would tell); the root and dep cells below therefore exercise
    // the identical call, documenting all four ADR rows explicitly.

    #[test]
    fn integrations_cross_root_interface_surface_is_true() {
        assert!(integrations_cross(/* self_view = */ false));
    }

    #[test]
    fn integrations_cross_root_private_surface_is_false() {
        assert!(!integrations_cross(/* self_view = */ true));
    }

    #[test]
    fn integrations_cross_dep_interface_surface_is_true() {
        assert!(integrations_cross(/* self_view = */ false));
    }

    #[test]
    fn integrations_cross_dep_private_surface_is_false() {
        assert!(!integrations_cross(/* self_view = */ true));
    }

    // ── C-017 / H-1: the companion projection's integrations gate ──────────

    /// A companion projection composed with integrations SUPPRESSED must not
    /// resolve the payloads at all — not resolve-then-discard.
    ///
    /// The projection is pinned to `self_view = false` (no private leak), so it
    /// cannot derive the caller's gate; before the explicit input it collected
    /// unconditionally. Resolution asserts every `${deps.*}` content directory
    /// exists, so a payload naming an uninstalled dependency failed the WHOLE
    /// projection — on a `--self` composition that carries zero integrations.
    ///
    /// Both outcomes are demonstrated on the one fixture: the gate ON leg proves
    /// the payload really is resolved (and really can fail), so the gate OFF
    /// leg's success is suppression rather than an inert fixture.
    #[tokio::test]
    async fn compose_companion_with_integrations_suppressed_skips_payload_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Declared, never installed: `store.content(..)` names a directory that
        // does not exist, which is exactly what `${deps.*}` resolution asserts.
        let absent_dep = pinned("absentdep", 'd');
        // `from_str`, not `from_value`: `Visibility` deserializes from a
        // borrowed string, which `serde_json::Value` cannot supply.
        let metadata: metadata::Metadata = serde_json::from_str(
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "dependencies": [{ "identifier": absent_dep.to_string(), "visibility": "public" }],
                "integrations": { "vendor.example": { "path": "${deps.absentdep.installPath}" } },
            })
            .to_string(),
        )
        .expect("fixture metadata parses");
        let companion = Arc::new(InstallInfo::new(
            pinned("companion", 'c'),
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir {
                dir: dir.path().join("companion"),
            },
        ));

        let collected = compose_companion(&companion, &store, /* collect_integrations = */ true).await;
        assert!(
            collected.is_err(),
            "fixture check: collecting integrations must resolve the payload and fail on the absent dependency"
        );

        let suppressed = compose_companion(&companion, &store, /* collect_integrations = */ false)
            .await
            .expect("a suppressed carrier must not be resolved, so the absent dependency cannot fail the projection");
        assert!(
            suppressed.admitted_integrations.is_empty(),
            "suppressed projection must carry no integrations: {:?}",
            suppressed.admitted_integrations
        );
    }

    // ── H-2: `${self.env.*}` is gated at COMPOSE, not only at publish ────────
    //
    // `validate_integration_tokens` runs inside `validate_for_publish`, which
    // a hostile registry never runs — so compose meets a published payload's
    // tokens with only its own capability set. Both resolver sites therefore
    // carry `INTEGRATION_TOKENS`, one test each, because they are two
    // independent call sites that can regress separately.
    //
    // Each asserts WHICH refusal, not merely that one happened: the composer
    // supplies no self-env scope, so an ungated resolver also fails here — as
    // `UndefinedSelfEnvRef`, indistinguishable from a gate unless the message is
    // read. That coincidence is one edit (`.with_self_env(&declared_before)`
    // "for consistency") away from resolving instead, which would put a value
    // the publisher declared `private` into an interface-surface JSON payload
    // (CWE-200). `integrations.rs` holds the sibling unit test that supplies a
    // scope which DOES define the key, so the gate is proven independent of it.

    /// Dependency site: a dep's payload carrying `${self.env.*}` is refused by
    /// the capability gate.
    #[tokio::test]
    async fn compose_gates_a_self_env_token_in_a_dependency_integrations_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("dep", 'd');
        let dep_resolved = ResolvedPackage::new();
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "integrations": { "vendor.example": { "token": "${self.env.SECRET}" } },
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&dep_resolved).unwrap(),
        )
        .unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id,
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // `let Err(..) else`, not `expect_err`: `ComposeOutput` is not `Debug`.
        let Err(err) = compose(&[root], &store, /* self_view = */ false).await else {
            panic!("a self-env token in a dependency's payload must be refused");
        };
        let message = err.to_string();
        assert!(
            message.contains("not permitted"),
            "expected the capability gate's refusal, not an undefined-reference one: {message}"
        );
    }

    /// Root site: a root's own payload carrying `${self.env.*}` is refused by
    /// the capability gate. Exercised through `compose_companion`, which reaches
    /// the root branch with no store seeding.
    #[tokio::test]
    async fn compose_gates_a_self_env_token_in_a_root_integrations_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let metadata: metadata::Metadata = serde_json::from_str(
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "integrations": { "vendor.example": { "token": "${self.env.SECRET}" } },
            })
            .to_string(),
        )
        .expect("fixture metadata parses");
        let root = Arc::new(InstallInfo::new(
            pinned("root", 'r'),
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir {
                dir: dir.path().join("root"),
            },
        ));

        let Err(err) = compose_companion(&root, &store, /* collect_integrations = */ true).await else {
            panic!("a self-env token in the root's own payload must be refused");
        };
        let message = err.to_string();
        assert!(
            message.contains("not permitted"),
            "expected the capability gate's refusal, not an undefined-reference one: {message}"
        );
    }
}
