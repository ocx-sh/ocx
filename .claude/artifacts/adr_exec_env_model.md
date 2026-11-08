# ADR: Execution-Environment Model — Two Transported Variables, Layered Exec, Pinned `ocx shell`

> Tier xhigh · Reversibility: one-way (high) — public `OCX_*` env surface, exec flag grammar, `ocx shell` semantics, execution-record field, shell-visible state, generated files on disk that outlive upgrades, and copies of the state that exporters persist into CI env files and direnv caches · Blast radius: external contract, cross-area (ocx_env, ocx_config, ocx_shell, ocx_package, ocx_package_manager, ocx_cli, ocx_script, ocx_sdkgen, ocx_shim/setup reads) · Required artifacts: this ADR, the system-design doc, and the seven research artifacts below.

## Metadata

- **Status**: Accepted
- **Accepted note (2026-11-07)**: Accepted under `/hex-loop` goal-loop authority (goal `.agents/goals/adr-exec-env-model.md`). OQ-1 to OQ-3 take their Recommended answers (OQ-3 confirmed by reading the pinned submodule source); the four owner confirmations (product principle 4 and differentiator 3 reworded; relaxing fields let an explicit ambient value beat a received one; the global tier stays out inside an inherited project; `__OCX_CREDENTIAL_HELPER=1` stays in the helper spawn until 0.7) take this ADR's stated positions. Recorded as D-01 to D-07 in [`plan_exec_env_model.md`](./plan_exec_env_model.md). The owner may revisit any of them.
- **Date**: 2026-11-06
- **Deciders**: Owner + `/hex-architect` tier `xhigh`
- **GitHub Issues**: [#678](https://github.com/ocx-sh/ocx/issues/678) (`ocx shell allow` vs hook selector), [grimoire#297](https://github.com/grimoire-rs/grimoire/issues/297) (leaked `OCX_GLOBAL`), [#679](https://github.com/ocx-sh/ocx/issues/679) (refresh of generated files, deferred), [#668](https://github.com/ocx-sh/ocx/issues/668) (origin of `--stack`, bare `ocx shell`, `__OCX_EXEC_SCOPE`), [#649](https://github.com/ocx-sh/ocx/issues/649) (selector clears), [#672](https://github.com/ocx-sh/ocx/issues/672) (declarative composition, separate ADR), [#363](https://github.com/ocx-sh/ocx/issues/363) (named shells), [#488](https://github.com/ocx-sh/ocx/issues/488) (`OCX_HOME` under `--clean`), [#623](https://github.com/ocx-sh/ocx/issues/623) (credential-helper marker)
- **Tech Strategy**: aligned. Rust 2024, Tokio. No new workspace dependency: `base64` (`Cargo.toml:176`), `flate2` (`Cargo.toml:213`), `sha2` and `hex` (`Cargo.toml:174-175`) are workspace dependencies already. One new crate-level edge: `crates/ocx_config/Cargo.toml` gains `base64` (it has `flate2` at `:59` and no `base64` today). Named in WP2 and in the lockstep row.
- **Domain Tags**: cli, shell, package-manager, interface-contract, security
- **Supersedes** (relations recorded here only; the target files are not edited):
  - [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md): Decision 1 envelope and the two-version split (lines 64, 81), the 16 KiB cap and marker-only overflow (line 86), the degradation rule for the hook (line 88), "`OCX_ENV` is not renamed and not repurposed" (lines 98-100), "`apply_ocx_config` does not strip `__OCX_ENV_STATE`" (line 102), and Amendment 2026-11-03 (`--stack`, `__OCX_EXEC_SCOPE`, bare `ocx shell`).
- **Amends**:
  - [`adr_shell_env_addenda.md`](./adr_shell_env_addenda.md): A-04 (lines 130-153: the additive rule is re-adopted as the minor version; the `v-1` revert-read arm is withdrawn, it was never implemented), A-38 (lines 1488-1516, a size warning is now required), A-34 (lines 1385-1411, the pin value changes; the hook still runs the `current` binary), A-06 (line 182, trust re-ruled; every value-supplying field listed).
  - [`adr_toolchain_activation.md`](./adr_toolchain_activation.md): D-9 (lines 153-167, 380-402). The trampoline body stays byte-identical. Item 38 (line 736) is restated: the test exercises the global-trampoline half.
  - [`adr_global_toolchain_tier.md`](./adr_global_toolchain_tier.md): Decision 2 and Amendment 2026-05-17 (lines 154-179) lose the `OCX_GLOBAL` env form, after one warn-once window. The exclusivity check stays. "`ocx exec` hermetic" (lines 55-88) now means "never composes the global tier", not "inherits nothing".
  - [`adr_package_entry_points.md`](./adr_package_entry_points.md): the Stable Surfaces list (lines 962-984) gains `OCX_BINARY_PIN` (name and absolute-path value shape) and the state's plaintext header grammar. The pin row (line 994) gets the new value rule.
  - [`adr_exec_resolution_record.md`](./adr_exec_resolution_record.md): F10 (line 100) and Records (line 389). The forwarded set folds into `__OCX_ENV_STATE`. The record gains `env_mode`.
  - [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md): no behaviour change. The pin's `IF DEFINED` semantics (lines 31-32, 362-368) are restated as binding on the new writer: never write an empty pin.
  - [`adr_project_env_declaration.md`](./adr_project_env_declaration.md): R1 (the `OCX_ENV` payload) moves into the state's `replay.env`. X1 (line 378, reserved-key refusal) is unchanged.
- **Related**: [`adr_live_env_reload.md`](./adr_live_env_reload.md) OD-4 (lines 166, 195: the mixed-version caveat applies to the new state), [`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md) (line 348: SDK spawn contract), [`adr_cli_plugin_pattern.md`](./adr_cli_plugin_pattern.md), [`adr_patch_env_resolution_uniformity.md`](./adr_patch_env_resolution_uniformity.md) (lines 75, 167: one compose-then-spawn seam).
- **Product context conflict**: product principle 4 and differentiator 3 in `.claude/rules/product-context.md` contradict layer-by-default. See "Conflict with product context".
- **Inputs**: discussion `.agents/discussions/exec-env-model.md` (data, owner decisions of 2026-11-06), claim diff [`discover_exec_env_model.md`](./discover_exec_env_model.md) (wins over the discussion), the seven research artifacts listed under Industry Context, and one review panel pass (Review record at the end).
- **Companion**: [`system_design_exec_env_model.md`](./system_design_exec_env_model.md) (C4 and flows).

## Context

ocx moves state between its own processes through about 31 environment variables. That set grew one incident at a time.

| Fact | Evidence |
|---|---|
| 26 public `OCX_*` variables are declared `child = Forward` | `crates/ocx_env/src/vars.rs:18,26,35,45,53,61,71,82,90,108,118,127,135,145,152,166,176,178,186,205,216,226,237,283,292,357` |
| `apply_ocx_config` writes, clears or set-or-removes them on every spawn | `crates/ocx_config/src/env.rs:207-294` |
| `Context::try_init` fills eight view fields by hand; a missed one is stripped from every child | `crates/ocx_cli/src/app/context.rs:425-450` |
| Five more `__OCX_*` plumbing variables cross processes | `vars.rs:492,497,506,512,518` |
| A leaked `OCX_GLOBAL` selects the global toolchain in an unrelated `ocx exec` | [grimoire#297](https://github.com/grimoire-rs/grimoire/issues/297); read at `crates/ocx_cli/src/app/context_options.rs:145-147` |
| `ocx shell allow` and the hook disagree when `OCX_GLOBAL` is exported | [#678](https://github.com/ocx-sh/ocx/issues/678); the hook never reads `global` (`crates/ocx_cli/src/command/self_group/activate.rs:547-558`) |
| Default `ocx exec` reverts the hook's contribution; launcher re-entry keeps it | `crates/ocx_shell/src/shell/reconcile.rs:104-108` vs `crates/ocx_cli/src/command/launcher/exec.rs:215`, `launcher/shim.rs:92-94` |
| Every forwarding addition came from `--clean` or a launcher losing a setting | [`research_ocx_env_transport_history.md`](./research_ocx_env_transport_history.md) table, shas [795920cb](https://github.com/ocx-sh/ocx/commit/795920cb), [e4ce1186](https://github.com/ocx-sh/ocx/commit/e4ce1186), [191b9324](https://github.com/ocx-sh/ocx/commit/191b9324) |
| `--stack`, bare `ocx shell` and `__OCX_EXEC_SCOPE` landed after v0.6.5 and are unreleased | [3f85e2e0](https://github.com/ocx-sh/ocx/commit/3f85e2e0), [8d1bc0f1](https://github.com/ocx-sh/ocx/commit/8d1bc0f1), [4b8b94d2](https://github.com/ocx-sh/ocx/commit/4b8b94d2) |
| The `OCX_GLOBAL` env form shipped in v0.6.5 | the 0.6.x release line; `context_options.rs:147` |
| No interface-contract baseline exists yet | `test/lint/test_contract_baseline.py:1-12`; `crates/ocx_schema/contract/` has no `baseline/` |
| Five written contracts contradict each other on what a child inherits | [`research_ocx_exec_env_transport_inventory.md`](./research_ocx_exec_env_transport_inventory.md) |

The owner's intent: fewer rules, not only fewer variables. Each transported variable carries one rule. Nesting `ocx --project a exec -- ocx --project b exec -- echo` must simply work.

### Rule inventory, before and after

The honest measure is what a contributor must decide, not how many variables exist.

| Task | Decisions today | Decisions after |
|---|---|---|
| Add a spawn path | Pick a start function (`Env::new`, `Env::clean`, `inherited_env`, `stacked_env`, `stacked_clean_env`; `env.rs:136-151`, `reconcile.rs:104-122`); fill the view fields the call site forwards; decide `set_forwarded_env`; decide credential scrubbing; decide `__OCX_EXEC_SCOPE` and `__OCX_LAUNCH_IDENTITIES`. Six decisions, none checked | Fill one `Spawn` value: mode, owner, replay, selection, or name an exemption; `composed_scope` is `None` except for `ocx exec`. Five decisions in one struct (mode, owner, replay, selection, or an exemption); `composed_scope` is fixed by the call site, not decided; the firewall test (C5) reds a spawn that skips it |
| Add a resolution setting | Declare it `child = Forward`; add an `OcxConfigView` field (`env.rs:14-54`); add a `forward!` line (`env.rs:125-133`); fill it in `context.rs`. Forgetting the fill silently strips it (set-or-remove) | Declare it; add a `Policy` field with its class (narrowing, neutral, relaxing) and its parser; decide whether a root flag writes it. Three decisions; an exhaustive match over the field enum reds an unclassified field |

| Rule set after this ADR | Count |
|---|---|
| `__OCX_ENV_STATE`: the projection rewrites it at every ocx spawn | 1 |
| `OCX_BINARY_PIN`: `binary_pin_value` sets it at every ocx spawn | 1 |
| Propagation classes for everything else (C1): user input, secret input, setup-written, hook-local, credential | 5 |
| Policy write rule (C2) and precedence split (C8) | 2 |
| Mode table: layer, isolate, clean (C4) | 1 |
| Named spawn exemptions (C5) | 1 |
| **Total** | **11** |

Today the same ground takes 26 forward declarations, about 7 scrub sites, 5 plumbing variables with their own rules, 5 start functions and 5 contradicting written contracts. The claim this ADR keeps is narrow: one write path, one projection, two transported variables.

## Decision Drivers

| ID | Driver | Weight |
|---|---|---|
| D1 | Rule count: one rule per transported variable, one projection function at every spawn | x3 |
| D2 | ocx never writes a public `OCX_*` into a child, except `OCX_BINARY_PIN` | x3 |
| D3 | Generated files on disk never change what they read (frozen pin name, argv shapes) | gate (pass/fail) |
| D4 | Live shells survive an ocx upgrade without silent wrong behaviour | x2 |
| D5 | Size headroom on every platform, cmd.exe included | x2 |
| D6 | Migration cost (tests, docs, lockstep satellite) | x2 |
| D7 | Debuggability of what crossed into a process | x1 |

D3 is a gate, not a score. Every option keeps the pin name and argv shapes, so a score of 5 everywhere carried no information. An option that failed D3 would be rejected outright.

## Industry Context & Research

**Research artifacts** (all dated 2026-11-06):

| Artifact | Finding used here |
|---|---|
| [`research_env_state_encoding.md`](./research_env_state_encoding.md) | zlib + base64url behind a plaintext prefix. cmd.exe binds through its 8,191-char line; the research's subtraction (`set X=`) is replaced by C3's derivation from the line ocx actually emits, which gives 8,169. cmd.exe also ignores an inherited variable longer than 8,191 (finding 5). Measured (zlib) 3,426 chars at 50 entries and 12,447 at 200 (table lines 55-56); about 120 entries fit. Linux 131,071 B per string incl. name; macOS `ARG_MAX` 1 MiB; Windows 32,767 chars per variable. Never store a credential. |
| [`research_env_transport_shape_council.md`](./research_env_transport_shape_council.md) | Three seats chose "B with A's invariant": an exec carrier, the hook ledger, a pin channel. Pure A is unreachable because generated files read the pin before any ocx runs. |
| [`research_ocx_exec_env_transport_inventory.md`](./research_ocx_exec_env_transport_inventory.md) | The variable table and the five contradicting contracts. |
| [`research_ocx_env_transport_history.md`](./research_ocx_env_transport_history.md) | Why each variable exists; [#668](https://github.com/ocx-sh/ocx/issues/668), [#672](https://github.com/ocx-sh/ocx/issues/672), [#649](https://github.com/ocx-sh/ocx/issues/649) have no design discussion. |
| [`research_nested_env_state_prior_art.md`](./research_nested_env_state_prior_art.md) | mise fixed its nested-exec leak by putting the diff in the child ([jdx/mise#9765](https://github.com/jdx/mise/pull/9765)). Environment builders (nix, devbox, flox) layer, with `--pure` opt-in. uv ignores an inherited `VIRTUAL_ENV`. conda stacks with a depth counter. |
| [`research_inherited_state_adjacent_fields.md`](./research_inherited_state_adjacent_fields.md) | git clears a fixed "repository-local" set at one boundary. systemd `LISTEN_PID` scopes inherited state to its recipient. Argv beats inherited env (git `-c`). |
| [`research_env_leak_community_reports.md`](./research_env_leak_community_reports.md) | rustup 1.25 `RUSTC` leak (selector overrode an inner choice; reverted). Bundler `with_unbundled_env` restores the pre-activation env (the `--isolate` analogue). direnv + tmux stale state ([direnv#106](https://github.com/direnv/direnv/issues/106)). |

**Trending approaches**: activation tools replace their own previous layer; environment builders layer by default and offer a pure mode; state lives in one opaque, versioned variable decoded by the tool itself (direnv, mise, flox).

**Key insight**: the leak class comes from public variables used as transport. A single tool-owned state variable that the child reads, plus argv beating inherited state, closes it. mise stores digests instead of values in its state ([jdx/mise#13950](https://github.com/jdx/mise/pull/13950)). This design needs literal values for `--isolate` and the hook (L-1, `adr_shell_env_overhaul.md` line 84), so it keeps them by default and follows mise for secret-classed keys: presence and a withheld prior only (C2, S-2).

## Considered Options

### Option A: two variables, one rule each (the owner's decision, corrected)

`__OCX_ENV_STATE` carries a frozen plaintext header (narrowing flags, home), then a compressed payload: policy, applied values with owner tags, replay data and session selection. Major and minor versions. `OCX_BINARY_PIN` stays as-is in name and shape. Public `OCX_*` are never written into a child.

| Pros | Cons |
|---|---|
| One write path, one projection; 11 rules in total (Rule inventory) | A major format break makes open hook shells stand down until reopened. Minor changes do not |
| Removes the confirmed leak class | Opaque payload; needs a dump surface |
| Pin unchanged, so every file on disk keeps working | Large migration: tests, docs, the ocx-mirror lockstep |
| Hook and exec share one record, so `--isolate` works at any depth | About 120 composed values fit under the cmd.exe cap; a larger project hits the overflow ladder |
| The header keeps narrowing policy alive across versions | Policy in an opaque payload is less visible than a public variable |

### Option B: the council's three classes

An exec carrier (same-version), a separate hook ledger with a cross-version revert-read arm, and a raw pin/home channel.

| Pros | Cons |
|---|---|
| Live hook shells keep reverting across an upgrade | Three rule sets; the owner rejected exactly this |
| Ledger and carrier can evolve separately | The `v-1` arm is a perpetual obligation, and it was promised but never built (`ledger.rs:173-174`, `:235-237`) |
| | Two blobs with overlapping data (applied values) must agree; `--isolate` needs both |

**Steelman**: B is the most robust to upgrades. A user who upgrades mid-day keeps every open shell reconciling. If live-shell continuity outranked rule count, B would win.

### Option C: today's shape, corrected

Keep about 26 forwarded public variables. Fix the registry declarations and docs. Remove `OCX_GLOBAL` and `--stack`.

| Pros | Cons |
|---|---|
| Smallest change; every value visible in `env` | Keeps the transport class that caused the incidents |
| Version-agnostic: an older ocx reads the same variables | About 26 rules plus about 7 scrub sites; declarations drifted once already |
| No size concerns | A long-lived descendant keeps launch-time public values |

**Steelman**: C is honest about what exists and cheap. If the incidents were declaration bugs rather than a design class, C would be enough.

### Option D: state file on disk with a per-process session id

`__OCX_ENV_STATE=<session-id>` points at `$OCX_HOME/state/sessions/<id>.json`. Every spawn writes a new id, so a subshell never mutates its parent's file. The pin stays.

| Pros | Cons |
|---|---|
| No environment size limit | Every spawn writes a file: I/O on the exec hot path, and a writable `$OCX_HOME` is required (read-only homes in CI images fail) |
| Readable with `cat` | Garbage collection with no reliable owner-death signal; pids are reused |
| One tool-owned variable, no public `OCX_*` written | A pointer captured by tmux, an IDE or `$GITHUB_ENV` outlives its file, or names a file on another host or container ([direnv#106](https://github.com/direnv/direnv/issues/106) is worse with files) |
| | Two shells sharing one captured pointer write one file unless every hook prompt also copies on write |
| | The files are a new persisted format: one-way, outside the process lifetime |
| | `--clean` children must find the home before they can read their own state |

**Steelman**: D is the only option with no size ceiling. A monorepo with hundreds of composed tools fits only in D. A per-process id removes the "subshell corrupts parent" defect.

### Trade-off matrix

Scores 1 (poor) to 5 (best). Weighted total = sum of score x weight (max 65). D3 is a gate that every option passes.

| Criterion (weight) | A | B | C | D |
|---|---|---|---|---|
| D1 rule count (x3) | 5 | 3 | 1 | 3 |
| D2 no public `OCX_*` written into a child (x3) | 5 | 5 | 1 | 5 |
| D4 live-shell upgrade robustness (x2) | 3 | 4 | 4 | 3 |
| D5 size headroom (x2) | 3 | 3 | 5 | 5 |
| D6 migration cost (x2) | 2 | 2 | 4 | 1 |
| D7 debuggability (x1) | 3 | 3 | 5 | 4 |
| **Weighted total** | **49** | **45** | **37** | **46** |

Score notes: A's D4 is 3, not 2, because minor versions no longer stand shells down and the header keeps narrowing policy across majors (C3). D's D1 is 3: a pointer rule plus file lifecycle rules (GC, copy-on-write). D's D6 is 1: a new persisted format, GC and locking on top of A's migration.

**Sensitivity**: A beats B exactly when D4's weight is below twice D1's (A - B = 2·w1 - w4); at D1 x3, B needs D4 at x7. A beats D exactly when D5's weight is below 3.5 (A - D = 7 - 2·w5); at D5 x4, D wins. The totals are close. They are not the reason for the choice.

| Option | Main risk | Reversibility |
|---|---|---|
| A | Open shells stand down after each major break; a pathological project overflows 8,169 chars | Payload format: two-way (internal, any release). Header grammar and public-surface removals: one-way |
| B | The `v-1` arm is skipped again and live shells silently lose their revert set | Same as A, plus a second format with a cross-version promise |
| C | The next leak, from a declaration nobody enforces | Two-way, but keeps the debt |
| D | Orphaned or shared files across tmux, IDEs and CI env files | Hard: files on disk become a persisted format |

## Decision Outcome

**Chosen option: A**, two variables with one rule each, corrected by the claim diff and the review panel.

**Rationale**: the decision rests on two things the matrix does not weigh. First, the owner's stated priority: fewer rules, and an explicit acceptance that "a format break makes already-open hook shells stand down until reopened". Second, the frozen-surface constraint: generated files read the pin before any ocx runs, which rules out pure single-variable designs and keeps the pin in every option. Within those, A has the fewest rules and no persisted format. D's size advantage costs a one-way on-disk format, GC and a writable home. B's upgrade advantage costs a third rule set and a perpetual cross-version arm. C keeps the incident class.

### Conflict with product context

`.claude/rules/product-context.md` states principle 4, "Clean-env execution — `ocx exec` run with package-declared vars only, no ambient PATH pollution", and differentiator 3, "Clean environment execution". Layer-by-default contradicts both. Today's default already starts from the caller's environment (`Env::new()`, `crates/ocx_config/src/env.rs:136-144`) and only reverts the hook's layer, so the principle overstated the default before this ADR; `--clean` is the form that matches it.

Decision: keep the owner's layer default. The implementation edits `.claude/rules/product-context.md` **in the same change series** (WP11), under that file's Update Protocol. Proposed wording for principle 4: "Composable execution — `ocx exec` layers package-declared vars over the caller's environment; `--clean` runs with package-declared vars only." Differentiator 3 changes the same way. The wording is a deferred owner confirmation.

### Owner decision 2026-11-07: the hook never reloads an inherited scope

A shell spawned by a project must not have that project loaded again by its hook, whatever the cwd. Without this rule, `ocx --project P -g x exec -- bash` inside P gets P's *default* groups composed over P-with-x at the first prompt, and the global tier composed over P elsewhere.

| Situation in an exec-spawned shell (FollowCwd) | Behaviour |
|---|---|
| cwd inside a project whose scope is inherited (P) | The hook composes nothing, the global tier included: the inherited layer stands as the spawner set it up. This is the yield `__OCX_EXEC_SCOPE` gave (`crates/ocx_env/src/vars.rs:507-512`) |
| `cd` into another project Q | Q (and the global tier, unless inherited) is layered on top as session records |
| `cd` back into P | The session Q layer is removed; inherited P is untouched |
| Outside every project | The global tier is composed only if it is not inherited |
| P's `ocx.toml` or `ocx.lock` edited | Nothing changes. The hook does not watch an inherited scope's files and never touches values it did not apply |

**Trade-off: exec = snapshot, shell = live.** An exec-spawned shell keeps exactly what its spawner composed, for its whole life. A user who wants edits to take effect opens `ocx shell`, whose pinned refresh is unchanged (C7). The mechanism is `ExecState.inherited_scopes` (C2), set by every spawn (C4) and read by the hook and the login stream (C7). It uses no pid and no parent-pid test. One reading is this design's own, not the owner's words: inside an inherited project the global tier also yields, because composing it on top would put global tools in front of the project's.

### Revisions of dossier decisions

The dossier's Decisions stand except where code shows a wrong fact. Each revision keeps the owner's intent with the narrowest change.

| # | Dossier position | Evidence | Revision |
|---|---|---|---|
| R1 | Layer by default (framed as a choice among equals) | Today's default exec already reverts the hook's contribution (`reconcile.rs:104-108`, `toolchain_exec.rs:216-221`); launchers already layer (`launcher/exec.rs:215`, `launcher/shim.rs:92`) | Layering is a **behaviour flip** for `exec`, `shell`, `package exec`, `package test`, `patch test`. It ships as a `!` commit. Launcher re-entry and exec finally agree |
| R2 | [#678](https://github.com/ocx-sh/ocx/issues/678) is fixed by honouring `OCX_PROJECT` everywhere | The hook already honours `OCX_PROJECT` through the loader (`ocx_config/src/loader.rs:494-512`); the divergence is `OCX_GLOBAL` (`context_options.rs:147`) | Ignoring the `OCX_GLOBAL` env form fixes [#678](https://github.com/ocx-sh/ocx/issues/678). `shell_allow.rs:32` (`is_this_shells_project` tests only the flag) is aligned with the resolved project as a cosmetic fix |
| R3 | Pin = "the stable install path `trampoline_ocx_binary` already bakes" | `trampoline_ocx_binary` prefers the installed path even when a different build runs (`crates/ocx_package_manager/src/launcher/generate.rs:85-94,100-110`) | New `binary_pin_value` (C6): installed path only when the canonical running binary equals the canonical installed binary and the path is safely owned; else the exact running binary |
| R4 | Public `OCX_*` are never written into a child **or emitted shell code**; resolved home always travels in policy | `ocx self setup`'s env shims and rc block export and read `OCX_HOME` (`crates/ocx_setup/src/shims.rs:19-20,94-98,118-122,188,244-245`, `rc_block.rs:412-416`); the Windows shim reads it only for an optional containment pre-check it skips when unset (`ocx_shim/src/main.rs:214-236`) | `OCX_HOME` gets a **setup-written** class: setup's files may export it, the projection never writes it. The home travels in the state's plaintext header (`h=`) when the child cannot re-derive it (C2). Under `--clean` the Windows shim skips its pre-check and `launcher exec` validates against the header home |
| R5 | The ledger in `ocx_shell` gains owner tags and policy | `ocx_config` has no edge to `ocx_shell` (`scripts/crate_map.toml:31`); plugins link ecosystem crates only | Wire format, codec and policy resolution go to `ocx_config::exec_state` (ecosystem). Exec semantics and the projection go to `ocx_package_manager::exec_env` (internal). `ocx_shell`'s `Ledger` is deleted (C0) |
| R6 | `OCX_GLOBAL` gets a `RETIRED` row and a contract-ledger entry | `--global` is the replacement, but a `Retired` row's `replacement` must be an `EnvVar` (`crates/ocx_env/src/retired.rs:43`). The two existing outcomes are both wrong here: a `Window` row is honoured under the replacement's name (`crates/ocx_env/src/lib.rs:326-344`), which is the leak; `Removed` exits 78 (`crates/ocx_cli/src/app/context.rs:1106,1119-1122`), which would fail every leaked environment. No baseline exists | The code cannot express a warn-once removal. WP1 extends `retired.rs` with a flag replacement and a `Withdrawn` change: warn once on stderr, never honour, never exit. The row lives until `REMOVAL_RELEASE` 0.7 (`retired.rs:32-36`) under the AGENTS.md batched-window carve-out, then the name becomes inert. No contract-ledger entry (R15) |
| R7 | "The ledger's `v-1` revert-read arm is dropped" | No arm exists; only a comment (`ledger.rs:173-174`); `decode` rejects any other `v` (`ledger.rs:235-237`) | Recorded as an amendment to A-04: the arm is withdrawn; the additive rule is kept as the minor version. Nothing is removed from code |
| R8 | "Every spawn adds its own composition as inherited" | A pinned hook must refresh "its own layer" (dossier requirement 6) | `ocx shell` tags its composition **session**. Every other spawn tags inherited |
| R9 | Hook "stands down" with one notice line | A hook that writes nothing reprints its notice every prompt; a hook that overwrites destroys state written by a dev build, a rollback or another format | The hook **never writes** an unreadable state. It prints the notice once and keeps an unexported shell-local copy of the stood-down value; the re-emitted gate skips while the state equals it (C7) |
| R10 | Overflow: "drop the applied-value record with a warning" | Session records are what the hook needs; inherited ones serve only `--isolate` | A three-rung ladder: drop inherited records, then all applied records, then the payload (C3). `inherited_scopes` survives rungs 1 and 2. Truncation is sticky across spawns |
| R11 | Spawn list in requirement 2 | Claim diff Spawn paths: self-update hand-off and the update-check probe use opposite policies; the credential-helper spawn is in an external crate; no "records subprocess" exists | Every spawn path gets a rule (C5). The update-check probe stays hermetic and outside the projection |
| R12 | `__OCX_LAUNCH_IDENTITIES` folds into state | It is also emitted by `ocx env --shell/--ci` and `ocx direnv export` into shells and `$GITHUB_ENV` (`crates/ocx_cli/src/command/toolchain_env.rs:225-228,468-471`) | Those exporters emit a **fresh minimal** `__OCX_ENV_STATE` instead, under the same condition (C9). Never their own inherited state |
| R13 | Drop the trampoline's `unset OCX_PROJECT` once a test proves `--project` beats `OCX_PROJECT` | The flag already wins (`loader.rs:494-501`); the unset protects `--global` trampolines from the exclusivity exit 64 (`context.rs:1065-1074`) | The unset stays byte-identical. The regression test checks the **global** trampoline half |
| R14 | "ocx itself never reads `OCX_BINARY_PIN`" | The Starlark host reads it from the composed base env (`crates/ocx_script/src/ocx_module.rs:284`) | Restated: ocx never reads the pin from its own process environment. Reading the child env it just built is allowed |
| R15 | Contract: the compat gate passes with ledger entries | No baseline exists; removals before the first rotation are the baseline | No contract-ledger entries. `cli.json`, the sdkgen `env.rs` golden and the record schema golden change |
| R16 | The generated SDK spawner uses the one projection | It is a template with no `ocx_*` dependency and scrubs user `OCX_*` (`crates/ocx_sdkgen/templates/rust/spawn.rs:36-66`) | A written spawn spec plus a conformance fixture (C12). Amended 2026-11-07 (D-45): the SDK strips nothing; child = parent snapshot, then the caller's pairs (a caller key replaces the parent entry of the same key), then `OCX_BINARY_PIN` written last |
| R17 | Credential class = every `secret` declaration | `OCX_ANNOUNCE_TOKEN` is `secret` but deliberately inherited by plugins (`vars.rs:450`); `credential_keys()` is `Child::Scrub` only (`crates/ocx_env/src/lib.rs:375-387`) | Credential = `secret, child = Scrub`. Secret declarations without `Scrub` are class SecretInput: inherited, never recorded (C1) |
| R18 | "Every field set by its own flag" enters policy | `--no-verify` is one-shot on purpose (`context.rs:431-432`) | Only five root flags write policy, through a type that has no other fields (C2) |

### Other decisions this ADR takes

| Topic | Decision | Why |
|---|---|---|
| `OCX_ENV` | Payload moves to `replay.env`. Declaration deleted; the name becomes inert | It is ocx-written and ocx-read (`ocx_package/src/metadata/env/apply.rs:89-94,307-316`). Requirement 1 forbids the write. Its fail-closed validation (`apply.rs:191-264`) moves with it. Cross-repo note in Migration |
| `OCX_GLOBAL` | Warn-once `Withdrawn` row until 0.7 (R6), then inert | A profile export changes behaviour silently otherwise; an exit 78 fails leaked environments |
| `OCX_PROJECT` | Never written and never cleared by ocx | Selectors are user input (requirement 3). The clear at `env.rs:231` ([ed5f17f8](https://github.com/ocx-sh/ocx/commit/ed5f17f8)) is unreleased |
| Exclusivity check | Unchanged (`--global` flag plus `OCX_PROJECT` exits 64) | Old trampolines and the Windows shim depend on it being the rule they work around |
| `__OCX_EXEC_SCOPE` | Deleted. `inherited_scopes` replaces its purpose for exec-spawned shells (the owner decision above); pinned selection replaces it for `ocx shell` | Requirement 6 |
| `__OCX_CREDENTIAL_HELPER` | Becomes the header's `credential_helper` narrowing name. The credential-helper spawn also keeps writing `__OCX_CREDENTIAL_HELPER=1` until 0.7, then the declaration is deleted | One channel, readable across versions. An older ocx reached inside the helper reads only the old variable; without it that ocx downloads under the outer pull's lock and waits on it (`vars.rs:513-518`). This is a third ocx-written variable, honestly: it is written into one spawn that is already exempt from the projection (C5), carries only `1`, is read by no new ocx, and leaves with the 0.7 window |
| `--stack` | Deleted (unreleased) | Requirement 4 |
| Owned-prefix repair | Removal authority is the record, not the path prefix. `repair_owned_segments` (`crates/ocx_shell/src/shell/reconcile/plan.rs:92,387-424`) runs only for a session whose records the writer dropped at rung 2, only while `inherited_scopes` is empty, never after rung 3, and never removes an element an inherited record names | Today it runs every prompt and would strip an outer frame's `$OCX_HOME` elements, which breaks layering |
| Absent state | Fresh follow-cwd session, no repair | An unrecorded element is an outer frame's value, not a stale one |

### Amendments to prior ADRs, stated once

| Prior rule | New rule |
|---|---|
| Overhaul D1: envelope tag (encoder) and inner `v` (shape) are two versions | A **major** in a frozen plaintext header and a **minor** in the payload. Within a major, minors are additive and readers ignore unknown fields. A major is bumped only on a semantic break (C3) |
| Overhaul D1: 16 KiB cap, marker-only overflow, no ladder | 8,169-char cap on every platform, a three-rung overflow ladder, one warning |
| Overhaul degradation: unknown or corrupt ledger = absent, rebuild, repair lists | Unreadable payload = the hook stands down **without writing**; others warn and ignore the payload. The header is honoured in both. Absent = fresh session, no repair |
| Overhaul: `apply_ocx_config` does not strip the carrier | The projection rewrites the state at every ocx spawn; it never passes the parent's value through unchanged |
| A-04: additive-only `v` plus mandatory `v-1` revert-read arm | Additive rule kept as the minor version. The `v-1` arm is withdrawn. No cross-major reader |
| A-38: no size accounting, no warning | Writer measures, applies the overflow ladder, warns once |
| A-34: pin = running ocx's `current_exe()` | Pin = `binary_pin_value` (C6). The hook still runs the `current` binary |
| A-06: carrier trusted at the privilege level that set it; names only a revert set | Same trust level, with the authority of the public variables it replaces. Value-supplying fields, exhaustively: header `n` and `h`; `policy`; `replay.env` (applied by launchers); `replay.launch_identities` (patch-rule matching by name in launchers); `Prior::Value` (restored under S-4); `AppliedValue.value` (an operand for removal and equality, never applied as a new value); `PinnedTier::Project.dir` (one bounded probe after consent, S-5); `Pinned.pinned_paths` (the `pinned` ladder's CLI tier on refresh); `session.tiers` (stat only, S-5). Compared only, never a value: `inherited_scopes` roots, `ProjectScopeHint.dir`. `Pinned.env` no longer exists |

## Component Contracts

### C0. Placement (crate boundaries)

`ocx_config` is ecosystem tier and links into ocx-mirror in lockstep. It holds only what a descendant outside the ocx binary must read: the wire format, the codec, the applied-value records and policy resolution. Exec semantics, selection and session behaviour, the hook and the projection go to internal crates.

| Item | Crate (tier) | Why there |
|---|---|---|
| Header grammar, `Narrowing`, `ExecState` and every payload type, `ComposedValue`, limits, `encode`, `decode`, `Inherited` | `ocx_config::exec_state` (ecosystem) | A plugin is a descendant. ocx-mirror links ecosystem crates only (`arch-principles.md` satellite rule) and must read received policy. `ocx_package`'s `From<&Entry> for ComposedValue` needs the type below `ocx_package` |
| `Policy`, `PolicyLayer`, `RootFlags`, `PolicyInputs::{env_slot, to_write}`, `FieldClass` | `ocx_config::exec_state::policy` (ecosystem) | The seams that consume policy live in `ocx_config`: `resolve_mirror_map` (`crates/ocx_config/src/mirror.rs:376-390`), `insecure_hosts` (`insecure.rs:16-32`), `RecordsOptions::merge` (`records.rs:57-72`) |
| `Verdict`, `ProjectScopeHint`, `ScopeId` | `ocx_config::exec_state` (ecosystem), moved from `ocx_shell` (`ledger.rs:84-89,98-111,134-152`) | They are payload fields; `ocx_config` cannot name an `ocx_shell` type |
| `ExecMode`, `Spawn`, `project`, coalescing, isolate revert, `ChildEnvironment`, `ProjectionWarning`, the `apply_child_env` adapter | `ocx_package_manager::exec_env` (internal, new module) | Exec semantics. Satellites drive spawns through the CLI. All `apply_child_env` callers are in `ocx_cli` |
| `impl From<&Entry> for ComposedValue` | `ocx_package` (`metadata/env/apply.rs`) | Orphan rule: `Entry`'s crate |
| Hook plan over owner-tagged records | `ocx_shell::shell::reconcile` (internal) | The pure planner, as today |
| Session token, pinned refresh, stand-down sequencing | `ocx_package_manager::activation` (internal) | Sequencing, as today (`crates/ocx_shell/CLAUDE.md`) |
| `binary_pin_value` | `ocx_package_manager::launcher::generate` (internal) | Beside `trampoline_ocx_binary` |
| `ExecModeOptions` | `ocx_cli::options` (interface) | Clap stays out of library crates |
| `Ledger`, `LedgerEntry`, `Scopes`, `ProjectScope`, `CARRIER_KEY`, `LEDGER_VERSION`, `ENCODER_TAG` | deleted (`ledger.rs:21-218`) | Replaced by `exec_state` |

**Edges**: no new crate edge. `ocx_config` already depends on `ocx_env`; `ocx_shell` and `ocx_package_manager` already depend on `ocx_config` (`scripts/crate_map.toml:31,38`). One external-dependency edit: `base64` on `ocx_config` (WP2, lockstep row).

### C1. Registry classes (`crates/ocx_env/src/vars.rs`, `crates/ocx_env/src/lib.rs`)

`enum Child { Inherit, Scrub, Forward }` (`lib.rs:80-87`) is replaced by one propagation class per variable. The `forward!` macro (`ocx_config/src/env.rs:125-133`) is deleted.

```rust
// crates/ocx_env/src/lib.rs
pub enum Propagation {
    UserInput,     // read by ocx, inherited untouched, never written by ocx
    SecretInput,   // UserInput that carries a secret: inherited, written by nobody in ocx
    SetupWritten,  // UserInput, but `ocx self setup`'s generated files may export it
    State,         // `__OCX_ENV_STATE`: written by the projection; read by ocx and plugins
    BinaryPin,     // `OCX_BINARY_PIN`: written by the projection; generated files read it
    HookLocal,     // plumbing for one direct child (hook bookkeeping, helper marker): removed at every projected spawn
    Credential,    // `secret, child = Scrub`: removed at every spawn, never recorded
}
```

| Class | Members | Child under layer / isolate | Child under clean | Written by |
|---|---|---|---|---|
| UserInput | every public `OCX_*` not listed below, Foreign and Testing variables that are not `secret` | inherited untouched | absent | nobody in ocx |
| SecretInput | `secret` without `child = Scrub`: `OCX_ANNOUNCE_TOKEN` (`vars.rs:450`), the `OCX_AUTH_{REGISTRY}_TOKEN` pattern (`vars.rs:480`), Foreign `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, `CI_JOB_TOKEN`, `SIGSTORE_ID_TOKEN`, `CIRCLE_OIDC_TOKEN_V2` (`vars.rs:601,608,624,626`) | inherited untouched (ocx-mirror reads `OCX_ANNOUNCE_TOKEN`) | absent | nobody in ocx |
| SetupWritten | `OCX_HOME` (`vars.rs:26`) | inherited untouched | absent (home travels in the header) | `ocx_setup` env shims only |
| State | `__OCX_ENV_STATE` (`vars.rs:492`) | rewritten by the projection | rewritten | projection, hook, exporters |
| BinaryPin | `OCX_BINARY_PIN` (`vars.rs:18`) | set (or removed when unknown) | set | projection |
| HookLocal | `__OCX_ENV_PWD` (`vars.rs:497`); `__OCX_CREDENTIAL_HELPER` (`vars.rs:518`) until 0.7 | removed | absent | elvish hook; the credential-helper spawn (C5) |
| Credential | `secret, child = Scrub`: `vars.rs:422,431,441,459`; `credential_keys()` (`lib.rs:375-387`) | removed | absent | nobody |

**Shell-local hook values are not transported variables.** The hook keeps bookkeeping in unexported shell variables today (`__ocx_pwd`, `__ocx_yield`, `__ocx_grant`, `crates/ocx_shell/src/shell/hook.rs:156-177`). They never reach a child process, so they are outside this table and outside the two-variable rule. This ADR adds one more: the stood-down copy of the state (C7). A shell whose hook cannot keep an unexported variable across prompts keeps the value in a HookLocal variable instead, as elvish does with `__OCX_ENV_PWD` today (`hook.rs:537`).

**No secret classification** (amended 2026-11-07, D-29). `ocx_env` exports no `is_secret_name` and no `value_has_url_userinfo`: no name list and no value shape decides what is recorded. The registry's own `secret` declarations still decide propagation (Credential, SecretInput), and `is_never_recorded` keeps ocx's four credential keys and `OCX_AUTH_*` out of the state. Everything else is recorded as composed, in `__OCX_ENV_STATE` and in execution records, and `--isolate` reverts by exact value. The state is readable wherever the environment is, which is the exposure the variable already has.

**Deleted declarations**: `OCX_ENV` (`:186`), `__OCX_LAUNCH_IDENTITIES` (`:506`), `__OCX_EXEC_SCOPE` (`:512`). Their names become inert foreign strings. `__OCX_CREDENTIAL_HELPER` (`:518`) is deleted in 0.7 (Other decisions). `OCX_GLOBAL` (`:118`) keeps its declaration only as the target of the `Withdrawn` row (R6) until 0.7.

**Doc updates in the registry** (they render into `cli.json` and `reference/environment.md`): `OCX_PROJECT` (`:80-81`) loses "a child ocx does not inherit it". `__OCX_ENV_STATE` (`:484-491`) describes the transport state and the `unset` gesture's meaning ("forget": the hook starts over and leaves applied values in place). `OCX_BINARY_PIN` states the value rule of C6.

**Enforcement**: one table-driven unit test iterates `ocx_env::all()` and asserts, per class, the child outcome under each `ExecMode`. A misdeclared variable turns it red. A second test asserts every `secret` declaration is in Credential or SecretInput.

### C2. State types (`crates/ocx_config/src/exec_state.rs` and children, new; ecosystem tier)

Every wire type, field by field. Payload field names are not a user contract; only the dump report (C10) is. Between ocx versions of one major they are a wire contract: serialized names, defaults and meaning stay stable, and a rename or removal bumps the major unless a serde alias keeps the old name readable (C3).

```rust
pub const MAJOR: u32 = 2;                     // in the plaintext header
pub const MINOR: u32 = 0;                     // in the payload

pub struct ExecState {
    pub narrowing: Narrowing,                 // header `n=`
    pub home: Option<PathBuf>,                // header `h=`; absolute; see write rule
    pub minor: u32,                           // payload; any value accepted within MAJOR; fields of a newer
                                              // minor are ignored, so each must be safe to lose (C3)
    pub policy: Policy,                       // payload; neutral and relaxing fields
    pub applied: Vec<AppliedValue>,           // payload; application order, oldest first
    pub inherited_scopes: Vec<Scope>,         // scopes an outer frame composed; distinct, oldest first,
                                              // at most MAX_INHERITED_SCOPES; survives rungs 1 and 2 (C3)
    pub replay: Replay,
    pub selection: Selection,
    pub session: SessionCache,                // reset at every spawn
    pub truncated: Truncation,                // sticky across Layer and Isolate spawns
}

pub struct Narrowing {                        // the header is the single source for these
    pub offline: bool,                        // replaces OCX_OFFLINE in transport
    pub frozen: bool,                         // replaces OCX_FROZEN
    pub no_consent: bool,                     // replaces OCX_NO_CONSENT
    pub credential_helper: bool,              // replaces __OCX_CREDENTIAL_HELPER; implies offline
}

pub struct Policy {                           // None = not written; each value is the string the
                                              // replaced variable would hold, validated by its parser
    pub remote: Option<bool>,                 // neutral    OCX_REMOTE
    pub no_project: Option<bool>,             // neutral    OCX_NO_PROJECT
    pub no_config_refresh: Option<bool>,      // relaxing   OCX_NO_CONFIG_REFRESH (a local =0 wins, env.rs:268-275)
    pub no_update_check: Option<bool>,        // relaxing   OCX_NO_UPDATE_CHECK (same, env.rs:276-280)
    pub config_path: Option<PathBuf>,         // relaxing   OCX_CONFIG
    pub no_config: Option<bool>,              // relaxing   OCX_NO_CONFIG
    pub index_dir: Option<PathBuf>,           // relaxing   OCX_INDEX
    pub toolchain_dir: Option<PathBuf>,       // relaxing   OCX_TOOLCHAIN_DIR
    pub mirrors: Option<String>,              // relaxing   OCX_MIRRORS (parse_url, mirror.rs:322-354)
    pub patches: Option<String>,              // relaxing   OCX_PATCHES
    pub patch_snapshot_path: Option<PathBuf>, // relaxing   OCX_PATCH_SNAPSHOT
    pub managed_config: Option<String>,       // relaxing   OCX_MANAGED_CONFIG
    pub no_verify: Option<bool>,              // relaxing   OCX_NO_VERIFY
    pub records_dir: Option<PathBuf>,         // relaxing   OCX_RECORDS_DIR
    pub records_name: Option<String>,         // relaxing   OCX_RECORDS_NAME
    pub allow_yanked: Option<bool>,           // relaxing   OCX_ALLOW_YANKED
    pub default_registry: Option<String>,     // relaxing   OCX_DEFAULT_REGISTRY
    pub insecure_registries: Option<String>,  // relaxing   OCX_INSECURE_REGISTRIES (host[:port] list)
}

pub struct AppliedValue {
    pub key: String,                          // valid env key; S-3, S-7
    pub value: RecordedValue,
    pub kind: ValueKind,
    pub separator: Option<String>,            // List only, non-empty, effective (ledger.rs:44-46)
    pub pre_existing: bool,                   // Path/List: the element was already present and
                                              // move_to_front only moved it; revert keeps it
    pub owner: Owner,
    pub prior: Option<Prior>,                 // Constant only
}
pub enum RecordedValue {
    Literal(String),                          // raw, byte-exact (L-1, L-2)
}
pub enum ValueKind { Path, List, Constant }   // ocx_package: From<ModifierKind>
pub enum Owner {
    Session { scope: ScopeId, source: SessionSource },
    Inherited,
}
pub enum ScopeId { Global, Project }          // moved from ledger.rs:84-89; a session record's scope
pub enum Scope {                              // a composed scope with its identity; compared only (S-5)
    Global,
    Project { root: PathBuf },                // dunce-canonical project dir; never opened or joined
}
pub enum SessionSource {
    Composed,                                 // from ocx.toml groups or the global tier; refreshed
    Invocation,                               // from `ocx shell --with/--env`; remove-only
}
pub enum Prior {
    Unset,                                    // the variable did not exist
    Value(String),                            // exact prior string; set-but-empty is Value("")
    Withheld,                                 // non-UTF-8 prior; never restored
}

pub struct ComposedValue {                    // projection input; not on the wire
    pub key: String, pub value: String, pub kind: ValueKind, pub separator: Option<String>,
}

pub struct Replay {
    pub env: Vec<ReplayValue>,                // former OCX_ENV payload
    pub launch_identities: Vec<IdentityRecord>, // former __OCX_LAUNCH_IDENTITIES
}
pub struct ReplayValue {
    pub key: String, pub kind: ValueKind, pub separator: Option<String>,
    pub value: ReplayPayload,
}
pub enum ReplayPayload {
    Literal(String),
}
pub struct IdentityRecord {
    pub digest: String,                       // `sha256:<64 hex>`
    pub names: Vec<String>,                   // binary names, validated like launcher names
    pub owner: Owner,
}

pub enum Selection { FollowCwd, Pinned(Pinned) }
pub struct Pinned {
    pub tier: PinnedTier,
    pub groups: Vec<String>,                  // group names (strict group-name charset); resolved
                                              // only against the consented ocx.toml of `tier`
    pub with_names: Vec<String>,              // display only: `--with` identifiers as typed; never resolved
    pub env_keys: Vec<String>,                // display only: `--env` keys; never values
    pub invocation_digest: String,            // 64 hex, sha256 over the canonical `--with` and
                                              // `--env` arguments; equality and display only
    pub pinned_paths: Option<bool>,           // the `--pinned`/`--no-pinned` tier of the `pinned` ladder
}
pub enum PinnedTier {
    Project { dir: PathBuf },                 // S-5 exception 1
    Global,                                   // always `$OCX_HOME/ocx.toml`; nothing from state
}

pub struct SessionCache {                     // today's ledger fields (ledger.rs:176-218), plus token
    pub token: Option<SessionToken>,          // None = unclaimed (C7)
    pub fp: String,                           // watch-set fingerprint (ledger.rs:179-183); "" = none
    pub verdict: Option<Verdict>,
    pub tiers: Vec<PathBuf>,                  // at most MAX_RECORDED_TIERS; S-5 exception 2
    pub ws: String,                           // watch-list digest (ledger.rs:194-202)
    pub messages_fp: String,                  // deferred-diagnostics digest (ledger.rs:204-209)
    pub project: Option<ProjectScopeHint>,
}
pub struct SessionToken {                     // shell-instance identity, not a secret
    pub pid: u32,                             // pid of the shell that claimed the session
    pub started: Option<u64>,                 // that process's start time, where readable (C7)
}
pub enum Verdict { Inert, NoProject }         // ledger.rs:98-111 without `Activate`: an
                                              // `activate` value fails to parse (activation.rs:386-388)
pub struct ProjectScopeHint {                 // ledger.rs:134-152; advisory, compared only
    pub key: String,                          // 16 hex project key
    pub dir: PathBuf,                         // absolute; never joined
}
pub enum Truncation { None, InheritedDropped, AllAppliedDropped, PayloadDropped }
```

`Pinned.env` is gone (SP-3): the `--env` values a pinned shell needs reach launchers through `replay.env` and the shell itself as `Invocation` session records. The hook never re-applies a value from `Pinned`.

**Why one scope list, not a scope per record.** The hook needs only the set of scopes an outer frame composed (C7). A root per record would repeat the root for every applied value, and an index per record would vanish with the records at rung 1. One distinct list, typically one to three entries of about 30 chars before compression, survives rungs 1 and 2. At a spawn, session records retag Inherited and their scope enters the list: `Global` as is, `Project` as the session's project root (`PinnedTier::Project.dir`, else `session.project.dir`; both written dunce-canonical). At rung 3 the list is lost with the payload, and the hook re-applies the scope: idempotent for path and list values (each element is already present and only moves, recorded `pre_existing`), while constants are re-set to what the hook composes for that scope (named residual).

**Policy write rule.** What a parent writes into a child's state. Inputs reach `to_write` through `RootFlags`, a type with exactly five fields, so no other flag can reach child policy by construction.

```rust
pub struct RootFlags {                        // root ContextOptions only
    pub config: Option<PathBuf>,              // --config  -> policy.config_path
    pub remote: bool,                         // --remote  -> policy.remote
    pub offline: bool,                        // --offline -> narrowing.offline
    pub frozen: bool,                         // --frozen  -> narrowing.frozen
    pub index: Option<PathBuf>,               // --index   -> policy.index_dir
}
```

| Source | Written into the child | Evidence |
|---|---|---|
| The five root flags above | yes, always | `context_options.rs:19-20,58-59,69-70,82-83,119-120` |
| `--no-consent` (subcommand of `exec`, `shell`) | `narrowing.no_consent`; the one named exception, narrowing only | `crates/ocx_cli/src/command/toolchain_exec.rs:229` |
| Received state (header and payload) | yes, passed on | |
| Ambient `OCX_*` | only under Clean, because the clean environment drops the variable. Each field gets its env-slot result (C8): relaxing fields write an explicit ambient value over a received one, narrowing fields write the OR, neutral fields write received over ambient | `PolicyInputs::to_write(Clean)` |
| `home` | header `h=` under Clean always; under Layer and Isolate only when the resolved home differs from what the child derives from its inherited env (an absolute ambient `OCX_HOME`, else the default under `HOME`) | |
| `--no-verify`, `--records-dir`, `--records-name` and every other subcommand flag | never | `context.rs:431-432`; `crates/ocx_cli/src/options/records.rs:15` |
| `--project`, `--global` (selectors); `--quiet`, `--jobs`, `--log-level`, `--color`, `--format` (presentation) | never | requirement 3; `quality-rust.md` checklist |
| Config-file values (mirrors, patches, records, toolchain root) | never; the child re-derives them from the same config chain | `context.rs:295-298,427-450` today forward them |

`no_verify` therefore enters a child's state only when received, or under Clean as its env-slot result: an explicit local `OCX_NO_VERIFY=0` beats a received `no_verify` and is written as `false`.

**Invariants**:

| ID | Invariant |
|---|---|
| S-1 | Recorded values are literal and raw (L-1, L-2), whatever the key is named. The one exception is a prior that is not UTF-8, recorded as `Prior::Withheld` and never restored. Amended 2026-11-07 (D-29): `RecordedValue::Present` is gone |
| S-2 | No record and no policy field for a Credential key or an `OCX_AUTH_*` key. Every other applied value is recorded as composed, literal and raw, with no name or value classifier (amended 2026-11-07, D-29: the secret classifier, `RecordedValue::Present` and `ReplayPayload::FromInherited` are deleted). Decode refuses an applied or replay record on any reserved `OCX_*`/`__OCX_*` key, the four credentials and `OCX_AUTH_*` included, with no exemption: the whole payload is unreadable (C3 steps 10-11). No writer records one, and reverting a forged record would drop the user's credential |
| S-3 | `PATH`/`PATHEXT` never `Constant` (A-02, `NEVER_CONSTANT` `reconcile.rs:29`) |
| S-4 | A record only undoes itself. A constant's prior is restored, or the key removed for prior `Unset`, only when the current value equals the recorded literal (forgery rule c), whatever the key is named. Path and list elements are removed only when present as recorded. With prior `Withheld` a record is kept and counted. Amended 2026-11-07 (D-29): the `Present` arm and its unchecked removal are gone, so a forged record cannot delete a variable it does not name the value of |
| S-5 | Outside `policy` and the header, nothing builds a filesystem path from state, with two named exceptions. (1) `PinnedTier::Project.dir`: absolute, equal to its own `dunce::canonicalize`, its consent is checked before `ocx.toml` is parsed, and only one bounded probe of `<dir>/ocx.toml` and `<dir>/ocx.lock` follows (as `ledger.rs:138`). (2) `session.tiers`: absolute, at most 8, only `stat`ed for the fingerprint, never opened. `policy` and `h` are settings with the authority of the variables they replace, validated by those variables' parsers. Compared only, never opened or joined: the `inherited_scopes` roots and `ProjectScopeHint.dir`; both are stripped of control characters (C7) before they are printed |
| S-6 | Coalescing at spawn keeps size bounded by distinct values, not depth. Two constant records for one key merge only when the chain is continuous: the newer record's prior is `Value` of the older record's `Literal` value. The merged record keeps the newer value and the older prior. A record whose prior differs (a user changed the variable between two ocx writes) stays separate. Revert runs newest first, so the user's value is restored and the older record then fails the equality guard and is kept. Path and list: one record per `(key, element)`, the oldest `pre_existing` wins. Amended 2026-11-07 (D-29): a constant coalesces on its recorded literal under any key, with no secret-name special case |
| S-7 | On Windows, keys compare ASCII-case-insensitively at encode, decode, coalesce (S-6) and the equality guard (S-4), as `EnvKey` folds them (`crates/ocx_config/src/env.rs:80-97`); the projection writes a key in the spelling the inherited environment already uses |

### C3. Codec (`ocx_config::exec_state::{encode, decode}`)

**Wire**:

```
value    = header "." payload
header   = major *( ";" segment )             ; frozen grammar: ASCII, at most MAX_HEADER = 2,048 bytes;
                                              ; every version reads at most that far
major    = 1*10DIGIT                          ; checked parse into u32
segment  = name "=" text                      ; unknown names ignored; n may repeat, in any order
  n=     narrow *( "," narrow )               ; offline | frozen | no_consent | credential_helper;
                                              ; unknown names ignored
  h=     base64url-nopad( UTF-8 absolute home path )
payload  = "" / base64url-nopad( zlib( compact JSON of the payload part of ExecState ) )
                                              ; "" is the rung-3 marker
```

The value alphabet is digits, ASCII letters, `;`, `=`, `,`, `.`, `-` and `_`. It holds none of `%`, `"`, LF or CR, so the cmd.exe emitter writes it unescaped (`crates/ocx_shell/src/shell.rs:543,613-615`; `crates/ocx_shell/src/shell/escape.rs:72-74`).

The header holds no `.` (base64url has none), so the first `.` ends it. A `1.` value from today parses as major 1 with no segments. Today's 1.x readers split on the first `.` and compare the tag with `"1"` (`ledger.rs:229-232`), so they treat every new value as absent.

**Versioning** (re-adopts A-04's additive rule):

| Change | Version | Readers |
|---|---|---|
| New optional payload field, safe to lose when an older-minor writer rewrites the state at the next spawn | minor | same major: accept any minor, ignore unknown fields |
| A field is renamed, removed or retyped, its default or meaning changes, or ignoring a newer field would make an older reader act wrongly | major (a rename may instead keep the old name readable through a serde alias) | other major: payload unreadable; header still honoured |
| Header grammar or a narrowing name's meaning | never | frozen; new narrowing names may be added and old readers ignore them |

```rust
pub const MAX_ENCODED: usize = 8_169;         // chars, whole value, every platform (derivation below)
pub const MAX_HEADER: usize = 2_048;          // bytes before the first "."; part of the frozen grammar
pub const MAX_INFLATED: usize = 256 * 1024;   // bytes
pub const MAX_RECORDS: usize = 4_096;         // applied + identity + replay records
pub const MAX_RECORDED_TIERS: usize = 8;      // ledger.rs:31-35
pub const MAX_INHERITED_SCOPES: usize = 16;   // the encoder drops the oldest above it, with the rung-3 residual

pub enum Inherited {
    Absent,
    Readable(ExecState),
    Unreadable { header: HeaderView, reason: UnreadableReason },
}
pub struct HeaderView { pub major: Option<u32>, pub narrowing: Narrowing, pub home: Option<PathBuf> }
pub enum UnreadableReason {
    HeaderInvalid, OtherMajor, TooLong, NotUtf8, Base64, Inflate, InflateLimit,
    TrailingBytes, Json, RecordLimit, TierLimit, ScopeLimit, Invalid { field: &'static str },
}

pub fn decode(raw: Option<&OsStr>) -> Inherited;
pub fn encode(state: &ExecState) -> Encoded;    // applies the overflow ladder
pub struct Encoded { pub value: Option<String>, pub truncated: Truncation, pub encoded_len: usize }
impl Inherited {
    pub fn from_process_env() -> Self;           // through the ocx_env read seam
    pub fn narrowing(&self) -> Narrowing;        // honoured for Readable and Unreadable
    pub fn home(&self) -> Option<&Path>;         // same
}
```

**Decode** (bounded; the value is untrusted input):

| Step | Input | Result |
|---|---|---|
| 1 | unset or empty | `Absent` |
| 2 | **header first**, before any check on the whole value: the raw bytes up to the first `.`, at most `MAX_HEADER`. Every `n=` name found there is honoured, whatever its order or repetition and whatever else fails. An invalid `h` (not base64url, not UTF-8, not absolute) is dropped alone. A non-ASCII byte or an unknown name ends only its own segment | a `HeaderView`. No `.` within `MAX_HEADER` bytes, or a `major` that is not 1-10 digits within `u32`, gives `Unreadable { reason: HeaderInvalid }`, still carrying the narrowing names found |
| 3 | `major` other than `MAJOR` | `Unreadable { reason: OtherMajor }` with the header view |
| 4 | whole value longer than `MAX_ENCODED`, or not UTF-8 | `Unreadable { reason: TooLong }` or `NotUtf8`, with the header view |
| 5 | payload empty (the rung-3 marker) | `Readable` with the default payload and `truncated = PayloadDropped`. Never `Unreadable`, never a stand-down |
| 6 | payload not strict base64url-nopad | `Base64` |
| 7 | inflate through `Read::take(MAX_INFLATED + 1)`: zlib error; more than `MAX_INFLATED` bytes; bytes after the zlib stream end | `Inflate`, `InflateLimit`, `TrailingBytes` |
| 8 | JSON error (including duplicate fields and an `activate` verdict), nesting past serde_json's recursion limit | `Json` |
| 9 | more than `MAX_RECORDS` records, `MAX_RECORDED_TIERS` tiers or `MAX_INHERITED_SCOPES` scopes | `RecordLimit`, `TierLimit`, `ScopeLimit` (fail closed; today tiers truncate, `ledger.rs:238`) |
| 10 | field validation: invalid env key; an applied record on a Credential or `OCX_AUTH_*` key, whatever its form; S-3 violated; `separator` outside `List` or empty; `prior` outside `Constant`; a relative path anywhere; a policy value its variable's parser refuses (for example `parse_url`'s userinfo refusal, `mirror.rs:338-341`); a reserved `OCX_*`/`__OCX_*` key in `replay.env` (no exemption for the four credentials or `OCX_AUTH_*`); a malformed digest, token or group name | `Invalid { field }` (fail closed on the whole payload, as `OCX_ENV` did, `vars.rs:182-183`) |
| 11 | no downgrade step (amended 2026-11-07, D-29): a literal under any key is kept as read. An applied or replay record on a reserved `OCX_*`/`__OCX_*` key is refused at step 10 (`applied.key`, `replay.env.key`), the whole payload unreadable | `Readable` |

Unknown JSON fields are ignored, not denied (minor versions). Size caps bound parsing either way.

**Cap derivation.** The tightest consumer is cmd.exe. Its emitter writes `SET "__OCX_ENV_STATE=<value>"` (`crates/ocx_shell/src/shell.rs:543`): `SET "` (5 chars), the name (15), `=` (1) and the closing `"` (1) add 22 chars to the value. `batch_set_value` escapes only `%` (`crates/ocx_shell/src/shell/escape.rs:72-74`), which the value alphabet never holds. cmd.exe's line limit is 8,191 chars, so the value may hold 8,191 - 22 = 8,169. An inherited variable is bound by the same 8,191 (research finding 5); even counting `__OCX_ENV_STATE=` (16 chars) that allows 8,175, so the emitted line decides. Validation runs the whole emitted line at the boundary.

**Encode overflow ladder** (writer side):

| Rung | Action | Effect |
|---|---|---|
| 0 | Encode in full | Normal |
| 1 | Drop every `Inherited` applied and identity record; keep `inherited_scopes` | `--isolate` downstream cannot fully revert; the hook still knows which scopes are inherited |
| 2 | Drop every applied record; keep header, policy, `inherited_scopes`, replay, selection, session cache | The hook cannot remove its own earlier session values (warned); repair only while `inherited_scopes` is empty (Other decisions) |
| 3 | Omit the payload; write the header and the empty payload marker | Decodes `Readable` with the default payload and `truncated = PayloadDropped` (C3 step 5). The child honours narrowing and home. `inherited_scopes` and the selection are lost, so a hook may re-apply an inherited scope (C2 residual) |

Each writer that reaches rung 1 or above warns once on stderr (spawn writers: once per spawn; the hook: once per transition, deduplicated through `messages_fp`). The warning names the rung and the encoded size.

**Truncation is sticky**: the projection writes `truncated = max(inherited.truncated, own rung)` under Layer and Isolate, so a deeper `--isolate` still knows it cannot fully revert. Clean writes its own rung only, because nothing unrecorded survives a clean start.

### C4. Exec modes and flag grammar

```rust
// crates/ocx_package_manager/src/exec_env.rs (internal)
pub enum ExecMode { Layer, Isolate, Clean }

// crates/ocx_cli/src/options/exec_mode.rs (replaces options/stack.rs)
#[derive(clap::Args)]
pub struct ExecModeOptions {
    /// Revert every value ocx applied to the calling environment, then compose.
    #[clap(long, conflicts_with = "clean")]
    isolate: bool,
    /// Start from an empty environment holding only the composed variables.
    #[clap(long)]
    clean: bool,
}
impl ExecModeOptions { pub fn mode(&self) -> exec_env::ExecMode; }
```

Flattened into `ocx exec`, `ocx shell`, `ocx package exec`, `ocx package test`, `ocx patch test`. `--isolate --clean` is a clap conflict (exit 64). `--stack` is deleted with `options/stack.rs`. New flags on `ocx shell` (`--isolate`, `--clean`) and `ocx patch test` (both) are additive.

**Semantics at each spawn**:

| Aspect | Layer (default) | Isolate | Clean |
|---|---|---|---|
| Start env | `Env::new()` (`env.rs:136-144`) | `Env::new()`, then revert every recorded applied value, newest first | `Env::clean()` (`env.rs:146-151`) |
| Revert rule | none | path and list: remove the element unless the record is `pre_existing`; constant: S-4 | none |
| Public `OCX_*` | inherited untouched | inherited untouched | absent |
| Credentials, HookLocal | removed | removed | absent |
| Composition | put in front: path `move_to_front` (`env.rs:162`), list `append_unique` (`env.rs:175`), constant replace; `pre_existing` set when the element was already present | same | same |
| Priors captured against | the start env | the start env after revert | the clean start env, so every prior is `Unset` |
| Header and `policy` written | C2 write rule | same | same, plus every ambient value and `h` |
| `applied` written | inherited records retagged and coalesced (S-6), plus this spawn's records | this spawn's records only | this spawn's records only |
| `inherited_scopes` written | the inherited list, plus the scopes of retagged session records (C2), plus `composed_scope` (C5), distinct | `composed_scope` only | `composed_scope` only |
| `replay.env` | this spawn's payload (replace) | same | same |
| `replay.launch_identities` | inherited (retagged) merged with this spawn's | this spawn's only | this spawn's only |
| `selection` | `FollowCwd`; `Pinned` for `ocx shell` | same | same |
| `session` | reset (token unclaimed) | reset | reset |
| `truncated` | sticky (C3) | sticky | own rung |
| `OCX_BINARY_PIN` | `binary_pin_value` | same | same |
| Inherited payload unreadable | one warning naming both majors; header honoured; child payload fresh | that warning plus "cannot isolate"; proceeds as layer | the warning; header honoured; received payload policy lost |
| Inherited state truncated | none | warning "cannot fully isolate" | none |

One isolate warning counts what it could not revert: "N values changed since ocx applied them were kept; M withheld values were kept".

### C5. The one projection function

```rust
// crates/ocx_package_manager/src/exec_env.rs (internal)
pub struct Spawn<'a> {
    pub mode: ExecMode,
    pub written: &'a PolicyLayer,              // PolicyInputs::to_write (C2, C8)
    pub binary_pin: Option<&'a Path>,          // Context memo (C6); None removes OCX_BINARY_PIN
    pub composed: &'a [ComposedValue],         // application order
    pub composed_owner: Owner,                 // Inherited, or Session{..,Composed} for `ocx shell`
    pub composed_scope: Option<Scope>,         // the scope an Inherited composition came from
    pub invocation: &'a [ComposedValue],       // `ocx shell --with/--env`: Session{..,Invocation}
    pub replay_env: &'a [ComposedValue],
    pub launch_identities: &'a [(String, Vec<String>)],
    pub selection: Selection,
}
pub struct ChildEnvironment { pub env: Env, pub warnings: Vec<ProjectionWarning> }

/// Builds the complete environment of a child. Callers spawn with
/// `env_clear().envs(child.env)`.
pub fn project(inherited: &Inherited, spawn: &Spawn<'_>) -> ChildEnvironment;
```

`composed_scope` is `Some` only for `ocx exec`: `Scope::Project { root }` for the project it composed (its `--with` packages included), or `Scope::Global` under `--global`. `ocx shell` composes session records, whose scope enters the list at the next spawn. The package-tier spawns, launchers and plugins compose no scope.

`ocx_package::metadata::env::apply::apply_child_env` (`apply.rs:71-86`) moves to `exec_env` as a thin adapter: it converts `Entry` to `ComposedValue` and `LaunchIdentities` to pairs, then calls `project`. `Env::apply_ocx_config` (`env.rs:207-294`), `set_forwarded_env` (`apply.rs:89-94`), `inherited_env`, `stacked_env`, `stacked_clean_env`, `keep_applied` and `revert_reconciled` (`reconcile.rs:104-186`) are deleted. `OcxConfigView` is replaced by `PolicyLayer`.

**Rule per spawn path**:

| Spawn path | Site today | Mode | `composed_owner` | `replay_env` | `selection` | Notes |
|---|---|---|---|---|---|---|
| `ocx exec` | `toolchain_exec.rs:216-243` | flag (default Layer) | Inherited | project `[env]` + `--env` | FollowCwd | `__OCX_EXEC_SCOPE` write (`:239-243`) deleted |
| `ocx shell` | `shell.rs:58-69` via `toolchain_exec` | flag | Session Composed (Project, or Global under `--global`); `--with`/`--env` as Session Invocation | same | Pinned | `warn_when_nested` (`shell.rs:58-59,79-91`) deleted |
| `ocx package exec` | `exec.rs:157-171` | flag | Inherited | `--env` | FollowCwd | |
| `ocx package test` | `package_test.rs:329-345` | flag | Inherited | `--env` | FollowCwd | Starlark `ocx.run` inherits this env |
| `ocx patch test` | `patch_test.rs:241-242` | flag (new) | Inherited | `--env` | FollowCwd | |
| `ocx launcher exec` | `launcher/exec.rs:215-218` | Layer (fixed) | Inherited | received `replay.env`, applied after the package env, passed on unchanged | FollowCwd | parity with today's forwarded payload |
| `ocx launcher shim` | `launcher/shim.rs:92-94,135` | Layer (fixed) | Inherited | none (parity with `forwarded: &[]`) | FollowCwd | |
| Plugin dispatch | `plugin_dispatch.rs:94-120` | Layer | none composed | none (parity: `OCX_ENV` cleared today, `env.rs:264`) | FollowCwd | Ambient `OCX_TOOLCHAIN_DIR`, `OCX_PATCHES`, `OCX_RECORDS_*` now reach plugins. Doc (`plugin_dispatch.rs:7-8`) corrected. Pin from a home-only `FileStructure` |
| Self-update hand-off | `app/update_check.rs:463-498` | Layer | none | none | FollowCwd | `binary_pin` = the new binary's path |
| Update-check version probe | `app/update_check.rs:355,384,402-421` | not projected | | | | Stays `env_clear` + composed entries: a hermetic, pure-version query. Named exemption |
| Credential helper | `context.rs:111-116` (`set_helper_env`); spawn in `external/docker_credential` | not projected | | | | A **fixed minimal state built once at init**, before any spawn mode or record exists: header `2;n=credential_helper`, an encoded default payload (empty policy, no records, empty replay, FollowCwd, reset session), plus `OCX_BINARY_PIN` from the Context memo, plus `__OCX_CREDENTIAL_HELPER=1` until 0.7 for an older ocx reached inside the helper (Other decisions). OQ-3 covers only whether the submodule's spawn inherits the ambient env |
| Starlark `ocx.run` | `ocx_script/src/ocx_module.rs:61-93` | `env_clear` + the projected base env of `package test` + the script overlay | | | | `RESERVED_ENV_KEYS` (`:31-42`) holds `OCX_BINARY_PIN` (`:34`) and gains `__OCX_ENV_STATE`. `is_reserved_env_key` (`:47-55`) gates both the overlay and `ocx.env` reads, so a script can neither read nor overwrite the pin or the state. The host reads the pin from `base_env` (`:284`) to recognise an ocx program, as R14 allows |
| Generated SDK spawner | `ocx_sdkgen/templates/rust/spawn.rs:42-87` | spec, not the Rust function; strips nothing (D-45, 2026-11-07): parent snapshot, caller pairs, pin last | | | | C12, OQ-2 |
| Exporters | `toolchain_env.rs:225-228,468-471`, `command/env.rs:179-183`, `direnv_export.rs:194` | not a spawn | | | | C9 |
| Hook | `activate.rs:178-238` | not a spawn | | | | C7 |
| Forge git, codesign, libc probe, launchctl, profile probe | `ocx_announce/src/forge/git_command.rs:107,382,523`; `ocx_store/src/codesign.rs:185,240`; `ocx_oci/src/host_capabilities.rs:445-472`; `ocx_setup/src/session_path/macos.rs:203-211`; `ocx_setup/src/profiles.rs:188-192` | unchanged | | | | Not ocx descendants. Named exemptions |
| Windows `.exe` shim | `ocx_shim/src/main.rs:435,477` | generated file, frozen | | | | Inherits; strips `OCX_GLOBAL`/`OCX_PROJECT` (`:238-248`, now harmless) |

**Seam enforcement**: `launch.rs` `firewall_tests` (`crates/ocx_package_manager/src/launch.rs:735-780`) gain one assertion. Every spawn site carrying `#[expect(clippy::disallowed_types)]` either calls `exec_env::project` (directly or through `apply_child_env`) or appears on the named exemption list above.

### C6. Pin value function

```rust
// crates/ocx_package_manager/src/launcher/generate.rs (beside trampoline_ocx_binary)
/// The value of `OCX_BINARY_PIN` for children of the running ocx.
pub fn binary_pin_value(fs: &FileStructure, running: Option<&Path>) -> Option<PathBuf>;
```

| Case | Result |
|---|---|
| `running` is `None` (`current_exe` failed) | `None`: the projection removes `OCX_BINARY_PIN`, so launchers fall back to their baked value or `PATH` |
| `dunce::canonicalize(running)` equals `dunce::canonicalize(installed)`, where `installed = ocx_install_bin_path().join("ocx[.exe]")` (`ocx_store/src/file_structure.rs:116-121`), **and** on Unix the `current` link, the resolved binary and every directory from `$OCX_HOME` down to both are owned by the effective uid or root and are not group- or world-writable | `installed` (the stable path through `current`) |
| otherwise (dev build, acceptance binary, Bazel, CI tool cache, a canonicalization error, a failed ownership check) | `running`, exactly as `current_exe` returned it |

Never an empty string (Windows `IF DEFINED` semantics, `adr_windows_exe_shim.md` lines 362-368). Windows applies the equality rule without an ownership check; the residual is a principal who can write `$OCX_HOME`, who can already replace package content there. **Memoised**: computed once in `Context::try_init` (replacing `context.rs:419-424`) and stored on `Context`; every spawn site and the credential-helper state read the memo. Plugin dispatch, which runs before `Context`, computes it once. `trampoline_ocx_binary` (`generate.rs:85-94`) is unchanged: the runtime pin overrides the baked value whenever it is set.

### C7. Hook behaviour per state (`ocx self activate --reconcile`, `activate.rs:178-238`)

The hook decodes once (replacing `activate.rs:187-188`). It writes only `session`, session-owned records and session-owned identity records. It never changes the header, `policy` or inherited records. It changes `inherited_scopes` and `selection` only in the two retag cases below (a foreign token; an unclaimed FollowCwd state holding Session records), and it **never writes over a state it cannot read**. It ignores a received `h`: a hook shell carries `OCX_HOME` from setup's files.

**Desired composition under FollowCwd** (owner decision 2026-11-07). The hook never composes a scope that `inherited_scopes` already holds; it only adds missing ones. Project scopes compare by dunce-canonical root.

| cwd | Desired D |
|---|---|
| inside a project whose root is in `inherited_scopes` | nothing, the global tier included: the inherited layer stands. The hook does not watch that project's `ocx.toml` or `ocx.lock`, so an edit changes nothing (exec = snapshot) |
| inside a project Q that is not inherited | the global tier (unless `Global` is inherited) and Q, Q in front, as today |
| outside every project | the global tier, unless `Global` is inherited |

**Session token.** The token names the shell process that claimed the session: its pid, which the emitted code passes in argv (`--shell-pid`, from each hook shell's own pid variable, for example `$$` in bash and zsh), plus that process's start time where ocx can read it cheaply (Linux: field 22 of `/proc/<pid>/stat`). It is an identity, not a secret, and nothing is stored in the shell for it. Re-sourcing the rc file and `exec $SHELL` keep the same process, so the session continues. A tmux pane, an IDE terminal or a nested shell is another process, so its token is **foreign**. A token without `started` is also foreign whenever this shell can read its own start time. Where no start time is readable (other platforms until WP4 adds a reader, or a failed read), the pid alone decides; pid reuse can then make a foreign token look like this shell's, and the shell manages session records whose values it already holds (named residual). Every spawn resets `session.token` to unclaimed.

| Inherited state | Behaviour |
|---|---|
| Absent | New follow-cwd session: claim the token, compose desired D for the cwd, apply, record as Session Composed. No repair |
| Readable, token foreign | Another shell instance owns these session records: a tmux or zellij server snapshot, a plain `tmux` started inside `ocx shell`, an IDE-captured environment (VS Code's `VSCODE_RESOLVING_ENVIRONMENT` login shell). Retag every Session record and identity record Inherited (their scopes enter `inherited_scopes`, as at a spawn), reset the session cache, set selection to FollowCwd, claim the token, continue as Absent but layered on top, under the desired-composition table. No special case per tool |
| Readable, token unclaimed or this shell's, FollowCwd | Claim if unclaimed. Before claiming an unclaimed token, retag every Session record and identity Inherited (and add their scopes to `inherited_scopes`): only `ocx shell` writes Session records, always Pinned, so Session records under FollowCwd with no claim are foreign or forged. Reconcile Session Composed records against D(cwd). Remove a session path or list element only when no inherited record names the same element and the record is not `pre_existing`. Restore a session constant under S-4 |
| Readable, token unclaimed or this shell's, Pinned | Claim if unclaimed. D is the recomposition of the pinned tier's **consented** `ocx.toml` groups, never the cwd and never `with_names`. Refresh on a fingerprint change of that `ocx.toml` or `ocx.lock` (S-5 exception 1). Session Invocation records stay exactly as they are: never re-applied, only kept or removed. A refresh never writes a key or element that a live Session Invocation record owns, so the invocation wins over a project edit of the same key. Ownership is read from the record's key and element; its literal is never re-applied. An unconsented pinned project keeps its spawn-time layer. On `cd` into another project: one notice naming the pinned selection, nothing applied |
| Readable, `truncated` is `AllAppliedDropped` or `PayloadDropped` | Owned-prefix repair for the session only under `AllAppliedDropped` with an empty `inherited_scopes`; a warned residual, because no inherited record is left to protect outer elements. Otherwise no repair: the hook cannot tell its own unrecorded values from an outer frame's, so stale session values may remain (warned). Under `PayloadDropped` an empty list proves nothing, because the list was lost with the payload; the hook then re-applies inherited scopes (C2 residual) |
| Unreadable (any reason, any major) | **Stand down without writing.** Revert nothing, apply nothing. Emit shell code that copies the current value **by reference** into an unexported shell variable (`__ocx_stood_down="$__OCX_ENV_STATE"` in bash and zsh, `set -g __ocx_stood_down $__OCX_ENV_STATE` in fish, `$global:__ocx_stood_down = $env:__OCX_ENV_STATE` in PowerShell; the nu and elvish forms are WP4's). The emitted code never embeds the value as a literal, so a hostile value cannot inject shell code. It re-emits the hook with a gate whose first clause skips the hook while `__OCX_ENV_STATE` equals that copy (re-emission precedent: the `ws` gate redefinition, `ledger.rs:194-202`). Print the notice once. `unset __OCX_ENV_STATE` makes the value differ, so the next prompt runs the hook and starts a fresh session |

**Stand-down notice** (stderr, ASCII). **Stripped characters**, here and everywhere a state-derived string is printed (the cd notice, `ocx shell state` plain output): C0 (U+0000-U+001F), DEL (U+007F), C1 (U+0080-U+009F, including the single-byte CSI U+009B), and the bidi controls U+061C, U+200E, U+200F, U+202A-U+202E and U+2066-U+2069.

- `found_major` lower or unparsed: `ocx: this shell's environment state was written by another ocx version (format 1; this ocx reads format 2). ocx stopped updating this shell. Run 'unset __OCX_ENV_STATE' to start over here (values already applied stay), or open a new shell.`
- `found_major` higher: the same text with "written by a newer ocx (format 3)".
- readable major, payload invalid: "could not be read (reason)".

**What a 1.x hook does with a new value**, verified: `Ledger::decode` returns `None` because the tag is not `"1"` (`ledger.rs:229-232`); `activate.rs:187-188` falls back to `Ledger::empty`; the planner then runs `repair_owned_segments` (`plan.rs:92`, `:387-424`), which strips every `$OCX_HOME`-owned `PATH` segment it did not record, and writes a `1.` value over the new one. This happens only when the shell's `current` binary is older than the ocx that wrote the state: a rollback, or a newer ocx pinned by a project. It is a named residual; old binaries cannot change. A 1.x `ocx exec` in the middle of a tree passes the new value through untouched: its `inherited_env` decodes nothing and reverts nothing (`reconcile.rs:104-108`), and its `apply_ocx_config` does not strip the carrier (`adr_shell_env_overhaul.md` line 102).

**Login stream** (`ocx self activate` without `--reconcile`, `crates/ocx_cli/src/command/self_group/activate.rs:148-161`): when the inherited selection is `Pinned` and the token is unclaimed, this is the entered shell; it claims the token and applies nothing. Under `FollowCwd` it applies the same desired-composition table as the hook: it composes only the scopes `inherited_scopes` does not hold, so a login shell started by `ocx exec -- bash -l` inside P composes nothing. This replaces both `apply_env: !yields_to_exec()` (`:153`, `:170-172`) and the reconcile yield block (`:202-217`), which read `__OCX_EXEC_SCOPE`. No pid or parent-pid test is involved. A claimed foreign token takes the retag row above. The owner's tests (a)-(e) are in Validation.

**Reversal recorded**: [833e8cb5](https://github.com/ocx-sh/ocx/commit/833e8cb5)'s "leaving the project reconciles" no longer applies to an entered (`ocx shell`) shell.

**`ExecScope`** (`ocx_package_manager/src/activation.rs:362-381`, `ocx_shell/src/shell/coexistence.rs:27,83-90`, `shell_state.rs:87-88`, `shell.rs:86`, `activate.rs:206`) is deleted. `shell_revoke.rs:38` ("open a new shell prompt") gets wording that holds in a pinned shell, where an unconsented pinned project keeps its layer.

### C8. Policy precedence in a child

| Field class | Order | How received enters |
|---|---|---|
| Narrowing (header names only) | flag ▸ received ▸ ambient ▸ config ▸ default. Booleans combine by OR: an ambient `OCX_OFFLINE=0` cannot undo a received `offline` | before the env read |
| Neutral (`remote`, `no_project`) | flag ▸ received ▸ ambient ▸ config ▸ default | before the env read |
| Relaxing (every other policy field, `no_config_refresh` and `no_update_check` included, and `home`) | flag ▸ explicitly set ambient ▸ received; then the seam's own config handling. `no_config_refresh` and `no_update_check` are relaxing because a local `=0` wins today (`crates/ocx_config/src/env.rs:268-280`) | received fills the **env slot** only when the ambient variable is unset, and the seam treats it exactly as the env value: `resolve_mirror_map`'s env entries (`mirror.rs:376-390`), `insecure_hosts(config, env)` (`insecure.rs:16-32`), `RecordsOptions::merge` (`records.rs:57-72`). SYSTEM locks clamp it as they clamp the variable today (`insecure.rs:21,26`; `records.rs:59-61`) |

```rust
// ocx_config::exec_state::policy
pub struct PolicyLayer { pub narrowing: Narrowing, pub home: Option<PathBuf>, pub policy: Policy }
pub struct PolicyInputs<'a> {
    pub flags: &'a RootFlags,
    pub no_consent_flag: bool,                 // the C2 narrowing exception
    pub received: Option<&'a PolicyLayer>,     // header always; payload when readable
    pub ambient: &'a PolicyLayer,              // explicitly set OCX_* only
}
impl PolicyInputs<'_> {
    /// The value each seam reads where it reads the variable today, plus disagreements.
    pub fn env_slot(&self) -> (PolicyLayer, Vec<Disagreement>);
    /// What this process writes into a child (C2 write rule).
    pub fn to_write(&self, mode: WriteMode) -> PolicyLayer;
}
pub enum WriteMode { Inherit, Clean }          // Layer and Isolate map to Inherit
pub struct Disagreement { pub field: &'static str }
pub enum FieldClass { Narrowing, Neutral, Relaxing }
```

`to_write(Clean)` is the `env_slot` result, field by field: the clean environment drops every ambient `OCX_*`, so the state must carry what this process itself resolved. An explicit local `OCX_NO_VERIFY=0` over a received `no_verify` therefore reaches a `--clean` child as `no_verify: false`. `to_write(Inherit)` writes the root flags and received values only; ambient values reach a Layer or Isolate child through the inherited environment.

`ContextOptions` keeps flag values apart from ambient values: `apply_env`'s fold of ambient booleans into flag fields (`context_options.rs:145-158`, the `|=` at `:153`) is replaced by `PolicyInputs`. The `OCX_GLOBAL` arm (`:147`) and its join row (`env_flags.rs:11-12`) are deleted. The credential-helper offline rule (`:156`) reads `narrowing.credential_helper`.

**Disagreement warning** (SO-8): when a received relaxing value and a different explicit ambient value meet, ambient wins and one stderr line names the field, once per process, for example `ocx: OCX_INDEX overrides the index directory the parent ocx set`.

**The trade, stated honestly**: for relaxing fields a later explicit export lower in the tree now beats a parent's flag (`ocx --index /a exec -- sh -c 'OCX_INDEX=/b ocx ...'` uses `/b`). For narrowing and neutral fields the parent's flag still wins. The split keeps a forged or stale received value from overriding an explicit local setting in the relaxing direction, at the cost of a parent flag that is not sticky. This is a deferred owner confirmation.

### C9. Exporters (`ocx env --shell/--ci`, `ocx package env --shell/--ci`, `ocx direnv export`)

When a `[patches]` tier is in effect (today's condition, `ocx_package_manager/src/lib.rs:661-681`), the exporter appends one constant entry `__OCX_ENV_STATE` at the site that appends `__OCX_LAUNCH_IDENTITIES` today. Its value is a **fresh minimal state**, never the exporter's own inherited state: header `2` carrying the narrowing names of the exporter's inherited header (so an export never drops an inherited `offline` or `frozen`) and no home, empty policy, no applied records, empty `inherited_scopes`, `replay.launch_identities` = this composition's identities owned `Inherited`, empty `replay.env`, FollowCwd, a reset session, `truncated = None`.

| Exporter | Inherited state readable | Inherited state absent or unreadable |
|---|---|---|
| `--shell`, `direnv export` | **no state entry**: the target is the running shell, and a fresh state would replace the hook's session records, its claimed token and `inherited_scopes` | the fresh minimal state |
| `--ci`, the structured report | the fresh minimal state | the fresh minimal state |

Residual: with a readable state, `eval "$(ocx env --shell)"` delivers no launch identities, so a launcher in that shell matches patch rules only by `*`, as a launch with no identities does today.

These copies are persisted: `$GITHUB_ENV` and GitLab dotenv files for the rest of a job, rc evals for a shell's life, direnv caches until the next export. A later ocx of another major reads the payload as unreadable (one warning) and still honours the header. Because the state holds no applied records, a later step with a step-level `env: PATH:` override, or a GitHub composite action's own steps, cannot have segments claimed or reverted by it.

### C10. Dump surface: `ocx shell state` carries it

`ocx shell state` already decodes the carrier (`crates/ocx_cli/src/command/shell_state.rs:71-75`). The report's `ledger` field (`crates/ocx_cli/src/api/data/shell_state.rs:249-259`) is replaced by `env_state`. `binary_pin` is added. `carrier_present`, `carrier_bytes` (`:238-247`) and `priors` (`:298-299`) stay. Plain rendering: the default tier prints the status and any note; `--verbose` prints the decoded fields, with the C7 strip set removed from every state-derived string. The JSON is complete at both tiers, as today; JSON escaping carries control characters safely there.

```json
{
  "env_state": {
    "status": "readable",
    "major": 2,
    "minor": 0,
    "narrowing": ["offline"],
    "home": "/home/u/.ocx",
    "truncated": "inherited_dropped",
    "policy": { "no_verify": true },
    "relaxing_policy": ["home", "no_verify"],
    "session_claimed": true,
    "inherited_scopes": [ { "type": "global" }, { "type": "project", "root": "/work/lib" } ],
    "applied": [
      { "key": "PATH", "value": "/home/u/.ocx/packages/ocx.sh/sha256/ab/cd/content/bin",
        "kind": "path", "owner": "inherited" },
      { "key": "JAVA_HOME", "value": "/opt/jdk-21", "kind": "constant",
        "owner": "session", "scope": "project", "source": "composed", "prior": { "type": "unset" } },
      { "key": "GITHUB_TOKEN", "value": "tok-sentinel", "kind": "constant",
        "owner": "session", "scope": "project", "source": "invocation", "prior": { "type": "unset" } }
    ],
    "replay": { "env_count": 2, "launch_identity_count": 1 },
    "selection": { "type": "pinned_project", "project_dir": "/work/app", "groups": ["default"] }
  },
  "binary_pin": { "path": "/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx",
                  "exists": true, "matches_running": true }
}
```

| Field | Rule |
|---|---|
| `status` | open enum: `absent`, `readable`, `unreadable` |
| `major`, `minor` | `major` always present (the major this ocx reads); `minor` when readable |
| `found_major` | present only for `unreadable` when the header parsed |
| `narrowing`, `home` | from the header; present for `readable` and `unreadable` |
| `truncated` | omitted when nothing was dropped; else `inherited_dropped`, `all_applied_dropped` or `payload_dropped` |
| `inherited_scopes` | omitted when empty; tagged union `global`, `project` (`root`), oldest first |
| `policy` | only set fields; same names as C2 |
| `relaxing_policy` | the set fields classified relaxing in C2, plus `home`, sorted |
| `session_claimed` | whether a shell instance claimed the session (the token itself is not printed) |
| `applied` | the current `ledger` report vocabulary (`kind`, not the persisted `type`, `ledger.rs:47`), plus `owner`, `scope` and `source` (session only), `pre_existing` (when true), `prior` (constants only, tagged union with `unset`, `value`, `withheld`). `value` is always present and is the exact string ocx wrote; there is no `withheld` field (removed 2026-11-07, D-29), and `withheld` survives only as a prior type for a non-UTF-8 prior |
| `selection` | tagged union: `follow_cwd`, `pinned_project` (`project_dir`, `groups`, optional `with_names`, `env_keys`), `pinned_global` (`groups`) |
| `binary_pin` | `path` omitted when unset; `exists`; `matches_running` (canonical comparison with `current_exe`) |

New `Note` variants (`shell_state.rs:104-105`, tagged): `env_state_unreadable { major, found_major?, reason }`, `env_state_stood_down`, `env_state_truncated { truncated }`, `relaxing_policy { names }`, `binary_pin_missing { path }`. Each carries a one-line fix. The report stays exit 0 in every reportable state. `env_state_stood_down` is reported when the payload is unreadable and the shell's hook is active, because the stood-down copy lives only in the shell. Removing `ledger` is a report break, pre-baseline, carried in a `!` commit subject.

### C11. Execution record

`Scope::Project.stack` (`crates/ocx_package_manager/src/record/execution_record.rs:319-321,565-566,674`, unreleased, [833e8cb5](https://github.com/ocx-sh/ocx/commit/833e8cb5)) is deleted. A record-level `env_mode` (`layer` | `isolate` | `clean`, snake_case, open enum) is added. `schemaVersion` stays 1 (`ocx_schema/src/lib.rs:44-47`). The existing `clean_env` field (`toolchain_exec.rs:280`, `exec.rs:198`, `launcher/exec.rs:256`) is kept unchanged; whether it is released is not verified here.

### C12. SDK spawn spec (`ocx_sdkgen` template and sibling SDKs)

The generated runtime cannot link `ocx_config`. It implements this subset, written into the SDK spawn contract (`adr_ocx_interface_contract.md` line 348) and checked by a conformance fixture under `crates/ocx_schema/contract/conformance/`:

| Rule | Today (`spawn.rs`) | Required |
|---|---|---|
| Child environment | the parent snapshot minus every `OCX_*`/`__OCX_*` (`:36-66`) | the parent snapshot, unchanged (D-45, 2026-11-07: the SDK strips nothing and the caller owns its environment), then the caller's pairs (a caller key replaces the parent entry of the same key), then `OCX_BINARY_PIN` written last |
| Pin = the binary the SDK runs, absolute, never empty | holds (`runtime.rs:519-532`) | holds |
| `__OCX_ENV_STATE` | dropped with every `__OCX_*` (`:36-40`) | passes through like any parent entry (D-45): an explicit `ocx exec` composes what it was asked for, so an inherited state gates only the activation hook |
| User-input `OCX_*` | dropped | passes through (D-45); the SDK removes nothing |
| Reading received narrowing | none | optional: an SDK may parse the frozen header grammar (C3) without decoding the payload |

## Non-Functional Requirements

| Area | Requirement | Measured by |
|---|---|---|
| Latency | Decoding a state at `MAX_ENCODED` adds at most 1 ms p99 to a hook prompt. The no-op path stays stat-only after decode (`activate.rs:234`). The stood-down shell skips the hook in the gate, with no process start | `test/bench/shell_latency.py` against a cap-sized fixture |
| Size: cmd.exe | Encoded value at most 8,169 chars: the emitted line `SET "__OCX_ENV_STATE=<value>"` is the value plus 22 chars, and cmd.exe's line limit is 8,191 (derivation in C3); cmd.exe also ignores a longer inherited variable | codec unit test; Windows boundary test of the whole emitted line |
| Size: Windows env block | 32,767 chars per variable; the cap is a quarter of it | codec unit test |
| Size: Linux, macOS | 131,071 B per string (Linux); 1 MiB `ARG_MAX` total (macOS). The cap uses under 7% of the Linux string limit | codec unit test |
| Size: headroom | About 120 composed values fit compressed (research, zlib: 3,426 chars at 50, 12,447 at 200). Overflow follows C3 | acceptance test near the cap |
| Security: untrusted input | Decode is bounded and fails closed (C3 steps 1-11): header first within the frozen `MAX_HEADER`, checked major parse, length before base64, `take`-bounded inflate, trailing-byte rejection, record, tier and scope caps, per-field validation by the replaced variable's parser; a record on a reserved key makes the payload unreadable. The stood-down gate copies the value by reference, never as a literal in emitted code (C7) | unit tests per arm, including a zlib bomb fixture; per-shell gate test with a hostile value |
| Security: policy | Trust equals today's public forwarding: any principal that can set `__OCX_ENV_STATE` can set every variable it replaces. Received relaxing values enter at the env slot, below SYSTEM locks, and lose to an explicit ambient value (C8). Under `sudo -E` the residual is the same as A-06 states and as today for `OCX_CONFIG` or `OCX_NO_VERIFY` | unit tests over `PolicyInputs`; `shell state` notes |
| Security: secrets | The state is readable wherever the environment is: `/proc/<pid>/environ`, `ps e`, crash dumps, CI log env headers. It never holds a Credential or `OCX_AUTH_*` value (S-2). Applied values are recorded as composed whatever the key is named (D-29). Under Clean, priors are captured against the clean start, so `--clean` never copies the caller's values into state. **Residual**: every value is recorded, secret-named or not, which is the exposure the environment already has; a prior stays in the state after the user changes the variable, so the state can widen exposure in time | registry table test; acceptance test with a `GITHUB_TOKEN` constant recorded verbatim |
| Operability | `ocx shell state` shows status, versions, narrowing, policy, relaxing fields, applied records with owners and values, selection and the pin, plus notes with fixes (C10). Every writer warns once on overflow; every reader warns once on an unreadable payload | acceptance tests on `--format json` |
| Compatibility: version | Same major: any minor. Other major: the hook stands down without writing; exec family, launchers and plugins warn once and ignore the payload; every reader honours the header; `shell state` reports | acceptance tests with a `1.` value and a crafted major-3 value |
| Compatibility: on disk | Frozen and unchanged: the name `OCX_BINARY_PIN` and its absolute-path shape; `launcher exec '<root>' --`; `launcher shim '<id>' --`; `--project '<root>' exec --`; `--global exec --`; the trampoline's `unset OCX_GLOBAL OCX_PROJECT` line; the Windows `.exec` sidecar `<root>\n<abs ocx>` (`body.rs:111-129`). New frozen surface: the header grammar (C3) | bodies of the previous release placed on disk in acceptance |

## Consequences

**Positive**:

- One write path, one projection; 11 rules where today needs dozens (Rule inventory).
- The leaked-selector class is closed: no ocx process writes `OCX_GLOBAL`, `OCX_PROJECT` or any other public `OCX_*`.
- Nesting gives "double env" everywhere; launchers and exec agree.
- `--isolate` works at any depth from one bounded record, and keeps a user's own pre-existing `PATH` entries.
- Launchers follow `ocx self update` through the stable pin.
- Narrowing policy and the home survive a version boundary through the header.
- An exec-spawned shell keeps what its spawner composed: the hook never reloads an inherited scope (owner decision), without `__OCX_EXEC_SCOPE` or any pid test.

**Negative**:

- Exec = snapshot: in an exec-spawned shell, an edit to the inherited project's `ocx.toml` or `ocx.lock` takes no effect. `ocx shell` is the live form.
- Rung 3 loses `inherited_scopes`, so a hook may re-apply an inherited scope; path and list values are idempotent, constants are re-set (C2 residual).
- With a readable state, `ocx env --shell` and `direnv export` deliver no launch identities (C9 residual).
- `__OCX_CREDENTIAL_HELPER=1` stays as a third ocx-written variable in one exempt spawn until 0.7 (Other decisions).
- Open hook shells stand down after the upgrade that introduces major 2, and after any later major break, until reopened or `unset __OCX_ENV_STATE`.
- An interactive shell started with `ocx exec -- bash`, or a plain `bash` typed inside a hook shell, is a fresh follow-cwd session layered on top. `ocx shell` is the way to enter an environment.
- For relaxing fields, a parent's flag is not sticky: an explicit export lower in the tree wins (C8).
- A `--clean` child gets no `OCX_HOME`, so a login shell started inside `--clean` does not activate ocx; ocx inside it finds the home through the header.
- An older ocx pinned by a project and called by name inside a new tree sees no forwarded policy and no replay (the `adr_live_env_reload.md` OD-4 caveat). A 1.x hook overwrites a new state (C7 residual).
- A tool that read a forwarded `OCX_*` to learn its parent's settings no longer sees one; it can read the header.
- Amended 2026-11-07 (D-29): no restore or removal skips the equality check; the former unchecked removal for a secret-classed key is gone.

**Risks**:

| Risk | Mitigation |
|---|---|
| A spawn site added later skips the projection | firewall assertion (C5) |
| Overflow in a large monorepo | three-rung ladder, warning, `shell state` note; revisit with digests for inherited records if real projects hit rung 1 |
| Received policy hides a relaxing setting | env-slot entry, ambient wins, disagreement warning, dump listing |
| Windows stable path does not canonicalize as expected | OQ-1; fallback is the exact running binary, which is today's behaviour |
| Per-shell re-emission of the stood-down gate differs across shells | WP4 tests every hook shell (bash, zsh, fish, nu, elvish, PowerShell), each with a value holding `'`, `$(...)` and a newline |
| A project root compares unequal across spellings (symlinked checkout, case on Windows) | roots are written and compared dunce-canonical; a mismatch composes the scope again on top, which is the pre-decision behaviour, not a loss |

## Migration and Rollout

| Item | Plan |
|---|---|
| Open hook shells | Hold a `1.` state. The new hook (it runs the `current` binary, `activate.rs:155`) reads it as major 1, stands down without writing, prints one notice and re-emits the gate. Exec calls between the upgrade and the next prompt warn once each |
| Rollback | A shell whose state a new ocx wrote, run by an older `current`, loses outer store paths and its state (C7 residual). Documented; no code can prevent it |
| Generated files on disk | Unchanged and working: launchers and shims read `${OCX_BINARY_PIN:-ocx}` (`body.rs:15-37`); trampolines keep `unset OCX_GLOBAL OCX_PROJECT` and `${OCX_BINARY_PIN:-<baked>}` (`body.rs:74-100`); the Windows shim reads the pin (`ocx_shim/src/main.rs:250-253`). Refresh stays deferred to [#679](https://github.com/ocx-sh/ocx/issues/679) |
| `OCX_GLOBAL` users | One stderr notice per process naming `--global`, until 0.7 (R6). The cli.json `retired` array publishes the row; the sweep (`test/lint/test_deprecated_spellings.py:151-181`) must exempt the frozen trampoline body (`body.rs`) and the Windows shim strip (`ocx_shim/src/main.rs:238-248`), which keep the name on purpose |
| Lockstep satellite | ocx-mirror is upgraded in the same change series and `task satellite:verify` must pass. What it sees: `crates/ocx_config/Cargo.toml` gains `base64` (lockfile change); `ocx_config` loses `apply_ocx_config` and `OcxConfigView` and gains `exec_state`; `ocx_package::apply_child_env` moves to an internal crate; `ocx_env` replaces `Child` with `Propagation`, extends `retired.rs` (it gained `is_secret_name` first; removed 2026-11-07, D-29). A plugin that read a forwarded `OCX_*` reads the header or decodes `exec_state` instead. Whether ocx-mirror calls `apply_ocx_config` today is not verified here |
| Cross-repo | ocx-sdk-python: its `OCX_ENV` pass-through becomes dead, and its fixtures that snapshot ocx's reserved-key sentence must be re-checked against wherever the sentence still renders (`--env` help, the `[env]` refusal). rules_ocx: omitting `OCX_ENV` from its neutralized set stays correct. grimoire: the `env -u OCX_GLOBAL` workaround becomes unnecessary after release |
| Product context | `.claude/rules/product-context.md` principle 4 and differentiator 3 are edited in the same change series (WP11); owner confirms the wording |
| Tests to rewrite (acceptance) | `test_exec_forwarding.py`, `test_exec_modes.py`, `test_exec_clean_home.py`, `test_plugin_dispatch.py`, `test_launcher_exec.py`, `test_global_toolchain.py`, `test_project_config_home_walk.py`, `test_toolchain_cli.py`, `test_trampoline_exec.py`, `test_shell_open.py`, `test_shell_reconcile.py`, `test_shell_reconcile_edge_cases.py`, `test_execution_records.py`, `test_patches.py`, `shell_matrix.py`, `bench/shell_latency.py`, `recordings/`, `manual/test-windows-activation.ps1`. `test/SUITE_FLOOR` may move |
| Tests to rewrite (unit and goldens) | `ocx_config/src/env.rs` `apply_ocx_config` tests (including `:1243-1270`, `:1616-1640`), `apply.rs` child-env tests (`:415`, `:770-815`), `plugin_dispatch.rs` tests, `ocx_module.rs:535-632`, `context_options.rs:227-238`, `render_toolchain.rs:2158`, `reconcile.rs`, `ledger.rs` (20 tests), `hook.rs`, `activate.rs`, `activation.rs` `ExecScope` tests, `coexistence.rs`, `options/stack.rs`, `shell.rs:260,300`, `ocx_package_manager/src/lib.rs:313-339`, `launch.rs` firewall, `retired.rs` tests (`:186-217`, `:282-306`), goldens `ocx_schema/tests/golden/cli.json` (`:95`, `:12015`, `:12414`, `:12701`, `:14126`, `:14219`, `:14605`, `:14629`), `execution-record.json:279`, `ocx_sdkgen/tests/golden/rust/env.rs:116` and `types.rs`. Kept unchanged: `body.rs:571-643`, `context.rs` exclusivity tests, `shims.rs:1196-1204`, `activate.rs:1671` |
| Docs to reconcile | `reference/environment.md` (pin `:106-115`, `OCX_GLOBAL` `:230-241`, `OCX_HOME` `:250`, the forwarded sentences, `OCX_PROJECT` `:604-606`, plumbing pages `:792-835`); `reference/env-composition.md` (`:10,37-47,59-62,73,253,274-275`); `explanation/environments.mdx` (`:169,191-199,273`); `explanation/shell-integration.mdx` (`:55-57,78-82`); `explanation/toolchain-activation.md` (`:110-112`); `explanation/launcher-forwarding.md` (`:40-41,47-64`); `reference/command-line/**` (regenerated); `guides/project-toolchain.mdx`; `reference/execution-records.md` (`env_mode`). One model, present tense, no migration prose |
| AI config follow-up (separate `chore:` commit) | `arch-principles.md:95`, `subsystem-cli.md:44,329-332` and its "OCX Configuration Forwarding" section, `subsystem-cli-commands.md:78`, `subsystem-package-manager.md` "OCX Configuration Forwarding", `quality-rust.md` checklist line naming `apply_ocx_config`. `product-context.md` is not here: it moves with WP11 |
| Commit-subject breaks (`!`) | `feat(exec)!: ocx exec, ocx shell, ocx package exec, ocx package test and ocx patch test layer onto the calling environment; --isolate reverts what ocx applied first` · `feat(env)!: ocx ignores OCX_GLOBAL with a warning; select the global toolchain with --global` · `feat(env)!: ocx no longer writes OCX_* settings into child processes, and shells opened before the upgrade stop reconciling until reopened` · `fix(env)!: OCX_BINARY_PIN names the installed ocx when it is the running one, so launchers follow ocx self update` · `feat(shell)!: ocx shell stays on the environment it opened and refreshes it when ocx.toml or ocx.lock change` · `feat(shell)!: ocx shell state reports env_state in place of ledger` |
| Non-breaking subjects | `feat(records): execution records carry env_mode` · `fix(plugin): plugins receive ambient OCX_TOOLCHAIN_DIR, OCX_PATCHES and OCX_RECORDS_* unchanged` · `fix(self): ocx self update hands off without the old binary's pin` |
| User docs | No migration prose. The changelog is the commit subjects |

## Implementation Plan

Contract-first: WP1 and WP2 land as stubs before the rest fan out. Parallel WPs are file-disjoint; WP13 runs alone at the end and is the only WP that deletes the old seams.

| WP | Scope | Files (owner) | Depends on |
|---|---|---|---|
| WP1 | Registry classes, `Withdrawn` row (C1, R6); the secret classifier it first built was removed 2026-11-07 (D-29) | `crates/ocx_env/src/{lib,vars,retired}.rs`, registry tests | none |
| WP2 | Wire types, header, codec, policy resolution (C0, C2, C3, C8 types) | `crates/ocx_config/src/exec_state.rs` and `exec_state/**`, `crates/ocx_config/src/lib.rs` (module line), `crates/ocx_config/Cargo.toml` (`base64`) | WP1 |
| WP3 | Pin value with ownership check (C6) | `crates/ocx_package_manager/src/launcher/generate.rs` | none |
| WP4 | Hook, session token, stand-down, pinned refresh (C7) | `crates/ocx_shell/src/shell/reconcile/{ledger,plan}.rs`, `crates/ocx_shell/src/shell/hook.rs` (token, stood-down gate, elvish `__OCX_ENV_PWD`), `crates/ocx_shell/src/shell/coexistence.rs`, `crates/ocx_package_manager/src/activation.rs`, `crates/ocx_cli/src/command/self_group/activate.rs`, `crates/ocx_cli/src/command/shell_allow.rs`, `crates/ocx_cli/src/command/shell_revoke.rs` | WP2 |
| WP5 | Projection and spawn sites (C4, C5) | `crates/ocx_package_manager/src/exec_env.rs` and `exec_env/**`, `crates/ocx_package_manager/src/launch.rs` (firewall), `crates/ocx_package/src/metadata/env/apply.rs` (`From<&Entry>`), `crates/ocx_cli/src/command/{toolchain_exec,exec,package_test,patch_test,shell}.rs`, `command/launcher/{exec,shim}.rs`, `app/plugin_dispatch.rs`, `app/update_check.rs`, `app/context.rs` (pin memo, helper state, `Withdrawn` notice arm), `crates/ocx_script/src/ocx_module.rs` | WP2, WP3, WP6 |
| WP6 | CLI grammar, `RootFlags`, precedence split (C4, C8) | `crates/ocx_cli/src/options/exec_mode.rs` (new), `options/stack.rs` (delete), `crates/ocx_cli/src/options.rs` (module lines), `app/context_options.rs`, `app/env_flags.rs` | WP2 |
| WP7 | Exporters, fresh minimal state (C9) | `crates/ocx_cli/src/command/{toolchain_env,env,direnv_export}.rs`, `crates/ocx_package_manager/src/lib.rs` | WP2 |
| WP8 | Record field (C11) | `crates/ocx_package_manager/src/record/execution_record.rs` | none |
| WP9 | Dump (C10) | `crates/ocx_cli/src/command/shell_state.rs`, `crates/ocx_cli/src/api/data/shell_state.rs` | WP2, WP4 |
| WP10 | SDK spec and conformance (C12) | `crates/ocx_sdkgen/templates/rust/spawn.rs`, conformance fixture | WP2 |
| WP11 | Goldens, docs, references, product context | `crates/ocx_schema/tests/golden/**`, `crates/ocx_sdkgen/tests/golden/**`, `website/**`, `.claude/rules/product-context.md` | WP1-WP10 |
| WP12 | Acceptance tests and exploratory session | `test/tests/**`, `test/lint/test_deprecated_spellings.py` (exemptions) | WP1-WP10 |
| WP13 | Delete the old seams | `crates/ocx_config/src/env.rs` (`apply_ocx_config`, `OcxConfigView`, `forward!`), `crates/ocx_shell/src/shell/reconcile.rs` (`inherited_env`, `stacked_env`, `stacked_clean_env`, `keep_applied`, `revert_reconciled`) | WP4, WP5, WP7 |

## Validation

Carried from the discussion's Verification section, corrected by the claim diff and the review panel. Every test is shown red against today's code (or against a planted mutation, for new code) before it counts.

**Unit**

- [ ] Registry table test over `ocx_env::all()`: each class has its child outcome under Layer, Isolate and Clean; every `secret` declaration is Credential or SecretInput (C1).
- [ ] No classifier (amended 2026-11-07): `ocx_env` exports neither `is_secret_name` nor `value_has_url_userinfo`; a grep over `crates/` and `test/` finds neither.
- [ ] Codec, one test per decode arm (C3 steps 1-11): over-length, non-UTF-8, no `.` within `MAX_HEADER` bytes, an 11-digit major, bad `h` (dropped alone), other major, padded base64, a zlib bomb fixture, trailing bytes, duplicate field, `verdict = activate`, nesting past the limit, `MAX_RECORDS` + 1, nine tiers, `MAX_INHERITED_SCOPES` + 1, relative policy path, invalid env key, `PATH` as constant, reserved replay key, an applied record under a Credential key and under `OCX_AUTH_*`, a mirror URL with userinfo. Each decodes to `Unreadable` with its reason. Header first: an over-length or non-UTF-8 value whose header carries `n=offline` still yields `offline`; two `n=` segments in either order are both honoured; an unknown segment before `n=` does not hide it.
- [ ] Codec (C3 steps 10-11, amended 2026-11-07): a literal under any key decodes `Readable` as read; an applied or replay record on a reserved `OCX_*`/`__OCX_*` key makes the payload unreadable; the retired `present` and `from_inherited` forms are unreadable too.
- [ ] Codec versioning: an unknown payload field decodes; a higher minor decodes; a payload field-name golden reds a rename that has neither a major bump nor a serde alias (X-4); overflow rungs 1 to 3; rungs 1 and 2 keep `inherited_scopes`; a header-only value (rung 3) decodes `Readable` with the default payload and `truncated = PayloadDropped`, never `Unreadable` (RV-S2).
- [ ] `PolicyInputs::env_slot`, one row per field and class: flag, received, ambient and their combinations; relaxing ambient beats received and yields one `Disagreement`; a local `OCX_NO_CONFIG_REFRESH=0` or `OCX_NO_UPDATE_CHECK=0` beats a received `true`; SYSTEM-locked insecure host and records dir clamp a received value.
- [ ] `PolicyInputs::to_write`: a process run with `--no-verify` writes no `no_verify` into child policy; a received `no_verify` passes on; `to_write(Clean)` equals `env_slot`, so a received `no_verify` with a local `OCX_NO_VERIFY=0` writes `false`; `RootFlags` has exactly five fields (compile-time shape test).
- [ ] Projection: a spawn retags session records inherited and coalesces (S-6, S-7); `ocx shell` tags Composed and Invocation; priors under Clean are `Unset`; a secret-named constant records its literal value; credentials and HookLocal removed; no public `OCX_*` written; `truncated` stays sticky after a rung-2 state is spawned under Layer and resets under Clean.
- [ ] `inherited_scopes`: Layer writes the inherited list plus retagged session scopes plus `composed_scope`, distinct and canonical; Isolate and Clean write `composed_scope` only; the 17th scope drops the oldest.
- [ ] Coalescing chain (X-2): `ocx exec` sets `JAVA_HOME=a`, the user sets `JAVA_HOME=u`, a nested `ocx exec` sets `b`. The two records stay separate; `--isolate` at the next depth restores `u` and keeps the older record (equality guard). Without the user change they merge into one record with the oldest prior.
- [ ] Isolate: newest-first revert; the equality guard keeps a changed constant; a `pre_existing` `PATH` element survives; a withheld prior is kept and counted; a depth-2 chain restores the pre-ocx value.
- [ ] Hook: touches only session records; never removes an element an inherited record names; a foreign token retags and starts a fresh session; Session records under FollowCwd with an unclaimed token are retagged before the claim; a token without `started` is foreign on a host that reads this shell's start time; a received `h` is ignored; Pinned refresh composes only consented groups and keeps Invocation records; unreadable state is never written over.
- [ ] Pinned refresh overlap (X-3): `ocx shell --env JAVA_HOME=/x`, then an `ocx.toml` edit that sets `JAVA_HOME=/y` and adds a `PATH` element an Invocation record already owns. After refresh `JAVA_HOME` is `/x` and the element is not duplicated.
- [ ] Hook scope table (owner decision): desired D is empty inside an inherited project; global plus Q in an uninherited project; global outside, unless `Global` is inherited. Repair is skipped while `inherited_scopes` is non-empty and after rung 3.
- [ ] Credential-helper state: header `2;n=credential_helper`, an encoded default payload, the pin, and `__OCX_CREDENTIAL_HELPER=1`.
- [ ] Pin: installed and running equal with safe ownership gives the stable path; a group-writable `current` parent gives the running path; a different build gives its exact path; `None` removes the pin; never empty; computed once per process.
- [ ] Windows keys: `Path`, `PATH` and `path` round-trip through encode, decode, coalesce and the guard ([rust-lang/rust#85270](https://github.com/rust-lang/rust/issues/85270), [rust-lang/cargo#11814](https://github.com/rust-lang/cargo/issues/11814)).
- [ ] The C7 strip set (C0, DEL, C1 including U+009B, the listed bidi controls) is removed from state-derived strings before the stand-down notice, the cd notice and `shell state` plain output; a project root holding each class is printed without it.
- [ ] Idempotency stays pinned: `move_to_front` and emitter parity (`shell/tests_path_parity.rs`), `append_unique` (`ocx_util/src/list.rs:23-38`), constants replace; the cmd.exe one-time non-dedup (`ocx_shell/src/shell.rs:250`) is the known exception.

**Acceptance (`test/tests/`)**

- [ ] Nesting `a` then `b` (default): `b`'s tools in front, `a`'s reachable. Red today only when the outer layer is the hook's (default exec reverts the hook's ledger values); a plain `ocx exec -- ocx exec` chain is green today because exec writes no carrier, so its red is shown by a planted mutation (plan D-10).
- [ ] Owner rule (2026-11-07): an explicit `ocx exec` composes what it was asked for regardless of `inherited_scopes`. `ocx --project a exec -g g1 -- ocx --project a exec -g g1 -- …` (same project, same group) gives the inner command g1's values, each path or list element once; `ocx --project a exec -g g1 -- ocx --project a exec -g g2 -- …` (same project, another group) gives it g1 and g2, g2 in front. Shown red first: run from a hook shell inside project `a` against today's default-exec revert, and for the plain chain by a planted guard that skips composition when `inherited_scopes` holds the scope. Paired with hook case (a) below: in the shell such a chain spawns inside `a`, the hook composes nothing. Inherited scopes gate only the hook and the login stream.
- [ ] Owner decision, in an interactive shell started by `ocx --project P -g x exec -- bash` (each shown red first): (a) inside P the first prompt changes nothing, global tier included, and the login stream composes nothing; (b) `cd` into Q layers the global tier and Q as session records; (c) `cd` back into P removes them and leaves P's values byte-identical; (d) an edit to P's `ocx.toml` changes nothing at the next prompt; (e) outside every project the global tier is composed only when `Global` is not inherited (`ocx --global exec -- bash` composes nothing there).
- [ ] Stood-down gate per hook shell (RV-X6): an unreadable value holding `'`, `$(touch pwned)` and a newline stands the hook down; no file is created; the next prompt skips the hook; `unset __OCX_ENV_STATE` re-arms it.
- [ ] `--clean` keeps a local relaxing override (RV-X5): with a received `no_verify` and `OCX_NO_VERIFY=0` exported, `ocx exec --clean -- ocx shell state --format json` shows `no_verify: false`.
- [ ] Exporters (RV-X2): `eval "$(ocx env --shell)"` and `direnv export` inside a hook shell with a readable state emit no `__OCX_ENV_STATE`, and the hook's next prompt sees its own state unchanged; `ocx --offline ... env --ci` writes a state whose header carries `n=offline`.
- [ ] `--isolate` at depth 1 under a hook shell and at depth 2 in an exec chain: pristine environment plus `c` only; a user's own pre-existing `PATH` entry survives.
- [ ] `--clean` is hermetic, and a launcher inside it finds ocx through the pin and the home through the header. On Windows, the shim skips its pre-check and `launcher exec` validates against the header home.
- [ ] Mixed-format launcher: a tree started under one major, a launcher run by a build that reads another (a crafted major-3 state carrying `n=offline` and `h=`): the launcher's ocx runs offline, finds the home and warns once.
- [ ] Previous release as a middle hop: new `ocx exec` then the previous release's `ocx exec` then new `ocx exec`. The inner ocx decodes the outer state passed through unchanged and layers both. The previous release's hook against a new state shows the documented C7 residual.
- [ ] Launcher re-entry and `ocx exec` produce the same layering.
- [ ] A parent's `--offline` meets a child's ambient `OCX_OFFLINE=0`: the child stays offline.
- [ ] A parent's `--index /a` meets a child's explicit `OCX_INDEX=/b`: the child uses `/b` and warns once.
- [ ] A descendant of `ocx --no-verify ...` (where the command accepts it) shows no `no_verify` in `ocx shell state --format json`.
- [ ] Long-lived descendant: a shell it spawns owns no session values and activates fresh (follow-cwd).
- [ ] tmux started inside `ocx shell`: a new pane activates for its cwd, not the pinned selection, and owns none of the pinned shell's session records.
- [ ] `source ~/.bashrc` and `exec $SHELL` in a hook shell keep the session: leaving the project afterwards removes the session's values.
- [ ] IDE-captured environment (a state with a claimed foreign token) opened in another project: the shell activates that project on top and reverts nothing inherited.
- [ ] Upgrade: after `current` moves to a new build, a launcher in a pre-existing environment runs the new build.
- [ ] On-disk compatibility: launcher, shim and trampoline bodies as the previous release generated them still work.
- [ ] Pinned `ocx shell`: `cd` into another project applies nothing and prints one notice; an `ocx.toml` edit refreshes the layer; `--with` and `--env` values survive the refresh unchanged; the rc's login stream applies nothing.
- [ ] [#678](https://github.com/ocx-sh/ocx/issues/678) regression: with `OCX_GLOBAL` exported, the hook and `ocx shell allow` resolve the same project (R2, `shell_allow.rs:32`). An exported `OCX_PROJECT` is honoured identically by both (already true; pinned).
- [ ] [grimoire#297](https://github.com/grimoire-rs/grimoire/issues/297) regression: an exported `OCX_GLOBAL` no longer selects the global toolchain, and one notice names `--global`; the exit code is unchanged.
- [ ] Trampoline: a **global** trampoline under an exported `OCX_PROJECT` does not exit 64 (the `unset` line's job).
- [ ] Idempotency: applying the global composition twice yields no duplicate `PATH` or list entries.
- [ ] Format mismatch: a `1.` state gives exactly one warning and the exec proceeds; the hook stands down with one notice, never rewrites the value, stays silent on later prompts, and `unset __OCX_ENV_STATE` re-arms it.
- [ ] Exporters under a `[patches]` tier: a launcher started from the exported `PATH` (and from a GitHub `$GITHUB_ENV`) matches patch rules by name; the exported state holds no applied records and no policy.
- [ ] GitHub Actions: an exported state across composite-action steps and a step-level `env: PATH:` override; `--isolate` in that step removes nothing the step set.
- [ ] Plugin dispatch: ambient `OCX_TOOLCHAIN_DIR` reaches the plugin; credentials do not; `OCX_ANNOUNCE_TOKEN` does.
- [ ] Starlark: `ocx.run(env=...)` overriding `OCX_BINARY_PIN` or `__OCX_ENV_STATE` is refused; `ocx.env("__OCX_ENV_STATE")` is refused.
- [ ] Self-update hand-off: the child does not inherit the old pin.
- [ ] Overflow: a project near the cap gets rung 1 and one warning; `shell state` reports `truncated`; a spawn after rung 2 keeps `all_applied_dropped`.
- [ ] Secrets: a project `[env]` constant `GITHUB_TOKEN` appears in `shell state` and in the decoded payload with its composed value; the four credentials never appear.
- [ ] cmd.exe boundary, whole emitted line: a batch file holding `SET "__OCX_ENV_STATE=<8,169 chars>"` sets the variable, and a child started from it sees all 8,169 chars; a cmd.exe child that inherits the variable at 8,169 chars sees it; a state of 8,170 chars is refused by the writer (it takes the overflow ladder instead).
- [ ] `shell state` JSON: every `env_state` field, the relaxing classification, each new `Note` variant and `binary_pin.matches_running`, each shown red first.
- [ ] Execution records: `env_mode` per mode, and `stack` absent, asserted beyond the golden.

**Exploratory hardening session** (owner, after the planned tests pass). Every finding gets a red-first regression test and a fix on the same branch; out-of-scope findings become issues; the report lists every scenario with its result. Scenarios carried from the discussion: exec chains three or more deep mixing modes; exec inside `ocx shell` and the reverse; `ocx shell` inside `ocx shell`; a script calling `ocx exec` from a launcher-started tool; shells from herdr and tmux started through a trampoline; `ocx self update` mid-session; `ocx.toml`/`ocx.lock` edits in a pinned shell; `cd` across projects in pinned and follow-cwd shells; two shells on one project; exported `OCX_PROJECT` with `--project`, `--global`, `-g`; a stale exported `OCX_GLOBAL`; ambient `OCX_*` contradicting the parent's flags; `__OCX_ENV_STATE` unset, corrupted, truncated, of another major, of a higher minor, near the cap, and unset mid-session; a dev build, the acceptance binary, no ocx on `PATH`, `--clean` with launchers; a pin naming a deleted build; previous-release bodies; every hook shell (bash, zsh, fish, nu, elvish, PowerShell), cmd.exe and the Windows `.exe` shim; values with spaces, Unicode, `:`/`;` and empty `PATH` segments; plugin dispatch, `package test --script`, the SDK spawner, credential-helper trampolines. Added: a pinned shell whose project was never consented; `ocx exec --clean -- bash -l`; zellij and VS Code terminals; a rollback of `current` under an open shell.

**Contract**

- [ ] Goldens regenerated: `cli.json` (no `OCX_ENV`, `__OCX_EXEC_SCOPE`, `__OCX_LAUNCH_IDENTITIES`, `--stack`; `__OCX_CREDENTIAL_HELPER` kept until 0.7; `OCX_GLOBAL` in `retired`; new `--isolate`), `execution-record.json`, the `shell state` report schema, sdkgen `env.rs` and `types.rs`.
- [ ] `test_contract_baseline.py` still reports no baseline; no contract-ledger entry is added.
- [ ] SDK conformance fixture passes for the Rust template (C12).

**Docs**

- [ ] The pages listed in Migration describe one model, with no contradiction between "hermetic" and "inherits". `task website:reference:check` passes.

## Open Questions

| OQ | Question | Recommended |
|---|---|---|
| OQ-1 | Resolved 2026-11-07 (plan D-01). Does `dunce::canonicalize` of the Windows installed path (`<home>/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx.exe` through a junction) equal the canonical `current_exe` when the installed ocx runs? | Accepted: ship C6 as written; when canonicalization fails or differs, the pin is the exact running binary, which is today's behaviour. A `cfg(windows)` junction test runs at the plan's integration gate. |
| OQ-2 | Resolved 2026-11-07 (plan D-02). Should generated SDK spawners strip or pass through `__OCX_ENV_STATE`? | Accepted: strip. An SDK child is a fresh root; passing state would let a hook below it act on records it cannot see the origin of. **Superseded 2026-11-07 (D-45)**: the SDK strips nothing, because an explicit `ocx exec` composes what it was asked for and an inherited state gates only the hook. |
| OQ-3 | Resolved 2026-11-07 (plan D-03). Does `external/docker_credential`'s helper spawn inherit the ambient environment and add the `set_helper_env` pairs on top? | Accepted, by evidence: at the pinned commit `9945be50`, `src/helper.rs:90-92` runs `Command::new(path).arg(action).envs(…)` with no `env_clear`, so the helper inherits. Pass the two pairs of the fixed helper state (C5) plus `__OCX_CREDENTIAL_HELPER=1`; the pairs override any ambient `__OCX_ENV_STATE`. |

### Disposition of the discussion's open questions

| Discussion open question | Disposition |
|---|---|
| Does ocx keep writing `OCX_HOME` into children? | Decided: no (C1 SetupWritten, C2 write rule, R4) |
| State policy vs a child's ambient export | Decided: split by field class (C8) |
| Encoding and size guard | Decided: C3 (header, zlib + base64url, major/minor, 8,169 chars, three-rung ladder) |
| `__OCX_ENV_PWD` | Decided: HookLocal, removed at every spawn (C1) |
| Does the trampoline still need `unset OCX_PROJECT`? | Decided: yes, byte-identical (R13) |
| Contract handling for `OCX_GLOBAL`, `--stack`, `--clean --stack` | Decided: `Withdrawn` warn-once row for `OCX_GLOBAL`; `--stack` deleted; no ledger entry; `!` subjects (R6, R15) |
| The execution record's `stack` field | Decided: replaced by `env_mode` (C11) |
| Pinned `ocx shell` and `cd` into another project | Decided: one notice, nothing applied (C7) |
| Plugin dispatch scrubs more than documented | Decided: routed through the projection; doc corrected (C5) |
| Unverified: `ctx.config_path.parent()` for a global exec | Resolved by the claim diff (row 9a): `$OCX_HOME`; moot once `__OCX_EXEC_SCOPE` is deleted |
| Unverified: the three nesting variants (leaf-`env` diff) | Moved to Validation (acceptance nesting and isolate tests, exploratory session) |
| Unverified: Windows stable install path | Carried as OQ-1 |

## Links

- [`system_design_exec_env_model.md`](./system_design_exec_env_model.md)
- [`discover_exec_env_model.md`](./discover_exec_env_model.md)
- [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md), [`adr_shell_env_addenda.md`](./adr_shell_env_addenda.md)
- [`adr_toolchain_activation.md`](./adr_toolchain_activation.md), [`adr_global_toolchain_tier.md`](./adr_global_toolchain_tier.md)
- [`adr_package_entry_points.md`](./adr_package_entry_points.md), [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md)
- [`adr_exec_resolution_record.md`](./adr_exec_resolution_record.md), [`adr_project_env_declaration.md`](./adr_project_env_declaration.md)
- [`adr_live_env_reload.md`](./adr_live_env_reload.md), [`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md)

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-11-06 | `/hex-architect` xhigh | Initial draft, Status Proposed |
| 2026-11-06 | `/hex-architect` xhigh | Review panel fix pass (Review record below); Status stays Proposed |
| 2026-11-07 | `/hex-architect` xhigh | Second fix pass: owner decision O-1 (`inherited_scopes`), cross-model findings, re-validation items (Review record below); Status stays Proposed |
| 2026-11-07 | `/hex-plan` xhigh (goal loop) | Status Accepted under goal-loop authority (Metadata note); OQ-1 to OQ-3 resolved; Validation gains the owner's explicit-exec rule and corrects where the nesting test is red today. Plan: [`plan_exec_env_model.md`](./plan_exec_env_model.md) |
| 2026-11-07 | A2 pipeline SEC | Amendment (owner decisions 2026-11-07, plan D-28, D-29, D-45): the secret classifier, `RecordedValue::Present`, `ReplayPayload::FromInherited` and the report's `withheld` field are removed, so applied values are recorded verbatim and `--isolate` reverts by exact value (S-1, S-2, S-4, S-6, §C1, §C3 steps 10-11, §C10); decode refuses any applied or replay record on a reserved `OCX_*`/`__OCX_*` key with no downgrade step; the SDK strips nothing (R16, inventory, §C12, OQ-2). The Review record below keeps its original wording as history |

## Review record

One line per finding of the single panel pass. The cross-model adversary did not run (codex refused, `unsupported`).

- SP-1 Block spec — fixed in §C0, §C2, §C3 (every wire type field by field; `Verdict`, `ProjectScopeHint`, `ScopeId` moved into `ocx_config`)
- SP-2 Block spec — fixed in §C5 (credential-helper row: fixed minimal state built at init); OQ-3 narrowed to spawn inheritance
- SE-1 Block security — fixed in §C2 (`Pinned` holds names and a digest), §C7 (refresh recomposes consented groups only; Invocation records remove-only), S-5 (`<dir>` exception), A-06 amendment row
- SE-2 Block security — fixed in §C2 (policy write rule, `RootFlags`), §Validation (`--no-verify` unit and acceptance tests)
- SE-3 Block security — fixed in §C1 (classifier), §C2 (`Present`, `Withheld`, `FromInherited`, S-2, S-4), §C4 (Clean priors `Unset`), §C10 (redaction), §NFR Security: secrets
- SO-1 Block SOTA — fixed in §Key insight (mise PR 13950 cited), §C2, §NFR Security: secrets
- QU-1 Block quality — fixed in §C3 (frozen plaintext header honoured when the payload is unreadable), §Validation (mixed-format launcher test)
- SE-6 Block security — fixed in §C3 (narrowing names and home in the header), §C5 (credential-helper marker in the header)
- QU-2 Block quality — fixed in §C7 (never writes over an unreadable state; shell-local stood-down copy; verified 1.x behaviour), R9
- SO-2 Block SOTA — fixed in §Validation (previous release as a middle hop)
- QU-3 Warn/Suggest quality — fixed in §C3 (major/minor table; re-adopts A-04's additive rule), §Amendments
- SO-5 Warn/Suggest SOTA — fixed in §C3 (with QU-3)
- QU-4 Warn/Suggest quality — fixed in §Trade-off matrix (D re-scored on D2; D3 made a gate; sensitivity lines), §Decision Outcome (rests on owner priority and the frozen-surface constraint)
- QU-5 Warn/Suggest quality — fixed in §Rule inventory (before and after; "two rules total" removed)
- QU-6 Warn/Suggest quality — fixed in §C0 (placement table checked against `crate_map.toml`), WP2 (`base64`), §Migration lockstep row
- SP-8 Warn/Suggest spec — fixed in §Migration lockstep row and WP2
- QU-7 Warn/Suggest quality — fixed in §Conflict with product context, WP11; owner confirmation of the wording deferred (owner-owned positioning text)
- QU-8 Warn/Suggest quality — fixed in §C7 (session token: claiming shell's pid plus process start time, the panel's second option, because a nonce is lost on `exec $SHELL`)
- SO-3 Warn/Suggest SOTA — fixed in §C7 (token covers tmux and zellij snapshots), §Validation (tmux inside `ocx shell`)
- SO-4 Warn/Suggest SOTA — fixed in §C7 (IDE-captured environments), §Validation (IDE row)
- QU-9 Warn/Suggest quality — fixed in R6 (correct reason; code cannot express it, so WP1 extends `retired.rs` with a warn-once `Withdrawn` change)
- QU-10 Warn/Suggest quality — fixed in §Option D (per-process id, real costs), §Decision Drivers (D2 reworded)
- QU-11 Warn/Suggest quality — fixed in §C2 (`pre_existing`), §C4, §C7
- QU-12 Warn/Suggest quality — fixed in §C9 (fresh minimal state), header reversibility line
- SE-4 Warn/Suggest security — fixed in §C9 (with QU-12)
- SE-5 Warn/Suggest security — fixed in §C8 (split, env slot, SYSTEM locks); owner confirmation of the trade deferred (parent-flag stickiness is a product choice)
- SO-8 Warn/Suggest SOTA — fixed in §C8 (disagreement warning on stderr)
- SE-7 Warn/Suggest security — fixed in §C3 (`MAX_RECORDED_TIERS = 8`, fail closed), S-5 exception 2
- SE-8 Warn/Suggest security — fixed in §C3 decode table
- SO-7 Warn/Suggest SOTA — fixed in §C3 decode table (with SE-8)
- SE-9 Warn/Suggest security — fixed in §C6 (ownership and mode check on Unix; Windows residual named)
- SP-11 Warn/Suggest spec — fixed in §C6 (memoised in `Context`)
- SE-10 Warn/Suggest security — fixed in §C7 notice, §C10 plain rendering
- SO-6 Warn/Suggest SOTA — fixed in S-7, §Validation (Windows key round trips)
- SO-9 Warn/Suggest SOTA — fixed in §C9, §Validation (GitHub composite-action row)
- SO-10 Warn/Suggest SOTA — fixed in §C7 (notice text with recovery; newer-major wording)
- SP-3 Warn/Suggest spec — fixed in §C2 (`Pinned` field semantics; `Pinned.env` dropped)
- SP-4 Warn/Suggest spec — fixed in §Validation (`env_slot` per field; `--offline` vs `OCX_OFFLINE=0`)
- SP-5 Warn/Suggest spec — fixed in §Validation (C10, C11, C3 arms, R2, cmd.exe, R4 items)
- SP-6 Warn/Suggest spec — fixed in §Implementation Plan (owners for `hook.rs`, `shell_allow.rs`, `shell_revoke.rs`, `shell_state.rs`)
- SP-7 Warn/Suggest spec — fixed in §C1 (SecretInput class; `OCX_AUTH_*` named)
- SP-9 Warn/Suggest spec — fixed in §C3 (sticky truncation), §C4, §Validation (rung-2 then spawn)
- SP-10 Warn/Suggest spec — fixed in §C5 Starlark row, §Validation

### Second fix pass (2026-11-07)

One owner decision, the cross-model findings and the re-validation items.

- O-1 owner decision — fixed in §Owner decision 2026-11-07 (trade-off "exec = snapshot, shell = live"), §C2 (`inherited_scopes: Vec<Scope>`, `Scope { Global, Project { root } }`, why one list), §C3 (`MAX_INHERITED_SCOPES`, `ScopeLimit`, survives rungs 1 and 2), §C4 (`inherited_scopes` row), §C5 (`Spawn.composed_scope`), §C7 (desired-composition table; login stream replaces `yields_to_exec` and the reconcile yield block; no pid test), S-5 (roots compared only, stripped before printing), §C10 (dump field), §Validation (tests (a)-(e)). The rung-3 re-apply residual is stated in §C2. Reading of our own: the global tier also yields inside an inherited project
- X-1 cross-model — fixed in §C3 (cap derived from the emitted line `SET "__OCX_ENV_STATE=<value>"`, 8,191 - 22 = 8,169; value alphabet has no `%`, `"`, LF, CR), §NFR, §Amendments, §Trade-off, §Industry Context, §Validation (whole-line boundary test)
- X-2 cross-model — fixed in S-6 (merge only a continuous chain), §Validation (nesting with a user change in between)
- X-3 cross-model — fixed in §C7 Pinned row (a refresh never writes a key or element a live Invocation record owns), §Validation (overlapping project edit)
- X-4 cross-model — fixed in §C2 intro and §C3 versioning table (names, defaults and meaning stable within a major; a rename bumps the major or keeps a serde alias), §Validation (field-name golden)
- X-5 cross-model — dropped as instructed: the system design delegates to the ADR by its stated convention
- RV-S1 re-validation — fixed in R10, §C3 ladder, §C7 truncated row (one ladder wording: inherited records, then all applied records, then the payload)
- RV-S2 re-validation — fixed in §C3 decode step 5 and rung 3 (a header-only state decodes `Readable` with the default payload and `PayloadDropped`), §Validation (unit test)
- RV-S3 re-validation — fixed in §C2 `minor` comment and §C3 versioning table (any minor within a major; newer fields ignored and safe to lose)
- RV-X1 re-validation — fixed in §C7 (unclaimed token under FollowCwd retags Session records first; a token without `started` is foreign where this shell's start time is readable). **Deviation**: decode does not refuse `Present` on a non-secret key; it keeps the record and never removes it (S-4, §C3 step 11), because RV-X7 requires classifier changes not to make a payload unreadable, and a refusal would do exactly that
- RV-X2 re-validation — fixed in §C9 (exporters copy the inherited narrowing names; `--shell` and `direnv export` emit no state when the inherited state is readable; launch-identity residual), §Validation
- RV-X3 re-validation — fixed in §C3 grammar (`MAX_HEADER` frozen) and decode step 2 (header parsed before the length and UTF-8 checks; every `n=` honoured), §Validation
- RV-X4 re-validation — fixed in §C2 `Policy` comments and §C8 (`no_config_refresh`, `no_update_check` are relaxing; `env.rs:268-280`), §Validation
- RV-X5 re-validation — fixed in §C2 ambient row and §C8 (`to_write(Clean)` is the `env_slot` result), §Validation (`OCX_NO_VERIFY=0` survives `--clean`)
- RV-X6 re-validation — fixed in §C7 (the gate copies by reference, per shell), §NFR Security, §Risks, §Validation (`'`, `$(...)`, newline)
- RV-X7 re-validation — fixed in S-2 and §C3 steps 10-11 (secret literals downgraded; only applied records on Credential and `OCX_AUTH_*` keys rejected)
- Suggest: received `h` — fixed in §C7 (the hook ignores it)
- Suggest: A-06 list — fixed in §Amendments (adds `replay.launch_identities`, `Pinned.pinned_paths`; lists compared-only fields)
- Suggest: classifier — fixed in §C1 (`PASS`, `AUTH`, `APIKEY`, `PRIVATE`, `DSN`, `COOKIE`, URL userinfo; `JWT`, `WEBHOOK`, `_PAT`, `_PWD` added later, see §C1)
- Suggest: strip set — fixed in §C7 (C0, DEL, C1 including U+009B, bidi controls), §C10
- Suggest: credential-helper deadlock — owner's call, taken as "keep": §Other decisions and §C5 write `__OCX_CREDENTIAL_HELPER=1` into the exempt helper spawn until 0.7; §C1 HookLocal row, §Consequences
