# ADR: Project Registry for `ocx clean` Backlinks

## Metadata

**Status:** Accepted
**Date:** 2026-04-29
**Deciders:** mherwig
**Beads Issue:** N/A
**Related Plan:** [`/home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md`](file:///home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md) Unit 6 (lines 103–110)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 + Tokio, no new external dep (reuses `serde_json`, `tempfile`, `FileLock`).
**Domain Tags:** architecture | storage | dx
**Related ADRs:** [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md) (sentinel + advisory lock primitive reused here), [`adr_three_tier_cas_storage.md`](./adr_three_tier_cas_storage.md) (GC roots model)

## Context

`ocx clean` runs a single-user, single-machine garbage collector across the three CAS tiers (`packages/`, `layers/`, `blobs/`) under `OCX_HOME`. Reachability roots today come from two places (`crates/ocx_lib/src/package_manager/tasks/garbage_collection/reachability_graph.rs:45-66`):

1. **Live install symlinks** — packages whose `refs/symlinks/` directory contains a still-pointing forward symlink (the `has_live_refs` probe).
2. **Profile content-mode entries** — `ProfileSnapshot::content_digests()` resolved through `PackageStore::path()` and unioned into `roots`.

Multi-project workflows break this model. A developer who runs `ocx pull` (or, after Unit 7, `ocx add` / `ocx update`) inside project A creates a populated `ocx.lock` that pins concrete digests but **does not create install symlinks** — `ocx pull` is the metadata-first path that downloads into the object store without touching `symlinks/`. Switching to project B and running `ocx clean` then collects every package A pinned, because:

- A's pinned packages have no `refs/symlinks/` entries (no install was performed),
- A's pinned packages do not appear in B's `ocx.lock` or in B's profile,
- The garbage collector has no other ledger to consult.

The user wants `ocx clean` to retain packages held by *other projects'* `ocx.lock` files without needing those projects to be the active one when `ocx clean` runs. The plan (Unit 6) constrains the design to a per-user file under `OCX_HOME`, no daemon, lazy stale-pruning, and a `--force` escape hatch.

### Why this is a One-Way Door Medium

The on-disk shape of the registry under `OCX_HOME` becomes durable contract: any binary running against that home directory must agree on filename, format, and field semantics. A schema rename later costs one release + one migration paragraph; structurally similar to the `.ocx-lock` rename (see [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md)). The breaking-compat memo (`project_breaking_compat_next_version.md`) authorises a clean break if needed before stabilisation.

### Why now, before Unit 2b

Unit 2b will eventually remove profile-snapshot roots from the GC graph in favour of project-lock roots. This ADR predates that work and must coexist with it:

- The new project-registry source is **added alongside** the existing profile snapshot source. `ReachabilityGraph::build()` accepts both.
- Unit 2b removes only the profile arg later; the project-registry arg stays.

Coupling the two changes in one PR conflates "add new root source" with "remove old root source" — Two-Hats Rule violation. Keep them split.

## Decision Drivers

- **No central daemon.** OCX is a single static binary; long-running coordinator rejected up front. Single-writer-at-a-time over a flat file is the upper bound on coordination complexity.
- **Single source of truth for project membership.** The registry is the only ledger of "which projects exist on this machine"; no parallel database, no hidden cache, no symlink-graph reconstruction.
- **Lazy stale-pruning.** Vanished `ocx.lock` paths drop silently on read. No background sweeper, no scheduled task, no `ocx clean --prune-registry` flag.
- **Same-volume locking only.** The advisory lock for the registry sits next to the registry under `OCX_HOME` — same pattern as the per-project sentinel from [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md). No NFS-blind-spot regressions vs today.
- **No new dependencies.** Reuse `FileLock` (`crates/ocx_lib/src/file_lock.rs:17-22`), `serde_json` (already pinned), `tempfile` (already used in `ProjectLock::save`).
- **Atomic writes.** Same tempfile + rename pattern as `ProjectLock::save` (lines 433–446 of `crates/ocx_lib/src/project/lock.rs`). Concurrent readers must always see a complete, parseable file.
- **Acceptance contract.** Package installed via project A's `ocx.lock`, then `ocx clean` run with project B active → package retained, dry-run preview names project A's path. Encoded as the Unit 6 acceptance test.
- **Forward-compatibility with Unit 7.** `ocx add` / `ocx remove` will mutate `ocx.toml` and `ocx.lock` in a single transaction; both must register through the same primitive without coordinating new write paths.
- **No optional fields, no fallback paths.** Per project memory (`project_breaking_compat_next_version.md`): clean schema today beats a runtime probe that supports two formats.

## Industry Context & Research

No standalone research artifact for this Unit — the design space is constrained enough that the architect-gate ADR captures it inline. Adjacent prior art:

- **Cargo's package cache** uses two sentinel files inside `~/.cargo/registry/` (`.crates.toml`, `.crates2.json`) plus a directory-scoped advisory lock. Same shape as this ADR's choice: single ledger file under the per-user home, advisory lock on a sibling sentinel.
- **uv's project mode** keys workspace/project state off the project directory but does not maintain a back-reference index — uv's `uv cache prune` is conservative on principle, never collecting anything held by *any* discoverable lockfile, which solves the cross-project problem by being maximally retentive. OCX cannot adopt that posture because it ships an aggressive content-addressed GC.
- **mise / asdf** sidestep the problem entirely — they don't deduplicate across projects.

**Key insight:** the cargo sentinel-file pattern transfers cleanly. The novel piece is *what to store* (a list of `ocx.lock` paths) and *who writes them* (`ProjectLock::save` and friends).

## Considered Options

### Option 1 — Single JSON file at `$OCX_HOME/projects.json` with sibling advisory lock

**Description:** Persist the full registry as a single JSON document under `$OCX_HOME/projects.json`. Concurrent writers serialize through a same-volume sibling sentinel (`$OCX_HOME/.projects.lock`) using `FileLock::try_exclusive`. Atomic save = tempfile + rename, mirroring `ProjectLock::save`.

| Pros | Cons |
|------|------|
| Single file = trivial to read, inspect, debug. `cat $OCX_HOME/projects.json` is the entire ledger. | Whole-file rewrite on every entry update. At realistic scale (≤ 100s of projects per developer machine) the cost is sub-millisecond, but unbounded growth would matter. |
| Concurrent-writer story is identical to `ocx.lock`: known-good code path, no new locking design. | Single hot lock path. Two `ocx pull` invocations on different projects momentarily contend on `.projects.lock`. Sub-millisecond contention; not user-visible. |
| Atomic rename gives readers a consistent snapshot — never partial bytes. | Schema migration is whole-file: any field change rewrites every entry. |
| Compact: one JSON document per machine. Greppable. | |
| Lazy pruning is trivial: read, drop entries whose `ocx_lock_path` doesn't exist, write back. | |

### Option 2 — Per-project sentinel directory `$OCX_HOME/projects/<hash>/lockfile-link`

**Description:** Each registered project gets its own subdirectory under `$OCX_HOME/projects/`, named by `sha256(absolute_ocx_lock_path)[:16]`. Inside: a symlink `lockfile-link` pointing to the project's `ocx.lock`, plus a small `meta.json` (last_seen, first_registered).

| Pros | Cons |
|------|------|
| Concurrent writes never contend — each project owns its directory. | Cross-platform symlink semantics under `OCX_HOME` are a known foot-gun. Junctions on Windows, symlink-aware backup tools, container mount semantics. We've already paid for this complexity once in `symlinks/`; adding a second symlink graph doubles the surface. |
| Stale-pruning is per-directory: `for d in projects/*; do test -f $d/lockfile-link || rm -rf $d; done`. | Directory-per-project explodes inode count on dense developer machines (one user has 200 worktrees). |
| Self-healing: removing a directory unregisters one project without touching others. | Reconstructing "which projects exist" on read requires scanning a directory tree, not parsing a single JSON file. Slower, more error paths. |
| | Inverse of the registry: the registry is a single small ledger; this is many tiny files. Cargo, uv, mise all model the equivalent state as a single file, not a tree. |

### Option 3 — Lockfile self-registration via fingerprint embedded in `ocx.lock`

**Description:** Add a `[registry]` section to `ocx.lock` itself listing `OCX_HOME` paths the lock has been resolved against. `ocx clean` enumerates all `ocx.lock` files visible somewhere on disk (a discovery problem) and trusts each one.

| Pros | Cons |
|------|------|
| No separate registry file under `OCX_HOME`. | **Discovery problem unsolved.** "Find every `ocx.lock` on this machine" requires either a global filesystem scan (slow, intrusive) or a separate index (which is exactly the registry this ADR rejects). The whole point of the registry is to avoid the discovery scan. |
| | Embeds machine-specific state (`OCX_HOME`) inside a VCS-committed file. Wrong layer — `ocx.lock` is shared across machines. |
| | Concurrent writers across projects fight over the same `ocx.lock` write paths: now changes to project A's lock invalidate state on machine X for project A. |
| | Recovery story when a lock is moved or copied is bad: every fingerprint becomes stale silently. |

### Option 4 — Global daemon coordinating registration RPCs

**Description:** Long-running `ocx-registry` daemon owns the project list in memory; CLI invocations RPC to it.

| Pros | Cons |
|------|------|
| In-memory state is fastest. | **Plan explicitly rejects this.** OCX is a single static binary; daemons are out of scope. |
| | Cross-platform daemon supervision (systemd, launchd, Windows services) is a separate distribution problem. |
| | Failure modes (daemon crashed, daemon stale) are user-visible in a way file-based state never is. |

### Option 5 — `OCX_HOME`-keyed digest registry under a hashed scheme

**Description:** `$OCX_HOME/locks/<sha256(absolute_project_path)[:16]>.json` — same idea as Option 2 but flat (no symlink), one tiny JSON file per project.

| Pros | Cons |
|------|------|
| No directory tree to walk; flat hashed namespace. | Same NFS-volume issue as the rejected branch in [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md) Option 5: if `OCX_HOME` is on a different volume than the project, advisory-lock semantics degrade silently — but the lock is on `OCX_HOME`, not the project, so this is actually fine. |
| | Reconstructing "all projects" still requires a directory listing, not a JSON parse. |
| | Per-project file means many tiny writes during a "register all my projects" boot pass. Single ledger does it in one `fsync`. |
| | First-glance confusable with `cas_shard_path` hashing scheme but unrelated — conceptual surface area. |

## Decision Outcome

**Chosen Option:** **Option 1 — single JSON file at `$OCX_HOME/projects.json` with sibling advisory lock at `$OCX_HOME/.projects.lock`.**

### Rationale

- It re-uses the exact pattern already proved by `ProjectLock::save` (atomic tempfile + rename, sibling sentinel for advisory exclusion). Zero new locking primitives, zero new on-disk shapes — only a new schema inside an existing pattern.
- Single-file model matches cargo's `.crates.toml` and uv's per-machine state file; a maintainer who has read either tool's source recognises the layout.
- Stale-pruning rules are trivial to specify and trivial to test: read all entries, drop those whose `ocx_lock_path` no longer exists, write back. No directory tree to walk, no symlink semantics to reason about.
- Schema evolution remains tractable: the file carries a `schema_version` integer; future migrations rewrite the whole file under the same lock, never producing a half-migrated state on disk.
- No fallback for the "registry empty after upgrade" case — see Migration / First Run below. Profile-snapshot roots cover the transition until Unit 2b lands.

I considered Option 2 (per-project sentinel directory) closely. It is the most "obvious-looking" filesystem design: one directory per project, no contention, self-healing. It loses on three concrete grounds: (a) doubling the symlink graph, (b) inode density on dense machines, (c) cargo/uv/mise picking the single-file shape independently for the same problem. Option 1's contention cost (one `FileLock::try_exclusive` per registration) is sub-millisecond on every platform we ship.

### Quantified Impact

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Files under `OCX_HOME` for project bookkeeping | 0 | 2 (`projects.json`, `.projects.lock`) | Both in `OCX_HOME` root, alongside `profile.json`. |
| Worst-case GC false positive (multi-project) | Every package not held by active project | 0 (modulo deliberate `--force`) | The reason the ADR exists. |
| Registry write cost (per `ocx lock` / `ocx update` / `ocx add` / `ocx remove`) | 0 | One read + one rewrite of a small JSON file | ≤ 1 ms at realistic scale (< 1000 projects). |
| New deps added | — | 0 | `serde_json`, `tempfile`, `FileLock` already in tree. |
| Lock contention granularity | per-project (`.ocx-lock`) | per-project + per-machine (`.projects.lock`) | The new lock is taken only briefly during register/unregister; no read path takes it. |

### Consequences

**Positive:**

- The Unit 6 acceptance contract is met: package installed via project A's `ocx.lock`, then `ocx clean` run with project B active, retains the package and surfaces project A's path in the dry-run preview.
- Single primitive (`ProjectRegistry::register`) for every write trigger; Unit 7's `ocx add` / `ocx remove` reuse it without coordinating new lock files.
- `--force` flag preserves the user's right to GC despite registered projects — destructive-action confirmation per project memory's destructive-git rule.
- Greppable ledger: `cat $OCX_HOME/projects.json | jq '.entries[].ocx_lock_path'` is the entire query for "which projects are pinning packages on this machine."

**Negative:**

- One new lock file under `OCX_HOME` (`.projects.lock`). Hidden dotfile, never seen unless the user `ls -a`s their home.
- Whole-file rewrite on every register/unregister. Acceptable at realistic scale; would matter at thousands of projects, which is not a deployment we ship to.
- Lazy pruning means a registered project that vanishes between two `ocx clean` runs will be silently forgotten — potentially confusing if a developer expects an audit log of unregistrations. **Mitigation:** the dry-run preview of `ocx clean` lists which projects were pruned this run; users see the consequence at the next GC.

**Risks:**

- **Corrupt registry file.** A `kill -9` mid-rename leaves the *tempfile* on disk (atomic rename did not happen) and the original `projects.json` intact. Recovery model is identical to `ProjectLock::save`. If the file is corrupted out-of-band, `ocx clean` must refuse with `ConfigError` (exit 78) rather than silently fall back to "no registry"; falling back would re-introduce the GC-false-positive bug this ADR fixes.
- **Permission errors writing under `OCX_HOME`.** Read-only home directory (CI sandbox) breaks every register call. **Mitigation:** registration failures are logged at WARN level and never propagate as fatal errors from `ProjectLock::save` — locking the project must still succeed. `ocx clean` separately reads the registry; if read fails with permission, it surfaces as `PermissionDenied` (exit 77) per `quality-rust-exit_codes.md`.
- **Race between two `ocx pull` invocations on different projects.** Both attempt to register concurrently. The registry's sibling lock serialises them; the loser blocks ≤ 1 ms. No correctness risk; both registrations land sequentially.
- **Registry path under `OCX_HOME` on read-only NFS.** Same pattern caveat as the rest of `OCX_HOME` — already documented in the user guide. No new exposure here.

### How Would We Reverse This?

This ADR is a One-Way Door Medium. Reversal:

1. Pick a new schema (drop the registry, gate `ocx clean` on `--force` only, or migrate to a per-project sentinel directory) and ship it as the next version.
2. Add a one-line `CHANGELOG.md` entry and user-guide migration paragraph: "`$OCX_HOME/projects.json` and `$OCX_HOME/.projects.lock` are no longer used; safe to delete."
3. No code-side compat shim — single shape per version, in keeping with `project_breaking_compat_next_version.md`.

The reversal cost is bounded: one release, one diff at the registry-read site, two files to delete on user machines.

## Technical Details

### On-disk Schema

`$OCX_HOME/projects.json`:

```jsonc
{
  "schema_version": 1,
  "entries": [
    {
      // Absolute, canonicalised path to the project's ocx.lock file.
      // Canonical form is what `ProjectLock::save` was called with.
      "ocx_lock_path": "/home/alice/dev/proj-a/ocx.lock",
      // ISO-8601 UTC. Bumped on every register; used for diagnostics only.
      "last_seen": "2026-04-29T13:42:01Z"
    },
    {
      "ocx_lock_path": "/home/alice/dev/proj-b/ocx.lock",
      "last_seen": "2026-04-29T13:55:14Z"
    }
  ]
}
```

**Field rationale:**

- `schema_version: u8` — required for future migrations. Unknown values reject the whole file with `ConfigError`.
- `ocx_lock_path` — the only field strictly required for the GC contract. All other fields are diagnostic.
- `last_seen` — populated on every register. Useful in `ocx clean --dry-run` output ("this project last touched the registry 14 days ago"). Not load-bearing for the GC decision.

**Fields explicitly NOT stored:**

- `ocx_toml_path` — derivable from `ocx_lock_path.parent().join("ocx.toml")`; storing it would be redundant and drift-prone.
- Project name — `ocx.toml` may not have one; sourcing it would force a TOML parse on every read.
- Hash of `ocx.lock` content — staleness is detected by file existence, not by hash.
- `first_registered_at` — not used by any current code path; YAGNI.

### Module Layout

```
crates/ocx_lib/src/project/
  registry.rs         (NEW — ProjectRegistry struct + register/load/prune)
  registry/error.rs   (NEW — ProjectRegistryError + ProjectRegistryErrorKind)
```

Sibling to `lock.rs` and `config.rs`. Re-exported from `crates/ocx_lib/src/project.rs` as `pub mod registry;` and a top-level `pub use registry::ProjectRegistry;`.

### API Contract

```rust
// crates/ocx_lib/src/project/registry.rs

/// Per-user registry of projects whose `ocx.lock` files pin packages on this
/// machine. Used by `ocx clean` to retain packages held by lockfiles in
/// projects other than the currently-active one.
///
/// On disk: `$OCX_HOME/projects.json`, atomically rewritten on each register
/// under an advisory exclusive lock at `$OCX_HOME/.projects.lock`.
pub struct ProjectRegistry {
    path: PathBuf,         // .../projects.json
    sentinel_path: PathBuf, // .../.projects.lock
}

impl ProjectRegistry {
    /// Constructs a registry rooted at `ocx_home`. Pure path math; no I/O.
    pub fn new(ocx_home: &Path) -> Self;

    /// Records (or refreshes) the entry for `ocx_lock_path`. Must be called
    /// with an absolute, canonicalised path. Updates `last_seen` to "now".
    /// Atomic: tempfile + rename under the sibling advisory lock.
    ///
    /// # Errors
    /// - `ProjectRegistryErrorKind::Io` on directory creation, sentinel
    ///   open, lock acquisition (when contended past the `try_exclusive`
    ///   single-shot — see Failure Modes), or rename failure.
    /// - `ProjectRegistryErrorKind::Corrupt` if the existing registry file
    ///   exists but parses as garbage (refuses to overwrite blindly).
    pub async fn register(&self, ocx_lock_path: &Path) -> Result<(), Error>;

    /// Loads the registry and lazily prunes entries whose `ocx_lock_path`
    /// no longer exists. Returns the pruned-in-memory entries; if the on-
    /// disk file is unchanged after pruning, no write occurs. If pruning
    /// drops at least one entry, the new shape is persisted under the
    /// same lock-then-rename protocol.
    ///
    /// First-run (file absent) returns an empty `Vec` and writes nothing.
    pub async fn load_and_prune(&self) -> Result<Vec<ProjectEntry>, Error>;
}

#[derive(Clone, Debug)]
pub struct ProjectEntry {
    pub ocx_lock_path: PathBuf,
    pub last_seen: String, // ISO-8601 UTC
}
```

**No `unregister` method.** Pruning is exclusively lazy at read time. Removing an entry by API would invite surprise regressions when a `ocx remove` accidentally drops a project's last entry; lazy pruning by file existence is the safer model.

### Reachability Graph Contract Change

`ReachabilityGraph::build()` gains a second roots-source argument **alongside** the existing `&ProfileSnapshot`:

```rust
// BEFORE
pub async fn build(
    file_structure: &FileStructure,
    profile: &ProfileSnapshot,
) -> crate::Result<Self>;

// AFTER (this ADR)
pub async fn build(
    file_structure: &FileStructure,
    profile: &ProfileSnapshot,
    project_roots: &[ProjectRootDigests],
) -> crate::Result<Self>;
```

`ProjectRootDigests` is a new type in the GC module (`tasks/garbage_collection/project_roots.rs`):

```rust
pub struct ProjectRootDigests {
    pub ocx_lock_path: PathBuf,        // diagnostic — surfaces in dry-run output
    pub digests: Vec<PinnedIdentifier> // resolved from the lock's [tool] entries
}
```

Producer (`ProjectManager::collect_project_roots`, new free function in `package_manager/tasks/clean.rs`):

1. `ProjectRegistry::load_and_prune()` to get current entries.
2. For each entry: read its `ocx.lock` via `ProjectLock::from_path(path)`; collect `tools[*].pinned`. A read failure on a single lock degrades that entry to "skip + WARN-log + drop from registry on the next prune"; it never aborts `ocx clean`.
3. Return `Vec<ProjectRootDigests>` for the graph builder; preserve order as in the registry (stable for diagnostics).

`ReachabilityGraph::build()` then resolves each digest to a packages-store path identically to today's profile path:

```rust
let mut project_root_paths: HashSet<PathBuf> = HashSet::new();
for project in project_roots {
    for pinned in &project.digests {
        let pkg_path = file_structure.packages.path(pinned);
        project_root_paths.insert(canonicalize_or_keep(&pkg_path));
    }
}
// At the join_next site:
if is_root || profile_roots.contains(&pkg_dir) || project_root_paths.contains(&pkg_dir) {
    roots.insert(pkg_dir.clone());
}
```

**Diagnostic preservation:** the GC also produces a `roots_attribution: HashMap<PathBuf, Vec<PathBuf>>` mapping each retained package to the list of `ocx_lock_path`s pinning it. Surfaces in the `ocx clean --dry-run` output (see CLI UX below).

### Write Triggers

The plan calls for the registry to be written "whenever `ocx.toml` is locked / installed". Concretely the call sites are:

| Trigger | File | Function | When |
|---|---|---|---|
| `ocx lock` | `crates/ocx_lib/src/project/lock.rs` | `ProjectLock::save` (after the existing rename completes successfully) | Every successful save, including no-content-change saves where only the timestamp updates. |
| `ocx update` | same | `ProjectLock::save` | (transitively, via the above) |
| `ocx add` (Unit 7) | same | `ProjectLock::save` | (transitively) |
| `ocx remove` (Unit 7) | same | `ProjectLock::save` | (transitively) |
| `ocx pull` invoked with `--project` (resolves a project but does not save lock) | `crates/ocx_cli/src/command/pull.rs` (and `ocx_lib::package_manager::tasks::pull` when invoked through the project resolver) | After `ProjectLock::from_path(...)` returns `Some(_)` — register the lock path the pull was satisfied from. | Every project-mode pull that read a non-empty `ocx.lock`. |
| `ocx init` (Unit 7, when it writes a fresh `ocx.toml`) | `crates/ocx_lib/src/project/config.rs` (Unit 7's `init` writer) | After config write — register if a sibling `ocx.lock` exists (it will not, on `init`, but the call shape makes future code symmetric). | Defensive; safe no-op when `ocx.lock` is absent. |

**Centralisation rule:** there is exactly one site that calls `ProjectRegistry::register`. It lives at the bottom of `ProjectLock::save` (lines after the `Ok(())` returned by the spawn_blocking closure, before the outer `Ok(())`). All other "write triggers" listed above reach the registry transitively via `ProjectLock::save`. The pull/init paths, which do not call `save`, hit the registry through a small `ProjectRegistry::register_if_present(path)` helper that no-ops when the lock file does not exist on disk.

**Failure mode at the call site:** registration runs *after* the data file rename has succeeded. A registration failure is logged at WARN and swallowed — it never aborts `ocx lock`. The next `ocx lock` re-registers; missing one registration window has bounded blast radius (one stale GC false positive until next register).

### Read-Side Path

`ocx clean` (`crates/ocx_cli/src/command/clean.rs`) → `PackageManager::clean(...)` → `GarbageCollector::build(...)`:

```rust
// crates/ocx_lib/src/package_manager/tasks/clean.rs
pub async fn clean(&self, dry_run: bool, force: bool) -> crate::Result<CleanResult> {
    let profile = self.profile.snapshot();
    let project_roots = if force {
        Vec::new()           // --force: ignore the registry entirely
    } else {
        collect_project_roots(self.file_structure().root()).await?
    };
    let garbage_collector = GarbageCollector::build(
        self.file_structure(),
        &profile,
        &project_roots,
    ).await?;
    // ... rest unchanged ...
}
```

`collect_project_roots` is a new free function in the same module (per `subsystem-package-manager.md` "task module architecture" rule: helpers stay free, `impl PackageManager` stays minimal).

### Lazy Pruning Rules

A registry entry is dropped on the next `load_and_prune` if any of the following holds:

1. **`ocx_lock_path` does not exist on disk.** Primary case — projects deleted, moved, or renamed.
2. **`ocx_lock_path` is not a regular file.** Defence-in-depth against a directory or special file replacing the lock.
3. **`ocx_lock_path` parses as a `ProjectLock` but its sibling `ocx.toml` does not exist.** Catches the rare case where a developer commits an `ocx.lock` to a directory that no longer contains a project. Pre-checked at read time, cheap.

**NOT pruned:**

- Entries whose `ocx.lock` contains zero tools — a project may have just removed its last tool; it is still a project.
- Entries whose `ocx.lock` references digests not present in the local store — that's exactly the case GC must protect (the digest may be re-pulled tomorrow).
- Entries older than some TTL — the design has no TTL. Lazy file-existence checks are the only staleness signal.

Pruning runs *only* at read time (during `ocx clean`). Write-time `register` does not prune; it just updates the entry for the current path. Rationale: `register` is on the hot path of every `ocx lock` save; pruning would force a directory-existence check per entry, scaling linearly with registry size on every lock write.

### `ocx clean` UX

**`--force` flag added.** `Clean::dry_run` stays; `Clean::force` is new. `--force` short-circuits `collect_project_roots` (returns `Vec::new()`), so all registered projects are ignored as roots. Live install symlinks and profile content-mode roots are always honoured even with `--force` — `--force` exists to break the registry pin, not to GC actively-installed packages.

**Dry-run preview shape (plain).** Today's `Clean` API outputs a 2-column table (Type | Path). The new design extends the API to surface holding projects:

```
Type    Held By                     Path
object  /home/alice/dev/proj-a      /home/alice/.ocx/packages/.../sha256/ab/cdef.../
object                              /home/alice/.ocx/packages/.../sha256/12/3456.../
temp                                /home/alice/.ocx/temp/abc...
```

A blank `Held By` cell means the entry was unreferenced and will be (or was) collected. A populated cell means **the entry would have been collected without the registry**, and lists the project(s) holding it. The cell repeats when more than one project pins the same package, comma-separated. **Held By is not shown for `temp` entries** — they aren't governed by the registry.

In a non-dry-run invocation, the `Held By` column is omitted (empty by definition — held entries are never collected). The presence of the column is conditioned on `--dry-run`, not `--force`.

**JSON output (`--format json`).** Per `subsystem-cli-api.md` "single-table rule" + "report actual results", the `CleanEntry` type gains a `held_by: Vec<PathBuf>` field that serialises as an array (empty when no project holds the entry). The plain printer collapses the array to a comma-separated cell.

```jsonc
[
  {
    "kind": "object",
    "dry_run": true,
    "path": "/home/alice/.ocx/packages/.../sha256/ab/cdef.../",
    "held_by": ["/home/alice/dev/proj-a/ocx.lock"]
  },
  {
    "kind": "object",
    "dry_run": true,
    "path": "/home/alice/.ocx/packages/.../sha256/12/3456.../",
    "held_by": []
  }
]
```

The `held_by` field is added to the existing `CleanEntry` struct (`crates/ocx_cli/src/api/data/clean.rs`), preserving the single-`Vec<CleanEntry>` shape and the existing serializer that flattens to an array.

### Migration / First Run

The registry file is absent on first run after upgrade.

- **`ocx clean` on a fresh install:** `load_and_prune` returns an empty Vec, no writes occur, no project roots are added to the GC graph. The profile-snapshot source still contributes roots until Unit 2b lands. Net behaviour: identical to today's `ocx clean` on the first post-upgrade run.
- **`ocx lock` on a fresh install:** the post-save register call creates `$OCX_HOME/projects.json` (and `$OCX_HOME/.projects.lock`) for the first time. Subsequent `ocx clean` invocations honour the entry.
- **Pre-registry installed packages:** packages installed before the upgrade have `refs/symlinks/` entries (they were installed via `ocx install`) or appear in the profile snapshot. Both paths still protect them. Packages pulled-but-not-installed prior to the upgrade have neither protection — they were already at risk of GC under the old design and will be collected on the first `ocx clean` after upgrade. **This is acceptable** because the previous behaviour had the same outcome; we are not regressing it.

The transition window (post-Unit-6, pre-Unit-2b) keeps both registry roots and profile roots in `ReachabilityGraph::build()`. Unit 2b drops the profile arg.

### Failure Modes

| Failure | Detection | Action |
|---|---|---|
| Corrupt `projects.json` (parse error) | `serde_json::from_str` fails inside `load_and_prune` | Return `ProjectRegistryErrorKind::Corrupt(path, source)`. `ocx clean` propagates as `ConfigError` exit 78. **Do not** silently overwrite — that would mask the cause and lose state. |
| Permission denied writing under `OCX_HOME` (read-only home, restricted CI sandbox) | `tempfile::NamedTempFile::new_in` returns `EACCES` | `register` logs at WARN, returns success to the caller. Lock save is unaffected. The next normal-permissions invocation re-registers. |
| Two `ocx pull --project` invocations on different projects racing | Both attempt `try_exclusive` on `.projects.lock` | One acquires immediately, the other contends. The contended call blocks via a brief `lock_exclusive_with_timeout(2s)` retry, then registers. 2 s is generous; expected wait is sub-millisecond. |
| `try_exclusive` returns `Ok(None)` (contended) and the timeout retry also fails | Returns `Err(Locked)` | `register` logs at WARN, returns success. Same loss-window as a permission failure. |
| Symlink at `.projects.lock` (TOCTOU attack) | Reuse `open_sidecar_no_follow` from `crates/ocx_lib/src/project/lock.rs:469-491` | Treated identically — `O_NOFOLLOW` rejects, `Err(Io)` surfaces. |
| `ocx_lock_path` recorded in registry has been deleted | `load_and_prune` checks `tokio::fs::metadata(path).is_ok()` | Entry dropped, registry rewritten under the lock. Silent. |

### Testing Surface

Unit tests in `crates/ocx_lib/src/project/registry/tests.rs`:

- `register_creates_file_when_absent`
- `register_updates_last_seen_for_existing_entry`
- `register_idempotent_for_same_path`
- `register_atomic_under_concurrent_callers` (two `register` futures, assert both entries present afterwards)
- `load_and_prune_drops_entries_with_missing_lockfile`
- `load_and_prune_no_write_when_nothing_to_prune` (assert no rename happens)
- `corrupt_registry_returns_corrupt_error`
- `register_rejects_symlink_at_sentinel` (Unix-only, mirrors `load_exclusive_rejects_symlink_at_sidecar_path`)

Acceptance test in `test/tests/test_clean_project_backlinks.py`:

- Set up project A with an `ocx.lock` pinning package P; confirm registration.
- Switch `cwd` to project B (no `ocx.lock`, no relation to P); run `ocx clean --dry-run --format json`.
- Assert: P is **not** in the would-collect set; the JSON for P's package has `"held_by": ["<absolute>/proj-a/ocx.lock"]`.
- Run `ocx clean --force --dry-run --format json`; assert P **is** in the would-collect set, and `held_by` is empty (because `--force` suppressed the registry).
- Delete project A's `ocx.lock`; run `ocx clean --dry-run` again with project B active; assert P is now collectable and the registry no longer contains the proj-a entry (lazy prune fired).

## Implementation Plan

Plan-document scope; this ADR records architecture only. The Unit 6 plan-artifact (separate file) breaks this into ordered tasks and assigns workers. Sketch:

1. New module `crates/ocx_lib/src/project/registry.rs` + error type. Contract-first: stub the public API, write the unit tests above against `unimplemented!()`.
2. Wire `ProjectRegistry::register(path)` into the tail of `ProjectLock::save` (`crates/ocx_lib/src/project/lock.rs:383-466`).
3. Add `--force` flag to `crates/ocx_cli/src/command/clean.rs::Clean`.
4. Extend `CleanEntry` (`crates/ocx_cli/src/api/data/clean.rs`) with `held_by: Vec<PathBuf>`; extend `print_plain` to emit a third column under `--dry-run`.
5. Extend `ReachabilityGraph::build` signature (`reachability_graph.rs:45`) with `&[ProjectRootDigests]`; add new struct + module under `tasks/garbage_collection/project_roots.rs`.
6. Wire `collect_project_roots` (new free function in `tasks/clean.rs`) into `PackageManager::clean`.
7. Update doc-strings on `ocx clean` (CLI), `ocx_lib::project::registry::ProjectRegistry`, `subsystem-package-manager.md` task table, `subsystem-file-structure.md` GC Safety section, `website/src/docs/reference/command-line.md` (clean section), `website/src/docs/user-guide.md` (multi-project workflow paragraph).
8. Acceptance test in `test/tests/test_clean_project_backlinks.py`.

## Validation

- [ ] Plan-document acceptance test (project A holds → project B clean retains) passes.
- [ ] `register` whole-cycle benchmark: < 5 ms median at 100-entry registry, < 20 ms at 1000-entry.
- [ ] `cargo deny check` passes — no new third-party deps introduced.
- [ ] User-guide section reviewed by `worker-doc-writer` per Plan Status Protocol.
- [ ] No regression: existing `ocx clean` acceptance tests (object collection, temp cleanup) pass unchanged.

## Links

- Plan: [`/home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md`](file:///home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md) — Unit 6 (lines 103–110)
- Sibling sentinel ADR: [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md)
- GC roots architecture: [`adr_three_tier_cas_storage.md`](./adr_three_tier_cas_storage.md)
- GC source today: `crates/ocx_lib/src/package_manager/tasks/garbage_collection/reachability_graph.rs:45-66`
- Lock save (write-trigger host): `crates/ocx_lib/src/project/lock.rs:383-466`
- File-lock primitive: `crates/ocx_lib/src/file_lock.rs:17-22`
- Clean command (UX surface): `crates/ocx_cli/src/command/clean.rs`, `crates/ocx_cli/src/api/data/clean.rs`
- Subsystem rules consulted: `subsystem-package-manager.md`, `subsystem-file-structure.md`, `subsystem-cli.md`, `subsystem-cli-api.md`, `subsystem-cli-commands.md`
- Project memory:
  - `project_breaking_compat_next_version.md` — authorises clean-break schema
  - `project_metadata_first_pull.md` — context for why pull-without-install creates the GC false positive

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-29 | worker-architect (Opus) | Initial draft — Option 1 (`projects.json` + sibling lock) chosen. |
