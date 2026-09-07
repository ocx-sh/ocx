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
- **Amended by**: [`adr_toolchain_activation.md`](./adr_toolchain_activation.md) (2026-09-04, Proposed — approved and implemented together with this record). Four changes, each marked inline below: `bin/` moves **inside** `toolchain/` and `bin` becomes a reserved group and tool name; the `[toolchain]` config section is replaced by an `ocx.toml` `pinned` key (+ `OCX_TOOLCHAIN_PINNED`) and a root-level `config.toml` `toolchain-dir`; `ocx pull` renders the home; consumer-matrix row 3's "linked bin dir" becomes launcher trampolines plus the hook-prepended link-tree bin dirs. Everything else in this record stands, including all three review rounds' outcomes.

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
| **Execution, ambient env suffices** | plain `bin/` binaries | exec() through PATH | **the link tree's own bin dirs**, prepended by the hook in `env` mode — zero per-exec cost; fresh binary at next spawn; env rides the shell. *(Amended 2026-09-04: where no hook runs — a GUI app, a hookless IDE, a CI step, a shell activated in `bin` mode — the same names are served by launcher **trampolines** in `<home>/toolchain/bin/`, which carry the per-package closure env themselves. See `adr_toolchain_activation.md` D2.)* |

Links carry *addressing*; launchers carry *env encapsulation*; neither expands into the other's role (D3).

### Layout

```
<project>/.ocx/toolchain/          # sibling of the (deliberately committed) .ocx/index/
├── .gitignore                     # "*" — written at materialization; scoped to toolchain/ only
├── bin/                           # AMENDED 2026-09-04: launcher trampolines for the default
│                                  #   group (adr_toolchain_activation.md D1/D2)
└── <group>/                       # "default" + named groups
    └── <entry>/                   # lock-entry name (slug grammar); mirrors composition — two
        │                          #   versions of one tool are two entries/groups by construction
        └── → $OCX_HOME/packages/<…digest root…>     (dir symlink; junction on Windows)
$OCX_HOME/toolchain/{bin,<group>/<entry>}/   # the global toolchain's tree — a FileStructure store (below)
```

**Amendment 2026-09-04 — `bin` is reserved.** `bin/` sits *inside* `toolchain/`, so `bin` becomes a reserved **group** name and a reserved **tool** name (joining `default` and `all`), the latter so a future per-group `<group>/bin/` stays possible without a layout break. `$OCX_HOME/bin` is **not** the trampoline directory; it does not exist on disk at all, and the trampoline consumer that needs ocx's own binary directory uses `ocx_install_bin_path`. *(Amended 2026-09-05: group names and `[tools]` keys additionally gain a strict `^[A-Za-z0-9][A-Za-z0-9._-]*$` charset validator — a security control, because both strings are baked into a generated shell script.)* Details and the exit code in `adr_toolchain_activation.md` § *Reserved and validated names*.

