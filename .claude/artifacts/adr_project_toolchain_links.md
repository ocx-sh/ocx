# ADR: Project-Local Toolchain Links — Stable Addressing for Composed Toolchains

## Metadata

- **Status**: Proposed
- **Date**: 2026-08-03 (rewritten 2026-09-02 against the post-reconciler tree; review rounds 2–3)
- **Deciders**: Owner + Principal Architect session (farm and home-keyed variants rejected en route, recorded below)
- **GitHub Issues**: [#189](https://github.com/ocx-sh/ocx/issues/189) (stable links from a toolchain — delivered by this ADR), [#193](https://github.com/ocx-sh/ocx/issues/193) (Dockerfile env staleness — global tier only; its aggregated-bin-dir question stays open)
- **Tech Strategy**: ☑ aligned (Rust 2024, no new deps)
- **Domain Tags**: file-structure, package-manager, package (env resolver), config, windows
- **Supersedes**: draft `adr_toolchain_farm.md` (rejected pre-approval, preserved as Option B)
- **Related, independent track**: `adr_shell_env_overhaul.md` — the shipped per-prompt reconciler. Neither ADR sequences the other; that record already reserves this one's hook ("if #189 lands, its `.ocx/toolchain/` tree joins the prefix set additively" — the `owned_prefixes` parameter of `shell::reconcile::plan`). Contract under *Reconciler interplay*.

## Context

Composed toolchain env is **digest-pinned everywhere**: PATH entries point at `$OCX_HOME/packages/<registry>/<algo>/<hex>/…`, deferred tools' shim slots at `$OCX_HOME/shims/…/bin`, and every package-declared var resolved against the install path — `JAVA_HOME`, `GOROOT`, SDK homes — bakes the same digest strings. Consequences:

1. **Every `ocx update` changes every emitted string.** The shipped reconciler heals shells on the next prompt by retiring the old prefix-owned elements and applying the new ones — correct, but it is work on every digest bump in every session, and it cannot reach a process that is not a shell.
2. **Running GUI processes stay stale until restart.** VS Code holding a toolchain env spawns old binaries forever; no env mechanism reaches a running process's environment.
3. **Dir-valued constants are the unfixable class for shims.** An IDE reading `JAVA_HOME` *dereferences a directory*; no execution boundary exists to intercept that — only a stable directory whose content is the selection can serve it.
4. Windows launchers are digest-bound by construction (the `.shim` sidecar written by `launcher/shim.rs` bakes `pkg_root`; `launcher exec` receives it) — correct for their job (consumer matrix), but nothing is stable-addressed.

Per-package stable links exist (`SymlinkStore` `candidates/` + `current`, `ocx package select`) but are **user-owned**: lock-pinned scopes must not consult `current` (`adr_global_toolchain_tier.md` D5), and two projects pinning different digests of one repo cannot share one link. [#189] asks for toolchain-level stable links; the questions are where they live, what they cover, and how a user opts out.

## Decision Drivers

- **D1 — stability follows the lock**: stable strings whose *targets* repoint when the lock changes; a version bump changes zero emitted env bytes for link-resolved values.
- **D2 — containment scoping by default**: project-dependent state lives in the project; a user who refuses tool-written dirs in a checkout gets a *placement* knob, not a feature loss.
- **D3 — one mechanism per consumer class**: symlinks, launchers, plain PATH dirs assigned by how the artifact is *consumed*; no mechanism expands past its born role (entrypoints solve diamond/env-encapsulation; they are not a shim distribution vehicle).
- **D4 — closure integrity**: a dependency's identity belongs to its root's resolution; dep paths stay digest-pinned.
- **D5 — Windows without privilege**: directory links only (junctions); the frozen `.shim` contract untouched.
- **D6 — never block, heal before emit**: link maintenance inherits the never-block posture of every emit path; on the emit paths consent does not gate, heal is the *only* integrity defense.
- **D7 — export/execute parity**: `ocx env` and `ocx exec` compose identically against one shared oracle (`subsystem-cli.md`); any following/pinned choice is a property of the *scope being composed*, never of an emit branch.

## Industry Context & Research

Full survey: `research_shell_env_reconciler_and_launcher_farm.md`. Load-bearing:

- Stable-dir lineage: Homebrew `opt/<formula>`, SDKMAN `current`, update-alternatives, scoop's per-app `current` **junction** (privilege-free Windows dir links — what D5 needs).
- mise's model argument: real dirs on PATH beat shims (per-exec tax multiplies through build fan-out — asdf's 120–150 ms lesson). Here dir links suffice; nothing new is shimmed.
- Nothing in the survey addresses the *dereference* class (`JAVA_HOME`) with anything but a stable directory — because nothing else can.
- Placement precedent: tools that write into a checkout ship a location override — direnv's `direnv_layout_dir`, uv's `UV_PROJECT_ENVIRONMENT`, `RUFF_CACHE_DIR` (all literal paths, hence per-invocation). Project-local trees self-gitignore (`.venv/` = `*`, the uv pattern).
- Atomic repoint = `rename(2)`, never `unlink`+`symlink` (research pitfall 7).

## Considered Options

### Option A — No stable addressing; reconciler only

| Pros | Cons |
|---|---|
| Zero new machinery (reconciler shipped) | Running GUIs unsolved (binaries *and* `JAVA_HOME`); every digest bump is reconcile work in every session |

### Option B — Farm store: one flattened per-scope bin dir under `$OCX_HOME/farm/<key>/` (rejected draft)

| Pros | Cons |
|---|---|
| One PATH element per scope | Parallel store next to `SymlinkStore`; flattening breaks per-package PATH ordering → collision policy; per-**file** links → Windows privilege → frozen-`.shim` amendment; readdir materializer; hash-keyed project namespace in home; entrypoints conflated into a shim vehicle |

### Option C — Home-keyed link kinds inside `SymlinkStore` (`symlinks/<reg>/<repo>/scopes/<key>`)

| Pros | Cons |
|---|---|
| One store | Project state rooted in `$OCX_HOME` by default; the `<key>` namespace is redundant once containment scopes; registry-repo keying mismatches composition (groups can hold two versions of one tool) |

### Option D — Project-local toolchain tree next to its `ocx.toml`, with a placement knob **(chosen)**

| Pros | Cons |
|---|---|
| Scope = containment (no keys by default); lifecycle dies with the project; lock-entry keying mirrors composition incl. groups; junctions on Windows; `current`/D5 untouched; Option C's keyed root survives as the *explicit* opt-in placement | Absolute cross-volume targets (healed); tree sits in attacker-controlled repos (heal-before-emit, with a stated live-writer residual); the resolver `install_path` override across the composer's call sites is the real cross-cutting cost |

### Option E — Shims for everything (volta-style per-exec resolution)

| Pros | Cons |
|---|---|
| Per-exec freshness for every binary | Cannot serve the dereference class at all; per-exec ocx spawn + compose on every binary (asdf lesson); duplicates what entrypoints already provide where needed |

## Decision Outcome

**Option D.** One rule: **the link tree lives next to the `ocx.toml` it materializes**, unless the user places it elsewhere.

### The consumer matrix (the decision's frame)

| Consumer | Example | Boundary | Mechanism |
|---|---|---|---|
| **Path dereference** — a process reads a dir path | `JAVA_HOME`, `GOROOT`, SDK homes | none — nothing executes | **toolchain link** (only possible answer) |
| **Execution needing composed env** | declared entrypoints; deferred tools' shim slots | exec() through launcher | **launcher** (exists, unchanged): re-composes per exec from `pkg_root` — closure-correct, encapsulated |
| **Execution, ambient env suffices** | plain `bin/` binaries | exec() through PATH | **linked bin dir** — zero per-exec cost; fresh binary at next spawn; env rides the shell |

Links carry *addressing*; launchers carry *env encapsulation*; neither expands into the other's role (D3).

### Layout

```
<project>/.ocx/toolchain/          # sibling of the (deliberately committed) .ocx/index/
├── .gitignore                     # "*" — written at materialization; scoped to toolchain/ only
└── <group>/                       # "default" + named groups
    └── <entry>/                   # lock-entry name (slug grammar); mirrors composition — two
        │                          #   versions of one tool are two entries/groups by construction
        └── → $OCX_HOME/packages/<…digest root…>     (dir symlink; junction on Windows)
$OCX_HOME/toolchain/<group>/<entry>/   # the global toolchain's tree — a FileStructure store (below)
```

- **Lock entries only (D4).** Dependencies get no links: a dep's identity is a function of its root's resolution (`resolve.json`); a repointable dep path would let A run against a B it never resolved with. `${deps.NAME.installPath}` stays digest-pinned on purpose. Residual: dep *interface* bin dirs on the ambient PATH churn on update — the reconciler retires/applies them next prompt; a frozen GUI env keeps them until restart. Tools that cannot tolerate that declare an entrypoint.
- **One target, in both materialization states.** A linked entry always targets the package root — pure digest arithmetic, valid before the package is materialized. This is exactly the property the composer already relies on for deferred (`lazy-mode`) roots: it emits the root's declared vars and its `entrypoints/` path against the package root *and* pushes the shim slot (`emit_shim_slot` + `emit_root_path_block`, unconditionally), so `${installPath}` resolves to the same value before and after first invocation. The **shim slot itself stays digest-pinned and unlinked**: it is a launcher path (consumer matrix row 2), materialization happens inside `ocx launcher shim` — a process that knows a pinned identifier and `argv0`, not which project or group linked it — and the shim tree is never retired. Consequence: a deferred tool's shim-slot element changes on a version bump like any launcher path; everything else about the entry is link-stable.
- **Repoint = atomic `rename()`** (temp name + rename; junctions likewise). Targets are **absolute** (junctions require it; the tree crosses volumes into `$OCX_HOME`). Toolchain links are written with `symlink::replace_atomic` directly, under the facility's own containment policy (a target must lie under `$OCX_HOME/packages/`, checked lexically — no canonicalization, because a deferred root's package dir may not exist yet), and take **no `refs/symlinks/` back-ref**: they are lock materializations, not install references, so they never widen the GC root set. That is the second named exception to the store-link rules, **ARCH-4c**, sibling of `projects/`'s ARCH-4b (which likewise bypasses `ReferenceManager::link` and `symlink::validate_target` because its targets are external), to be recorded in `subsystem-file-structure.md`. `ReferenceManager::link` (back-refs = GC roots, canonicalizes its target) and the archive symlink-escape guard `validate_target` (CWE-22) are both **untouched**. Portability follows: the tree is position-independent (copy/move it freely), and if `$OCX_HOME` itself moves, heal-before-emit repoints every link on the next command.
- **Self-gitignored**, created at materialization; reversal deletes `toolchain/` only, never `.ocx/`.

