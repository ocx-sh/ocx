# ADR — Two-Phase Visible-Package Pipeline (Import → Apply)

## Metadata

**Status:** Accepted
**Date:** 2026-04-26
**Deciders:** worker-architect (opus), review-fix loop on `feat/package-entry-points`
**Branch:** `feat/package-entry-points`
**Related ADRs:**
- [`adr_three_tier_cas_storage.md`](./adr_three_tier_cas_storage.md) — establishes `packages/.../resolve.json` and per-package GC reachability
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — entrypoint metadata, launcher generation, `<current>/entrypoints` PATH integration
- [`adr_deps_name_interpolation.md`](./adr_deps_name_interpolation.md) — `${deps.NAME.installPath}` interpolation that the apply phase resolves
**Reversibility Classification:** **One-Way Door Medium.** Phase A/B function shape is internal; can be reshaped freely. The deletion of the on-disk `EntrypointsIndex` (Finding 9) is a pre-release breaking change — once `feat/package-entry-points` ships, restoring it would require reintroducing the JSON schema, the lock file, and the snapshot/restore protocol. The branch already carries `feat!:` markers and no release has shipped the index, so this is acceptable now.

## Context

OCX has two distinct moments at which a package's transitive dependency closure becomes visible:

1. **Install time** — `pull` resolves the closure, computes propagated visibility, persists `resolve.json` next to the assembled package.
2. **Consumption time** — `ocx env`, `ocx exec`, `ocx ci export`, `ocx shell env`, `ocx shell profile load` need the same closure to emit env vars and PATH entries for the consuming process.

Today these two moments are linked only by the on-disk `resolve.json`, but the **way the closure is consumed** is duplicated, ad-hoc, and mixes orthogonal concerns. Three concrete pain points motivate this ADR.

### Today's `resolve.json`

`crates/ocx_lib/src/package/resolved_package.rs` defines:

```rust
pub struct ResolvedPackage {
    pub dependencies: Vec<ResolvedDependency>, // topological, deps before dependents
}
pub struct ResolvedDependency {
    pub identifier: PinnedIdentifier,
    pub visibility: Visibility,
}
```

`ResolvedPackage::with_dependencies` already runs the visibility algebra (`Visibility::propagate` + `Visibility::merge` for diamond deps) at install time. The result is a topologically ordered, visibility-annotated list.

**Today this list feeds exactly two consumers:**
- GC reachability walk (`reachability_graph.rs`) — visibility ignored, all dep edges followed.
- `PackageManager::resolve_env` (`tasks/resolve.rs:209`) — re-walks the list inline, builds a fresh `dep_contexts` map per visit, calls `super::common::export_env` for each visible dep then for the root. The walk, the visibility filter, the dedup tracker (`check_exported`), and the env emission are all interleaved in a single function.

Future consumers — SBOM merge, license aggregation, lockfile pinning, audit reporting — would each need to re-implement the same walk against `resolve.json`, with no shared abstraction for "what is visible from this root".

### Today's `EntrypointsIndex`

Finding 9's target. Defined in `crates/ocx_lib/src/package_manager/tasks/common.rs:262-336` as a per-registry on-disk JSON file at `symlinks/{registry}/.entrypoints-index.json`, owned by an exclusive lock at `symlinks/{registry}/.entrypoints-index.lock`.

```rust
pub(super) struct EntrypointsIndex {
    pub schema_version: u8,
    pub entries: BTreeMap<String, IndexOwner>, // launcher name -> (registry, repo, digest)
}
```

Maintained by `wire_selection` (`tasks/common.rs:367-481`) at every `--select` and `select`/`deselect` operation. The flow is:

1. Acquire registry-scoped index lock, then per-repo `.select.lock` (documented lock order).
2. Read index from disk, defaulting to empty.
3. Cross-check the new package's entrypoint names against current index entries; reject with `EntrypointNameCollision` if any name is already owned by a different `(registry, repository)`.
4. Snapshot prior index bytes + prior `current` symlink target.
5. Write the index, then write the symlink. Roll both back on failure.

**Three problems with this design:**

- **Wrong granularity for the actual constraint.** A "launcher name collision" is only a real conflict when two packages with the same entrypoint name end up in the same consumer's closure at the same `ocx exec` / `ocx env` invocation. The current index treats every selection across the entire registry as a potential collision — it fires even on packages that no consumer will ever combine. Two unrelated tools that both ship a `serve` launcher cannot coexist as `current` selections under the same registry, even when no `ocx exec` ever sees them together.
- **Selection-time semantics for a consumption-time question.** Sealed dependencies have no entrypoints visible to consumers (by the visibility algebra), but the index check at `--select` time is unaware of visibility. Future visibility-aware features (e.g., a tool that ships an internal helper with a common name like `format`) cannot land cleanly without retrofitting the index.
- **Heavyweight on-disk machinery.** A JSON file, a per-registry lock, a snapshot/restore protocol, and an atomic-write contract all exist purely to maintain a derived state that can be re-derived from `resolve.json` plus the `current` symlinks at any time. The on-disk footprint exists only to serve a check that fires in the wrong place.

