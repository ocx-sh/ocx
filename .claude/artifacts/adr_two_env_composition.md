# ADR: Two-Env Composition Model for Package Visibility

> **Current implementation state** — for the live module map, facade pattern, error model, and task surface, see [`subsystem-package-manager.md`](../rules/subsystem-package-manager.md), [`subsystem-package.md`](../rules/subsystem-package.md), and [`subsystem-cli.md`](../rules/subsystem-cli.md). This ADR records the design rationale; read it for *why*, not *what is true today*.

## Metadata

**Status:** Accepted
**Date:** 2026-04-30
**Deciders:** mherwig, architect
**Beads Issue:** N/A
**Related PRD:** [`prd_package_entry_points.md`](./archive/prd_package_entry_points.md)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in [`product-tech-strategy.md`](../rules/product-tech-strategy.md)
**Domain Tags:** data | api
**Supersedes:** [`adr_visibility_two_axis_and_exec_modes.md`](./archive/adr_visibility_two_axis_and_exec_modes.md) — Algorithm v2 at lines 482-501 conflated three independent concerns into a single `intersects(view)` mask and produced a semantically wrong answer for the matrix walk-through (Interface entry on a private-edge dep is dropped under `--self`, even though R is a consumer of A and should see A's interface contribution as part of R's private runtime).
**Superseded By:** N/A

**Reversibility Classification:** **One-Way Door Medium-to-High.** Deletes a 1700-line load-bearing module (`visible.rs`); replaces three call-site shapes; rewrites every doc surface that uses the "view filter / intersects mask / exec mode" vocabulary. The on-disk wire format for `Visibility` and `EntryVisibility` stays byte-identical, so installed packages and shipped launcher scripts (which embed `--self`) are not invalidated. The on-disk `resolve.json` shape is **unchanged** from today (the same `{ dependencies: Vec<ResolvedDependency> }` flat TC). Only the algebra used to BUILD the TC is renamed (`Visibility::propagate` → `through_edge`) and the predicate consumed at exec changes from `intersects` to `has_interface()` / `has_private()`. Existing on-disk `resolve.json` files continue to parse against the post-refactor struct.

**Cross-model review recommendation:** Per [`workflow-swarm.md`](../rules/workflow-swarm.md), Medium-to-High reversibility ADRs auto-trigger Codex cross-model plan review. Recommended `--codex` overlay on the `/swarm-execute high` run that lands this ADR's plan.

## Context

The pre-refactor model encoded visibility as a 4-state value (`sealed | private | public | interface`) on every dependency edge **and** on every env entry. A single predicate `Visibility::intersects(view: Visibility) -> bool` was reused across three independent concerns:

1. **Edge filter** — should this dep contribute at all? (`visible.rs:143`, `tasks/resolve.rs:250`)
2. **Synth-PATH gate** — should the entrypoint synth-PATH be emitted for this package? (`visible.rs:390`)
3. **Entry filter** — within the env vars of a contributing package, which ones land in the result? (`metadata/env/resolver.rs:104`)

`view` was set from a CLI flag: `Visibility::INTERFACE` for default exec, `Visibility::PRIVATE` for `--self`. Re-using the same mask for three different decisions is the bug.

### The matrix walk-through that broke the model

Setup: package R depends on package A through a `private` edge. A declares one env var `FOO` with `EntryVisibility::Interface`. User runs `ocx exec R --self` (`self_view = true`), expecting R's full self runtime including everything A contributes.

Pre-refactor algorithm (Algorithm v2):

```
view = Visibility::PRIVATE  // because --self
edge filter:    A's edge.visibility = private; private.intersects(PRIVATE) = true → keep A
entry filter:   FOO.visibility = interface;    interface.intersects(PRIVATE) = false → drop FOO
```

A is kept; FOO is dropped. But semantically R is a consumer of A. R running as itself should see *every*thing A publishes to its consumers, *plus* its own private surface. Algorithm v2 conflates "R running as itself" with "R hiding A's consumer-surface from itself" — those are not the same statement.

The intersects-mask approach cannot distinguish them because it has only one knob (`view`) and three independent filters consuming that knob. No tightening of `view` semantics fixes this; the algebra is structurally wrong for the workload.

### Why a refactor, not a patch

A point-fix to Algorithm v2 (e.g., per-package mask override) preserves the conflation and adds another knob. The right model already lives in CMake (1985) and Bazel (`exports`/`runtime_deps`/`deps`): each target owns separately *what consumers see* and *what the target uses internally*. OCX's two-env composition is the runtime-env analog of CMake's compile-time INTERFACE/PRIVATE/PUBLIC properties. Research artifact [`research_env_composition_models.md`](./archive/research_env_composition_models.md) confirms the model across five package managers.

## Decision Drivers

- **Correctness on the matrix walk-through** — A's Interface entry must reach R under `--self` when the R→A edge is `private`, because R is A's consumer.
- **Eliminate predicate-overloading** — three independent decisions deserve three explicit data structures, not one boolean predicate fan-out.
- **CMake parity** — CMake INTERFACE/PRIVATE/PUBLIC is the only proven full two-axis model in wide use; matching its mental model lowers the doc burden.
- **Code-deletion impact** — pre-refactor `visible.rs` is 1700 lines threaded through three task layers. Two-env composition collapses to a single recursive composer.
- **Wire-format stability** — `Visibility` and `EntryVisibility` JSON shapes are already shipped; their custom Serialize/Deserialize/JsonSchema impls must stay byte-identical.
- **Doc burden** — every reference to "view", "intersects", "mask", "mode filter" across 12+ doc surfaces becomes inaccurate; a clean replacement vocabulary is needed.

## Industry Context & Research

**Research artifact:** [`research_env_composition_models.md`](./archive/research_env_composition_models.md) — five-tool comparison (CMake, Nix, Guix, Spack, Bazel) confirms the model.

**Key insight:** CMake's `target_include_directories(T <PRIVATE|PUBLIC|INTERFACE> ...)` puts the keyword on *what the target declares* (entry axis); `target_link_libraries(T <PRIVATE|PUBLIC|INTERFACE> dep)` puts the keyword on *the link edge* (edge axis). Two orthogonal axes; one shared vocabulary. OCX's two-env composition is the direct runtime-env translation. The intersects-mask approach was an attempt to compress both axes into one predicate; CMake's lesson is that the axes must stay separate.