- **Lock entries only (D4).** Dependencies get no links: a dep's identity is a function of its root's resolution (`resolve.json`); a repointable dep path would let A run against a B it never resolved with. `${deps.NAME.installPath}` stays digest-pinned on purpose. Residual: dep *interface* bin dirs on the ambient PATH churn on update — the reconciler retires/applies them next prompt; a frozen GUI env keeps them until restart. Tools that cannot tolerate that declare an entrypoint.
- **One target, in both materialization states.** A linked entry always targets the package root — pure digest arithmetic, valid before the package is materialized. This is exactly the property the composer already relies on for deferred (`lazy-mode`) roots: it emits the root's declared vars and its `entrypoints/` path against the package root *and* pushes the shim slot (`emit_shim_slot` + `emit_root_path_block`, unconditionally), so `${installPath}` resolves to the same value before and after first invocation. The **shim slot itself stays digest-pinned and unlinked**: it is a launcher path (consumer matrix row 2), materialization happens inside `ocx launcher shim` — a process that knows a pinned identifier and `argv0`, not which project or group linked it — and the shim tree is never retired. Consequence: a deferred tool's shim-slot element changes on a version bump like any launcher path; everything else about the entry is link-stable.
- **Repoint = atomic `rename()` on POSIX; a bounded non-atomic window on Windows** (temp name + rename). *(Corrected 2026-09-05 — "junctions likewise" was wrong.)* `symlink::replace_atomic`'s Windows arm is a documented **remove-then-rename** (`crates/ocx_lib/src/symlink.rs:203-222` and `:283-300`), because `MoveFileEx` with `REPLACE_EXISTING` refuses a directory destination. A concurrent reader can observe no entry inside that window, and the convergence argument in that module's own comment rests on "concurrent same-hash installers stage an *equivalent* junction" — i.e. on cooperating ocx writers, a premise that does not hold inside an attacker-writable project home. Recorded as residual R8 in `adr_toolchain_activation.md`. Targets are **absolute** (junctions require it; the tree crosses volumes into `$OCX_HOME`). Toolchain links are written with `symlink::replace_atomic` directly, under the facility's own containment policy (a target must lie under `$OCX_HOME/packages/`, checked lexically — no canonicalization, because a deferred root's package dir may not exist yet), and take **no `refs/symlinks/` back-ref**: they are lock materializations, not install references, so they never widen the GC root set. That is the second named exception to the store-link rules, **ARCH-4c**, sibling of `projects/`'s ARCH-4b (which likewise bypasses `ReferenceManager::link` and `symlink::validate_target` because its targets are external), to be recorded in `subsystem-file-structure.md`. `ReferenceManager::link` (back-refs = GC roots, canonicalizes its target) and the archive symlink-escape guard `validate_target` (CWE-22) are both **untouched**. Portability follows: the tree is position-independent (copy/move it freely), and if `$OCX_HOME` itself moves, heal-before-emit repoints every link on the next command.
- **Self-gitignored**, created at materialization; reversal deletes `toolchain/` only, never `.ocx/`.

### Placement and opt-out — **amended 2026-09-04**

The `[toolchain]` section this record originally proposed (`dir` + `links`) **never ships**. Both knobs survive with different spellings, decided in `adr_toolchain_activation.md` D3:

```toml
# config.toml — root-level scalar, any tier including [managed]
toolchain-dir = "/abs/root"   # optional; unset ⇒ <project>/.ocx/toolchain/

# ocx.toml — toolchain-level key, beside lazy-mode
pinned = false                # true ⇒ digest paths everywhere; no <group>/<entry> links rendered
```

- **`toolchain-dir`** is the former `[toolchain] dir`, unchanged in meaning: a *root*, not a literal path. ocx materializes at `<root>/<project-key>/{bin,<group>/<entry>}/`, so a globally configured root never collides across projects (the literal-path knobs of direnv/uv/ruff only work per invocation). The key is the one the reconciler derives for `state/projects/<key>/` (`ProjectIdentity` → `ReferenceManager::name_for_path`, 16 hex of SHA-256 over the canonical dir); note this **promotes it from a lookup index to a namespace** — a moved project gets a new key, and its old keyed dir is pruned when the `projects/` ledger reports that entry dead. This is the rejected Option C shape, acceptable because the user chose it. **The global toolchain ignores it.** It stays in `config.toml`, every tier, because "never write into checkouts" is a legitimate fleet policy; the env spelling is `OCX_TOOLCHAIN_DIR`, resolution-affecting, forwarded through `Env::apply_ocx_config` and documented in `environment.md`. *(Amended 2026-09-05 by `adr_toolchain_activation.md`:* the root is **refused at parse unless it resolves inside `$HOME`/`%USERPROFILE%` or `$OCX_HOME`**, in addition to the root/system-prefix and owner-and-mode refusals. An owned prefix is a PATH *deletion* authority and this location supplies executables, so a `/tmp` root that passes an owner check but sits under a writable ancestor is not acceptable. Managed-tier values are therefore of the shape `~/.cache/ocx/toolchain`.*)* *(Corrected 2026-09-06 — D-V16:* the sentence above previously offered `%LOCALAPPDATA%\ocx\toolchain` as a second managed-tier shape. **A leading `~` is the only expansion `toolchain-dir` performs, on every platform; `%VAR%` is never expanded.** An unexpanded `%LOCALAPPDATA%\ocx\toolchain` is not an absolute path, so it is refused at parse with exit 78. A Windows managed-tier value is `~/...` or a literal absolute path.*)* Project-level (`ocx.toml`) placement is still deliberately not offered — the person objecting is the contributor, not the author.
- **`pinned = true`** replaces `links = false`, and moves tier: it is an **`ocx.toml`** key, not a `config.toml` one, because following-versus-pinned is a property of the toolchain being composed rather than a machine policy. Same meaning — no link indirection anywhere, every command composes digest paths — for the same audience (reproducibility debugging, tooling that canonicalizes paths, junction-averse Windows hosts). The env spelling is `OCX_TOOLCHAIN_PINNED`, and it is the **weakest** ladder tier (`--pinned` ▸ `ocx.toml` ▸ `OCX_TOOLCHAIN_PINNED` ▸ `false`), exactly like `OCX_LAZY_MODE`. `OCX_NO_TOOLCHAIN_LINKS` is not introduced.
- **One difference in effect, stated.** `links = false` wrote *no tree at all*. `pinned = true` still renders `<home>/toolchain/bin/` — only the `<group>/<entry>` links are omitted, and the link pass is suppressed **whole**, prunes included (RUL-23). *(Corrected 2026-09-06 — D-9, C-046:* this bullet previously continued "and each trampoline bakes a digest root instead of a link path. Consequence: a pinned tree's trampoline bodies churn on every version bump, where a following tree's are byte-stable." **No trampoline bakes a digest root.** A body bakes only the home selector and the `ocx` install path, so every body is byte-identical whatever `pinned` says: a pinned tree's `bin/` is exactly as byte-stable as a following tree's, and flipping the key takes effect at the next re-entry with no re-render. What `pinned` now selects is what the *emitters* compose — digest paths rather than `<group>/<entry>` links.*)*