### Today's PATH/env emission inline-ness

`PackageManager::resolve_env` (`tasks/resolve.rs:209-318`) is the only path. It interleaves three concerns:

1. **Walk the closure.** Iterate `pkg.resolved.dependencies` for each input `InstallInfo`, filter by `visibility.is_visible()`.
2. **Per-package state.** Reload metadata from disk, reconstruct dep-context maps for `${deps.NAME.installPath}` interpolation.
3. **Side effects.** Push entries onto a flat `Vec<Entry>`.

There is no separation between "what packages are visible from these roots" and "what to do with each visible package". Adding the synthetic `PATH ⊳ <installPath>/entrypoints` entry per visible package (Finding 8b) would mean editing this function. Adding a SBOM merger (a hypothetical future consumer) would mean copying the walk shape elsewhere. Adding `${deps.NAME.version}` / `${deps.NAME.digest}` interpolation (the next deps interpolation extension) means threading new fields through the inlined per-visit logic.

### Findings being addressed

This ADR captures the architectural decision behind four review-fix-loop findings:

- **F8a (refactor).** Split the inline walk into two phases so the closure becomes a first-class value, not a control-flow shape.
- **F8b (feat!).** Emit a synthetic `PATH ⊳ <installPath>/entrypoints` entry per visible package so consumers automatically receive launcher search paths from their dependencies' entrypoints, gated by visibility (sealed deps don't propagate).
- **F8c (feat).** Auto-inject `.cmd` into PATHEXT on Windows where the env we emit is consumed inside an `ocx`-controlled child process; warn at the other shell-export sites.
- **F9 (feat!).** Delete `EntrypointsIndex`, replace with a closure-scoped collision check shared by Stage 1 (install) and Stage 2 (consumption).

## Decision

Introduce a two-phase pipeline for every consumer that needs a package's transitive closure: **Phase A — Import** produces the visible-package list; **Phase B — Apply** runs the consumer over the list. Both phases live in a new module `crates/ocx_lib/src/package_manager/visible.rs`.

### Phase A — `import_visible_packages`

```rust
// crates/ocx_lib/src/package_manager/visible.rs

/// One package in a root's visible closure.
///
/// The same on-disk `Arc<InstallInfo>` is reused — no clone of metadata or
/// content path. `scope` carries the propagated visibility plus any
/// import-time-resolved metadata callers want to thread through to the apply
/// phase (e.g. resolved version/digest for `${deps.NAME.version}`).
pub struct VisiblePackage {
    pub install_info: Arc<InstallInfo>,
    pub scope: ImportScope,
}

pub struct ImportScope {
    /// Effective visibility from the root, post-propagation/merge.
    pub visibility: Visibility,
    /// True for the input root packages themselves (not transitive deps).
    pub is_root: bool,
}

/// Walks the dependency closure of every root, applies
/// `Visibility::propagate` + `Visibility::merge` (diamond OR per axis), and
/// returns the visible packages in topological order (deps before
/// dependents). Re-walks `resolve.json` only — does not call `find_in_store`
/// on each dep; the caller has already loaded the roots.
///
/// Roots come last in their own slice; transitive deps come first, so that a
/// downstream `for visible in import_visible_packages(&roots)` iteration
/// gives consumers a topological, deduplicated view in one pass.
///
/// Detects same-repo digest conflicts (today's `check_exported` warning) and
/// surfaces them through the same logging path. Future revisions may upgrade
/// these warnings to a hard error here without changing the apply-phase
/// signature.
pub async fn import_visible_packages(
    fs: &FileStructure,
    roots: &[Arc<InstallInfo>],
) -> Result<Vec<VisiblePackage>, Error>;
```

**Invariants:**

- The returned `Vec<VisiblePackage>` is in topological order (deps before dependents). Any apply-phase consumer can rely on the fact that, when it reaches a package, every dep that resolves into that package's `${deps.*}` interpolation surface has already been visited.
- Sealed deps do **not** appear in the output. The visibility algebra is the gate.
- The same physical package (by stripped identifier) appears at most once. Diamond deps merge upward.
- The roots' own `Arc<InstallInfo>` are reused — no extra disk reads, no metadata reload.
- Same input always produces the same output (pure function of `resolve.json` + roots). No I/O beyond reading transitive `metadata.json` / `resolve.json` for visible deps that the caller did not pre-load.

