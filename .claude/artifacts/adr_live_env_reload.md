# ADR: Live Env Reload — Typed Shell-Environment Reconciler

## Metadata

- **Status**: Superseded by `adr_shell_env_overhaul.md`
- **Date**: 2026-08-02
- **Deciders**: Owner + Principal Architect session
- **GitHub Issues**: [#170](https://github.com/ocx-sh/ocx/issues/170) (native shell hook), [#26](https://github.com/ocx-sh/ocx/issues/26) (idempotent PATH — **already implemented**: `utility::path::move_to_front` + per-shell `Shell::export_path`, `adr_idempotent_path_move_to_front.md`; issue stale-open, recommend closing), [#265](https://github.com/ocx-sh/ocx/issues/265) (unset directive — enabled, deferred), [#148](https://github.com/ocx-sh/ocx/issues/148) (deferred "live in-session env reload"), [#152](https://github.com/ocx-sh/ocx/issues/152) (shell-activation test coverage)
- **Tech Strategy**: ☑ aligned (Rust 2024; serde_json already in tree; no new deps)
- **Domain Tags**: cli, setup, shell, package-manager, security
- **Supersedes**: the `OCX_PATH_BACKUP` conda-style stash/restore sketch in `adr_self_setup.md` "Future Work" (this file is the sibling ADR reserved there as `adr_live_env_reload.md`)
- **Amends**: `handshake_toolchain_cli.md` **§2** (`ocx shell hook` deletion row, line ~86) and **§7/§7a** (reconciliation register); `adr_idempotent_path_move_to_front.md` capture invariant (re-scoped, see Decision 6)

## Context

Shell-session environments are shared mutable stores with multiple writers: ocx activation, the user (`export`), other tools (nvm, virtualenv, starship), RC files. OCX today has three env surfaces and no lifecycle for any of them:

- `ocx self activate` — global scope, evaluated once per shell start from the `env.*` shims; never re-evaluated, so a global-toolchain change or self-update leaves every running shell stale ([#148]'s explicitly deferred item).
- `ocx direnv export` — project scope, stateless, bash-only, direnv owns enter/leave; requires direnv installed and `direnv allow` ceremony; installs by default on evaluation.
- `ocx run --` / `--ci` — one-shot, no session.

Pains: post-`ocx update` staleness in the *same* shell; project switching needs direnv; Windows (PowerShell) has no project story; env-var pollution (`OCX_ENV` JSON blob visible in every `env` dump).

Phase 1 (`adr_project_toolchain_links.md`) makes composed toolchains **stable-addressed**: per-entry toolchain links serve PATH dirs *and* dir-valued constants (`JAVA_HOME` class), so `ocx update` changes zero emitted env bytes. What remains for a session mechanism: env-declaration *shape* changes, dep-path churn (digest-pinned by design), `[env]`-contributed PATH dirs, scope enter/leave, link heal for the active scope.

`handshake_toolchain_cli.md` §2 deleted the old `ocx shell hook` command as "stateful per-prompt `_OCX_APPLIED` diff; **redundant with direnv (project) + the login exporter (global)**". This ADR amends that decision deliberately: the redundancy premise is exactly what this initiative removes (replacing direnv is the point), and the reconciler below has none of the stateful-guard-variable defects the deletion targeted.

## Decision Drivers

- **D1 — correctness under concurrency**: never clobber foreign writes (the direnv `#82`/`#1249` class); revert must commute with other tools' PATH edits.
- **D2 — convergence over atomicity**: shell-land has no transactions; every prompt re-converges; races self-heal.
- **D3 — no ceremony**: no `direnv allow`-per-edit; consent bounded to real risk.
- **D4 — hook must never break a prompt**: malformed state degrades, warns once, exits 0; `set -u`-safe by construction.
- **D5 — per-prompt cost**: no-op path = a handful of `stat` calls, target < 5 ms (mise ships ~4 ms on the same mtime-check shape; research §1).
- **D6 — wire-format firewall**: package metadata `Env` (published artifacts, read-path compat) is untouchable; all new semantics live in session-internal state.
- **D7 — per-shell mechanism honesty**: PowerShell and nushell are first-class but divergent; claims of parity require demonstrated primitives (no pretending nushell can `eval`).

## Industry Context & Research

Full survey: `research_shell_env_reconciler_and_launcher_farm.md`. Load-bearing:

- **mise validates the typed-diff hook**: `EnvDiffOperation::{Add,Change,Remove}` in `__MISE_DIFF`, session fast-path ~4 ms via **mtime checks** — "typed diff over byte snapshot" is shipping practice, not novel risk. But mise stores value pairs, not provenance; its `__MISE_ORIG_PATH` ownership heuristic patches exactly the gap our ledger closes structurally.
- **direnv's untyped `{Prev,Next}` model** is the design-against case: restore-by-value clobbers concurrent edits; leave-cleanup best-effort by its own admission (#798).
- **Trust lessons**: mise's path-keyed trust produced a real CVE (settings loaded before the trust check — GHSA-436v-8fw5-4mj8); direnv's content-hash allow is the reference for file-based trust. Consent must be evaluated before the project file influences behavior, and must not be path-keyed alone.
- **PowerShell**: wrap the `prompt` function, never clobber. **Nushell**: no `eval`; static generated file, regenerated on logic change.

## Considered Options

### Option A — Keep direnv as the only project-session mechanism

| Pros | Cons |
|---|---|
| Zero new code | Windows story stays absent; direnv install + allow ceremony; global scope still stale in-session; `.envrc` indirection remains |

### Option B — Conda-style stash/restore (`OCX_PATH_BACKUP`, the `adr_self_setup.md` sketch)

| Pros | Cons |
|---|---|
| Simple mental model | Byte-snapshot semantics: restoring the stash clobbers every foreign PATH edit made since activation (direnv #82 class); no per-key ownership; constants not covered by the sketch at all |

### Option C — direnv-style untyped diff, natively

| Pros | Cons |
|---|---|
| Proven shape | Same clobbering class as B for concurrently-edited keys; whole-env diffing captures foreign changes into our diff; typed knowledge OCX already has (Path/Constant) is thrown away |

### Option D — Typed three-way reconciler with provenance ledger **(chosen)**

| Pros | Cons |
|---|---|
| Commutes with foreign edits (list-element algebra); constants CAS-guarded on exit; only touches keys in desired ∪ ledger — foreign vars structurally invisible | Most design-novel option (mitigated: primitives are mise-validated; novelty is provenance tagging — research §c) |
| Update propagation = each session's local reconcile; no coordination | Ledger format to maintain (session-internal, freely changeable — D6) |

## Decision Outcome

**Option D** — a reconciler driving current env **C** toward desired env **D** using last-applied ledger **L**, per prompt, per scope stack.

### Decision 1 — Reconciliation model

Per key, three values: **D** (composed from toolchain files now), **C** (current shell env), **L** (ledger: what ocx last wrote + captured priors).

**List-kind vars** (PATH, `LD_LIBRARY_PATH`, any colon-list): element operations only.
- Apply: ensure desired elements present, front, in order — the shipped move-to-front primitive (`Shell::export_path`, `utility::path::move_to_front`).
- Revert: **remove our elements**, never restore an old string. Element removal commutes with foreign prepends/appends. Delete-if-found; absence is not an error. This is a **new named `Shell` primitive — `Shell::remove_path_element`** — routed through the same per-shell `escape_value` machinery, with the full 10-shell idempotency matrix and the already-documented hazards of this idiom class (zsh glob over-match, bash `${//}` pattern escaping — `adr_idempotent_path_move_to_front.md` Risks).
- Ownership: elements under `$OCX_HOME` or the scope's `.ocx/toolchain/` tree are self-identifying; arbitrary elements (project `[env]` additions) are listed in the ledger. Prefix test doubles as the repair path when the ledger is lost.
- With phase 1, D's store-rooted PATH block is a set of *stable* toolchain-link dirs — the common reconcile outcome is "already present, no-op"; PATH work remains only for membership and decl-shape changes.

**Constant-kind vars**: overwrite-on-apply, guard-on-exit.
- Apply/update: set to D unconditionally (project wins while inside the project; no mid-session ownership bookkeeping). Entering a scope records prior state (`Unset` | previous value) in L.
- Exit/removal: restore prior **only if C == L** (one comparison); else leave C — never clobber a user's mid-session override on the way out (the most-hated direnv behavior).
- Coincidence (C ≠ L, C == D): claim silently, prior = C.
- Unset-as-desired: representable in the ledger (prior capture + restore); blocked only on config syntax — [#265], deferred.

Keys outside D ∪ L are never read or written: foreign variables are structurally invisible to the reconciler (dodges direnv's whole-env capture).

### Decision 2 — Ledger: `__OCX_ENV_STATE`, in the environment itself

One env var: `{ fingerprint, scopes: { global: {applied, priors}, project?: {key, applied, priors} } }` — `applied` reuses the existing `Entry {key, value, kind}` serialization (D6: no second schema).

- Env placement is **correctness**, not preference: the ledger needs process lifetime + inheritance. A nested shell inherits applied env and matching ledger atomically; cd-to-project-B in the subshell reconciles correctly while the parent stays consistent. PID/session-keyed disk state breaks exactly there (research: pyenv lock-wedge is the on-disk cautionary tale).
- **Size bound**: compact JSON, hard cap 8 KiB encoded (Windows caps a single env var at 32,767 chars; children inherit the block). Over-cap degradation, in order: drop list-element records (lists repair via ownership prefix) → drop priors (exit restores nothing it can't prove) → operate ledger-less in repair mode. Compression (mise: MessagePack→zlib→base64) is a follow-up option, not v1.
- Malformed/absent ledger: treat as absent, warn once, repair lists via ownership prefix, leave constants in place (never guess-unset `JAVA_HOME`) — D4.
- **All warnings and the change summary are emitted as `printf … >&2` statements inside the eval'd script** — the shims discard the binary's own stderr (`2>/dev/null`, `shims.rs:63,103,105,239`), so the script body is the only reliable user-visible channel on the startup path. stdout of the binary remains exclusively the script.
- `set -u` discipline is binding: every read of the ledger and hook state uses default-expansion (`${__OCX_ENV_STATE-}` and per-shell equivalents); the var is unset on the first prompt *by construction*. A `set -u` shell test is part of the matrix.
- Double-underscore prefix from birth; hidden from diff printing like `DIRENV_*`.

**Ledger integrity (spoof channel closed).** Today `is_reserved_ocx_key` gates only `ocx run --env`, project/group `[env]`, and the `OCX_ENV` decode — **package metadata env is ungated** (`Var.key` is a plain `String`), so a published package could declare a constant named `__OCX_ENV_STATE` and have the emitter export it: forged priors would restore attacker-chosen values on scope exit, a forged fingerprint would pin the fast path forever. Therefore: `package create` (write path) hard-rejects `OCX_*`/`__OCX_*` env keys; compose/emit (read path) **skips such keys with a warning** — read-path compat for anything already published, no hard failure. Additionally, every ledger-derived value emitted into a script passes the same `escape_value` path as composed entries (no raw text reaches `eval`), and ledger decode is strict/fail-closed (unknown kind ⇒ whole payload treated as absent — the existing `OCX_ENV` posture).

### Decision 3 — Scope stack & fingerprint

- **D = ordered concat**: project block first, global block behind (overlay; a user's global tools stay reachable unless shadowed). Resolution isolation is untouched — global still never composes *into* project resolution (`adr_global_toolchain_tier.md`); the overlay is shell-level concatenation only.
- **Fingerprint watch set** (matches and extends what `.envrc` already watches): project `ocx.toml` + `ocx.lock`, global `$OCX_HOME/ocx.toml` + `ocx.lock`, managed-config snapshot, ocx version, project dir. `[env]` is read independently of the lock ("a declared `[env]` applies on its own authority" — `toolchain_env.rs:490-515`), so watching locks alone would miss `[env]`-only edits.
- **Fast path = mtime+size** of the watch set (mise's shape); content hashes are computed only after an mtime/size change, to decide whether recomposition is actually needed. The CWD upward walk to locate the project is a separate, bounded cost on every prompt (stops at `.git`/`OCX_CEILING_PATH`).
- Leaving a project reverts the project ledger section only. Global section reverts only on hook disable.
- Cross-shell update propagation = each session reconciling independently from its own L. No coordination channel.

### Decision 4 — Delivery: `self activate`, wrapper function, shims

No new command group. Three emission layers, all owned by `ocx self activate`:

1. **Startup** (shims call it today): emits completions + own-bin PATH + hook installation when enabled — prompt hook (bash `PROMPT_COMMAND`, zsh `precmd`, fish prompt event, PowerShell `prompt` **wrapped never clobbered**, nushell `env_change.PWD` in the static regenerated file) + the `ocx` **wrapper function** (interactive shells only). On PowerShell, hook/wrapper emission **must follow the completion block** — clap_complete's pwsh output opens with `using namespace`, which `Invoke-Expression` accepts only as the first statement (`activate.rs:126-135`).
2. **Per prompt**: the hook re-invokes `self activate` with a hidden plumbing flag → mtime fast path → reconcile script on change. Replaces the current nested `ocx --global env` shell-out with the reconciler (one mechanism, both scopes).
3. **Wrapper function**: runs the real binary, then fingerprint-check + eval before returning — `ocx update && cmake --build .` sees the new env within the same command line (no prompt boundary), and it is the only possible host for same-shell refresh semantics: a child process can never mutate its parent's environment. Non-interactive shells and scripts always hit the real binary (functions don't propagate).

**Interactivity is decided shell-side, never guessed.** The binary's `is_terminal` probe is structurally false on the POSIX startup path (shims eval with `2>/dev/null`, so stderr is never a TTY — `activate.rs:102` + `shims.rs:62-64`); the fish shim already passes an explicit flag for exactly this reason (`status is-interactive`, `shims.rs:98-106`). Hook emission follows the same shape: each shim probes itself (`$-` on POSIX, `status is-interactive` on fish, `[Environment]::UserInteractive` on pwsh) and passes `--hook`/`--no-hook` explicitly.

**No emitted snippet ever calls bare `ocx`.** The wrapper function is named `ocx`, and `command -v ocx` finds functions — a bare call inside the emitted stream (today: `activate.rs:220-222` emits `eval "$(ocx --global env …)"`) would execute the wrapper *inside a command substitution* and capture its output into the env stream. Every ocx-emitted call site uses the resolved absolute binary path (the `$_ocx_bin` pattern the shims already use), and a regression test asserts the wrapper is unreachable from the activation stream.

Delivery vehicle: the `env.*` **shims, never the profile fences** — shims are refreshed by every `self update` (`refresh_shims`), fences are frozen one-liners with heal-only lag. `SHIM_CONTRACT_VERSION` (reserved in `setup.rs`) gets its first consumer. **Rollout lag is real and stated**: `refresh_shell_integration_after_swap` runs in the *old* binary (`update.rs:106-108`), so the update that first ships hook-bearing shims does not deliver them — default-on reaches existing users on the **second** `self update`, or immediately via `ocx self setup` re-run. Nushell: static file regenerated by `self setup`/`self update` (research pitfall 10); its **removal primitive is unproven** — the shipped nu integration is apply-only (`NU_ENV_APPLY_LOOP`), and `hide-env` scoping inside hook blocks is a known hazard: the nushell work package starts with a red+green spike of element-removal/unset on a real nushell before any parity claim (D7).

Change printing: one summary line (`ocx: +JAVA_HOME ~PATH −PYENV_ROOT (acme, lock a1b2c3)`) as a `printf >&2` statement inside the script (see Decision 2). Verbosity `silent | summary | full`.

### Decision 5 — Enablement ladder & `[shell]` config section

```
1. CLI paired flags   --hook / --no-hook          (overrides_with, last-wins — completions pattern; set by the shims from their own probe)
2. Env kill-switch    OCX_NO_HOOK                 (negative-only, house NO_* style)
3. Config             [shell] hook = true|false    (persistent; ocx self setup --no-hook writes it)
4. Default            on, interactive shells only (interactivity = shim-probed, Decision 4)
```

New `[shell]` config section (`hook`, `completion` — singular, matching the `--completion` flag — retrofitted for symmetry, `hook_log`): follows `Config::merge()` fold, schemars pipeline (`schemas/config/v1.json`), **no `deny_unknown_fields`** (fleet forward-compat: older ocx reading a config with `[shell]` degrades silently).

**Fast-path discipline**: `self activate` is deliberately `Context`-free ("must not pay the full `Context::try_init` cost" — `activate.rs:64-72`), and the completions gate is a pure function today. The `[shell]` config rung is therefore read **once at shell-start emission** (a bounded local-file read, budgeted; the managed snapshot is a local file by design), and the **per-prompt path reads no config at all** — flags, env, and ledger fingerprint only. Hook presence in a session is decided at startup; per-prompt work is reconciliation, which loads config only when the fingerprint already demands recomposition.

**Managed-tier eligibility is an open decision (OD-2)**: `fold_managed_tier` strips only the payload's own `[managed]` section — any other section merges, so a fleet-published config could toggle every host's per-prompt hook. Recommendation: declare `[shell]` **local-tier-only** and strip it from the managed payload alongside `[managed]`.

### Decision 6 — Doctrine amendments

- `handshake_toolchain_cli.md` **§2** deleted `ocx shell hook` as "stateful per-prompt `_OCX_APPLIED` diff; redundant with direnv (project) + the login exporter (global)". **Amended**: the redundancy premise falls with direnv replacement, and the reconciler eliminates the stateful-guard defect (idempotent by construction, no `_OCX_APPLIED`-class variable — regression-pinned already). §7/§7a's reconciliation register gains this ADR's rows. §4's "one env file per shell family, always current" survives unchanged.
- `adr_idempotent_path_move_to_front.md` capture invariant ("emitted snippets may not depend on ocx at eval time"): **re-scoped, not repealed**. It continues to bind every *exported env statement* (`export_path` output, `--ci` lines, shims' static parts). The hook body is explicitly a different surface: it calls ocx each prompt *by definition*, and must degrade to a silent no-op when the binary is missing (probe guard, same posture the shims already have).

### Decision 7 — Consent model (security)

Threat: `ocx.toml` references arbitrary OCI registries; silent activation on cd into a fresh clone puts attacker-controlled binaries in front of `cmake`, `cargo`, `git`. Install executes no package code (verified — no hooks, no scripts), so risk materializes at first tool invocation — which is guaranteed. Default-on hook therefore requires:

- **The hook never installs.** Compose-only, from lock + local store; missing tools → one hint line. (Distinct from `direnv export`, which pulls by default under direnv's allow gate; that command keeps its semantics for direnv users.)
- **Never activate an untouched project.** Consent = any explicit project-scoped ocx command (`add`, `remove`, `lock`, `update`, `pull`, `run`) — recorded as a stamp in `StateStore` (`state/activation-consent/<project-key>`, pure-associated-fn path helper so the pre-`Context` `self activate` path can read it — the `managed_config_snapshot_path` precedent). Fresh clone → hint line, zero env change.
- **Stamp binds path AND project identity, re-confirms on drift.** The stamp stores the consented **source set** (registries + namespaces from the lock) and a **project identity** (first-consented tool-name set). Activation re-confirms when the current lock's source set is **not a subset** of the consented set (so a same-cardinality swap `ghcr.io/acme → ghcr.io/evil` triggers — "growth" alone is not the predicate) or when identity diverges (guards path reuse: delete a consented repo, clone a different one at the same path — path-keyed-only trust is mise's CVE shape, research pitfall 1).
- **Sequencing (mise CVE lesson)**: the stamp is read before any project file influences *configuration*; computing the source-set comparison necessarily parses the current lock — a lock that is unreadable or unparseable means **no activation** plus one hint line. Config load happens strictly after the stamp check.
- **Residual risk, stated plainly (OD-3)**: within a consented namespace, anyone who can publish (publisher compromise) gets PATH-front code at the victim's next prompt with no signal — digest bumps inside consented sources ride silently by design. Tightening to digest-set re-confirm would fire on every legitimate `git pull` lock bump. Recommendation: accept + document; owner call.
- Ledger hardening per Decision 2 (reserved-key gate, escape-everything, fail-closed decode).

### Decision 8 — `__` namespace cleanup

`OCX_ENV` → `__OCX_ENV` (docs already say "Managed by OCX; not set manually"). **Mixed-version caveat (OD-4)**: `OCX_ENV` is the forwarding channel between an *outer* ocx and the *inner* one a launcher spawns, and `OCX_BINARY_PIN` can pin the inner binary to an **older** release — a rename breaks that channel silently, reverting project overrides to package values (the exact failure the forward exists to close). Options: one release of dual-name **read** on the decode path (write new name only) — the read-path-compat exception class this repo already sanctions — or declare mixed-version launcher re-entry unsupported with an enforcing assertion. Recommendation: dual-read for one release. `OCX_PATCH_SNAPSHOT` stays user-facing. `OCX_PATCHES` audited during implementation (forward-channel-shaped; possibly CI-set like `OCX_MIRRORS`). `OCX_CEILING_PATH` gets its missing `environment.md` entry.

### Quantified Impact

| Metric | Before | After |
|---|---|---|
| No-op prompt cost | n/a (direnv: its own hook) | stat-only watch-set check, < 5 ms target (mise ships ~4 ms on the same shape) |
| Post-`ocx update` staleness, same shell | until new shell | next prompt (env decls) / next spawn (binaries, phase 1) |
| Project env on Windows | none | PowerShell 7+ full; PS 5.1 prompt-hook-only fidelity |
| Foreign-var clobber classes (direnv #82/#798) | inherited from direnv | structurally eliminated (D1) |
| Bootstrap deps | direnv (self-hosting cycle in this repo's toolchain) | none |
| `direnv allow` ceremony | per `.envrc` edit | consent stamp per project; re-confirm on source-set/identity drift |

### Consequences

**Positive**: direnv optional everywhere (kept as `ocx direnv export` for its users — additive per [#170]); one reconciler serves both scopes; [#148] deferred item closed; [#265] unblocked at the ledger level.

**Negative / Risks**:
- Per-shell emission matrix is the cost center; nushell + PowerShell are divergent and must be their own work packages (nushell gated on the removal-primitive spike).
- Prompt-hook coexistence (starship, direnv-still-installed, VS Code shell integration) needs explicit tests — wrap-don't-clobber on pwsh, ordering on bash `PROMPT_COMMAND`.
- A user running direnv *and* the hook double-manages project env: detect `DIRENV_DIR` and yield (hook defers, one info line) — explicit non-goal to fight direnv.
- Interactive-only wrapper means non-interactive `ocx update; cmake` scripts don't self-refresh — documented; scripts use `ocx run`.
- Test harness must drive real interactive sessions (pty) per shell — [#152]'s scope grows; www-setup's docker shell matrix is the substrate.
- First-update rollout lag (Decision 4) — default-on lands fleet-wide only on the second update hop.

### Open Decisions (owner)

- **OD-2** — `[shell]` in the managed tier: local-tier-only (recommended, strip alongside `[managed]`) vs fleet-controllable hook toggle.
- **OD-3** — consent residual: accept silent digest-swap within a consented namespace (recommended, documented) vs digest-set re-confirm (fires on every legitimate lock bump).
- **OD-4** — `OCX_ENV → __OCX_ENV` vs `OCX_BINARY_PIN` mixed-version channel: one-release dual-read (recommended) vs declaring mixed-version launcher re-entry unsupported.

### How Would We Reverse This?

Flip the default (`[shell] hook` default off) or remove hook emission from `self activate`; shims regenerate on next `self update` (plus one hop of lag); `__OCX_ENV_STATE` evaporates with sessions (no persisted format, no migration). `direnv export` path is untouched throughout. The `[shell]` config section degrades silently on older binaries (no `deny_unknown_fields`).

## Technical Details (component placement)

- Reconciler in `ocx_lib` (new module adjacent to `env.rs`/`shell.rs`; consumes `resolve_env_*` output; per-shell rendering stays in `Shell`/`emit_lines` — thin, per the nu shim's existing typed `load-env` precedent). New `Shell::remove_path_element` primitive (Decision 1).
- `self activate` grows: hidden per-prompt flag, hook + wrapper emission (absolute-binary-path call sites only), shim-supplied `--hook`/`--no-hook`, consent-stamp read (pre-`Context`, pure-fn path helpers).
- `[shell]` section: `config/shell.rs`, `Config::merge()` wiring, managed-payload strip per OD-2, schema regen.
- Consent stamps: `StateStore` subdir; stamp content = consented source set + identity (JSON).
- Package-metadata reserved-key gate: reject at `package create`, skip+warn at compose/emit (Decision 2).
- setup surfaces: shims updated (auto-ships via `self update`, one-hop lag); profile fences untouched; setup-ocx action unaffected (CI non-interactive); setup.ocx.sh unchanged while default-on (`OCX_INSTALL_NO_SETUP` installs never get shims → never get the hook — documented).
- **Documentation reconciliation (mandatory per handshake §7a)**: `handshake_toolchain_cli.md` §2 + §7 rows; `adr_cli_high_low_layering.md` (per-prompt evaluator framing); `arch-principles.md`; `subsystem-cli.md`; `subsystem-cli-commands.md`; `subsystem-cli-api.md`; `website/src/docs/reference/environment.md:55-59` (currently documents the per-prompt hook as REMOVED — must be rewritten, not appended); new shell-integration docs page; `environment.md` entries for `__OCX_ENV_STATE`, `OCX_NO_HOOK`, `__OCX_ENV`, `OCX_CEILING_PATH`.

## Implementation Plan

Phase 2, after `adr_project_toolchain_links.md` lands (consumes its stable addressing). The move-to-front primitive this design leans on is **already shipped** ([#26] scope: `utility::path::move_to_front`, per-shell `Shell::export_path` idioms, `OCX_ACTIVATED` guard removal — commits `e30f94c1`/`ae6c3b08`; close the stale issue). Plan via `/swarm-plan`; work packages by surface: reconciler core (lib, platform-neutral, unit-testable), `Shell::remove_path_element` (10-shell matrix), POSIX-family emission, PowerShell, nushell (spike-gated), wrapper functions, consent, `[shell]` config + managed strip, reserved-key gate, docs reconciliation, pty test harness.

## Validation

- Unit: reconciler case table (every row of the constants table, list algebra with foreign elements interleaved, ledger corruption + over-cap degradation, case-insensitive keys on Windows), `remove_path_element` idempotency matrix, reserved-key gate (create-reject, compose-skip).
- Acceptance (pty, per shell × docker matrix): enter/leave restores foreign state; `ocx update && tool` same-line freshness; foreign `export` survives exit; `set -u` shells; direnv-coexistence yield; wrapper unreachable from activation stream; consent: fresh clone inert, post-`add` active, source-swap re-prompts, unreadable lock inert. Each check demonstrated red and green (unchecked-green rule).
- Perf: no-op prompt benchmark (stat-only path) in CI with budget assert.

## Links

- Research: `research_shell_env_reconciler_and_launcher_farm.md`
- Sibling: `adr_project_toolchain_links.md`
- Superseded sketch: `adr_self_setup.md` "Future Work"
- Amended: `handshake_toolchain_cli.md` §2 + §7, `adr_idempotent_path_move_to_front.md`
- Issues: [#170](https://github.com/ocx-sh/ocx/issues/170), [#26](https://github.com/ocx-sh/ocx/issues/26), [#148](https://github.com/ocx-sh/ocx/issues/148), [#152](https://github.com/ocx-sh/ocx/issues/152), [#265](https://github.com/ocx-sh/ocx/issues/265), [#193](https://github.com/ocx-sh/ocx/issues/193)

## Changelog

| Date | Change |
|---|---|
| 2026-08-02 | Initial draft (discussion-thread synthesis + discovery/research passes) |
| 2026-08-02 | [#26] prerequisite framing corrected — move-to-front already shipped |
| 2026-08-02 | Adversarial review round 1: ledger spoof channel closed (reserved-key gate + escape-everything); shim-probed interactivity; script-embedded stderr; absolute-binary call sites (wrapper shadowing); fingerprint = full watch set, mtime-first; `[shell]` off the per-prompt path + OD-2; consent identity binding + subset predicate + OD-3; `OCX_ENV` mixed-version OD-4; doctrine cite fixed to §2/§7; §7a doc register enumerated; `Shell::remove_path_element` named; ledger size cap; rollout one-hop lag; nushell spike gate; pwsh ordering; `set -u` discipline |
| 2026-08-03 | Phase-1 sibling replaced: farm store rejected by owner review → `adr_project_toolchain_links.md` (project-local toolchain trees, `${installPath}` override map). Reconciler consumes stable link paths; ownership prefixes = `$OCX_HOME` + `.ocx/toolchain/` |