## Considered Options

### Option A — Per-package role override on top of the intersects mask

**Description:** Keep Algorithm v2's mask-based filter; add a per-package `consumer_role: bool` carried through dep traversal that flips `view` for consumer-surface entries on private-edge deps.

**Concrete counter-example (why this is "partial" not "full"):** Take the matrix walk-through (R --private→ A; A.env=[FOO=interface]). With `consumer_role=true` on A under R's private-edge traversal, the entry filter at `metadata/env/resolver.rs:104` evaluates `interface.intersects(PRIVATE) = false` and still drops FOO. The role-override fixes the *edge* filter (A is kept) but the *entry* filter is a separate `intersects` site sharing the same `view` parameter — fixing one site without the other still loses FOO. To fix this, the `consumer_role` would also need to flip `view` to `INTERFACE` *just for the entry filter on A*, which means the predicate is no longer "filter by view" but "filter by view, except when role and edge combination is private/consumer in which case use INTERFACE". The conflation is preserved in another shape.

| Pros | Cons |
|---|---|
| Smaller diff (no module deletion) | Adds a fourth knob to the predicate-overloading problem |
| Wire format unaffected | Two `intersects` sites must coordinate via the role flag — author cannot reason locally |
| Wire format for `resolve.json` unchanged | Doc burden barely shrinks — "view + role + edge + entry" is harder to explain |
| | Does not match CMake's mental model |

### Option B — Two-env composition refactor (RECOMMENDED)

**Description:** Each package owns two env surfaces, `interface_env` and `private_env`. Entry visibility partitions the package's own contributions between surfaces. Edge visibility decides which consumer surface receives the dep's `interface_env`. Dep's `private_env` never crosses any edge. `--self` selects which surface emit_env reads. No mask, no `intersects`, no `view`.

| Pros | Cons |
|---|---|
| Correct on the matrix walk-through (verified below) | 1700-line module deletion |
| Single predicate per concern: partition for own contributions, edge-routing for deps, surface-pick for exec | Doc rewrite across 12+ surfaces |
| Matches CMake mental model 1:1 | Acceptance test rewrites (Suites B/C/D flip per new semantics) |
| Code shrinks substantially — flat compose over each root's pre-built TC at exec replaces three-pass intersects pipeline. Composer is one walker function (~150 lines). | `with_dependencies` algebra renamed (`propagate` → `through_edge`); 29 unit tests PORT (algebra unchanged, only the method name) |
| Wire format for `Visibility`/`EntryVisibility` stays byte-identical | |
| `resolve.json` shape unchanged from today — keeps the inductive TC primitive as the single source of truth | |
| Lower long-term doc burden — "two surfaces, one boolean flag" replaces "view × edge × entry intersects mask" | |

### Option C — Flip the install-gate mask to INTERFACE only

**Description:** Minimal patch. Keep `visible.rs` and intersects mask. Only change the `pull.rs:430` collision check to use `Visibility::INTERFACE` instead of `Visibility::PUBLIC`. Documents that "private-dep collisions are runtime-only".

**Concrete counter-example:** The matrix walk-through bug lives at `metadata/env/resolver.rs:104` (entry filter), not `pull.rs:430` (install gate). Replaying the walk-through with C applied: `view = PRIVATE` (because `--self`), edge filter at `visible.rs:143` evaluates `private.intersects(PRIVATE) = true` (A is kept — same as before), entry filter at `resolver.rs:104` evaluates `interface.intersects(PRIVATE) = false` (FOO is dropped — same as before). The mask flip changes only the install-gate row and leaves the runtime exec-env wrong. Three of the four `intersects` call sites are untouched.

| Pros | Cons |
|---|---|
| Smallest diff (one mask constant) | Does not fix the matrix walk-through bug |
| Zero doc churn | Predicate-overloading problem remains at three other call sites |
| | Authors will keep hitting the conflated-mask issue elsewhere |
| | Migration debt accumulates against the inevitable real fix |

### Option D — Rename `intersects` to `surface_membership`, split call sites into named accessors

**Description:** Keep `Visibility` as a struct, keep the 4 named constants, keep all four call sites. Rename `intersects` to `surface_membership(surface: Visibility) -> bool` (or split into `has_interface()`/`has_private()`). Refactor each of the four call sites to use the named accessor that captures its intent: `dep.visibility.contributes_to(consumer_surface)`, `var.visibility.in_surface(emit_surface)`, etc. No data model change; no module deletion; wire format untouched on every level (including `resolve.json`).

| Pros | Cons |
|---|---|
| Zero wire-format change at any layer (including `resolve.json`) | Mental model still has three knobs (view, edge, entry) — does not collapse to "two surfaces" |
| Smallest semantic-preserving diff | Predicate-overloading deferred, not removed: same boolean still drives three independent filters |
| `visible.rs` stays | Doc burden barely shrinks — banned-term taxonomy still applies, just renamed |
| Reversible at any time | Does not match CMake mental model |
| | Authors still must reason about `view` parameter through 4 call sites to predict effective env |

### Weighted criteria

| Criterion | Weight | Option A | Option B | Option C | Option D |
|---|---|---|---|---|---|
| Correctness on matrix walk-through | 5 | partial | full | none | partial (depends on accessor semantics — user must still pick correct surface arg per call site, easy to get wrong) |
| Code-deletion impact | 3 | small | large | none | small |
| Doc-burden reduction | 3 | small | large | none | small (rename ≠ collapse) |
| Migration risk | 4 | low | medium | very low | very low |
| Reversibility | 2 | high | medium | high | very high |
| Preserves on-disk `resolve.json` wire format | 3 | full (no change) | full (shape unchanged; only the algebra used to build the TC is renamed) | full | full |
| **Weighted total** | | partial | **highest** | minimal | partial-low |

## Decision Outcome

**Chosen Option:** B — Two-env composition refactor. **`resolve.json` shape unchanged from today; only the algebra used to build the TC simplifies (rename), and the predicate consumed at exec changes (`intersects` → `has_interface()` / `has_private()`).**