### Phase B — `apply_visible_packages`

```rust
/// Per-package entries produced by the apply phase.
pub struct AppliedEntry {
    pub key: String,
    pub value: String,
    pub kind: ModifierKind,
}

/// Records launcher-name collisions discovered while iterating the visible set.
/// One report per `apply_visible_packages` call; collisions are scoped to that
/// consumer (per `ocx env`, per `ocx exec`).
pub struct CollisionReport {
    pub collisions: Vec<EntrypointNameCollision>,
}

pub struct EntrypointNameCollision {
    pub name: EntrypointName,
    pub first: PinnedIdentifier,
    pub second: PinnedIdentifier,
}

/// Walks the visible packages in topological order and emits, per package:
///   1. The package's declared env vars (path/constant), resolved against
///      `info.content` for `${installPath}` and against the
///      already-imported deps for `${deps.NAME.installPath}`.
///   2. A synthetic `PATH ⊳ <info.content>/entrypoints` entry when
///      `info.metadata.bundle_entrypoints()` is non-empty.
///   3. A consumer-scoped collision update: insert `(name -> identifier)` for
///      every entrypoint name; on second insertion of the same name with a
///      different identifier, push an `EntrypointNameCollision` into the
///      report (do not abort iteration — let the caller decide policy).
///
/// Returns the flat `Vec<AppliedEntry>` in emission order plus the
/// `CollisionReport`. The apply phase NEVER touches disk; all metadata is
/// already on `info.metadata`.
pub fn apply_visible_packages(
    visible: &[VisiblePackage],
) -> (Vec<AppliedEntry>, CollisionReport);
```

**Invariants:**

- Order of emission == order of input. Callers that want to compose env layering rely on this.
- The synthetic PATH entry is gated by `bundle_entrypoints().is_some_and(|e| !e.is_empty())`. Packages with no entrypoints never contribute a launcher PATH.
- Collisions are **consumer-scoped**: each `apply_visible_packages` call has its own tracker. Two unrelated `ocx exec` invocations that each select a `serve` launcher do not collide; only an invocation whose closure contains two `serve` launchers does.
- The apply phase MUST be infallible at the I/O layer (it does no I/O). Template-resolution failures use the existing `Exporter` error path and are caller-surfaced via `Result` upgrade if a future caller wants strict mode — initial impl returns a `Result` and propagates `Exporter` errors.

### Collision detection: shared helper

Both Stage 1 (install — invoked from `pull.rs` while building `ResolvedPackage`) and Stage 2 (consumption — invoked from `apply_visible_packages`) need the same per-closure dedup logic. Extract:

```rust
/// Builds a name -> identifier map by scanning every visible package's
/// entrypoints in topological order. The first owner wins; subsequent
/// owners surface as `EntrypointNameCollision`.
///
/// Stage 1 caller (pull.rs::pull_one): runs against the just-resolved
/// closure to catch intra-closure dupes at install time, before
/// `resolve.json` is persisted. A failure here aborts the install with
/// `PackageErrorKind::EntrypointNameCollision`.
///
/// Stage 2 caller (apply_visible_packages): runs as part of the per-call
/// emission tracker. A collision here surfaces in `CollisionReport` and
/// the consuming command (`ocx exec`, `ocx env`) decides whether to warn,
/// fail, or pick first-wins.
pub fn collect_entrypoints(
    visible: &[VisiblePackage],
) -> Result<BTreeMap<EntrypointName, PinnedIdentifier>, EntrypointNameCollision>;
```

Single source of truth for "what does it mean for two packages to claim the same launcher name". Both call sites get identical behavior; the registry-wide on-disk index disappears.

### EntrypointsIndex deletion (Finding 9)

The following code is removed in the F9 commit:

- `EntrypointsIndex` struct + `IndexOwner` (`tasks/common.rs:262-336`).
- `acquire_index_lock` (`tasks/common.rs:229-249`).
- `clear_index_owner` (`tasks/common.rs:520-535`).
- `restore_index_snapshot` (`tasks/common.rs:549-572`).
- `SelectionLocks` registry-arm (the per-repo `.select.lock` survives — it still serializes the single `current` symlink update).
- The Phase 2 / Phase 4a logic inside `wire_selection` (`tasks/common.rs:407-468`) — the index lock acquisition, the cross-repo collision check, the index snapshot, the index write, and the rollback of the index snapshot.
- `SymlinkStore::entrypoints_index` and `SymlinkStore::entrypoints_index_lock` accessors.
- All unit tests in `tasks/common.rs::tests` keyed on `EntrypointsIndex` (`entrypoints_index_default_pins_schema_version_to_one`, `freshly_created_index_file_persists_schema_version_one`, `wire_selection_rolls_back_index_on_symlink_failure`).

