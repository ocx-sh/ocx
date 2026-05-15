# ADR: Project GC Ledger as a Symlink Store

## Metadata

**Status:** Accepted
**Date:** 2026-05-15
**Deciders:** Architect (planning agent), Michael Herwig
**Beads Issue:** N/A
**Related Plan:** [`okay-we-are-currently-lexical-shannon.md`](file:///home/mherwig/.claude/plans/okay-we-are-currently-lexical-shannon.md)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 + Tokio, no new external dep (drops `serde_json`/`tempfile`/`FileLock` usage at this site, reuses existing `symlink` primitives).
**Domain Tags:** architecture | storage | dx
**Supersedes:** [`adr_clean_project_backlinks.md`](./adr_clean_project_backlinks.md)
**Superseded By:** N/A

## Context

`adr_clean_project_backlinks.md` (Accepted 2026-04-29, one-way-door medium)
introduced a per-user JSON ledger — `$OCX_HOME/projects.json` plus a sibling
advisory-lock sentinel `$OCX_HOME/.projects.lock` — recording every project
whose `ocx.lock` pins packages on this machine, so `ocx clean` retains packages
held by *other* projects' lockfiles. It deliberately chose a single JSON
document (its Option 1) over a per-project symlink directory (its Option 2),
citing "doubling the symlink graph", inode density on dense developer machines,
and cargo/uv single-file precedent.

During the project-toolchain hardening phase this ledger was flagged as the one
storage surface inconsistent with the rest of OCX. Every other GC-liveness
relationship in the store is a **symlink**: install candidates/current under
`symlinks/`, each paired with a `refs/symlinks/` back-reference, swept by a
single BFS over `refs/{symlinks,deps,layers,blobs}` with `broken_refs()` as the
staleness probe (`adr_three_tier_cas_storage.md`,
`crates/ocx_lib/src/reference_manager.rs`). `projects.json` is a parallel,
bespoke schema (custom JSON doc + version field + advisory-lock sentinel +
whole-file rewrite + lazy-prune-and-rewrite) that a maintainer must reason about
separately from the symlink model that governs everything else.

Two concrete costs of the JSON model surfaced in use:

1. **Style/maintenance split.** `registry.rs` carries a serde schema,
   `RegistryDoc`/`EntryOnDisk`, a `SCHEMA_VERSION` gate, a `FileLock` sentinel
   protocol, atomic tempfile+rename, and a parallel `JoinSet` lazy-prune — all
   to express "this project is a GC root", a relationship the store already
   expresses elsewhere with one symlink.
2. **Noisy benign churn.** A departed project (the developer deleted the
   directory) is the *common* case, not an anomaly. Under the JSON model the
   register path canonicalises the current project's paths on every
   `ocx.lock`/`ocx.toml` mutation; when those resolve through stale state it
   emits `log::warn!` lines ("Project registry: … (non-fatal …)"). Warnings for
   expected churn train the user to ignore warnings.

The Option-2 rejection in the prior ADR was specifically against a *per-project
directory tree* (`$OCX_HOME/projects/<hash>/lockfile-link` + `meta.json`). It
did not evaluate a **flat one-symlink-per-project store** integrated into the
existing symlink-liveness model — which is what industry CAS package managers
actually ship (pnpm) and what OCX's own architecture already standardises on.

### Why this is a One-Way Door Medium

Same class as the ADR it supersedes: the on-disk shape under `$OCX_HOME`
becomes durable contract. `$OCX_HOME/projects/<16hex> → <project dir>` must be
agreed by any binary running against that home. Reversal cost is bounded (one
release, one diff at the register/read sites, one directory to delete on user
machines). `project_breaking_compat_next_version.md` authorises a clean break
before stabilisation — no migration shim.

## Decision Drivers

- **One liveness model, not two.** GC roots should be expressed the same way
  everywhere: a symlink whose existence-and-resolvability *is* the liveness
  signal. A maintainer who understands `refs/symlinks/` should understand the
  project ledger without learning a second schema.
- **Silent self-heal for benign churn.** A vanished project must never WARN and
  must never block an `ocx.toml`/`ocx.lock` mutation. It is pruned at the next
  `ocx clean` with a debug message. This is the documented industry convention
  (Nix indirect gcroots, pnpm `store/projects`, cargo cache) — see Industry
  Context.
- **No new dependencies, fewer primitives.** Reuse `crates/ocx_lib/src/symlink.rs`
  (`create`/`update`/`remove`/`is_link`, junction-aware on Windows) and the
  `ReferenceManager::name_for_path` hashing scheme. Drop the `serde_json` schema,
  the `FileLock` sentinel, and the tempfile+rename dance at this site.
- **No global hot lock.** Each project owns its own symlink; register is a
  crash-safe atomic per-project replace (temp link + same-dir `rename(2)`,
  via a `symlink::replace_atomic` helper — NOT remove-then-create). Two
  concurrent `ocx pull` runs on
  different projects never contend (the JSON model serialised both on
  `.projects.lock`).
- **Preserve the multi-project GC contract.** The acceptance behaviour from the
  superseded ADR is unchanged: a package pinned by project A is retained when
  `ocx clean` runs with project B active; `--force` suppresses project roots;
  dry-run attributes retained packages to the holding project.
- **Browsability is a feature.** A symlink target of the project *directory*
  means `ls -l $OCX_HOME/projects/` and editor file-trees resolve straight into
  the project — the ledger doubles as a "projects that pin tools here" index a
  human can click through.

## Industry Context & Research

Research artifact basis: `worker-researcher` survey (2026-05-15), persisted
context below; cross-tool comparison of symlink-as-GC-root ledgers.

- **Nix gcroots / indirect roots** (`/nix/var/nix/gcroots/auto/<hash> →
  project/result → /nix/store/…`). The Nix manual states verbatim that when the
  project-side path is removed "the GC root in the auto directory becomes a
  dangling symlink and **will be ignored by the collector**" — silent, no
  warning, documented behaviour. This is the canonical precedent for
  symlink-liveness with silent stale handling.
  (`nix.dev/manual/nix/2.25/package-management/garbage-collector-roots`,
  `mankier.com/1/nix-store`.)
- **pnpm** stores its content-addressable packages under
  `~/.local/share/pnpm/store/vN/` and tracks project liveness via **one symlink
  per project** under `{storeDir}/vN/projects/`. `pnpm store prune` mark-sweeps
  from live projects; a deleted project becomes a dangling symlink handled as a
  silent `ENOENT` cleanup. This is exactly the flat one-symlink-per-project
  store this ADR adopts. (`pnpm.io/cli/store`.)
- **lorri** keeps per-project gcroot symlinks named by the project's nix-file
  path hash; removing the cache dangles them; next GC sweeps silently.
- **cargo** uses a SQLite last-use tracker and silently age-prunes; never warns
  on a vanished consumer.

**Key insight:** the prior ADR's Option-2 rejection ("doubling the symlink
graph", inode density, single-file precedent) argued against a *directory tree*.
The actual industry shape for a CAS package manager is a **flat
one-symlink-per-project store** (pnpm). It has one inode per project (not a
subtree), is trivially browsable, and the staleness model is *identical to the
one OCX already runs* for `refs/symlinks/`. The single-file precedent the prior
ADR cited (cargo `.crates.toml`, uv) is from tools that do **not** run an
aggressive content-addressed GC keyed on symlink liveness; OCX does, so pnpm/Nix
are the closer prior art. The unanimous cross-tool convention — stale reference
is collected silently, never warned — directly validates the user's "don't WARN
on a departed project" requirement as the standard, not a local preference.

## Considered Options

### Option 1 — Flat symlink store `$OCX_HOME/projects/<hash> → project dir` (chosen)

One symlink per registered project, name = first 16 hex of
`SHA-256(canonical_abs_project_dir)` (reuses `ReferenceManager::name_for_path`
convention), target = the **project directory** containing `ocx.lock`.

| Pros | Cons |
|------|------|
| One liveness model: same symlink+broken-link staleness as `refs/symlinks/`. One mental model for the whole store. | Adds a second symlink graph (the prior ADR's stated con). Accepted: it is *flat* (one inode/project, not a tree) and shares the existing staleness model, so it is one more instance of a known pattern, not a new pattern. |
| No JSON schema, no `SCHEMA_VERSION`, no `.projects.lock` sentinel, no whole-file rewrite, no lazy-prune-and-rewrite. Net code deletion. | Symlink target is an absolute path outside `$OCX_HOME` (like Nix indirect roots). Must bypass the `validate_target` containment helper (which is for in-`refs/` links) and use the lower-level `symlink::create/update` directly. Documented invariant. |
| Per-project atomic `symlink::update` — no global lock, no cross-project contention. | Browsable target = project dir assumes the lock is the canonical sibling `ocx.lock`. Confirmed: `lock_path_for(config)` always yields `<dir>/ocx.lock`, even under `--project=<custom>.toml`. Closes the plan's open item. |
| Browsable: `ls -l $OCX_HOME/projects/` / editor trees resolve into the project. | Stale detection is two cheap probes (`is_link` + `try_exists(<target>/ocx.lock)`) instead of one JSON existence check. Sub-millisecond; runs only at `ocx clean`. |
| Matches pnpm exactly; staleness matches Nix indirect roots — proven shape. | |

### Option 2 — Keep `projects.json` (status quo, superseded ADR)

Single JSON doc + advisory-lock sentinel.

| Pros | Cons |
|------|------|
| No migration; already implemented and tested. | The sole storage surface inconsistent with OCX's symlink-liveness model. |
| `cat projects.json \| jq` is the whole query. | Bespoke schema + lock protocol + lazy-prune to express a relationship the store already expresses with a symlink. |
| | Register path produces WARN noise on benign departed-project churn. |
| | Global hot lock serialises concurrent multi-project `ocx pull`. |

### Option 3 — Two-level Nix-style indirect chain (`projects/<hash> → project/.ocx-root → …`)

A ledger symlink pointing at a *project-side* symlink the user could `cd` via.

| Pros | Cons |
|------|------|
| Closest to Nix `auto` indirect roots. | Requires writing a new symlink *into the user's project tree* (`.ocx-root`), polluting the working copy and the VCS status — OCX deliberately keeps project-side artefacts to `ocx.toml`/`ocx.lock`/`.envrc`. |
| | Two links to keep consistent; failure modes multiply. The directory target (Option 1) already gives the "jump into the project" browsability without a project-side file. |

### Option 4 — Per-project directory `$OCX_HOME/projects/<hash>/…` (the prior ADR's rejected Option 2)

| Pros | Cons |
|------|------|
| Self-healing per directory. | Inode density on dense machines (200+ worktrees) — the prior ADR's decisive con; still valid. A flat store (Option 1) avoids it entirely. |
| | Reconstructing "which projects" = directory-tree walk vs a flat readdir. |

## Decision Outcome

**Chosen Option:** **Option 1 — flat symlink store at `$OCX_HOME/projects/`,
one `sha256(canonical_abs_project_dir)[:16]` symlink per project targeting the
project directory.**

### Rationale

- Collapses two liveness models into one. The project ledger becomes "another
  symlink graph with the same staleness rule as `refs/symlinks/`", not a
  bespoke schema. Reviewer cognitive load drops; net code is deleted.
- Adopts the proven pnpm shape (flat one-symlink-per-project store) and the
  proven Nix staleness rule (dangling = silently collected). The prior ADR's
  Option-2 rejection does not apply: it argued against a *directory tree* with
  inode-density and single-file-precedent reasoning drawn from tools that do
  not run a symlink-keyed CAS GC. OCX does; pnpm/Nix are the right prior art.
- Resolves the plan's open item: targeting the project *directory* is sound
  because `lock_path_for(config_path)` is invariantly `<dir>/ocx.lock` (proven
  by `lock_path_for_always_produces_ocx_lock_in_config_dir`), so GC reads
  `<target>/ocx.lock` regardless of a custom `--project` config basename.
- Eliminates the WARN-on-benign-churn class: register touches only the current
  project's own symlink (no JSON merge, no cross-entry revalidation); a
  departed project is detected only at `ocx clean` and pruned at **debug**,
  never WARN, never blocking a mutation. Matches the unanimous industry
  convention.

### Quantified Impact

| Metric | Before (JSON) | After (symlink store) | Notes |
|--------|---------------|-----------------------|-------|
| On-disk surface for project bookkeeping | `projects.json` + `.projects.lock` | `projects/` dir, one symlink per project | No sentinel; no document. |
| New deps | 0 | 0 | Drops the `serde_json`/`FileLock` *usage* at this site. |
| Lock contention granularity | per-machine `.projects.lock` on every register | none (per-project atomic symlink) | Concurrent multi-project `ocx pull` never contend. |
| WARN lines on benign departed project | ≥1 per mutation | 0 (debug only, at clean) | Direct user-reported defect fixed. |
| `registry.rs` schema/lock/prune code | `RegistryDoc`/`EntryOnDisk`/`SCHEMA_VERSION`/sentinel/tempfile/JoinSet-prune | `symlink::{replace_atomic,remove,is_link}` calls + flat readdir | Net deletion. |
| Multi-project GC acceptance contract | held | **held, unchanged** | `test_clean_project_backlinks.py` passes unchanged in contract. |

### Consequences

**Positive:**
- Single storage/liveness model across the entire store.
- `$OCX_HOME/projects/` is a human-browsable index of tool-pinning projects.
- Benign departed-project churn is silent and self-healing.
- No global lock; better concurrency for multi-project workflows.

**Negative:**
- A second symlink graph exists (accepted cost; it is flat and shares the
  existing staleness model, so it is one more instance of a known pattern).
- The `ProjectRegistry` public API changes shape (see Technical Details);
  acceptable pre-stable.

**Risks:**
- **Symlink-hostile filesystems / Windows privilege.** Mitigated: `symlink.rs`
  already falls back to NTFS junctions for directory links without
  `SeCreateSymbolicLinkPrivilege`; the target here is always a directory.
- **Absolute external target.** The ledger symlink points outside `$OCX_HOME`
  (the project dir). This is intentional (Nix indirect-root shape). It must use
  `symlink::create/update` directly and **not** route through `validate_target`
  (whose containment policy is for `refs/`-internal links). Documented invariant
  + unit test.
- **Self-register failure is WARN, not debug (review correction, ARCH-3 —
  silent-data-loss class).** Earlier text said "any error → debug". That is
  over-broad. Split by class: a failure to create/update **the project's own**
  link (store dir unwritable, ENOSPC) means the user's just-pinned packages
  will not be a GC root and can be silently collected by a later non-`--force`
  `ocx clean` — this is *not* benign churn and `feedback_no_warn_on_common_benign`
  does **not** cover it. It is logged at **WARN** (still non-fatal: never
  blocks the mutation, never propagates). Only the *departed-other-project*
  prune in `live_projects` is debug-level (the common benign case). The
  write-side non-fatal stance is unchanged from the superseded JSON model —
  this ADR does not claim it eliminated; it is accepted because a persistently
  unwritable `$OCX_HOME/projects/` implies `$OCX_HOME` is largely unusable and
  `--force` is opt-in, but the WARN ensures it is not silent.
- **64-bit `name_for_path` collision blast-radius (ARCH-1a, accepted risk).**
  Reusing the 16-hex (64-bit) `ReferenceManager::name_for_path` scheme is
  sound (probability ≈10⁻¹³ at realistic worktree counts), but the blast
  radius differs from the precedent: a `refs/symlinks/` collision is a
  recoverable local mislink; a `projects/` collision drops one of two live
  projects from the GC root set → silent collection of its pinned packages.
  Accepted at realistic scale; stated here so the reuse is a conscious
  decision, not precedent inertia.
- **Corrupt-state failure mode removed, not relocated.** The superseded ADR
  exited `ocx clean` with `ConfigError` (78) on a corrupt `projects.json` to
  avoid silently collecting everything. There is no parse surface to corrupt
  here: a bad/dangling symlink is simply pruned. This is a deliberate
  *elimination* of a failure mode (no JSON ⇒ nothing to corrupt), explicitly
  called out so the behavioural delta is not mistaken for a regression. A
  filesystem I/O error while reading `projects/` is still non-fatal (debug,
  proceed with the symlink/`current` roots), matching the superseded ADR's
  "registry unreadable ⇒ non-fatal" stance.

### How Would We Reverse This?

One-Way-Door Medium. Reversal: pick a new shape, ship it next version, add one
CHANGELOG + user-guide line ("`$OCX_HOME/projects/` is no longer used; safe to
delete"), no compat shim (per `project_breaking_compat_next_version.md`).
Bounded: one release, one diff at register/read sites, one directory deleted on
user machines.

## Technical Details

### On-disk shape

```
$OCX_HOME/
  projects/
    1f3a9c0b5e7d2a84   ->  /home/alice/dev/proj-a      (symlink, target = project dir)
    9b2c4e6a8d0f1357   ->  /home/alice/dev/proj-b
```

- Name = `hex(sha256(canonical_abs_project_dir.as_os_str().as_encoded_bytes())[..8])`
  — identical scheme to `ReferenceManager::name_for_path`. Single
  source-of-truth helper; do not reinvent.
- Target = canonicalised absolute **project directory** (the directory
  containing `ocx.lock`). Never the lock file, never the config file.
- No `projects.json`, no `.projects.lock`, no `last_seen`, no `config_path`,
  no `schema_version`.

### No self-link invariant

`register` is a no-op when `project_dir` **is the same directory as**
`$OCX_HOME`. The global toolchain (`$OCX_HOME/ocx.toml`, ADR
`adr_global_toolchain_tier.md`) lives in `$OCX_HOME`; a
`$OCX_HOME/projects/<hash> → $OCX_HOME` link is recursive and awkward, and the
global tools are already GC roots via their `current` install symlinks. The
ledger never points at its own home.

**Identity test (review correction, ARCH-1b — silent-data-loss class).** The
identity check MUST be device+inode identity (Unix `dev`/`ino`; Windows
canonicalized-handle equivalence), **not** canonical-path byte equality.
`tokio::fs::canonicalize` does not case-fold on case-insensitive/normalizing
filesystems (macOS APFS default, Windows — both first-class platforms per
`product-context.md`). Byte equality there can return *false* when the paths
denote the same directory, allowing the forbidden self-link the invariant
exists to prevent. The earlier draft's "`canonical(project_dir) ==
canonical($OCX_HOME)`" framing was optimistic; the binding rule is
same-directory identity, not string equality.

### Revised `ProjectRegistry` API

```rust
// crates/ocx_lib/src/project/registry.rs
pub struct ProjectRegistry { projects_dir: PathBuf } // $OCX_HOME/projects

impl ProjectRegistry {
    pub fn new(ocx_home: &Path) -> Self;

    /// Idempotent. No-op when project_dir is the same directory as
    /// $OCX_HOME (dev/inode identity — see No-self-link invariant).
    /// Crash-safe atomic replace: temp link in projects/ + same-dir
    /// rename(2) (NOT remove-then-create). Non-fatal — never blocks the
    /// caller's `ocx.toml`/`ocx.lock` mutation, never propagates — but
    /// failure to create THIS project's own link logs at **WARN**
    /// (silent-data-loss class; not benign churn). Returns Ok(()).
    pub async fn register(&self, project_dir: &Path) -> Result<(), Error>;

    /// readdir `projects/` (skip `.tmp-*` staging names); for each link:
    /// if broken OR `<target>/ocx.lock` absent → **re-stat immediately
    /// before removal** and only `symlink::remove` if it still fails with
    /// the same observed target (TOCTOU guard: a concurrent `register`
    /// re-pointing the same hash must not be deleted — if it now resolves,
    /// treat as live), DEBUG log; else yield the resolved project dir.
    /// Deterministic order (sort by link name). Filesystem I/O error →
    /// DEBUG, return what was collected (non-fatal, matches superseded ADR).
    pub async fn live_projects(&self) -> Result<Vec<PathBuf>, Error>;
}
```

`ProjectRegistryErrorKind` shrinks to `Io(std::io::Error)` only —
`Corrupt`/`UnknownVersion`/`Locked` are deleted with the JSON doc and sentinel.

### Register call sites (placement unchanged)

Tail of `ProjectLock::save` and `MutationGuard::commit`, exactly where the JSON
`register` was called. Failure to create THIS project's own
link (incl. pre-register canonicalize failure of its own paths) logs at
**`log::warn!`** (non-fatal — never blocks the save/commit, never propagates —
but it is the silent-data-loss class, not benign churn; see §Risks). Only the
departed-*other*-project prune in `live_projects` is `log::debug!`. Call now
takes the **project directory** (`config_path.parent()` canonicalised), not the
lock+config path pair (safe per the `lock_path_for` invariant —
`lock_path_for_always_produces_ocx_lock_in_config_dir`).

### `ocx clean` read side (contract preserved)

`collect_project_roots(ocx_home, file_structure)` calls
`ProjectRegistry::live_projects()` instead of `load_and_prune()`. For each live
project dir it reads `<dir>/ocx.lock` via `ProjectLock::from_path` and resolves
pinned identifiers through `resolve_to_package_digests` **unchanged**
(index→child manifest parse is orthogonal storage; not touched). `--force`
still short-circuits to empty roots. `roots_attribution` / `held_by` now keys on
the project directory path (still a clickable absolute path in `--dry-run`).
`ProjectRootDigests.ocx_lock_path` is retained for diagnostic continuity
(derived as `<project_dir>/ocx.lock`).

### Legacy file handling

No migration code. `projects.json` / `.projects.lock`, if present from a prior
version, are ignored. `ocx clean` opportunistically `remove`s them once if it
encounters them (single DEBUG line). One CHANGELOG entry + one user-guide line:
"`$OCX_HOME/projects.json` and `$OCX_HOME/.projects.lock` are obsolete and safe
to delete."

## Validation

- [ ] `test/tests/test_clean_project_backlinks.py` passes **unchanged in
      contract** (A pins → B `ocx clean` retains; `--force` suppresses;
      delete A's lock ⇒ collectable + ledger self-pruned).
- [ ] Unit tests in `project/registry`: idempotent re-register; broken-link
      prune; `<target>/ocx.lock`-absent prune; concurrent register of two
      projects (both links present, no contention); no self-link when
      project_dir == `$OCX_HOME`; symlink target is the project dir not the
      lock file.
- [ ] No WARN emitted on `ocx add`/`remove`/`lock` when a *different*
      registered project has departed; the stale link is pruned at next
      `ocx clean` with a DEBUG line.
- [ ] `ls -l $OCX_HOME/projects/` shows symlinks resolving into project dirs.
- [ ] `cargo deny check` — no new deps.
- [ ] `task rust:verify` for `crates/ocx_lib`; `task --force verify` final gate.

## Links

- Supersedes: [`adr_clean_project_backlinks.md`](./adr_clean_project_backlinks.md)
- Sibling: [`adr_global_toolchain_tier.md`](./adr_global_toolchain_tier.md) (no-self-link rationale)
- GC roots architecture: [`adr_three_tier_cas_storage.md`](./adr_three_tier_cas_storage.md)
- Reuse: `crates/ocx_lib/src/symlink.rs`, `crates/ocx_lib/src/reference_manager.rs` (`name_for_path`)
- Register hosts: `crates/ocx_lib/src/project/lock.rs` (`ProjectLock::save`), `crates/ocx_lib/src/project/mutation.rs` (`MutationGuard::commit`)
- Read site: `crates/ocx_lib/src/package_manager/tasks/clean.rs` (`collect_project_roots`)
- Plan: [`okay-we-are-currently-lexical-shannon.md`](file:///home/mherwig/.claude/plans/okay-we-are-currently-lexical-shannon.md)
- Project memory: `project_breaking_compat_next_version.md`, `feedback_no_warn_on_common_benign.md`, `feedback_extend_dont_duplicate.md`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-15 | Architect (Opus) | Initial — supersede JSON `projects.json` ledger with a flat symlink store; silent debug prune; no-self-link; multi-project contract preserved. |
