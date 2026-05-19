# ADR: Global Toolchain Tier (`--global`, no implicit home fallback)

## Metadata

**Status:** Accepted
**Date:** 2026-05-15
**Deciders:** Architect (planning agent), Michael Herwig
**Beads Issue:** N/A
**Related Plan:** [`okay-we-are-currently-lexical-shannon.md`](file:///home/mherwig/.claude/plans/okay-we-are-currently-lexical-shannon.md)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024, no new dep; reuses install/select symlinks and the shell-activation surface.
**Domain Tags:** api | integration | dx
**Supersedes:** Amendment C of [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md) (home-tier-as-implicit-fallback)
**Superseded By:** N/A (activation surface, command taxonomy, and Decisions 3/4/6 re-anchored by [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md), 2026-05-16 — Decisions 1, 2, 5 still hold)
**Amends:** [`adr_cli_high_low_layering.md`](./adr_cli_high_low_layering.md) (layer table — adds the global tier and the strict-isolation rule)

> ## ⚠ SAFEGUARD — PARTIALLY SUPERSEDED. DO NOT IMPLEMENT FROM THIS FILE.
>
> The authoritative spec for activation, command taxonomy, and the
> global-env exporter is **[`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md)**
> (2026-05-16). From *this* ADR, only **Decisions 1, 2, 5** still hold.
> **Decisions 3, 4, 6, 7 are SUPERSEDED** and each carries an inline
> `[SUPERSEDED]` marker — they describe a model that was rejected
> (`install --global` sugar, `run` never sees global, per-prompt-hook /
> static-`init.<shell>` activation, downstream `setup.ocx.sh` website
> repo). A reviewer or cross-model adversary MUST treat the marked
> decisions as historical context, never as an implementation target.

## Context

`adr_project_toolchain_config.md` introduced `$OCX_HOME/ocx.toml` as a **Tier-4
implicit fallback**: `ConfigLoader::project_path()` walks CWD for `ocx.toml`
and, if nothing is found, falls back to `home_project_path()` →
`$OCX_HOME/ocx.toml`. Amendment C froze the composition rule as "project tier
*replaces* home tier in full" — one file wins, no merge — to protect
reproducibility (D1).

Two problems remain:

1. **Implicit fallback is itself a reproducibility hazard.** Even with
   wholesale replacement, the *presence or absence* of `$OCX_HOME/ocx.toml`
   silently changes what `ocx run`/resolution does when a developer is outside
   any project. Behaviour depends on machine-local state that is invisible from
   the command line — the exact "state-dependent semantics" the backend-first
   principle (`product-context.md` #1, `adr_cli_high_low_layering.md` driver
   "No layer-mixing surprises") rejects everywhere else.
2. **No first-class "my usual tools, everywhere" capability.** Users want an
   apt-like set of tools present in any shell ("just browsing"), *and*
   reproducible project builds. Today there is no command surface for the first
   need, and the implicit fallback is the wrong vehicle for it (invisible,
   non-explicit, entangled with project resolution).

The user articulated the tension explicitly: a global convenience set is
desirable (apt replacement), but if global tools ever leak into a project's
resolved environment, a collaborator gets a non-reproducible build. The
research is unambiguous on the resolution (see Industry Context): for a
reproducibility-first backend tool, the correct model is **hard isolation**
(Volta), not additive merge (mise/asdf). This ADR makes the global tier
**explicit and strictly isolated**.

### Why this is a One-Way Door Medium

`--global` flag name + semantics, the `$OCX_HOME/{ocx.toml,ocx.lock}` global
pair, the removal of implicit home discovery, and the shell-activation contract
for the global set all become contract embedders read. Pre-stable CLI
(`CLAUDE.md` "Current State"); breaking change is allowed now and cheap later
relative to post-stable. `project_breaking_compat_next_version.md` authorises
the clean break with no shim.

## Decision Drivers

- **Reproducibility (D1) is non-negotiable.** A project's resolved toolchain
  must depend only on its committed `ocx.toml`/`ocx.lock`, never on
  machine-local global state. This is the principle Amendment C protected; this
  ADR strengthens it by removing the implicit fallback entirely.
- **Explicit over implicit.** A backend tool must not switch behaviour on
  invisible filesystem state. Selecting the global toolchain must be an
  explicit, command-line-visible act (`--global`), discoverable from `--help`.
- **Reuse, don't duplicate.** The global toolchain is materialised by the
  *existing* install→`current` stable-symlink mechanism (`SymlinkStore`
  `current` + `refs/symlinks/` back-ref). No new store. Shell exposure reuses
  the existing `shell` activation surface, not a new subsystem.
- **Clean-env / hermetic execution (`product-context.md` #4).** `ocx exec` and
  `ocx run` must stay hermetic. The global set must never enter their resolved
  environment.
- **KISS.** Strict tiers (no composition) is fewer rules than gap-fill merge,
  and removes an entire class of "did a global tool leak in?" debugging.

## Industry Context & Research

Research artifact basis: `worker-researcher` survey (2026-05-15).

| Tool | Global↔project model | Project shadows global | Global leaks into builds? |
|------|----------------------|------------------------|---------------------------|
| **Volta** | **Hard isolation** — "Volta covers its tracks … your npm/Yarn scripts never see what's in your toolchain" | yes | **No (by design)** |
| Nix / devbox | Separate activation; `nix develop` replaces env | yes | No (hermetic) |
| mise / asdf / proto | Additive hierarchical merge | yes (if overridden) | **Yes** (global bleeds unless overridden) |

**Key insight:** mise/asdf's additive merge is precisely the reproducibility
hole Amendment C was written to close. The research recommendation is explicit:
*"If OCX grows a global tool set, follow Volta's model (hard isolation: global
never leaks into project builds) rather than mise's additive merge. For a build
tool backend, reproducibility is critical."* Shell-exposure prior art: the
**static sourced env file** (`~/.cargo/env`, Nix `profile.d/nix.sh`) is the
zero-overhead, CI-safe, install-script-friendly mechanism for making a global
set visible in a fresh shell — distinct from a per-prompt eval hook (mise) which
is interactive-only. OCX already has both surfaces (`ocx shell init` emits the
rc snippet; `ocx shell hook` is the per-prompt evaluator;
`ConfigLoader::home_init_path(shell)` already computes `$OCX_HOME/init.<shell>`
but nothing writes it yet).

This resolves the plan's open "exact global/project composition rule" item:
**there is no composition.** Strict isolation is the cited-correct answer.

## Considered Options

### Option 1 — Explicit `--global`, strict isolation, shell-only exposure (chosen)

`$OCX_HOME/ocx.toml` is reachable only via an explicit `--global` flag. It never
participates in project resolution or in `ocx run`/`ocx exec`. Its tools are
materialised as `current` install symlinks and surfaced to interactive shells
via the existing shell-activation surface when no project is in effect.

| Pros | Cons |
|------|------|
| Zero reproducibility hazard: project resolution depends only on committed files; global is structurally incapable of leaking into a build. | A new top-level flag on `ContextOptions`; `--help` grows by one. |
| Explicit & discoverable; no invisible state-dependent behaviour. | Users must learn one rule: "global = shell convenience; projects are authoritative and hermetic." Documented plainly. |
| Reuses install/select symlinks and the shell surface — net-small implementation, no new store. | The interactive global set is not auto-refreshed in a long-lived shell when the global toolchain changes (static-file model). Acceptable; the per-prompt hook path covers interactive refresh; full re-import-on-change is an explicit out-of-scope follow-up. |
| Matches Volta (cited-correct) and the cargo `~/.cargo/env` exposure shape. | |

### Option 2 — Keep implicit home fallback, add `--global` as an alias

| Pros | Cons |
|------|------|
| Smaller diff (fallback stays). | Keeps the invisible state-dependent behaviour (problem 1). Two ways to reach the same file (implicit + explicit) — exactly the ambiguity backend-first design rejects. |

### Option 3 — `--global` + gap-fill composition (global fills tools the project does not declare)

| Pros | Cons |
|------|------|
| "Convenient" — undeclared tools still available in project shells. | **Reintroduces the reproducibility hole.** A collaborator without the same `$OCX_HOME/ocx.toml` gets a different resolved environment for tools the project happens not to declare. This is the mise leakage model the research explicitly tells a backend tool to avoid. Rejected. |

## Decision Outcome

**Chosen Option:** **Option 1 — explicit `--global`, strict isolation,
shell-only exposure.**

### Decisions (binding)

1. **Remove implicit home discovery.** `home_project_path()` and its call from
   `ConfigLoader::project_path()` (Tier-4) are deleted. Project discovery is:
   explicit `--project`/`OCX_PROJECT` > CWD walk > **None**. There is no home
   fallback. This **supersedes Amendment C** (its "home tier is the fallback
   when no project found" premise no longer exists) and obsoletes the
   home-tier clauses of Amendment F (line 778) and the "Amendment C unchanged"
   note in Amendment A (line 687).

2. **`--global` is an explicit project-file selector.** New global flag on
   `ContextOptions` (`pub global: bool`), peer of `--project`/`--offline`.
   When set, the project tier in effect is `$OCX_HOME/ocx.toml` and its lock is
   `lock_path_for($OCX_HOME/ocx.toml)` = `$OCX_HOME/ocx.lock`. `--global` and
   `--project` are mutually exclusive (both pick a project file) → conflict =
   `UsageError` (64), enforced by clap `conflicts_with`. Flags precede
   positionals (project CLI convention).

3. **[SUPERSEDED → handshake §2, §7]** `--global` on the toolchain-tier
   mutators (`add`/`remove`/`lock`/`upgrade`/`run`) still holds. The
   `ocx install --global` sugar described here is **rejected**: `install`
   is OCI-tier, moves under `ocx package`, and never touches any
   `ocx.toml`. Do not implement the sugar.

   **[Amendment 2026-05-17 — root-only collapse]** The per-command
   `--global` surface has been further collapsed: `--global` is now a
   **single root flag** on `ContextOptions`, peer of `--project`, declared
   once via clap `long, conflicts_with = "project"`. Per-command `--global`
   flags and the `with_command_global` reconcile seam are **deleted**.
   `check_global_project_exclusivity` (in `crates/ocx_cli/src/app/context.rs`,
   called from `Context::try_init`) closes the env-sourced gaps (`OCX_GLOBAL`
   default value, `OCX_PROJECT` env) that clap `conflicts_with` cannot see.
   `ProjectConfig::resolve` global arm survives unchanged. Canonical CLI
   form is now `ocx --global <subcommand>` — e.g. `ocx --global add ripgrep:14`.
   ~~`--global` is accepted on project-tier and mutator commands: `add`,
   `remove`, `lock`, `upgrade`, `pull`, `run`. `ocx install --global <pkg>`
   is defined as sugar = `ocx add --global <pkg>` + re-lock + install and
   `select`.~~

4. **[SUPERSEDED → handshake §5]** No-composition / strict tiers still
   holds, but the framing "`run` never sees global" is softened:
   `ocx run --global -- cmd` *explicitly* composes the global toolchain
   for that child only (single tier, no merge, no shell mutation). Bare
   `ocx run` is project-only. Implement per handshake §5, not this text.
   ~~The global toolchain never composes into project resolution; `ocx run`
   and `ocx exec` never consult `$OCX_HOME/ocx.toml`.~~

5. **Materialisation via existing install→`current` symlinks.**

   > **AMENDMENT 2026-05-19 (supersedes D5's resolution + GC coupling).**
   > The original D5 made `ocx --global env` resolve each tool through its
   > `current` symlink and relied on `current` install back-refs as the *only*
   > GC root for global packages. Both are reversed:
   >
   > - **Resolution = lock-pinned digest, offline.** `ocx --global env`
   >   resolves each `$OCX_HOME/ocx.lock` tool by its **pinned digest**
   >   against the local object store (`resolve_global_pinned_env`) — exactly
   >   the project-tier model. The `current` symlink is a **separate
   >   abstraction**, mutated only by install/uninstall/select, targeted at
   >   devcontainer / IDE stable-anchor use. It is NOT consulted by env, so
   >   `ocx --global upgrade` re-pins the lock and the exported env follows
   >   immediately with no select step. A pinned tool not materialised
   >   locally is silently skipped (login exporter must never block a shell).
   > - **GC is not a global-tier exception.** Global lock-pinned packages are
   >   kept reachable by an **implicit** `$OCX_HOME/ocx.lock` root added in
   >   `tasks::clean::collect_project_roots` — the global tier is the project
   >   tier with a different load site, and GC treats it identically. The
   >   global file still gets **no `$OCX_HOME/projects/` self-link** (its
   >   project dir is `$OCX_HOME`, barred by
   >   `adr_project_gc_symlink_ledger.md`); reachability comes from the
   >   implicit lock scan, not from `current` back-refs. No new storage.
   >
   > Original D5 text retained below for rationale history.

   ~~Global tools
   become `current` selections through the unchanged install/select path
   (`SymlinkStore::current` + `ReferenceManager` `refs/symlinks/` back-ref).
   They are GC roots by the existing mechanism. The global file's own project
   directory is `$OCX_HOME`, so per `adr_project_gc_symlink_ledger.md` it gets
   **no `$OCX_HOME/projects/` self-link** — it is protected purely by its
   `current` install symlinks. No new storage.~~

6. **[SUPERSEDED → handshake §4, §7] DO NOT IMPLEMENT.** The entire
   per-prompt-hook + static `$OCX_HOME/init.<shell>` activation model
   below (including the "Non-interactive coverage" correction) is
   rejected. Activation is a stateless `ocx env --global --shell=<S>`
   exporter sourced from a thin installer-generated env file; `ocx shell
   hook` and `ocx shell init` are deleted. Use handshake §4 only.
   The strikethrough text is retained for rationale history.

   ~~**Shell exposure of the global set.**~~ The existing shell-activation surface
   (`ocx shell hook`, the per-prompt evaluator described in
   `adr_cli_high_low_layering.md` line 231) is extended: **when no project
   `ocx.toml` is in effect**, the hook additionally emits the global toolchain's
   `current` set. Entering a project, the project's activation output replaces
   it (project authoritative — consistent with strict isolation; the global set
   is shell-ambient, never merged into project resolution). The static
   install-script entrypoint (`ocx shell init` snippet; the
   `$OCX_HOME/init.<shell>` path `home_init_path` already computes) is the
   stable surface the OS install script sources, analogous to `~/.cargo/env`.

   **Non-interactive coverage (review correction, SOTA-2a).** The per-prompt
   hook is the mise-`activate` shape and **never fires** in non-interactive
   shells (CI `bash --norc`, `bash -c`, editor-spawned terminals). Prompt-hook
   exposure alone is therefore insufficient for the apt-replacement goal.
   `$OCX_HOME/init.<shell>` MUST be a **static PATH-prepend** entrypoint
   (cargo `~/.cargo/env` / Volta shim-dir shape) that adds the global
   `current` bin dir to PATH *without* invoking the hook — sourced
   non-interactive shells then see global tools. The per-prompt hook only
   layers dynamic project switching on top. The static entrypoint uses POSIX
   `.` (not bash `source`) for dash/sh CI compatibility (SOTA-2b).

7. **[SUPERSEDED → handshake §4] DO NOT IMPLEMENT.** The "downstream
   `setup.ocx.sh` website repo" framing is **invalid** — that repo does
   not exist and is not planned now. Per handshake §4 the **in-repo OCX
   install script** itself modifies the user's shell rc/profile to
   `source` a thin generated `$OCX_HOME` env file that `eval`s
   `ocx env --global --shell=<S>`. No separate repo, no `$OCX_HOME/init.<shell>`
   static render.
   ~~Install-script integration is a contract implemented downstream in the
   `setup.ocx.sh` website repo; in-scope = provide `$OCX_HOME/init.<shell>`.~~

### Quantified Impact

| Metric | Before | After |
|--------|--------|-------|
| Ways to reach `$OCX_HOME/ocx.toml` | 1 implicit (fallback) | 1 explicit (`--global`) |
| Invisible state-dependent resolution paths | 1 (home fallback) | 0 |
| New CLI surface | — | 1 global flag `--global` |
| Global-leaks-into-build risk | possible outside project | 0 (structurally impossible) |
| New storage subsystems | — | 0 (reuses install/select + shell surface) |

### Consequences

**Positive:**
- Project builds depend only on committed files — strongest reproducibility
  posture; closes the residual hole Amendment C left.
- A first-class apt-like global toolchain without any composition risk.
- No new store, no new subsystem; net-small change reusing proven surfaces.

**Negative:**
- Behavioural break: scripts/users relying on the implicit `$OCX_HOME/ocx.toml`
  fallback must add `--global`. Acceptable pre-stable; CHANGELOG + user-guide
  call-out.
- Two "run a tool" mental models persist (`exec` OCI-tier, `run` project-tier)
  plus the global-shell-convenience model; mitigated by one plainly-documented
  rule.

**Risks:**
- **User expects global tools inside a project shell.** By design they are not
  in the *resolved/hermetic* env, but the interactive shell PATH (outside a
  project) does carry them. Doc must state the boundary explicitly: "global =
  interactive convenience outside projects; inside a project the project
  toolchain is authoritative; `run`/`exec` are always hermetic."
- **`--global` + `--project` both given.** Hard `UsageError` (64) via clap
  `conflicts_with`; no precedence guessing.

### How Would We Reverse This?

One-Way-Door Medium. Reversal = restore `home_project_path()` (small, localised
in `loader.rs`) and/or drop `--global`; one CHANGELOG + user-guide line; no
shim. Bounded: a few files in `loader.rs`/`ContextOptions`/command flags.

## Technical Details

### Discovery (loader.rs)

Delete `home_project_path()` and its Tier-4 call. `project_path()` returns
`None` when explicit/env/CWD-walk all miss. `--global` does not flow through the
CWD walk at all: `ProjectConfig::resolve` selects `$OCX_HOME/ocx.toml`
directly when `context.global()` is set (peer to the explicit `--project`
branch), with `lock_path = lock_path_for(that)` = `$OCX_HOME/ocx.lock`.

### CLI plumbing

`ContextOptions { …, pub global: bool }` (`clap` `long`, `conflicts_with =
"project"`). `Context` exposes `global()`. Project-tier prologues
(`load_project_with_lock`, `load_project_for_mutate` in
`crates/ocx_cli/src/app/project_context.rs`) branch on `global()` to resolve
the `$OCX_HOME` file instead of the CWD walk; downstream logic
(`MutationGuard`, `compose_tool_set`, `expand_all_keyword`) is unchanged — it
operates on whichever `ProjectConfig`/`ProjectLock` it is handed.

### Strict isolation enforcement

- `ocx exec`: unchanged (reads no `ocx.toml`).
- `ocx run`: reads the in-effect project file only. Without `--global` inside a
  project, that is the project file; the global file is never read. No
  gap-fill, no union with global — `compose_tool_set` is fed exactly one tier.
- Acceptance test asserts a project `run` cannot see a tool that exists only in
  `$OCX_HOME/ocx.toml`.

### Shell activation

`ocx shell hook` (per-prompt) gains: if no project `ocx.toml` resolves for the
CWD, emit exports for the global toolchain's installed `current` set; on
entering a project, the project activation output supersedes (no merge —
strict isolation holds at the shell layer too). `ocx shell init`'s emitted rc
snippet is unchanged in shape; it is the stable thing the OS install script
sources.

### Amendment to `adr_cli_high_low_layering.md`

Add a "GLOBAL TIER" row to the layer table: *`--global` re-targets
project-tier/mutator commands to `$OCX_HOME/ocx.toml`; shell activation emits
the global `current` set only when no project is in effect; `run`/`exec` are
hermetic and never read the global file.* Add a changelog row dated 2026-05-15
noting the global tier + removal of implicit home discovery.

## Validation

- [ ] `$OCX_HOME/ocx.toml` is **not** discovered without `--global` (unit:
      `project_path` returns `None`; regression for deleted `home_project_path`).
- [ ] `ocx --global add` / `ocx --global lock` writes `$OCX_HOME/ocx.toml` + `ocx.lock`
      and materialises the pinned package locally. (`install --global` no longer
      exists. Per D5 amendment 2026-05-19, env resolution is lock-pinned-digest,
      not `current`-symlink; `ocx --global upgrade` takes effect with no select.)
- [ ] Fresh shell sees the global lock-pinned tool; entering a project shadows
      it with the project's tool; leaving restores global. `ocx clean` does not
      reap a global lock-pinned package (implicit `$OCX_HOME/ocx.lock` root).
- [ ] Project `ocx run` cannot resolve a tool present only in
      `$OCX_HOME/ocx.toml` (hermetic; strict isolation).
- [ ] `--global` + `--project` ⇒ `UsageError` (64).
- [ ] No `$OCX_HOME/projects/` self-link for the global file (cross-check with
      `adr_project_gc_symlink_ledger.md`).
- [ ] Documentation: user guide states the global-vs-project boundary and the
      hermetic-`run`/`exec` rule; command-line reference documents `--global`;
      `adr_cli_high_low_layering.md` layer table + changelog amended.
- [ ] `task --force verify` final gate.

## Links

- Supersedes: Amendment C of [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md)
- Amends: [`adr_cli_high_low_layering.md`](./adr_cli_high_low_layering.md)
- Sibling: [`adr_project_gc_symlink_ledger.md`](./adr_project_gc_symlink_ledger.md) (no-self-link for the global file)
- Discovery / env: `crates/ocx_lib/src/config/loader.rs` (`home_project_path`, `home_init_path`, `project_path`)
- CLI: `crates/ocx_cli/src/app/context_options.rs`, `app/project_context.rs`, `command/{add,remove,lock,upgrade,pull,run,install}.rs`
- Shell: `crates/ocx_cli/src/command/shell_*.rs`
- Plan: [`okay-we-are-currently-lexical-shannon.md`](file:///home/mherwig/.claude/plans/okay-we-are-currently-lexical-shannon.md)
- Project memory: `project_setup_ocx_canonical_install.md`, `project_breaking_compat_next_version.md`, `feedback_extend_dont_duplicate.md`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-15 | Architect (Opus) | Initial — explicit `--global` tier, strict isolation (no composition), shell-only exposure; remove implicit home fallback (supersedes Amendment C); amend high/low layering ADR. |