`wire_selection` collapses to: acquire `.select.lock`, optionally write candidate symlink, write `current` symlink, return outcome. Same atomicity guarantees, smaller surface.

The `PackageErrorKind::EntrypointNameCollision` variant **stays** — same error type, raised from two different sites:
- Stage 1: `pull.rs::pull_one`, when `collect_entrypoints` over the closure being built returns `Err`.
- Stage 2: surfaced from `CollisionReport` by the consuming command (e.g. `ocx exec` aborts; `ocx env` may warn).

### Naming discipline

Per project convention (see `arch-principles.md` "Code Style Conventions" — "Full descriptive names, not abbreviations"):

- `import_visible_packages` (not `walk` / `gather` / `vis`)
- `apply_visible_packages` (not `apply` / `emit`)
- `VisiblePackage` (not `VPkg` / `Visible`)
- `ImportScope` (not `Scope` / `Ctx`)
- `collect_entrypoints` (not `collect` / `entrypoints`)
- `add_for_package` if a builder pattern emerges (not `add` / `push`)

### PATHEXT (Finding 8c)

Scope-noted here, implementation lives in its own commit. Decision recorded so the F8b commit does not have to re-decide:

- **At `ocx exec` / `ocx env`** — the child process env is fully controlled by `ocx`. When the host is Windows and PATHEXT does not already contain `.cmd`, prepend `.cmd` to PATHEXT on the fly so generated launchers (`name.cmd` only — the `name` Unix script never appears on Windows) actually resolve via PATH search.
- **At `ocx install`, `ocx select`, `ocx shell env`, `ocx ci export`, `ocx shell profile load`** — the env we emit is consumed by something OTHER than `ocx` (the user's interactive shell, a CI runner's `$GITHUB_ENV`, a profile script). Auto-mutating the user's PATHEXT is out of scope. Emit a one-shot warning (gated by a once-cell to keep it from spamming CI logs) suggesting the user add `.cmd` to PATHEXT manually.

PATHEXT lives outside `apply_visible_packages` — it is a host-environment fix-up, not a per-package emission. It belongs at the boundary where the env we emit meets the OS, i.e. in `command/exec.rs` and `command/env.rs` for the controlled cases, and in `command/{shell,ci}/*` for the warn cases.

## Consequences

### Positive

- **Single source of truth for the visible closure.** `import_visible_packages` is the only function that walks `resolve.json` and applies the visibility algebra at consumption time. Future consumers (SBOM, license merge, lockfile audit, dependency graph reporting) reuse it.
- **Env, entrypoints, and collision detection share one ordered list.** They cannot drift. Today they are three separate code paths.
- **Sealed deps no longer claim entrypoint ownership at `select` time.** A library package that ships a private launcher named `serve` no longer prevents an unrelated tool with a `serve` entrypoint from being selected globally. Collisions surface only when both end up in the same consumer's closure.
- **Same-closure dupes caught earlier.** Stage 1 invocation from `pull.rs` catches intra-closure entrypoint collisions at install time, before the bad state reaches disk. Today this only fires later, at `--select`.
- **`resolve.json` schema unchanged.** Phase A walks the existing structure; Phase B reads it. No on-disk migration for `resolve.json`.
- **Smaller `tasks/common.rs`.** ~250 lines of index machinery deleted.
- **Future `${deps.NAME.version}` / `${deps.NAME.digest}` lands additively.** Add fields to `ImportScope`; the apply phase reads them; no other code changes.
- **Consumer-scoped collision policy is configurable per command.** `ocx exec` can fail-fast on `CollisionReport.collisions.is_empty() == false`; `ocx env --json` can include the collisions in its structured output for tooling to react to; `ocx shell profile load` can warn.

### Negative

- **Breaking on-disk change.** `EntrypointsIndex` JSON file and lock file disappear. Any user upgrading across this branch sees stale `.entrypoints-index.json` / `.entrypoints-index.lock` files at `symlinks/{registry}/`. They are harmless (nothing reads them) but cosmetically present until the user runs `ocx clean` or removes `~/.ocx/symlinks/`. The branch already carries `feat!:` markers for the broader package-entrypoints work; this fits the same breaking-window.
- **Stage 2 walks `resolve.json` on every `ocx exec` / `ocx env`.** Already true today; not a regression. The walk is cheap (in-memory after the first metadata read) and the metadata is on the critical path anyway.
- **Two collision call sites.** A test that exercises only Stage 1 must not be assumed to cover Stage 2 and vice versa. Both must be tested. `collect_entrypoints` having a unit-test layer makes this tractable.
- **Slightly more allocation per apply call.** The `Vec<VisiblePackage>` is materialized; today the equivalent state is implicit in the loop. The cost is one `Vec` per consumer call, holding `Arc<InstallInfo>` clones; negligible compared to the existing per-call metadata read.