**Rationale:** Only Option B fixes the matrix walk-through without leaving the predicate-overloading problem in place. The migration cost is bounded: visibility wire format unchanged, `resolve.json` shape unchanged, CLI flag (`--self`) preserved, generated launcher scripts on disk remain valid. The doc-burden reduction is compounded by replacing three concepts (view, intersects, mode) with one ("two surfaces, one boolean").

### Why the inductive TC is the right primitive

Each package's `resolve.json` already stores its TC: a flat list where every entry's visibility is the EFFECTIVE visibility from THIS package's perspective. The visibility was computed inductively at install via `with_dependencies`: parent edge `through_edge` child's effective visibility, with diamond merge as OR-per-axis.

The inductive property: for any path R→A→B→...→D, if every edge on the chain has `has_interface()`, then the FIRST edge (R→A) decides R's surface assignment for D. Once `has_interface()` fails on any edge, the rest of the chain seals D away from R's interface — but R may still see D on its private surface if `has_private()` survives.

Storage consequence: reading `root.resolve.json` gives the entire surface assignment in one shot. Composer iterates flatly. No recursive walk at compose time. The same TC drives single-root, multi-root, and the install-time entrypoint collision check.

"Atomic vs composite" symmetry: a single-package exec is `compose(&[a], ...)`. Multi-package exec is `compose(&[a, b, c], ...)`. Identical algorithm; the slice length is the only difference. Cross-root dedup is built in via `seen: HashSet<DepKey>`.

### `resolve.json` Storage Decision

The current `resolve.json` (`crates/ocx_lib/src/package/resolved_package.rs:29-99`) stores the **full transitive closure** with **pre-computed effective visibility from the root's perspective** via `with_dependencies`. All readers (`tasks/find.rs`, `tasks/find_symlink.rs`, `tasks/find_or_install.rs`, `tasks/clean.rs`, `tasks/garbage_collection.rs`, the composer) get the full reachable set with surface assignment from a single JSON read.

Three storage models were considered:

