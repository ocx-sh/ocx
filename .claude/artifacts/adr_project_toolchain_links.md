# ADR: Project-Local Toolchain Links — Stable Addressing for Composed Toolchains

## Metadata

- **Status**: Proposed
- **Date**: 2026-08-03
- **Deciders**: Owner + Principal Architect session (three design rounds; farm + scope-key variants rejected en route, recorded below)
- **GitHub Issues**: [#189](https://github.com/ocx-sh/ocx/issues/189) (stable links from a toolchain — delivered by this ADR), [#170](https://github.com/ocx-sh/ocx/issues/170) (PATH-surface coupling), [#193](https://github.com/ocx-sh/ocx/issues/193) (Dockerfile env staleness, partial), [#23](https://github.com/ocx-sh/ocx/issues/23) (relative symlinks — home store only, unaffected), [#177](https://github.com/ocx-sh/ocx/issues/177) (machine-readable bins — companion of the PATH break)
- **Tech Strategy**: ☑ aligned (Rust 2024, no new deps)
- **Domain Tags**: file-structure, package-manager, package (env resolver), windows
- **Supersedes**: draft `adr_toolchain_farm.md` (rejected pre-approval, preserved as Option B below)
- **Sibling**: `adr_live_env_reload.md` (phase 2 — consumes this ADR's stable-addressing decision)

## Context

Composed toolchain env today is **digest-pinned everywhere**: PATH entries point at `$OCX_HOME/packages/<registry>/<algo>/<hex>/…`, and every package-declared var templated on `${installPath}` — `JAVA_HOME`, `GOROOT`, SDK homes — bakes the same digest strings. Consequences:

1. **Every `ocx update` invalidates every emitted string.** Shells stay stale until re-export; running GUI apps (VS Code holding a toolchain env) stay stale until restart — no env mechanism can reach a running process.
2. **Dir-valued constants are the worst case.** An IDE reading `JAVA_HOME` dereferences a directory; after an update or re-select the old digest dir is semantically wrong, and *no execution boundary exists to intercept a path dereference* — shims/launchers cannot fix this class at all.
3. **Windows launchers are digest-bound by construction** (`.shim` sidecar bakes `pkg_root` — `exec.rs:31-33`), which is correct for their job (see consumer matrix) but leaves nothing stable-addressed.

Per-package stable links exist (`SymlinkStore` `candidates/` + `current`, `ocx package select`) but are **user-owned**: lock-pinned scopes must not consult `current` (`adr_global_toolchain_tier.md` D5), and two projects pinning different digests of one repo cannot share one link. [#189] asks for toolchain-level stable links; the question is where they live and what they cover.

## Decision Drivers

- **D1 — stability follows the lock**: stable strings whose *targets* repoint when the lock changes; `ocx update` must change zero emitted env bytes.
- **D2 — containment scoping**: project-dependent state lives in the project, not keyed into `$OCX_HOME` (scope = the directory you're in; no hash namespaces).
- **D3 — one mechanism per consumer class**: assign symlinks, launchers, and plain PATH dirs by how the artifact is *consumed*; expand no mechanism beyond its born role (entrypoints solve the diamond/env-encapsulation problem — they are not a shim distribution vehicle).
- **D4 — closure integrity**: a dependency's identity belongs to its root's resolution; dep paths must stay digest-pinned.
- **D5 — Windows without privilege**: directory links only (junctions); the frozen `.shim` contract stays untouched.
- **D6 — never block, heal before emit**: link maintenance inherits the never-block posture of activation paths; emitted paths are verified against the lock before emission.

## Industry Context & Research

Full survey: `research_shell_env_reconciler_and_launcher_farm.md`. Load-bearing:

- Stable-dir lineage: Homebrew `opt/<formula>` (the hardcode-me path whose target moves), SDKMAN `current`, update-alternatives, scoop's per-app `current` **junction** (privilege-free Windows dir links — the exact mechanism D5 needs).
- mise's model argument: real dirs on PATH beat shims (per-exec tax multiplies through build fan-out — asdf's 120–150ms lesson); shims only where the platform forces them. Here: nowhere — dir links suffice.
- volta proves per-exec re-resolution is viable but solves only the *execution* class; nothing in the survey addresses the *dereference* class (`JAVA_HOME`) with anything but a stable directory — because nothing else can.
- Ecosystem precedent for project-local trees: `node_modules/.bin`, `.venv/` (self-gitignored via `.gitignore` = `*`, the uv pattern), direnv `.direnv/`.
- Atomic repoint = `rename(2)`, never `unlink`+`symlink` (research pitfall 7).

## Considered Options

### Option A — No stable addressing; reconciler only (phase 2 alone)

| Pros | Cons |
|---|---|
| Zero new machinery | Running GUIs unsolved (binaries *and* `JAVA_HOME`); PATH + constants churn per update in every session |

### Option B — Farm store: one flattened per-scope bin dir under `$OCX_HOME/farm/<key>/` (rejected draft)

| Pros | Cons |
|---|---|
| Single PATH element per scope | Parallel store next to `SymlinkStore` (duplicate mechanism); flattening breaks per-package PATH ordering → needs a collision policy; per-**file** links → Windows privilege problem → frozen-`.shim`-contract amendment (OD-1); readdir materializer + `admitted_binaries` gap; hash-keyed project namespace in home; conflated entrypoints into a shim vehicle |

### Option C — Scope-keyed link kinds inside the home `SymlinkStore` (`symlinks/<reg>/<repo>/scopes/<key>`)

| Pros | Cons |
|---|---|
| One store, reuses link machinery | Project-dependent state rooted in `$OCX_HOME`; `<key>` hash namespace is redundant once you notice containment can scope; registry-repo keying mismatches composition (groups can hold two versions of one tool) |

### Option D — Project-local toolchain tree next to its `ocx.toml` **(chosen)**

| Pros | Cons |
|---|---|
| Scope = containment (no keys); lifecycle dies with the project; lock-entry keying mirrors composition incl. groups; junctions on Windows; `current`/D5 untouched | Links cross volumes into `$OCX_HOME` (absolute targets, healed); tree sits in attacker-controlled repos (heal-before-emit required); needs the `${installPath}` override map (the real cross-cutting cost) |

### Option E — Shims for everything (volta-style per-exec resolution)

| Pros | Cons |
|---|---|
| Per-exec env freshness for every binary | Cannot serve the dereference class at all (`JAVA_HOME` has no exec boundary) — links needed anyway; per-exec ocx spawn + compose cost on every binary (asdf lesson); duplicates what entrypoints already provide where it's actually needed |

## Decision Outcome

**Option D.** One rule: **the link tree lives next to the `ocx.toml` it materializes.**

### The consumer matrix (the decision's frame)

| Consumer | Example | Boundary | Mechanism |
|---|---|---|---|
| **Path dereference** — a process reads a dir path | `JAVA_HOME`, `GOROOT`, SDK homes | none — nothing executes | **toolchain link** (only possible answer) |
| **Execution needing composed env** | declared entrypoints | exec() through launcher | **entrypoint launcher** (exists, unchanged): re-composes per exec from `pkg_root` — closure-correct, auto-fresh on update, encapsulated. Publishers opt a binary in by declaring it an entrypoint. |
| **Execution, ambient env suffices** | plain `bin/` binaries | exec() through PATH | **linked bin dir** — zero per-exec cost; fresh binary at next spawn; env rides the shell |

Links carry *addressing*; launchers carry *env encapsulation*; neither expands into the other's role (D3).

### Layout

```
<project>/.ocx/toolchain/          # global toolchain: $OCX_HOME/toolchain/
├── .gitignore                     # contains "*" — written at materialization (uv/.venv pattern)
└── <group>/                       # "default" + named groups
    └── <entry>/                   # keyed by ocx.toml entry name (mirrors composition; groups
        │                          #   may hold two versions of one tool — distinct entries/groups)
        └── → $OCX_HOME/packages/<…digest root…>    (dir symlink; junction on Windows)
```

- **Lock entries only (D4).** Dependencies get no links: a dep's identity is a function of its root's resolution (`resolve.json`); a repointable dep path would let A run against a B it never resolved with. Dep paths — including `${deps.NAME.installPath}` — stay digest-pinned on purpose. Residual: dep *interface* bin dirs on the ambient PATH churn on update (shells heal next prompt via phase 2; a frozen GUI env keeps them until restart) — the escape hatch for tools that cannot tolerate that is declaring an entrypoint.
- **Emission per scope/group**: `…/toolchain/<group>/<entry>/content/bin` (+ that entry's `entrypoints/` dir) plus entry-owned constants resolved through the link (below). Stable strings; `ocx update` repoints targets and changes **zero emitted bytes** (D1).
- **Repoint = atomic `rename()`** of the link (temp name + rename; junctions likewise). Targets are **absolute** (junctions require it; cross-volume anyway) and healed on mismatch. [#23]'s relative-link work concerns the home store and is unaffected.
- **Self-gitignored**, created at materialization.

### `${installPath}` override map — the load-bearing change

`EnvResolver`/`TemplateResolver` (and `env/dep_context.rs`) today template every var against the digest store path. Scope emission gains an **override map (lock entry → materialized link path)**: an entry's own vars — `JAVA_HOME=…/toolchain/default/java/content` — resolve against the link; `${deps.*.installPath}` keeps digest paths (D4). One-shot paths (`ocx run`, `--ci`, `launcher exec` self-view) keep store paths — launchers stay digest-bound by design. This touches `env/resolver.rs`, `env/dep_context.rs`, composer plumbing, and every `resolve_env_*` caller: **the largest work package of phase 1.** Payoff: after `ocx update`, the exported env is byte-identical — a running IDE's `JAVA_HOME` dereferences the new JDK through the link. Re-select convenience for the Java class, solved by the only mechanism that can.

### Freshness, integrity, failure posture

- Materialized/repointed by resolution-mutating commands (`add`, `remove`, `lock`, `update`, `pull` into scope). No shell-side trigger; every compose-consuming ocx invocation **heals before emitting**: per link, `readlink` compared against the lock-pinned target, repointed on mismatch (one syscall per entry; under `lock_scoped`).
- **Heal-before-emit is also the security boundary**: the tree sits inside attacker-controlled repos — a clone can ship pre-made `.ocx/toolchain/` links pointing anywhere. No path is emitted before its link verifies against the lock, and phase 2's consent gate means an untouched clone is never emitted at all.
- Write failure (read-only checkout, foreign-owned dir, lock timeout): **skip-and-continue** — fall back to digest paths for that run, never block or fail emission (D6). CI typically lands here and loses nothing (ephemeral env).
- Crash mid-materialization: per-entry atomic renames keep each link individually valid; heal-before-emit re-converges — no marker files, no lock-wedge class (research pitfall 5).

### GC

**No new GC surface.** GC roots are already "digests pinned by any *registered* project's lock" plus the global lock (`$OCX_HOME/projects/` ledger — `adr_project_gc_symlink_ledger.md`, implemented in `project/registry.rs` with liveness probing). Every link target is by construction such a pinned digest. GC never walks project trees (outside `$OCX_HOME`); a stale link is a heal case, not a leak. One invariant to enforce: **materialization registers the project** in the ledger (same command, same transaction posture).

### What stays untouched

`candidates/` + `current` + `ocx package select` (user-owned selection; D5 of `adr_global_toolchain_tier.md` intact — no lock-driven path consults `current`); entrypoint semantics and the frozen `.shim`/launcher contract; package metadata and every wire format. A future project-facing `select` convenience **edits the lock** and lets materialization follow — links always follow the lock, selection is a lock operation, no second mechanism.

### PATH surface (consumed by the sibling ADR)

Store-rooted emitted paths become toolchain-link paths — N stable per-package dirs per scope (per-package PATH ordering and today's shadowing semantics preserved verbatim; no collision policy needed). Project/group `[env]` PATH additions are unaffected. Consumer-visible break in `ocx env` / `.envrc` / `--ci` output — pre-1.0, changelog = commit subject; [#177] (`bins` real-path listing) rides along as the machine-consumer companion. `ocx direnv export` emits the same link paths under its existing semantics (phase 1 works with direnv still installed; `.envrc` watch set unchanged).

### Quantified Impact

| Metric | Before | After |
|---|---|---|
| Emitted env bytes changed by `ocx update` (PATH *and* constants) | all digest strings | **0** |
| `JAVA_HOME`-class dereference in a running IDE after update/re-select | stale digest dir | correct through link |
| Binary freshness in running shells / GUIs | never until re-export / restart | next spawn |
| Per-exec overhead | 0 plain / launcher for entrypoints | unchanged (links add none) |
| Windows privilege requirement | n/a | none (junctions) |
| Dockerfile ([#193]) | digest `ENV` paths, stale | stable `ENV` paths, repoint under image rebuild |

### Consequences

**Positive**: [#189] delivered with containment scoping; phase-2 reconciler's PATH/constant work becomes a no-op in the common case (only decl-*shape* changes remain); Java-class reselect convenience; Windows needs no contract change.

**Negative / Risks**:
- `${installPath}` override map is cross-cutting (resolver, dep_context, composer, all `resolve_env_*` callers) — the phase's real cost.
- Compose-consuming commands become guarded writers (heal) — read-only/concurrency posture must be tested.
- Mixed stability is by design (entries stable, deps pinned) — must be documented so users understand *why* `${deps.*}` paths churn.
- Junction fragility class on Windows (scoop's SSH/KB precedent) — accepted, same exposure as every junction-based tool.

### How Would We Reverse This?

Revert emission to digest paths (resolver override map off — single seam), delete `.ocx/toolchain/` trees (self-contained, gitignored) and `$OCX_HOME/toolchain/`. No wire format, metadata, or lock schema touched; `candidates`/`current` were never involved. Reversal is a local change + changelog subject.

## Technical Details (component placement)

- Link mechanics (kind, atomic replace, readlink-verify) extracted **root-parametric** from `SymlinkStore` — home store and toolchain trees become two instances of one facility (no parallel implementation).
- Toolchain-tree materializer in `package_manager/` (input: resolved lock entries per group); project-rooted trees are managed via the `project/` module — **not** a `FileStructure` store (stores are home-rooted; only the global tree lives under `$OCX_HOME`).
- Resolver override map: `env/resolver.rs`, `env/dep_context.rs`, `composer.rs`, `resolve_env_*` signatures.
- Registration invariant: materializer ensures the `projects/` ledger entry.
- Docs surfaces: storage-layout page (`.ocx/toolchain/` + `$OCX_HOME/toolchain/`), env-composition reference (override map, mixed-stability rationale), `subsystem-file-structure.md` + `subsystem-package-manager.md` rules.

## Implementation Plan

Phase 1 of the two-phase initiative (`adr_live_env_reload.md` is phase 2). Plan via `/swarm-plan` after ADR approval. Work packages: root-parametric link facility; resolver override map (largest); materializer + heal-before-emit + registration; junction backend; emission switch + [#177] companion; docs. Acceptance tests are plain pytest — no shell/pty surface in this phase.

## Validation

- Repoint demonstrated both states (old digest resolved before update, new after) — unchecked-green rule.
- Byte-identical `ocx env` output across an update (PATH + constants).
- Poisoned committed link: pre-seeded `.ocx/toolchain/` pointing elsewhere → healed before first emission (red: emission without heal exposes the foreign target).
- Read-only checkout: heal skipped, digest-path fallback emitted, exit 0.
- `${deps.*.installPath}` remains digest-pinned while entry vars resolve through links (mixed-stability contract test).
- Windows: junction create/repoint/read-through; no privilege; `.shim` files byte-unchanged.

## Links

- Research: `research_shell_env_reconciler_and_launcher_farm.md`
- Sibling: `adr_live_env_reload.md`
- Constraining: `adr_global_toolchain_tier.md` (D5), `adr_project_gc_symlink_ledger.md` (GC ledger), `adr_windows_exe_shim.md` (untouched contract), `handshake_toolchain_cli.md`
- Issues: [#189](https://github.com/ocx-sh/ocx/issues/189), [#170](https://github.com/ocx-sh/ocx/issues/170), [#193](https://github.com/ocx-sh/ocx/issues/193), [#23](https://github.com/ocx-sh/ocx/issues/23), [#177](https://github.com/ocx-sh/ocx/issues/177)

## Changelog

| Date | Change |
|---|---|
| 2026-08-02 | Farm-store draft (`adr_toolchain_farm.md`) + adversarial review round 1 |
| 2026-08-03 | Farm rejected (owner review): parallel store, entrypoint conflation, per-file-link Windows problems. Scope-keyed home-store variant rejected next (containment beats keys). Rewritten as project-local toolchain tree: consumer matrix, lock-entry-only links, `${installPath}` override map, heal-before-emit, junctions |