### Following vs pinned — a scope property, and export/execute parity

A linked path **follows** the lock: repoint under a running process and its next `exec()`/dereference sees the new toolchain. That is the feature for shells and IDEs, and a hazard for a long-running process that must keep the toolchain it started with — a daemon, a build/language server, a JVM opening `lib/`/`conf/` lazily while `JAVA_HOME` repoints beneath it (the "restart required after upgrade" class every OS package manager has; a multi-entry update is N atomic renames, so a process resolving mid-update can see a mixed set until its next spawn).

Decision: the lane is a **field of `EnvScope::Project`** (`package_manager/tasks/resolve.rs`) — the type that already distinguishes project-tier from package-tier composition, so "pinned + package tier" is unrepresentable rather than meaningless, and no `resolve_env_*` signature changes. Default *following*; per invocation, a single positive flag **`--pinned`** (`options::Pinned`, flattened into both `ocx env` and `ocx exec`, orthogonal to `--shell`/`--ci`/`--format`) flips the scope to pinned. **Amended 2026-09-04:** the config rung is now `ocx.toml`'s `pinned` key, resolved by a `LazyModeLadder`-shaped ladder (`--pinned` ▸ `ocx.toml` ▸ `OCX_TOOLCHAIN_PINNED` ▸ `false`), so `--pinned` is the ladder's top tier rather than the only pinning channel. Still no `--no-pinned`: nothing below the flag can express "following" more specifically than the floor already does; add the pair only if a lane-only rung ever needs overriding downward. Because the lane lives on the scope, parity holds in both lanes: `ocx env` ≡ `ocx exec`, `ocx env --pinned` ≡ `ocx exec --pinned`, and `--shell`, `--ci`, `--format json` render the same entries vector. `EnvScope::Project` is a struct variant, so every construction site names the lane explicitly — six today (`direnv_export`, `launcher/exec`, `toolchain_env` ×2, `toolchain_exec`, `activation`) — and the **launcher re-entry constructs it pinned**: the replay path must never resolve a root through a link.