| Option | Shape | Trade-off |
|---|---|---|
| M1 (rejected) | `resolve.json` carries pre-composed `interface_env: Env` and `private_env: Env` materialized at install time. | Loses cross-root dedup (the per-pkg materialization can't see what other roots in a multi-root exec already contributed). Doubles env storage. Forces recomputation on any consumer-context change. |
| M2 (rejected) | Two sibling files per package: `compose_interface.json`, `compose_private.json`. | Same problem as M1, plus three writes per install (atomicity harder); one extra disk read per exec. |
| **Adopted: inductive TC + flat compose** | `resolve.json` shape unchanged: `{ dependencies: Vec<ResolvedDependency> }`. The `visibility` on each TC entry is the effective visibility from this package's root perspective (built inductively at install via `through_edge` + `merge`). Composer at exec is a flat iteration over each root's TC with cross-root dedup. | Single read per root. No recursion at compose time. Cross-root dedup natural. Templates resolved fresh at compose time against each visited dep's content path (looked up via PackageStore) — keeps `${installPath}` / `${deps.NAME.installPath}` correct under any consumer context. |

**`resolve.json` shape (unchanged from today):**

```rust
// crates/ocx_lib/src/package/resolved_package.rs

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPackage {
    /// Transitive dependency closure with effective visibility from this
    /// package's root perspective. Built inductively at install via
    /// `Visibility::through_edge` (replaces old `propagate`) plus diamond
    /// merge via `Visibility::merge`. Deps before dependents. The root
    /// package itself is not included. Leaf packages have an empty vec.
    pub dependencies: Vec<ResolvedDependency>,
}

pub struct ResolvedDependency {
    pub identifier: PinnedIdentifier,
    pub visibility: Visibility,  // effective vis from THIS package's root perspective
}
```

No new fields. No new types. The wire format is byte-identical to today.

**Composer at exec is a flat iteration over each root's TC.** The composer reads `root.resolve.json`'s `dependencies` once and iterates flatly: each entry's `visibility` already encodes which of root's surfaces it reaches (`has_interface()` / `has_private()`). Templates are resolved fresh at compose time against each visited dep's content path — looked up via `PackageStore`. No transitive walk, no per-package recursion. Cross-root dedup is a shared `HashSet<DepKey>` that prevents emitting the same dep twice across multiple roots in `ocx exec a b c`.

**GC reachability preserved:** The `dependencies` field is the same flat any-identifier list it was before. GC walks it the same way. Consumers of `dependencies` read the `identifier` field for reachability; the `visibility` field is what now does the surface-assignment job at compose time.

**Reversibility:** Medium. Reverting requires re-introducing the deleted module and re-threading the `view: Visibility` parameter through 4 call sites. `resolve.json` files written by either side are mutually parseable: only the `Visibility::propagate` method name shifts to `through_edge`, and the `Visibility` accessors change. The `ResolvedPackage` struct itself is wire-stable across the refactor.

## Algorithm v3 — Two-Env Composition

Replaces Algorithm v2 (`archive/adr_visibility_two_axis_and_exec_modes.md` lines 482-501).

The algorithm splits across two timings: **install-time TC build** (the existing inductive `with_dependencies`, with `propagate` renamed to `through_edge`) and **exec-time flat compose** (one walker function over each root's pre-built TC; cross-root dedup via shared `HashSet<DepKey>`).

No `Surface` enum is introduced. Surface selection at exec is a single boolean (`self_view`) tied directly to the CLI `--self` flag. The composer emits an `Env`.

### Install-time: TC build via `with_dependencies` (algebra unchanged, methods renamed)

```rust
// crates/ocx_lib/src/package/resolved_package.rs (existing body, with rename)

pub fn with_dependencies(
    mut self,
    deps: impl IntoIterator<Item = (PinnedIdentifier, ResolvedPackage, Visibility)>,
) -> Self {
    let mut seen: HashMap<PinnedIdentifier, usize> = HashMap::new();

    for (dep_id, dep, edge) in deps {
        // Bubble up transitive deps first (preserves topological order).
        for transitive in dep.dependencies {
            // RENAMED: was edge.propagate(transitive.visibility)
            let composed = edge.through_edge(transitive.visibility);
            let key = transitive.identifier.strip_advisory();
            if let Some(&idx) = seen.get(&key) {
                // Diamond merge — OR per axis.
                self.dependencies[idx].visibility = self.dependencies[idx].visibility.merge(composed);
            } else {
                let idx = self.dependencies.len();
                seen.insert(key, idx);
                self.dependencies.push(ResolvedDependency {
                    identifier: transitive.identifier,
                    visibility: composed,
                });
            }
        }

        // Direct dep with declared edge visibility.
        let key = dep_id.strip_advisory();
        if let Some(&idx) = seen.get(&key) {
            self.dependencies[idx].visibility = self.dependencies[idx].visibility.merge(edge);
        } else {
            let idx = self.dependencies.len();
            seen.insert(key, idx);
            self.dependencies.push(ResolvedDependency { identifier: dep_id, visibility: edge });
        }
    }
    self
}
```

Same algorithm and tests as today; the rename `propagate` → `through_edge` makes the inductive intent legible ("compose an edge with the child's effective visibility"). Result: each entry's `visibility` is the effective visibility from THIS package's root perspective.

### Exec-time: flat compose over each root's TC

```rust
// crates/ocx_lib/src/package_manager/composer.rs

/// Compose env from one or more roots. Reads each root's pre-built TC from
/// resolve.json (single read per root), iterates flatly with cross-root dedup,
/// emits per-surface gated entries. No recursion at compose time.
pub async fn compose(
    roots: &[Arc<InstallInfo>],
    store: &PackageStore,
    self_view: bool,
) -> crate::Result<Env> {
    let mut seen = HashSet::<DepKey>::new();
    let mut buf  = Env::default();

    for root in roots {
        // Each root's TC is already flat. Iterate without recursion.
        for tc_entry in &root.resolved.dependencies {
            if !seen.insert(tc_entry.identifier.into_key()) { continue; }

            let want = if self_view {
                tc_entry.visibility.has_private()
            } else {
                tc_entry.visibility.has_interface()
            };
            if !want { continue; }

            // Dep contributes its INTERFACE side (only interface crosses edges).
            let dep = store.lookup(&tc_entry.identifier).await?;
            for var in &dep.metadata.bundle().env {
                if var.visibility.has_interface() {
                    buf.append(resolve_template(var, &dep.content, &dep.dep_contexts())?);
                }
            }
            if !dep.metadata.bundle().entrypoints.is_empty() {
                buf.append(synth_path_entry(&dep.content));
            }
        }

        // Root's own contributions — partitioned by self_view.
        if seen.insert(root.identifier.into_key()) {
            for var in &root.metadata.bundle().env {
                let in_surface = if self_view {
                    var.visibility.has_private()
                } else {
                    var.visibility.has_interface()
                };
                if in_surface {
                    buf.append(resolve_template(var, &root.content, &root.dep_contexts())?);
                }
            }
            if !self_view && !root.metadata.bundle().entrypoints.is_empty() {
                buf.append(synth_path_entry(&root.content));
            }
        }
    }

    Ok(buf)
}
```

**Multi-root semantics**: explicit. Cross-root dedup via shared `seen`. Each root's TC iterated once; deps shared between roots emit once.

**Atomic vs composite framing**: a single-package exec (`ocx exec a`) is `compose(&[a], ..., self_view)`. Multi-package exec (`ocx exec a b c`) is `compose(&[a, b, c], ..., self_view)`. Identical algorithm; the `roots` slice length is the only difference. No special-casing.

**Composition order is fixed:** for each root, TC entries first (in topological order, deps before dependents), then root's own envvars, then root's entrypoint synth-PATH. Later in iteration wins on collision because `Modifier::Path` prepends and `Modifier::Constant` replaces. Root's own contributions win PATH lookup over its dep contributions.

**No `intersects`. No `view`. No mask. No `Surface` enum.** Each filter is a structural accessor on the existing `Visibility`: TC entry `has_interface()` / `has_private()` against the boolean `self_view`. Chain selection at exec is one boolean.

**Templates resolved at compose time:** `${installPath}` is resolved against the visited dep's content path (looked up via `PackageStore`). `${deps.NAME.installPath}` is resolved against the dep's own `dep_contexts`. This keeps templates correct under any consumer context (and avoids the cross-root dedup problem M1 had).

## Worked Examples

Each example walks the inductive TC at install (via `with_dependencies`), then the flat compose at exec.

### Example 1 — Sealed dep contributes nothing

```
R → A (edge: sealed)
A: env=[FOO=public]
```

R's TC after install: `[(A, SEALED)]` (the edge is sealed, so A's effective vis from R = SEALED).

| Surface (compose) | Gate | Result |
|---|---|---|
| Default exec (`self_view=false`) | A entry: `SEALED.has_interface()=false` → skip | {} |
| `--self` (`self_view=true`) | A entry: `SEALED.has_private()=false` → skip | {} |

Both empty. Sealed = "load only at install time", which is the existing semantic.

### Example 2 — Interface entry on private-edge dep (the matrix walk-through)

```
R → A (edge: private)
A: env=[FOO=interface]
```

R's TC after install: `[(A, PRIVATE)]` (the edge declared `private` is the effective vis since A has no further deps).

A's `Bundle.env` contains `FOO` with `EntryVisibility::Interface`. The composer emits a dep's contributions only on the interface side (only interface crosses edges); for FOO that means `var.visibility.has_interface()=true`.

| Surface (compose) | Gate on TC entry | Emits FOO from A? |
|---|---|---|
| Default exec (`self_view=false`) | `PRIVATE.has_interface()=false` → skip A | no |
| `--self` (`self_view=true`) | `PRIVATE.has_private()=true` → visit A; FOO has `has_interface()=true` → emit | yes — {FOO} |

`ocx exec R` (default): FOO not present. Correct — R hides A from its consumers under a private edge.
`ocx exec R --self`: FOO present. Correct — R running as itself sees A's interface contribution because R is A's consumer.

This is the case Algorithm v2 got wrong.

### Example 3 — Diamond merge across mixed edges

```
R → A (edge: public)
R → B (edge: private)
A → C (edge: interface)
B → C (edge: public)
C: env=[BAR=public]
```

C is reachable from R via two paths: R→A→C and R→B→C. The TC build at install computes effective vis inductively:

- C@A = `INTERFACE` (declared edge); C@B = `PUBLIC` (declared edge).
- C@R via A: `PUBLIC.through_edge(INTERFACE) = PUBLIC` (INTERFACE has_interface=true → keep parent).
- C@R via B: `PRIVATE.through_edge(PUBLIC) = PRIVATE`.
- Diamond merge: `PUBLIC.merge(PRIVATE) = PUBLIC` (OR per axis).

R's TC after install includes `(C, PUBLIC)`, `(A, PUBLIC)`, `(B, PRIVATE)`.

| Surface (compose) | Gate on C entry | Emits BAR? |
|---|---|---|
| Default exec | `PUBLIC.has_interface()=true` → visit C; BAR has `has_interface()=true` → emit | yes — {BAR} |
| `--self` | `PUBLIC.has_private()=true` → visit C; BAR has `has_interface()=true` → emit | yes — {BAR} |

The `seen: HashSet<DepKey>` prevents emitting C twice (once shared across both paths). The diamond merge already did the union work at install — at compose time we just gate by the effective visibility.

## Entrypoint Collision Truth Table

Setup: root R declares entrypoint `e`; dep A also declares entrypoint `e`. Install gate asserts uniqueness on the **interface projection of R's TC** only — the user-facing default surface. Collisions on the private surface are tolerated (user explicitly opts in via `--self`; topological PATH order resolves at runtime, root prepends last so root wins).

| Edge R→A vis (effective vis of A in R's TC) | A in interface projection? | A in private projection? | Install gate (interface-only) | `ocx exec R -- e` | `ocx exec R --self -- e` |
|---|---|---|---|---|---|
| sealed (SEALED) | no (`has_interface()=false`) | no (`has_private()=false`) | OK | R's `e` | R's `bin/e` (only R contributes to private) |
| private (PRIVATE) | no (`has_interface()=false`) | yes (`has_private()=true`) | OK | R's `e` | R's `bin/e` wins by PATH order (root prepends last over A's contribution) |
| interface (INTERFACE) | yes | no (`has_private()=false`) | **FAIL — collision in interface projection** | n/a | n/a |
| public (PUBLIC) | yes | yes | **FAIL — collision in interface projection** | n/a | n/a |

Two install-fail rows (interface, public) — both because A's `e` reaches R's interface projection. Private and sealed rows are install-OK. Note that the `private` edge case yields a private-surface duplicate (both R and A contribute `e` at compose time under `--self`); this is **not** a hard error — runtime PATH order resolves it.

**Policy:** collision check fires on the interface projection of R's TC only. A package's own declared entrypoints are always interface-tagged at declaration time. But when a private-edge dep contributes (under `--self` because `PRIVATE.has_private()=true`), the dep's entrypoint synth-PATH legitimately lands in R's private surface — not a collision-error, just topological PATH composition.

This narrows the install-gate from the pre-refactor `Visibility::PUBLIC` mask (which would have flagged all four rows as potential collisions and rejected install spuriously for the private/sealed cases).

## Banned-Term Taxonomy

Replace these terms across all source code, doc surfaces, ADRs, and rules:

| Banned | Replace with |
|---|---|
| `intersects` | `has_interface()` / `has_private()` |
| `view filter`, `view mask`, `view-dispatched` | "interface surface" / "private surface" |
| `mode filter`, `mode.includes` | surface selection |
| `is_visible`, `is_self_visible`, `is_consumer_visible` | (delete — no replacement; truth table over `has_interface`/`has_private`) |
| `ExecMode`, `ExecModeFlag` | (already deleted in prior refactor; do not re-introduce) |
| `import_visible_packages`, `apply_visible_packages` | `composer::compose` (single composer entry point at exec) |
| `VisiblePackage`, `ImportScope` | (no replacement — composer reads `Bundle` + `ResolvedPackage` directly) |
| `Surface`, `SurfaceEntry`, `SurfaceEntryKind`, `PackageEnv` | (deleted — composer reads `Bundle` + `ResolvedPackage` directly; output is one `Env`; surface selection is `bool self_view`) |
| `Visibility::propagate` (method) | `Visibility::through_edge` (renamed, identical algebra) |
| `axis filter`, `consumer-axis` (as filter), `self-axis` (as filter) | "interface projection" / "private projection" of the TC |
| `Phase A`, `Phase B`, `two-phase pipeline` | "composition pass" / "composer" |
| "view-mask the env" | "compose the env" |

Replacement vocabulary: **interface surface**, **private surface**, **`compose`** (the exec-time composer), **`through_edge`** (the inductive TC composition step, replacing `propagate`), **two-env composition**, **composition pass**, **`self_view`** (boolean for surface selection).

Additional banned method name: `propagate` (renamed to `through_edge`). Detection uses `\.propagate\(` and `::propagate\(` to avoid false positives on the English word "propagate" appearing in prose.

Every doc, code comment, test name, and rule must use the replacement vocabulary post-refactor. Phase 5 reviewer runs the extended grep:

```bash
rg -i "intersects|is_(self_|consumer_)?visible|exec.?mode|view.?(mask|filter)|mode\.includes|import_visible_packages|apply_visible_packages|VisiblePackage|ImportScope|SurfaceEntry|PackageEnv|\.propagate\(|::propagate\(" \
   website/src/docs/ .claude/ crates/ test/ \
   --glob '!**/test_*_legacy*.py'
```

and asserts zero hits in non-test code. Hits in `crates/ocx_lib/src/.../tests` allowed only if the test name asserts the term is gone (e.g., a banned-term regression test). Hits in changelog entries / superseded ADR ("supersedes" cross-references) allowed.

## Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| Lines in `visible.rs` | ~1700 | 0 | Module deleted |
| Predicate-overloaded call sites | 4 (3 production + 1 entry filter) | 0 | Each concern gets explicit structural code |
| `Visibility::intersects` call sites | 4 | 0 | Method deleted |
| `Visibility::propagate` call sites | 1 (in `with_dependencies`) | 0 | Method renamed to `through_edge` (same algebra, clearer name) |
| `Visibility::merge` call sites | 2 (in `with_dependencies`) | 2 | Kept — diamond dedup at install time still applies (unchanged) |
| Unit tests covering algebra (`resolved_package.rs::tests`) | 29 | 29 | **Ported, not deleted.** Algebra is identical post-rename; tests update method name `propagate` → `through_edge` and continue to validate the inductive TC build. |
| Doc surfaces using "view/intersects/mask" | 12+ | 0 | Banned-term grep gate enforces |
| Wire format for `Visibility`/`EntryVisibility` | shipped | unchanged | Custom Serialize/Deserialize/JsonSchema preserved verbatim |
| `resolve.json` shape | `{ dependencies: Vec<ResolvedDependency> }` (TC with effective visibility) | unchanged | No new fields. Wire-stable across the refactor. The inductive TC is the storage primitive. |
| Exec-time on-disk reads per `ocx env`/`ocx exec` | 1 per root (root's `resolve.json`) | 1 per root (root's `resolve.json`) | Composer iterates the flat TC; no recursive on-disk loads. Multi-root: still one read per root, with shared dedup. |
| CLI flag | `--self` | `--self` | Unchanged; semantics shift to surface selector |
| `PackageErrorKind::EntrypointNameCollision` (2-owner shape) | 1 variant | 0 variants (deleted) | Replaced by `EntrypointCollision { name, owners: Vec<_> }` supporting N>2 owners. |

### Consequences

**Positive:**
- Matrix walk-through correct on every cell.
- 1700-line module deletion; net code shrinks — the composer is the only new module, no `Surface`/`SurfaceEntry`/`PackageEnv` indirections.
- Doc model collapses to "two surfaces, one boolean" — narrative is one paragraph instead of three.
- Each filter concern lives in its own structural code, not a shared predicate.
- CMake mental model maps 1:1 (entry visibility = `target_include_directories` keyword; edge visibility = `target_link_libraries` keyword; chain selection = "compile T" vs "consume T", expressed as `self_view: bool`).

**Negative:**
- Acceptance test rewrites for Suites B/C/D (test_exec_modes.py + test_entrypoints.py) per new semantics.
- 12+ doc surfaces need the banned-term sweep.
- `PackageErrorKind::EntrypointNameCollision` (2-owner shape) deleted in favor of N-owner `EntrypointCollision`.

**Risks:**
- **Partial doc migration leaves stale "intersects" references.** Mitigation: explicit extended grep gate in Phase 5 reviewer (now covering `import_visible_packages`, `apply_visible_packages`, `VisiblePackage`, `ImportScope`, `view-dispatched`, `axis filter`, `Phase A/B`, `two-phase pipeline`, `consumer-axis`, `self-axis`, plus `propagate` as a method name), automated zero-hit assertion.
- **CLI surface preservation but semantic shift.** `--self` flag stays; semantics move from "tighten the view mask" to "select the private surface". Mitigation: existing acceptance test `test_self_mode_includes_private_dep_env` keeps green. Test `test_env_self_mode_excludes_interface_deps` flips per Suite D — explicitly called out in the plan as a semantic-shift test rewrite.
- **Composer perf** is a flat iteration over each root's pre-built TC: one disk read per root (the existing `resolve.json` deserialize), no transitive recursion at compose time. Multi-root exec adds N reads where N = root count; cross-root dedup keeps total work bounded by `|union of TCs|`, not N×|TC|.

## Technical Details

### Data Model

**Consolidation note:** No `Surface` enum, no `SurfaceEntry`, no `SurfaceEntryKind`, no `PackageEnv` intermediate struct. The composer reads `Bundle.env`, `Bundle.entrypoints`, and `ResolvedPackage.dependencies` directly and decides emission using `Visibility::has_interface()` / `has_private()` accessors. Output is one `Env`. Templates are resolved at compose time against each visited dep's content path.

Surface selection at exec is a single `bool self_view` boolean (matches the CLI `--self` flag directly). No enum, no two-state dispatch helpers.

#### `ResolvedPackage` (unchanged from today)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPackage {
    pub dependencies: Vec<ResolvedDependency>,  // TC with effective visibility from this package's perspective
}

pub struct ResolvedDependency {
    pub identifier: PinnedIdentifier,
    pub visibility: Visibility,  // EFFECTIVE visibility from THIS package's root perspective
}
```

No new fields. The existing wire format is preserved.

#### `Visibility` API (post-refactor)

```rust
impl Visibility {
    /// Does this visibility contribute to the interface surface?
    pub const fn has_interface(self) -> bool { self.interface }

    /// Does this visibility contribute to the private surface?
    pub const fn has_private(self) -> bool { self.private }

    /// Inductive TC composition. Compose a parent edge with a child's effective
    /// visibility (from the child's own root perspective). Replaces the old
    /// `propagate` name. Rule: if child's effective vis has_interface (interface_env
    /// crosses edges), parent edge's vis flows through unchanged; otherwise sealed.
    pub const fn through_edge(self, child_eff: Self) -> Self {
        if child_eff.has_interface() { self } else { Self::SEALED }
    }

    /// Diamond merge — OR per axis. Used by `with_dependencies` when a TC entry
    /// is reachable via multiple paths.
    pub const fn merge(self, other: Self) -> Self {
        Self {
            interface: self.interface || other.interface,
            private:   self.private   || other.private,
        }
    }
}
```

Truth table for the 4 named constants:

| Constant | `has_interface()` | `has_private()` |
|---|---|---|
| `SEALED` | false | false |
| `PRIVATE` | false | true |
| `PUBLIC` | true | true |
| `INTERFACE` | true | false |

**Kept:** `merge` (used by diamond dedup in `with_dependencies`, unchanged).

**Renamed:** `propagate` → `through_edge` (clearer name; identical algebra). Tests update to call the new name.

**Deleted:** `intersects`, `is_visible`, `is_self_visible`, `is_consumer_visible`. The four `Visibility` constants combined with `has_interface()` / `has_private()` cover every use site.

#### `PackageErrorKind::EntrypointCollision`

Add to `crates/ocx_lib/src/package_manager/error.rs`:

```rust
#[error("entrypoint name collision: '{name}' declared by {owners:?}; deselect one before selecting another")]
EntrypointCollision {
    name: EntrypointName,
    owners: Vec<oci::PinnedIdentifier>,
},
```

This **replaces** the existing `PackageErrorKind::EntrypointNameCollision { name, first, second }` (which limits to 2 owners). The replacement is required because composition over a flat TC can yield N>2 owners contributing the same entrypoint name — the install gate must report all of them. The `EntrypointNameCollision` variant is **deleted**, not deprecated, per `project_breaking_compat_next_version`.

### Composer API

```rust
// crates/ocx_lib/src/package_manager/composer.rs (NEW, replaces visible.rs)

/// Compose env from one or more roots. Reads each root's pre-built TC from
/// resolve.json (single read per root), iterates flatly with cross-root
/// dedup, emits per-surface gated entries. No recursion at compose time.
///
/// `roots` may be a single root (`ocx exec a`) or multiple (`ocx exec a b c`).
/// The algorithm is identical; the slice length is the only difference.
/// Cross-root dedup via shared `HashSet<DepKey>` ensures a dep shared between
/// roots emits once.
pub async fn compose(
    roots: &[Arc<InstallInfo>],
    store: &PackageStore,
    self_view: bool,
) -> crate::Result<Env>;

/// Install-gate uniqueness check on entrypoint names across the interface
/// surface of the given root. Iterates root's TC, projects interface side,
/// reads each visited pkg's `Bundle.entrypoints[]`, asserts unique names.
///
/// Collisions in the private surface are tolerated — runtime PATH order
/// resolves them.
pub async fn check_entrypoint_collision(
    root: &Arc<InstallInfo>,
    store: &PackageStore,
) -> Result<(), PackageErrorKind>;
```

**Offline mode:** Composer reads each root's `resolve.json` (already on disk) and looks up each TC entry's content via the local `PackageStore`. No network. Offline-safe by construction.

**Empty-input behavior:** `compose(&[root], ..., self_view)` on a leaf package (no TC entries) emits only the root's own contributions partitioned by `self_view`. `compose(&[], ...)` returns an empty `Env`.

### Call-Site Surface

Six CLI commands thread `--self` through `manager.resolve_env(packages, self_view: bool)`:

- `crates/ocx_cli/src/command/exec.rs:81-85`
- `crates/ocx_cli/src/command/env.rs:44-48`
- `crates/ocx_cli/src/command/deps.rs:62-66`
- `crates/ocx_cli/src/command/shell_env.rs:48-52`
- `crates/ocx_cli/src/command/ci_export.rs:46-50`
- `crates/ocx_cli/src/command/shell_profile_load.rs:36-40`

CLI surface unchanged: `--self` flag stays. The internal API takes `self_view: bool` directly — no `Surface` enum. `let self_view = self.self_view;` replaces the prior `let view = if self.self_view { Visibility::PRIVATE } else { Visibility::INTERFACE };`. The command threads `self_view` into `composer::compose(roots, store, self_view)` for the env emission.

### Install Gate

`crates/ocx_lib/src/package_manager/tasks/pull.rs:409-435` is rewritten:

```rust
// before:
//   pull.rs:18  use ...visible::{collect_entrypoints, import_visible_packages};
//   pull.rs:430 import_visible_packages(roots, Visibility::PUBLIC) + collect_entrypoints
// after:
//   composer::check_entrypoint_collision(root, &store).await?;
```

The collision check iterates the root's TC, projects the interface side via `tc_entry.visibility.has_interface()`, reads each visited package's `Bundle.entrypoints[]`, and asserts unique names. Collisions in the private surface (entrypoints from a private-edge dep that reach R's private surface under `--self`) are tolerated — runtime PATH order resolves them.

Pre-refactor `Visibility::PUBLIC` mask was the union — flagged false-positive collisions on private-edge deps with shared entrypoint name. Post-refactor gate runs exclusively on the interface projection of the TC.

**Audit of `pull.rs` callers of `import_visible_packages` / `apply_visible_packages`:** Phase 1 of the plan runs LSP `findReferences` on both functions and documents each call site's migration target. Confirmed sites today: `pull.rs:18` (use import) and `pull.rs:427` / `pull.rs:430` (collision check). If `PullTracker` or download walks use these for transitive set computation (rather than just collision check), those sites migrate to reading `resolve.json` v2's `dependencies` field (the any-surface chain) — not `interface_env` or `private_env`. The `dependencies` field IS the transitive set today and continues to be in v2.

## Implementation Plan

Plan artifact: [`.claude/state/plans/plan_two_env_composition.md`](../state/plans/plan_two_env_composition.md).

High-level phases (full breakdown in plan):

1. [ ] **Stub** — `composer.rs` (one module: `compose`, `check_entrypoint_collision`), signatures only. Add `Visibility::has_interface()` / `has_private()` accessors and `through_edge` (rename of `propagate`). Update `tasks/resolve.rs::resolve_env` and 6 CLI commands to take `self_view: bool`. Empty bodies = `unimplemented!()`. Audit (LSP `findReferences`) of `import_visible_packages`, `apply_visible_packages`, `resolve_visible_set`, `Visibility::intersects/propagate/is_visible/is_self_visible/is_consumer_visible`. Gate: `cargo check`.
2. [ ] **Verify Architecture** — `worker-reviewer` (post-stub, spec-compliance) validates stubs.
3. [ ] **Specify** — Port 21 `visible.rs` unit tests + 4 `tasks/resolve.rs` unit tests; flip entry-filter semantics; add Suite A/B/C/D/E acceptance tests. The 29 `with_dependencies` algebra tests are PORTED (not deleted) — they call `through_edge` instead of `propagate`.
4. [ ] **Implement** — Fill composer (`compose` over each root's TC + cross-root dedup); rewrite `pull.rs` collision check to call `composer::check_entrypoint_collision(root, &store)`; delete `visible.rs` + `Visibility::intersects/is_visible/is_self_visible/is_consumer_visible`; rename `Visibility::propagate` → `through_edge` (keeping `merge`); `EnvResolver::resolve_env(env, view)` deletion happens here after composer wires through.
5. [ ] **Doc + Review** — Two-pass doc rewrite (narrative + reference, then rules + catalog); banned-term grep gate (extended, includes `Surface`/`SurfaceEntry`/`PackageEnv`/`propagate`); spec-compliance + composition-order tests; JSON Schema regen.

## Validation

- [ ] Suite A/B/C/D/E acceptance tests green
- [ ] 21 ported unit tests + 29 ported `with_dependencies` algebra tests (now calling `through_edge`) + new composition tests green
- [ ] `task verify` passes
- [ ] Extended banned-term grep returns zero hits in non-test code (scope: `website/src/docs/`, `.claude/`, `crates/`, `test/`); covers `Surface`/`SurfaceEntry`/`SurfaceEntryKind`/`PackageEnv` and `\.propagate\(` / `::propagate\(`
- [ ] JSON Schema regen produces no diff for `Visibility`/`EntryVisibility`/`ResolvedPackage`
- [ ] LSP `findReferences` audit on `import_visible_packages`, `apply_visible_packages`, `resolve_visible_set`, `Visibility::intersects`, `Visibility::propagate` shows zero remaining call sites post-Phase 4
- [ ] Cross-model review (`--codex`) finds no blocking concerns

## Links

- [`research_env_composition_models.md`](./archive/research_env_composition_models.md) — five-tool comparison validating the model
- [`adr_visibility_two_axis_and_exec_modes.md`](./archive/adr_visibility_two_axis_and_exec_modes.md) — superseded ADR (Algorithm v2)
- [`adr_package_dependencies.md`](./adr_package_dependencies.md) — original four-level edge visibility
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — entrypoint launcher mechanism
- CMake [`target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html), [`target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html)

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-30 | architect (opus) | Initial draft. Supersedes `adr_visibility_two_axis_and_exec_modes.md` Algorithm v2. Algorithm v3, three worked examples, entrypoint-collision truth table, banned-term taxonomy, three-option trade-off (A=role-override, B=two-env, C=mask-flip), weighted criteria. |
| 2026-04-30 | architect (opus) | Round 2 amendment per review reports. **B1**: Per-surface materialization at install time persisted into `resolve.json` v2 (M1 chosen, M2/M3 documented as alternatives). Algorithm v3 split into `compose_recursive` (install) and `compose_env` (exec). **B2**: `PackageErrorKind::EntrypointCollision { name, owners: Vec<_> }` documented; replaces 2-owner `EntrypointNameCollision`. **B3**: `Visibility::has_interface()` / `has_private()` accessors + truth table documented. **B4**: `pull.rs` audit task added (Phase 1 LSP `findReferences`). **W1**: `PackageEnv::partition` semantics clarified — composer does not re-filter. **W4**: `resolve_version: u8` discriminant via `serde_repr` + `deny_unknown_fields` for v1↔v2 rollback gate. **W5**: Banned-term taxonomy extended (`import_visible_packages`, `apply_visible_packages`, `VisiblePackage`, `ImportScope`, `view-dispatched`, `axis filter`, `Phase A/B`, `two-phase pipeline`, `consumer-axis`/`self-axis`). **W7**: `Surface` moved to `package_manager/`. **W9**: Trade-off section honesty — Option C/A counter-examples, Option D added (rename-only), preserves-resolve.json criterion added. Decision: M1 storage shape. |
| 2026-04-30 | architect (opus) | Round 3 consolidation. **C1**: Dropped `resolve_version` schema versioning per `project_breaking_compat_next_version` — straight breaking change, no `serde_repr` discriminant, force-reinstall via `find_or_install` on parse error. **C2**: Entrypoints can land in `private_env` when inherited from a private-edge dep's interface_env — synth-PATH no longer specially gated. Collision check scoped to `interface_env` only; `private_env` duplicates resolved at runtime by topological PATH order. Truth table updated. **C3**: Dropped `Surface` enum, `SurfaceEntry`, `SurfaceEntryKind`, `PackageEnv` — composer reads `Bundle` + `ResolvedPackage` directly via `Visibility::has_interface()`/`has_private()` accessors. Chains are `Env` (existing env-entry type) with `${installPath}`/`${deps.NAME.installPath}` resolved eagerly at install time. Single `compose_envs` returns `(interface, private)` in one walk. Exec-time reads `install_info.resolved.{interface,private}_chain` directly (or via `env_for(self_view: bool)` accessor). CLI commands plumb `bool self_view` instead of `Surface`. Banned-term taxonomy extended with the dropped names. |
| 2026-04-30 | architect (opus) | Round 5 consolidation per user pushback on M1. **R1**: Reverted M1 storage shape — `resolve.json` shape unchanged from today (`{ dependencies: Vec<ResolvedDependency> }`). The inductive TC is the right primitive: each entry's `visibility` is the effective visibility from THIS package's root perspective, computed at install via `with_dependencies`. Reading `root.resolve.json` gives the entire surface assignment in one shot — no recursive walk at compose time. **R2**: Composer is a flat iteration over each root's pre-built TC with cross-root dedup via shared `HashSet<DepKey>`. `compose(roots, store, self_view) -> Env`. Atomic vs composite framing: single-package and multi-package exec are `compose(&[a], ...)` vs `compose(&[a, b, c], ...)` — identical algorithm; slice length is the only difference. **R3**: `Visibility::propagate` renamed to `through_edge` (clearer name, identical algebra). `Visibility::merge` kept (diamond dedup). `Visibility::intersects`/`is_visible`/`is_self_visible`/`is_consumer_visible` deleted; truth table covers all uses via `has_interface()` / `has_private()`. **R4**: 29 algebra tests in `resolved_package.rs::tests` PORT (not delete) — they remain valid since the algebra is identical, only the method name changes. **R5**: Phase 1 stub work simplified (no `interface_env`/`private_env` fields on `ResolvedPackage`). Phase 4 budget reduced to ~2h opus (composer is one walker function ~150 lines). **R6**: Banned-term grep extended with `\.propagate\(` / `::propagate\(` to catch the renamed method without false-positives on prose. |