### Placement and opt-out — `[toolchain]` config section

```toml
[toolchain]            # config.toml — distinct from ocx.toml's top-level "toolchain-level" keys (lazy-mode, lazy-report)
dir   = "/abs/root"    # optional; unset ⇒ <project>/.ocx/toolchain/
links = true           # false ⇒ pinned (digest) paths everywhere; no tree is ever written
```

- **`dir`** is a *root*: ocx materializes at `<root>/<project-key>/<group>/<entry>/`, so a globally configured root never collides across projects (the literal-path knobs of direnv/uv/ruff only work per invocation). The key is the one the reconciler derives for `state/projects/<key>/` (`ProjectIdentity` → `ReferenceManager::name_for_path`, 16 hex of SHA-256 over the canonical dir); note this **promotes it from a lookup index to a namespace** — a moved project gets a new key, and its old keyed dir is pruned when the `projects/` ledger reports that entry dead. This is the rejected Option C shape, acceptable because the user chose it. The global toolchain ignores `dir`.
- **`links = false`** is the pinned-by-default policy for people who want no link indirection at all (reproducibility debugging, tooling that canonicalizes paths, junction-averse Windows hosts): every command composes digest paths — the read-only fallback made deterministic.
- Lives in `config.toml` (all tiers — unlike the reconciler's hook toggle, "never write into checkouts" *is* a legitimate fleet policy; the forward-compat test already parses a hypothetical `[toolchain]` table) with env overrides `OCX_TOOLCHAIN_DIR` / `OCX_NO_TOOLCHAIN_LINKS` (no collisions with existing `env::keys`). Both are resolution-affecting: forwarded through `Env::apply_ocx_config` and documented in `environment.md`. Project-level (`ocx.toml`) placement is deliberately not offered — the person objecting is the contributor, not the author; revisit only on demand.

### Following vs pinned — a scope property, and export/execute parity

A linked path **follows** the lock: repoint under a running process and its next `exec()`/dereference sees the new toolchain. That is the feature for shells and IDEs, and a hazard for a long-running process that must keep the toolchain it started with — a daemon, a build/language server, a JVM opening `lib/`/`conf/` lazily while `JAVA_HOME` repoints beneath it (the "restart required after upgrade" class every OS package manager has; a multi-entry update is N atomic renames, so a process resolving mid-update can see a mixed set until its next spawn).

Decision: the lane is a **field of `EnvScope::Project`** (`package_manager/tasks/resolve.rs`) — the type that already distinguishes project-tier from package-tier composition, so "pinned + package tier" is unrepresentable rather than meaningless, and no `resolve_env_*` signature changes. Default *following* wherever links are enabled; per invocation, a single positive flag **`--pinned`** (`options::Pinned`, flattened into both `ocx env` and `ocx exec`, orthogonal to `--shell`/`--ci`/`--format`) flips the scope to pinned. No `--no-pinned`: the only config that pins (`links = false`) does so by writing no tree at all, so a negation would have nothing to select; add the pair only if a lane-only config rung ever exists. Because the lane lives on the scope, parity holds in both lanes: `ocx env` ≡ `ocx exec`, `ocx env --pinned` ≡ `ocx exec --pinned`, and `--shell`, `--ci`, `--format json` render the same entries vector. `EnvScope::Project` is a struct variant, so every construction site names the lane explicitly — six today (`direnv_export`, `launcher/exec`, `toolchain_env` ×2, `toolchain_exec`, `activation`) — and the **launcher re-entry constructs it pinned**: the replay path must never resolve a root through a link.

| Surface | Lane |
|---|---|
| `self activate --reconcile` (per-prompt), `ocx direnv export`, `ocx env` (shell / ci / json), `ocx exec` | following by default; `--pinned` where the flag exists |
| entrypoint launchers, deferred-tool shims (`launcher exec`) | pinned by construction (baked `pkg_root`), unchanged |
| `ocx package env` / `ocx package exec` (OCI tier), `ocx inspect` (declared entries, not composed), `ocx status` (`[env]` verbatim) | not applicable — no composed project tree |
| `[toolchain] links = false` | pinned everywhere; `--pinned` is then a no-op |

Rule of thumb for docs: *a shell follows; a process you hand off pins* — `ocx exec --pinned -- <daemon>`, or write its unit/env file from `ocx env --pinned`. Nushell consumes the reconciler's JSON plan rather than an eval'd snippet; same following cadence. A pinned process still depends on `ocx clean` not collecting its digest — the pre-existing GC-vs-running-process exposure, unchanged here.

### Resolver `install_path` override — where it applies and where it must not

The interpolation grammar (`adr_interpolation_token_grammar.md`) has several spellings of "this package's install path": `${installPath}`, its alias `${self.installPath}`, each with `:native`/`:posix` render modifiers — and a `Modifier::Path` value authored as a bare relative path (`bin`) is joined onto the install path implicitly, with no token at all (`EnvResolver`'s relative-join branch). A token-keyed override map would miss the last case silently. Therefore the override is applied **at the resolver's input** — `EnvResolver::new` / `TemplateResolver::new` receive `install_path = <entry-link>/content` — and every spelling and the implicit join resolve through the link.

It is not one seam. The composer constructs resolvers per call site, and one `EnvResolver::new(content, …)` shape serves roots and deps alike, so the override is a **per-site decision keyed on "is this a linked lock entry"**:

| Composer site | Linked entry (following) | Always digest |
|---|---|---|
| root declared vars, root `integrations` | link | — |
| dep declared vars, dep `integrations`, `DependencyContext` (`${deps.*}`) | — | digest (D4) |
| `synth_entrypoints_path_for` (bypasses resolvers) | `<entry-link>/entrypoints` | — |
| `synth_shim_path_for` (bypasses resolvers) | — | digest (launcher path) |
| `launcher exec` re-entry (composes via `resolve_env` while replaying the forwarded `OCX_ENV` payload) | — | digest — its `EnvScope::Project` is constructed with the lane **pinned**; the frozen `.shim` contract never inherits a following default |

Pinned-lane composition passes digest paths at every site — same code, different input. Mixed-token values are legal and churn by design: `"${installPath}:${deps.cmake.installPath}/bin"` is half stable, half digest-pinned — the 0-bytes claim below is scoped to values carrying no `${deps.*}` reference.

### Freshness, integrity, failure posture

- Materialized/repointed by resolution-mutating commands (`add`, `remove`, `lock`, `update`, `pull` into scope). No shell-side trigger; no materialization-state trigger (links do not change on deferred → materialized).
- **Heal before emit, on every composing emit path.** The tree sits inside attacker-controlled repos; a clone can ship pre-made links pointing anywhere. **No path is emitted before its link's `readlink` verifies against the lock-pinned package root.** The composing emit paths are `ocx env` (shell / ci / json), `ocx exec`, `ocx direnv export`, and `self activate --reconcile`. On the first three, consent plays no part — they compose and emit with no consent evaluation (a `[shell.consent]` `/*` subtree grant likewise activates a never-touched clone on the reconciler) — so heal is the *sole* integrity defense there. On the reconciler, `activation` evaluates consent first and *composes only what consent authorized*; heal therefore runs strictly after consent, and a consent-refused project's tree is left untouched. `ocx inspect` reports declared entries and never composes — it is neither an emitter nor a healer.
- **Accepted residual (CWE-367).** Heal defends the *static* poisoned-clone case. Against a live writer in the same repo, a link can be repointed between `readlink` and the shell's later dereference — the emitted string is a mutable indirection by design. Mitigation for a hostile live repo is the pinned lane (`--pinned`, or `links = false`).
- The resolver's `required` existence probe follows links and cannot distinguish a poisoned-but-resolvable link from a healed one — it runs strictly *after* heal.
- **Lock-free on the read side.** The reconciler recomposes every prompt for an active project (≈21 ms vs the 4.5 ms stat-only path); heal adds one `readlink` per entry and no lock. Only an observed mismatch acquires `lock_scoped` and repoints (atomic rename is idempotent under the content-addressed invariant, so concurrent healers converge).
- Write failure (read-only checkout, foreign-owned dir, lock timeout): **skip-and-continue** — compose digest paths for that run, exit 0, never block a prompt or an emit (D6). CI typically lands here and loses nothing.
- Crash mid-materialization: per-entry atomic renames keep each link individually valid; heal re-converges. No marker files, no lock-wedge class.

### GC

**No new GC surface.** Roots are already "digests pinned by any *registered* project's lock" plus the implicit `$OCX_HOME/ocx.lock` root (`clean::collect_project_roots`; ledger in `project/registry.rs`, `adr_project_gc_symlink_ledger.md`). Every link target is such a pinned digest, and toolchain links write **no `refs/symlinks/` back-refs** (ARCH-4c). GC never walks project trees; a stale link is a heal case, not a leak. Invariants: **materialization of a project tree registers the project** in the ledger; the **global tree registers nothing** — `register` is a no-op for `$OCX_HOME` by the no-self-link invariant (ARCH-1b), and the global lock is rooted implicitly. `$OCX_HOME/toolchain/` is declared **outside the GC graph** the way `ShimBinStore` and `locks` are ("never walked by `ocx clean`").

### Reconciler interplay (contract with `adr_shell_env_overhaul.md`)

- `.ocx/toolchain/` and a configured `[toolchain] dir` root join `plan(..., owned_prefixes)` **additively** — the hook that record reserved by name; no change to its decisions.
- Payoff: with linked entries, a digest bump leaves D byte-identical for link-resolved values, so the reconciler's retire/apply work on a version bump reduces to the shim-slot, mixed-token, and dep-interface residuals. Membership changes remain ordinary recomposes.
- **Stated residual — lost-ledger repair.** `plan`'s repair arm removes only segments under the *current* project's `owned_prefixes`; a stale `<projectA>/.ocx/toolchain/…` segment can survive a lost ledger while project B is active (the constant `$OCX_HOME` prefix has no such hole). A configured `[toolchain] dir` root restores a constant prefix and closes it; the project-local default accepts it as the price of containment.

### What stays untouched

`candidates/` + `current` + `ocx package select` (user-owned; D5 intact — no lock-driven path consults `current`); entrypoint semantics, the launcher pipeline, the shim tree, the frozen `.shim` sidecar contract; package metadata and every wire format; the reconciler's decisions; `ReferenceManager::link` and `symlink::validate_target`. A future project-facing `select` convenience **edits the lock** and lets materialization follow — links always follow the lock.

### PATH surface

Store-rooted emitted paths become link paths — N stable per-package dirs per scope (per-package ordering and shadowing semantics preserved verbatim; no collision policy). Shim slots and project/group `[env]` PATH additions are unaffected. Consumer-visible break in `ocx env` / `.envrc` / `--ci` / `--format json` output — pre-1.0, changelog = commit subject.

### Quantified Impact

| Metric | Before | After |
|---|---|---|
| Emitted env bytes changed by a version bump (link-resolved values, no `${deps.*}`) | all digest strings | **0** |
| `JAVA_HOME`-class dereference in a running IDE after update/re-select | stale digest dir | correct through the link |
| Binary freshness in running shells / GUIs | shells next prompt (reconciler); GUIs never | next spawn, both |
| Reconciler work per version bump (linked values) | retire + apply per element | none |
| Per-exec overhead | 0 plain / launcher for entrypoints | unchanged |
| Windows privilege requirement | n/a | none (junctions) |
| Dockerfile, global tier ([#193]) | digest `ENV` paths, stale | stable `ENV` paths, repoint under image rebuild |

### Consequences

**Positive**: [#189] delivered with containment scoping and escape hatches for both placement and pinning; Java-class reselect convenience; the reconciler's common case becomes a no-op; Windows needs no contract change.

**Negative / Risks**:
- The `install_path` override is a per-site decision across the composer (five sites) plus the `EnvScope::Project` lane field.
- Every composing emit path becomes a guarded writer (heal); read-only and concurrent-healer posture must be tested per path.
- Mixed stability is by design (entries stable, deps pinned, shim slots pinned, mixed-token values churn) — must be documented so users understand *why*.
- Running processes on the following lane see updates mid-run (dpkg class) — accepted; `--pinned` is the answer, not a toggle.
- Live-writer TOCTOU on the following lane — accepted, stated.
- Junction fragility class on Windows (scoop's SSH/KB precedent) — accepted.

### How Would We Reverse This?

Default the lane to pinned (one seam: the `EnvScope::Project` field's default), delete `toolchain/` trees (self-contained, gitignored) and the `$OCX_HOME/toolchain/` store, drop the `[toolchain]` section (degrades silently on older binaries — no `deny_unknown_fields`). No wire format, metadata, or lock schema touched; `candidates`/`current` never involved.

## Technical Details (component placement)

- **Link facility** extracted from `SymlinkStore`, parametric on **root and link policy**: the home store keeps `ReferenceManager::link` (back-refs are its GC roots); toolchain trees use `symlink::replace_atomic` with the ARCH-4c containment policy and write no back-refs. Two instances of one facility, two policies.
- **`ToolchainStore`** — a `FileStructure` store for `$OCX_HOME/toolchain/` (always `fs.toolchain`, never a bare join; declared outside the GC graph like `ShimBinStore`). Project-rooted trees (default and `[toolchain] dir` roots) are managed via `project/`, not as stores.
- **Materializer** in `package_manager/` — input: resolved lock entries per group; output: links; registers the project; runs the atomic-rename repoint under `lock_scoped` only on mismatch.
- **Override + lane**: per-site `install_path` choice in `composer.rs` (table above); lane field on `EnvScope::Project` (`tasks/resolve.rs`); `options::Pinned` (`crates/ocx_cli/src/options/pinned.rs`) flattened into `ocx env` and `ocx exec`; `launcher/exec.rs` constructs its `EnvScope::Project` with the lane pinned.
- **Config**: `config/toolchain.rs` (`dir`, `links`), `Config::merge()` wiring, schema regen; `OCX_TOOLCHAIN_DIR` / `OCX_NO_TOOLCHAIN_LINKS` in `env::keys` + `apply_ocx_config`.
- **Docs surfaces**: storage-layout page (`.ocx/toolchain/` beside the existing `.ocx/index/` entry, `$OCX_HOME/toolchain/`), `configuration.md` (`[toolchain]`, disambiguated from `ocx.toml`'s toolchain-level keys), `environment.md` (two vars), env-composition reference (override table, lanes, mixed-stability rationale), `subsystem-file-structure.md` (ARCH-4c, new store) + `subsystem-package-manager.md` rules.

## Implementation Plan

Plan via `/swarm-plan` after ADR approval. Work packages: link facility (containment-policy parametric); `ToolchainStore` + registration invariants; per-site override + `EnvScope::Project` lane (largest); materializer + heal-before-emit on the four composing emit paths; junction backend; `[toolchain]` config + env keys + schema; `options::Pinned`; docs. Acceptance tests are plain pytest — no shell/pty surface (the reconciler's matrix already covers prefix ownership).

## Validation

- Repoint demonstrated both states (old digest resolved before update, new after) — unchecked-green rule.
- Byte-identical `ocx env` output across a version bump for a fixture without `${deps.*}` or deferred roots; a mixed-token fixture and a deferred root's shim slot demonstrably change.
- Parity: `ocx env` vs `ocx exec` against the shared oracle in both lanes.
- Poisoned committed link: pre-seeded `.ocx/toolchain/` pointing elsewhere → healed before first emission on **each** of the four composing paths (red: emission without heal exposes the foreign target); on the reconciler, a consent-refused project's tree stays untouched; `required` probe observed to run after heal.
- Deferred root: link targets the package root before materialization; `${installPath}`-resolved values identical before/after first invocation; shim slot digest-pinned.
- `${installPath}`, `${self.installPath}`, `:native`/`:posix`, and the bare relative `Path` value resolve through the link at root sites; `${deps.*}` and `launcher exec` do not.
- `[toolchain] dir` root: keyed per project, no collision across two projects, moved project's old keyed dir pruned via the ledger; `links = false`: no tree written, digest paths emitted, `--pinned` no-op.
- Read-only checkout: heal skipped, digest fallback emitted, exit 0. Concurrent healers converge.
- Registration: materializing a project tree creates its `projects/` ledger entry (positive), the global tree creates none (negative); both survive `ocx clean` — the project via its lock pin, the global tree via the implicit lock root; no `refs/symlinks/` back-ref appears for any toolchain link.
- Windows: junction create/repoint/read-through without privilege; `.shim` files byte-unchanged.

## Links

- Research: `research_shell_env_reconciler_and_launcher_farm.md`
- Related: `adr_shell_env_overhaul.md` (reconciler, shipped), `adr_interpolation_token_grammar.md` (grammar the override must cover), `adr_global_toolchain_tier.md` (D5), `adr_project_gc_symlink_ledger.md` (GC ledger, ARCH-1b), `adr_windows_exe_shim.md` (untouched contract), `subsystem-cli.md` (export/execute parity), `subsystem-file-structure.md` (ARCH-4b precedent)
- Issues: [#189](https://github.com/ocx-sh/ocx/issues/189), [#193](https://github.com/ocx-sh/ocx/issues/193)

## Changelog

| Date | Change |
|---|---|
| 2026-08-02 | Farm-store draft (`adr_toolchain_farm.md`) + adversarial review round 1 |
| 2026-08-03 | Farm rejected (owner review); home-keyed variant rejected (containment beats keys); rewritten as project-local toolchain tree |
| 2026-09-02 | Rewritten against the post-reconciler tree (review round 2): sibling → `adr_shell_env_overhaul.md`; override at the resolver input; following/pinned as a composition property; consent-backstop claim removed; `[toolchain] dir`/`links` knob; global tree = `ToolchainStore`; closed issues dropped |
| 2026-09-02 | Review round 3: links target the package root in both materialization states, shim slots stay digest-pinned and unlinked, no materialization trigger; ARCH-4c restated as a `ReferenceManager::link` carve-out (`validate_target` untouched); reconciler heals after consent; `ocx inspect` removed from emitters; lane as an `EnvScope::Project` field, `--pinned` only; per-site override table incl. synth entries and the `launcher exec` exclusion; TOCTOU + lost-ledger residuals stated; precedent cites corrected (`ShimBinStore`, direnv/uv); `[toolchain]` disambiguated; #193 scoped to global |
| 2026-09-02 | Review round 3 verification: ARCH-4c restated as `symlink::replace_atomic` + own containment policy, no back-refs (`ReferenceManager::link` has no containment check, canonicalizes, and would add GC roots); launcher re-entry constructs `EnvScope::Project` pinned; `--no-pinned` rationale corrected; positive registration check added |