### Risks

- **Order-dependent regressions.** Today `resolve_env` emits in a specific order: visible deps in `resolve.json` order, then root. The two-phase split must preserve byte-equivalent order for the F8a refactor commit; F8b changes order intentionally (synthetic PATH inserted per package). Unit tests asserting current emission order must be updated to assert the new shape.
- **Stale `.entrypoints-index.json` files in user `$OCX_HOME`.** Mitigation: documented in CHANGELOG; `ocx clean` could be extended to sweep these in a follow-up commit if support requests warrant.
- **Future "global" features that legitimately want cross-closure visibility.** A hypothetical "list every launcher name in use across this `$OCX_HOME`" feature could not be served by Stage 2 alone and would have to walk `symlinks/.../current` directly. We accept this as an acceptable trade — that feature does not exist, and the per-closure model serves the actual product surface.

## Alternatives Considered

### A1. Keep `EntrypointsIndex`, teach it about visibility

Add a `visibility: Visibility` column to `IndexOwner`, skip rows where the package's entrypoint is sealed-from-consumers, retain the registry-wide index file otherwise.

**Rejected.** The global state file is the wrong primitive. Even with a visibility column, the file still tracks "which packages are currently selected and what launchers they expose" — but the question that actually matters at install/consumption time is "in this specific closure, are there two packages claiming the same launcher?". The global index treats every cross-repo selection as relevant; the closure scope treats only the actually-combined packages as relevant. A visibility column papers over the granularity mismatch without fixing it.

Adding a visibility column also doubles the on-disk schema surface and forces every Stage 1 caller to re-derive visibility before writing — duplicating the algebra that already runs in `ResolvedPackage::with_dependencies`.

### A2. Persist resolved env into `resolve.json`

Write the per-package env entries (post-template-resolution, post-`${deps.*}`-substitution) into `resolve.json` so consumption skips the resolve step entirely.

**Rejected.** Two reasons:

1. `resolve.json` must stay GC-focused. It is read by `reachability_graph.rs` to determine forward-ref edges; bloating it with env entries grows every read in the GC path for no GC benefit. The current schema (just `dependencies: [{identifier, visibility}]`) is the right shape for that consumer.
2. Env resolution is cheap. The metadata is already on the critical path of `ocx exec`/`ocx env` (we have to read `metadata.json` to honor it), and the template substitution is in-memory string work. Caching its output saves microseconds and costs an on-disk schema migration.

If a future profiling pass shows env resolution is a hot path (it is not today; the OCI client and the child-process spawn dominate every `ocx exec`), this can be revisited as a memo cache in `PackageManager`, not a `resolve.json` field.

### A3. Single fused `walk_and_emit` function