| Surface | Lane |
|---|---|
| `self activate --reconcile` (per-prompt), `ocx direnv export`, `ocx env` (shell / ci / json), `ocx exec` | following by default; `--pinned` where the flag exists |
| entrypoint launchers, deferred-tool shims (`launcher exec`) | pinned by construction (baked `pkg_root`), unchanged |
| `ocx package env` / `ocx package exec` (OCI tier), `ocx inspect` (declared entries, not composed), `ocx status` (`[env]` verbatim) | not applicable — no composed project tree |
| `ocx.toml` `pinned = true` (was `[toolchain] links = false`) | pinned everywhere; `--pinned` is then a no-op |

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
- **Amendment 2026-09-04 — `ocx pull` renders the home.** `pull` is not merely one of five triggers: it is *the* command that brings a fresh clone to a working tree. `ocx pull` renders `<home>/toolchain/` **always** — `bin/` for the default group and the `<group>/<entry>` links its `-g` selection names — as a whole-compose pass after roots resolve, because `ocx.lock` carries no `binaries`/`entrypoints` and the name set needs a resolved metadata read. `--dry-run` reports the tree delta and writes nothing. **Consent is not materialization**: `[shell.consent]`, `ocx shell allow` and subtree `/*` grants gate the per-prompt reconciler and nothing else, so `pull` renders without any consent evaluation, exactly as it composes today. Full contract in `adr_toolchain_activation.md` D4.
- **Heal before emit, on every composing emit path.** The tree sits inside attacker-controlled repos; a clone can ship pre-made links pointing anywhere. **No path is emitted before its link's `readlink` verifies against the lock-pinned package root.** The composing emit paths are `ocx env` (shell / ci / json), `ocx exec`, `ocx direnv export`, and `self activate --reconcile`. On the first three, consent plays no part — they compose and emit with no consent evaluation (a `[shell.consent]` `/*` subtree grant likewise activates a never-touched clone on the reconciler) — so heal is the *sole* integrity defense there. On the reconciler, `activation` evaluates consent first and *composes only what consent authorized*; heal therefore runs strictly after consent, and a consent-refused project's tree is left untouched. `ocx inspect` reports declared entries and never composes — it is neither an emitter nor a healer.
- **Accepted residual (CWE-367).** Heal defends the *static* poisoned-clone case. Against a live writer in the same repo, a link can be repointed between `readlink` and the shell's later dereference — the emitted string is a mutable indirection by design. Mitigation for a hostile live repo is the pinned lane (`--pinned`, or `pinned = true`).
- The resolver's `required` existence probe follows links and cannot distinguish a poisoned-but-resolvable link from a healed one — it runs strictly *after* heal.
- **Lock-free on the read side.** The reconciler recomposes every prompt for an active project (≈21 ms vs the 4.5 ms stat-only path); heal adds one `readlink` per entry and no lock. Only an observed mismatch acquires `lock_scoped` and repoints (atomic rename is idempotent under the content-addressed invariant, so concurrent healers converge).
- Write failure (read-only checkout, foreign-owned dir, lock timeout): **skip-and-continue** — compose digest paths for that run, exit 0, never block a prompt or an emit (D6). CI typically lands here and loses nothing.
- Crash mid-materialization: per-entry atomic renames keep each link individually valid; heal re-converges. No marker files, no lock-wedge class.

### GC

**No new GC surface.** Roots are already "digests pinned by any *registered* project's lock" plus the implicit `$OCX_HOME/ocx.lock` root (`clean::collect_project_roots`; ledger in `project/registry.rs`, `adr_project_gc_symlink_ledger.md`). Every link target is such a pinned digest, and toolchain links write **no `refs/symlinks/` back-refs** (ARCH-4c). GC never walks project trees; a stale link is a heal case, not a leak. Invariants: **materialization of a project tree registers the project** in the ledger; the **global tree registers nothing** — `register` is a no-op for `$OCX_HOME` by the no-self-link invariant (ARCH-1b), and the global lock is rooted implicitly. `$OCX_HOME/toolchain/` is declared **outside the GC graph** the way `ShimBinStore` and `locks` are ("never walked by `ocx clean`").

### Reconciler interplay (contract with `adr_shell_env_overhaul.md`)