Keep one function that walks the closure and emits entries in a single pass (i.e., a slightly-cleaner version of today's `resolve_env`).

**Rejected.** Conflates "which packages are visible from these roots" with "what we do with them". The phase split lets future consumers (SBOM, license merge, lockfile audit) reuse Phase A without dragging in env-emission logic. It also makes the per-phase invariants testable in isolation: Phase A unit tests assert order, dedup, visibility; Phase B unit tests assert emission order, synthetic PATH gating, collision reporting — without each test having to set up the full closure walk.

The phase split costs one function boundary and one `Vec<VisiblePackage>` allocation per call. Both are cheap. The composability gain is large.

### A4. Per-consumer collision policy in `apply_visible_packages`

Pass a `CollisionPolicy { fail_fast, first_wins, last_wins }` parameter into the apply phase so each command's policy lives at the call site.

**Rejected for v1.** Returning a `CollisionReport` keeps the apply phase pure (no policy-driven branching) and lets each command apply its own policy on the report. If duplicate code emerges across commands (e.g. every command actually does fail-fast), extracting a policy enum is a Two-Way refactor at that point. Premature abstraction now.

## Implementation Order

Four commits, in this order, on `feat/package-entry-points`. Each commit ships its own tests; each is reviewable in isolation; no commit leaves the tree in a broken state.

### Commit 1 — F8a (refactor)

**Type:** `refactor:` (Two Hats — structural change, behavior preserved).

**Scope:**
- New file `crates/ocx_lib/src/package_manager/visible.rs` housing `VisiblePackage`, `ImportScope`, `import_visible_packages`, `apply_visible_packages`. Initial `apply_visible_packages` does ONLY what today's `resolve_env` does (no synthetic PATH yet, no collision reporter — return an empty `CollisionReport`).
- `tasks/profile_resolve.rs::resolve_profile_env` (today's wrapper around `resolve_env`) routes through the new abstraction.
- `tasks/resolve.rs::resolve_env` becomes a thin shim around `import_visible_packages` + `apply_visible_packages`, preserved as a public entry point until F8b lands.
- Move `check_exported` (`tasks/resolve.rs:348-368`) into Phase A as a private helper. Same warning behavior, same conflict semantics — just inside `import_visible_packages`.

**Tests:** No new acceptance tests. Existing unit tests in `tasks/resolve.rs::spec_tests` and any callers of `resolve_env` continue to pass byte-equivalent. Reviewer perspective: behavior-preservation.

**Gate:** `task rust:verify` passes; existing acceptance tests pass without modification.

### Commit 2 — F9 (feat!)

**Type:** `feat!:` (breaking — on-disk `EntrypointsIndex` deleted).

**Scope:**
- Delete `EntrypointsIndex`, `IndexOwner`, `acquire_index_lock`, `clear_index_owner`, `restore_index_snapshot`, `SymlinkStore::entrypoints_index*`, and the index-related logic inside `wire_selection`.
- Add `collect_entrypoints(visible: &[VisiblePackage]) -> Result<BTreeMap<EntrypointName, PinnedIdentifier>, EntrypointNameCollision>` in `visible.rs`.
- Stage 1 wire-up: in `tasks/pull.rs::pull_one`, after the closure is resolved and before `resolve.json` is written, build the in-memory `Vec<VisiblePackage>` for the just-built closure and call `collect_entrypoints`. On `Err`, abort the pull with `PackageErrorKind::EntrypointNameCollision`. Order matters: this check fires after dependency resolution but before any on-disk artifact for this package becomes reachable.
- Stage 2 wire-up: have `apply_visible_packages` populate `CollisionReport.collisions` via `collect_entrypoints` over the same input slice.
- Update `command/exec.rs` to fail-fast on `CollisionReport.collisions.is_empty() == false`; update `command/env.rs` to surface collisions in its plain/JSON output (warn or include, per command discretion — reviewer to confirm specific behavior in commit body).
- Delete obsolete `tasks/common.rs::tests` keyed on `EntrypointsIndex`.
- Add new tests:
  - Unit (`visible.rs::tests`) — `collect_entrypoints` returns `Err` on intra-closure dupe.
  - Unit (`tasks/pull.rs::tests`) — Stage 1 catches a closure with two deps both declaring `serve`, before `resolve.json` is persisted.
  - Acceptance (`test/tests/test_entrypoints.py`) — two unrelated tools with overlapping launcher names can both be `--select`-ed under the same registry (proves the false-positive at `--select` is gone). Distinct invocation: `ocx exec` over a closure containing both fails with the same `EntrypointNameCollision` kind (proves the true-positive moved to consumption time).

**Gate:** `task verify` passes (full gate; this is breaking and acceptance tests must cover both the false-positive removal and the true-positive shift).

### Commit 3 — F8b (feat!)

**Type:** `feat!:` (additive but visibility-gated; consumers see PATH change).

**Scope:**
- `apply_visible_packages` emits, per visible package whose `metadata.bundle_entrypoints()` is non-empty, an `AppliedEntry { key: "PATH", value: "<info.content>/entrypoints", kind: ModifierKind::Path }`. Order: prepend the synthetic entry **after** the package's declared env vars but before the next visible package's emissions (so PATH layering still goes deps-first → root-last, matching the existing semantics).
- Visibility-gated by Phase A — sealed deps already absent from the input slice, so no sealed dep contributes a launcher PATH.
- New acceptance tests:
  - `test/tests/test_dependencies.py` — root package with a `Public` dep that has entrypoints: `ocx env root` includes `<dep>/entrypoints` in PATH; `ocx env root` with the dep `Sealed` does NOT include it.
  - `test/tests/test_entrypoints.py` — `ocx exec root -- <dep-launcher>` resolves the dep's launcher via the synthetic PATH (no `--select` needed).
- Updated unit tests in `visible.rs::tests` — emission-order assertions for the new synthetic entry.

**Gate:** `task verify` passes; both new acceptance tests pass.

### Commit 4 — F8c (feat)

**Type:** `feat:` (additive, Windows-only behavior).

**Scope:**
- `command/exec.rs` and `command/env.rs`: where the child env is constructed (today inside the `--install-dir` / identifier branches that hand off to the child-process builder), if `cfg!(windows)` and PATHEXT does not contain `.cmd` (case-insensitive), prepend `.cmd;` to the inherited PATHEXT in the child's env. The host's env is untouched.
- `command/shell/env.rs`, `command/ci/export.rs`, `command/shell/profile/load.rs`: emit a one-shot warning (gated by a `OnceLock<()>`) when `cfg!(windows)` and PATHEXT does not contain `.cmd`. Message points the user at adding `.cmd` to their PATHEXT manually.
- New acceptance tests deferred to a Windows-runner-only smoke; tracked in the commit body.

**Gate:** `task rust:verify` passes; no acceptance test changes.

## Touched Documentation Surfaces

When the implementation commits land, the following documentation must be updated in the same commit (per the `feedback_plans_must_list_docs.md` user-feedback rule):

- **F9 commit**: `website/src/docs/reference/storage.md` — remove any mention of `.entrypoints-index.json` and `.entrypoints-index.lock`. `website/src/docs/user-guide.md` — update the entry-points section to describe consumer-scoped (not registry-scoped) collision detection. `CHANGELOG.md` — `BREAKING:` note describing the removal of the on-disk index and the shift of collision detection to consumption time.
- **F8b commit**: `website/src/docs/user-guide.md` "Dependencies" + "Entry Points" sections — document that visible deps' entrypoint directories are auto-prepended to PATH at `ocx env` / `ocx exec`. `website/src/docs/reference/metadata.md` — note that visibility now affects PATH propagation as well as env propagation.
- **F8c commit**: `website/src/docs/reference/command-line.md` (the `exec` and `env` sections) — note the auto-injection of `.cmd` into PATHEXT for Windows hosts. Adjacent shell/CI command sections — note the warning behavior.
- All four commits: `.claude/rules/subsystem-package-manager.md` — update the "Module Map" table to add `visible.rs` and remove the `EntrypointsIndex` references in `tasks/common.rs` description.

No CLI flags change. No new commands. No metadata schema change.

## Component Contracts (handoff to implementation agents)

### `visible.rs`

| Item | Signature | Invariants |
|---|---|---|
| `VisiblePackage` | `{ install_info: Arc<InstallInfo>, scope: ImportScope }` | Topologically ordered in any returned `Vec`; same physical package appears at most once |
| `ImportScope` | `{ visibility: Visibility, is_root: bool }` | `is_root` is true iff the package was in the input `roots` slice |
| `import_visible_packages(fs, roots) -> Result<Vec<VisiblePackage>, Error>` | Returns visible closure in topological order | Pure function of `roots` + on-disk `resolve.json`/`metadata.json` for transitive deps |
| `apply_visible_packages(visible) -> (Vec<AppliedEntry>, CollisionReport)` | Emits env + synthetic PATH entries; collects collisions | No I/O; emission order == input order; synthetic PATH gated by `bundle_entrypoints().is_some_and(\|e\| !e.is_empty())` |
| `collect_entrypoints(visible) -> Result<BTreeMap<EntrypointName, PinnedIdentifier>, EntrypointNameCollision>` | First-owner-wins collision detector | Used by both Stage 1 (pull) and Stage 2 (apply) |

### Files deleted (F9)

| File | Lines |
|---|---|
| `tasks/common.rs` (partial — index machinery) | ~262-336 (`EntrypointsIndex`, `IndexOwner`); ~229-249 (`acquire_index_lock`); ~520-535 (`clear_index_owner`); ~549-572 (`restore_index_snapshot`); ~407-468 (index logic inside `wire_selection`) |
| `file_structure/symlink_store.rs` (partial) | `entrypoints_index`, `entrypoints_index_lock` accessors |

### Files added

| File | Purpose |
|---|---|
| `crates/ocx_lib/src/package_manager/visible.rs` | Phase A + Phase B + `collect_entrypoints` |

### Files re-routed

| File | Change |
|---|---|
| `tasks/resolve.rs::resolve_env` | Delegates to `import_visible_packages` + `apply_visible_packages` |
| `tasks/profile_resolve.rs::resolve_profile_env` | Reads `apply_visible_packages` output instead of inlining env composition |
| `tasks/pull.rs::pull_one` | Calls `collect_entrypoints` over the just-resolved closure before persisting `resolve.json` |
| `tasks/common.rs::wire_selection` | Collapses to lock + symlink-write only (no index touch) |
| `command/exec.rs`, `command/env.rs` | Consume `CollisionReport`; F8c: PATHEXT auto-inject |
| `command/shell/env.rs`, `command/ci/export.rs`, `command/shell/profile/load.rs` | F8c: PATHEXT warn-once |

## UX Scenarios

### Happy path — visible dep contributes its launchers

```
$ ocx install myorg/cmake-cli:3.28 --select
 ...
$ ocx env myorg/cmake-cli:3.28 | grep PATH
PATH=/home/me/.ocx/packages/.../content/bin:/home/me/.ocx/packages/<dep>/content/entrypoints:...
$ ocx exec myorg/cmake-cli:3.28 -- ninja --version       # ninja is a dep launcher
1.11.1
```

### Sealed dep does NOT contribute

```
$ cat metadata.json
{ "type":"bundle", ..., "dependencies":[{"name":"helper","reference":"...","visibility":"sealed"}] }
$ ocx env myorg/cmake-cli:3.28 | grep helper
(no output — sealed dep is invisible to consumers)
```

### Cross-repo, non-overlapping closures: no collision

```
$ ocx install ocx.sh/foo:1 --select       # foo provides launcher 'serve'
$ ocx install ocx.sh/bar:1 --select       # bar provides launcher 'serve' too — different repo
ok                                          # selection succeeds (no global collision)
$ ocx exec ocx.sh/foo:1 -- serve            # uses foo's serve
$ ocx exec ocx.sh/bar:1 -- serve            # uses bar's serve
```

### Same-closure collision at consumption

```
$ ocx exec ocx.sh/foo:1 ocx.sh/bar:1 -- serve
error: entrypoint name collision in this exec closure: 'serve'
hint: first owner: ocx.sh/foo:1@sha256:... ; second owner: ocx.sh/bar:1@sha256:...
exit 65 (DataError)
```

### Same-closure collision at install (Stage 1)

```
$ ocx install ocx.sh/myapp:1     # myapp transitively pulls in foo + bar
error: entrypoint name collision in dependency closure of ocx.sh/myapp:1: 'serve'
hint: first owner: ocx.sh/foo:1@sha256:... ; second owner: ocx.sh/bar:1@sha256:...
exit 65 (DataError)
```

### Windows PATHEXT auto-inject (F8c)

```
PS> $env:PATHEXT
.COM;.EXE;.BAT
PS> ocx exec ocx.sh/cmake:3.28 -- cmake --version
# child process's PATHEXT is .CMD;.COM;.EXE;.BAT — cmake.cmd resolves
cmake version 3.28.1
```

### Windows PATHEXT warn-once (F8c)

```
PS> ocx ci export ocx.sh/cmake:3.28 >> $env:GITHUB_ENV
warning: PATHEXT does not include '.cmd' — generated launchers may not resolve.
         Add '.cmd' to PATHEXT in your shell profile or CI runner config.
```

## Validation

- [ ] F8a: existing unit tests in `tasks/resolve.rs::spec_tests` and `tasks/profile_resolve.rs` continue to pass byte-equivalent
- [ ] F8a: new unit tests in `visible.rs::tests` assert topological order, sealed-dep exclusion, diamond-dep merge
- [ ] F9: acceptance test proves two packages with overlapping launcher names CAN be `--select`-ed under the same registry simultaneously (proves the false-positive at `--select` is gone)
- [ ] F9: acceptance test proves `ocx exec` over a closure containing two such packages fails with `EntrypointNameCollision` (proves the true-positive moved to consumption)
- [ ] F9: unit test in `tasks/pull.rs` proves Stage 1 catches intra-closure entrypoint dupes before `resolve.json` is persisted
- [ ] F8b: acceptance test proves `ocx env <root>` includes `<visible-dep>/entrypoints` in PATH; sealed dep's entrypoints absent
- [ ] F8b: acceptance test proves `ocx exec <root> -- <dep-launcher>` succeeds without `--select`-ing the dep
- [ ] F8c (Windows smoke): PATHEXT-less host: `ocx exec` succeeds, `ocx ci export` warns
- [ ] All four commits: `task verify` passes; deferred findings (if any) recorded in commit body

## Links

- [`adr_three_tier_cas_storage.md`](./adr_three_tier_cas_storage.md) — `resolve.json` schema and per-package GC contract
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — entrypoint metadata, launcher generation, `<current>/entrypoints` PATH integration
- [`adr_deps_name_interpolation.md`](./adr_deps_name_interpolation.md) — `${deps.NAME.installPath}` resolution surface
- `crates/ocx_lib/src/package_manager/tasks/resolve.rs` — today's `resolve_env` (the inline walk being split)
- `crates/ocx_lib/src/package_manager/tasks/common.rs` — today's `EntrypointsIndex` + `wire_selection` (the index machinery being deleted)
- `crates/ocx_lib/src/package_manager/tasks/pull.rs` — Stage 1 entry point for `collect_entrypoints`
- `crates/ocx_lib/src/package/resolved_package.rs` — `ResolvedPackage::with_dependencies` (existing visibility algebra Phase A reuses)

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-26 | worker-architect (opus) | Initial ADR for two-phase visible-package pipeline; covers F8a/F8b/F8c/F9 from review-fix loop on `feat/package-entry-points` |