- `.ocx/toolchain/` and a configured `toolchain-dir` root join `plan(..., owned_prefixes)` **additively** — the hook that record reserved by name; no change to its decisions. *(Amended 2026-09-05:* what joins is `<toolchain-dir>/<project-key>/toolchain/` for the consented in-scope project, never the bare root; and `$OCX_HOME/toolchain/bin` plus `ocx_install_bin_path` are **always in the desired set**, because `repair_owned_segments` deletes an owned segment the desired set omits.*)*
- Payoff: with linked entries, a digest bump leaves D byte-identical for link-resolved values, so the reconciler's retire/apply work on a version bump reduces to the shim-slot, mixed-token, and dep-interface residuals. Membership changes remain ordinary recomposes.
- **Stated residual — lost-ledger repair.** `plan`'s repair arm removes only segments under the *current* project's `owned_prefixes`; a stale `<projectA>/.ocx/toolchain/…` segment can survive a lost ledger while project B is active (the constant `$OCX_HOME` prefix has no such hole). A configured `toolchain-dir` root restores a constant prefix and closes it; the project-local default accepts it as the price of containment.

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

Default the lane to pinned (one seam: the `EnvScope::Project` field's default), delete `toolchain/` trees (self-contained, gitignored) and the `$OCX_HOME/toolchain/` store, drop the `pinned` key and `toolchain-dir` (both degrade silently on older binaries — no `deny_unknown_fields`). No wire format, metadata, or lock schema touched; `candidates`/`current` never involved.

## Technical Details (component placement)

- **Link facility** extracted from `SymlinkStore`, parametric on **root and link policy**: the home store keeps `ReferenceManager::link` (back-refs are its GC roots); toolchain trees use `symlink::replace_atomic` with the ARCH-4c containment policy and write no back-refs. Two instances of one facility, two policies.
- **`ToolchainStore`** — a `FileStructure` store for `$OCX_HOME/toolchain/`, **the global tree only** (always `fs.toolchain`, never a bare join; declared outside the GC graph like `ShimBinStore`). Project-rooted trees (default and `toolchain-dir` roots) are managed via `project/`, not as stores: *(amended 2026-09-05)* a project home is the **value** type `ToolchainHome`, resolved per project by `project::resolve_toolchain_home(project_dir, config)` to `<project>/.ocx/toolchain` or `<toolchain-dir>/<project-key>/toolchain`. `adr_toolchain_activation.md` § *On-disk layout grammar* and `system_design_toolchain_activation.md` § *Component responsibilities* state this in the same words.
- **Materializer** in `package_manager/` — input: resolved lock entries per group; output: links; registers the project; runs the atomic-rename repoint under `lock_scoped` only on mismatch.
- **Override + lane**: per-site `install_path` choice in `composer.rs` (table above); lane field on `EnvScope::Project` (`tasks/resolve.rs`); `options::Pinned` (`crates/ocx_cli/src/options/pinned.rs`) flattened into `ocx env` and `ocx exec`; `launcher/exec.rs` constructs its `EnvScope::Project` with the lane pinned.
- **Config** *(amended 2026-09-04)*: root-level `toolchain-dir` in `config.toml` + `OCX_TOOLCHAIN_DIR`, `Config::merge()` wiring, schema regen; `pinned` as an `ocx.toml` toolchain-level key + `OCX_TOOLCHAIN_PINNED`, resolved by a `LazyModeLadder`-shaped ladder. No `config/toolchain.rs`, no `[toolchain]` section, no `OCX_NO_TOOLCHAIN_LINKS`.
- **Docs surfaces** *(amended 2026-09-04)*: storage-layout page (`.ocx/toolchain/` beside the existing `.ocx/index/` entry, `$OCX_HOME/toolchain/`, `bin/`), `configuration.md` (`toolchain-dir`; `pinned` beside the existing `ocx.toml` toolchain-level keys; `bin` in the reserved-name list), `environment.md` (`OCX_TOOLCHAIN_DIR`, `OCX_TOOLCHAIN_PINNED`), env-composition reference (override table, lanes, mixed-stability rationale), `subsystem-file-structure.md` (ARCH-4c, new store) + `subsystem-package-manager.md` rules. The full enumerated set, including the trampoline and activation surfaces, is in `adr_toolchain_activation.md` § *Migration and Rollout*.

## Implementation Plan

Plan via `/hex-plan` after ADR approval, **jointly with `adr_toolchain_activation.md`** — the two records share a store, a config surface and a render pass, so they decompose as one work-package DAG (that ADR's § *Implementation Plan* is the merged list). Work packages from this record: link facility (containment-policy parametric); `ToolchainStore` + registration invariants; per-site override + `EnvScope::Project` lane (largest); materializer + heal-before-emit on the four composing emit paths; junction backend; `toolchain-dir` + `pinned` keys + env keys + schema; `options::Pinned`; docs. Acceptance tests are plain pytest — no shell/pty surface (the reconciler's matrix already covers prefix ownership).

## Validation

- Repoint demonstrated both states (old digest resolved before update, new after) — unchecked-green rule.
- Byte-identical `ocx env` output across a version bump for a fixture without `${deps.*}` or deferred roots; a mixed-token fixture and a deferred root's shim slot demonstrably change.
- Parity: `ocx env` vs `ocx exec` against the shared oracle in both lanes.
- Poisoned committed link: pre-seeded `.ocx/toolchain/` pointing elsewhere → healed before first emission on **each** of the four composing paths (red: emission without heal exposes the foreign target); on the reconciler, a consent-refused project's tree stays untouched; `required` probe observed to run after heal.
- Deferred root: link targets the package root before materialization; `${installPath}`-resolved values identical before/after first invocation; shim slot digest-pinned.
- `${installPath}`, `${self.installPath}`, `:native`/`:posix`, and the bare relative `Path` value resolve through the link at root sites; `${deps.*}` and `launcher exec` do not.
- `toolchain-dir` root: keyed per project, no collision across two projects, moved project's old keyed dir pruned via the ledger; `pinned = true`: no `<group>/<entry>` links written, digest paths emitted, `--pinned` no-op (`bin/` still rendered — amended 2026-09-04).
- Read-only checkout: heal skipped, digest fallback emitted, exit 0. Concurrent healers converge.
- Registration: materializing a project tree creates its `projects/` ledger entry (positive), the global tree creates none (negative); both survive `ocx clean` — the project via its lock pin, the global tree via the implicit lock root; no `refs/symlinks/` back-ref appears for any toolchain link.
- Windows: junction create/repoint/read-through without privilege; `.shim` files byte-unchanged.

## Links

- Research: `research_shell_env_reconciler_and_launcher_farm.md`
- **Amending record**: [`adr_toolchain_activation.md`](./adr_toolchain_activation.md) + its companion [`system_design_toolchain_activation.md`](./system_design_toolchain_activation.md)
- Related: `adr_shell_env_overhaul.md` (reconciler, shipped), `adr_interpolation_token_grammar.md` (grammar the override must cover), `adr_global_toolchain_tier.md` (D5), `adr_project_gc_symlink_ledger.md` (GC ledger, ARCH-1b), `adr_windows_exe_shim.md` (untouched contract), `subsystem-cli.md` (export/execute parity), `subsystem-file-structure.md` (ARCH-4b precedent)
- Issues: [#189](https://github.com/ocx-sh/ocx/issues/189), [#193](https://github.com/ocx-sh/ocx/issues/193), [#359](https://github.com/ocx-sh/ocx/issues/359)

## Changelog

| Date | Change |
|---|---|
| 2026-08-02 | Farm-store draft (`adr_toolchain_farm.md`) + adversarial review round 1 |
| 2026-08-03 | Farm rejected (owner review); home-keyed variant rejected (containment beats keys); rewritten as project-local toolchain tree |
| 2026-09-02 | Rewritten against the post-reconciler tree (review round 2): sibling → `adr_shell_env_overhaul.md`; override at the resolver input; following/pinned as a composition property; consent-backstop claim removed; `[toolchain] dir`/`links` knob; global tree = `ToolchainStore`; closed issues dropped |
| 2026-09-02 | Review round 3: links target the package root in both materialization states, shim slots stay digest-pinned and unlinked, no materialization trigger; ARCH-4c restated as a `ReferenceManager::link` carve-out (`validate_target` untouched); reconciler heals after consent; `ocx inspect` removed from emitters; lane as an `EnvScope::Project` field, `--pinned` only; per-site override table incl. synth entries and the `launcher exec` exclusion; TOCTOU + lost-ledger residuals stated; precedent cites corrected (`ShimBinStore`, direnv/uv); `[toolchain]` disambiguated; #193 scoped to global |
| 2026-09-02 | Review round 3 verification: ARCH-4c restated as `symlink::replace_atomic` + own containment policy, no back-refs (`ReferenceManager::link` has no containment check, canonicalizes, and would add GC roots); launcher re-entry constructs `EnvScope::Project` pinned; `--no-pinned` rationale corrected; positive registration check added |
| 2026-09-04 | **Amended by `adr_toolchain_activation.md`** (four changes, each marked inline). (1) **Layout** — `bin/` moves inside `toolchain/`, and `bin` becomes a reserved group *and* tool name, because the trampoline directory is now a sibling of `<group>/` and a future per-group `<group>/bin/` must stay possible. (2) **Config** — the proposed `[toolchain]` section never ships: `dir` becomes the root-level `config.toml` key `toolchain-dir` (unchanged meaning, still project-only, global home never moves), and `links = false` becomes the `ocx.toml` toolchain-level key `pinned = true` with `OCX_TOOLCHAIN_PINNED` as its weakest ladder tier, because following-versus-pinned is a property of the toolchain composed rather than a machine policy; one effect differs, `pinned` still renders `bin/`. `OCX_NO_TOOLCHAIN_LINKS` is not introduced. (3) **Trigger** — `ocx pull` renders the home, which is the fresh-clone story this record had no answer for; consent is not materialization, so no consent evaluation is involved. (4) **Consumer matrix row 3** — "linked bin dir" becomes the hook-prepended link-tree bin dirs *plus* launcher trampolines for every context where no hook runs. Sections not listed here are unaffected, and all three review rounds' outcomes stand. |
| 2026-09-05 | Review round 1 of the amending ADR, applied here: the Windows junction repoint is corrected from "atomic" to a documented remove-then-rename with a bounded non-atomic window whose convergence premise is cooperating ocx writers (residual R8); `ToolchainStore` is restated as the **global** tree only, with a project home as the `ToolchainHome` value type resolved by `project::resolve_toolchain_home`, in the same words the other two records now use; the `bin` reservation gains a strict group-name and `[tools]`-key charset validator; `$OCX_HOME/bin` is stated as non-existent rather than merely "not the trampoline directory". |
| 2026-09-05 | Cross-model gate on the amending ADR (Codex `sol`, one-shot), applied here: `toolchain-dir` is refused at parse unless it resolves inside `$HOME`/`%USERPROFILE%` or `$OCX_HOME`, and the `owned_prefixes` note is narrowed to `<toolchain-dir>/<project-key>/toolchain/` for the consented in-scope project while recording that `$OCX_HOME/toolchain/bin` and `ocx_install_bin_path` must stay permanently in the desired set. |
| 2026-09-05 | Owner decisions on the amending ADR, applied here: the env floor for `pinned` is spelled `OCX_TOOLCHAIN_PINNED` under the new naming rule (bare inside `ocx.toml`, noun-qualified outside it), and the name set the rendered tree exposes is the metadata-declared `binaries` ∪ `entrypoints` claims of the roots **and every interface-admitted dependency**, with no refusal or warning on any name collision. |
| 2026-09-06 | **Implementation corrections from the amending ADR, applied here (append-only, WP-13b).** Two statements this record made are false against what shipped, and both are corrected inline with the superseded text quoted. (1) **`toolchain-dir` expansion** — the managed-tier example offered `%LOCALAPPDATA%\ocx\toolchain`; a **leading `~` is the only expansion on any platform**, `%VAR%` never expands, and the unexpanded value is refused at parse (exit 78) as a non-absolute path. (2) **`pinned = true`** — the "each trampoline bakes a digest root … a pinned tree's trampoline bodies churn on every version bump" consequence is void under D-9: a body bakes only the home selector and the `ocx` install path, so every body is byte-identical whatever `pinned` says and a flip needs no re-render; what `pinned` selects is what the emitters compose, and the link pass is suppressed whole, prunes included. Full record: [`adr_toolchain_activation.md`](./adr_toolchain_activation.md) § *Amendment — 2026-09-06*. |
