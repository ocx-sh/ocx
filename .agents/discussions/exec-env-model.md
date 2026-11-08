# Discussion: Simplify the execution-environment model and ocx's transported variables

State: handed-off → architect · Updated: 2026-11-06
Participants: owner + hex-discuss; one session, 2026-11-06
Ratified: 2026-11-06 → architect
Confidence: ratified by the owner (explicit yes, 2026-11-06). Six research lanes dated 2026-11-06. Binary-pin and render-trigger facts checked in code and simulation on 2026-11-06. Three items are still unverified on a built binary (Open questions).

## Intent

The problem, in the owner's words:
- Leaked `OCX_GLOBAL` (grimoire#297, ocx#678) is a design bug tied to nested environments.
- It is unclear how bare `ocx shell` behaves under it.
- The variables ocx transports between its own processes only need to be compatible within one ocx version.
- Their number should drop drastically, preferably to one.
- The execution-environment model must get simpler and more predictable.
- Nesting ocx toolchains has already tripped agents working in this repo. Nesting should simply work: `ocx --project a exec -- ocx --project b exec -- echo "double env"`, normally built by scripts and command builders.
- `--clean` already exists as the explicit "do not inherit" switch.

Added later in the session: simpler means fewer *rules*, not only fewer variables. Each variable should carry one rule.

Why now:
- `--stack` (3f85e2e0, from #668), bare `ocx shell` (8d1bc0f1) and `__OCX_EXEC_SCOPE` (4b8b94d2) landed after v0.6.5 and are not yet a released contract.
- The next release also ships the `OCX_GLOBAL` leak fix (451c54a8, closes #649).
- grimoire#297 and ocx#678 are open.

Outcome: one ADR that replaces the execution-environment and transport model. It is produced by `/hex-architect`, tier floor `high`. It amends or supersedes:
- `.claude/artifacts/adr_shell_env_overhaul.md` (Decision 1; Amendment 2026-11-03);
- `.claude/artifacts/adr_toolchain_activation.md` (D-9 trampoline);
- `.claude/artifacts/adr_global_toolchain_tier.md` (`OCX_GLOBAL`).

Out of scope:
- Declarative project composition (#672, separate ADR). This ADR only has to leave layering expressible for it.
- The named-shell feature itself (#363).
- Package metadata env modifiers: `path`/`list`/`constant` semantics stay as they are.
- Any change to what generated launchers on disk read (requirement 9).
- When generated launchers, shims and trampolines are refreshed: deferred by the owner on 2026-11-06 to ocx-sh/ocx#679 (priority/high, before 1.0).
  - Evidence: trampolines are rendered only by `ocx pull`, `add`, `remove`, `lock`, `update` and `upgrade`. `ocx self update` and `self setup` never render (0 render calls in `crates/ocx_setup/src` or `crates/ocx_cli/src/command/self_group`). Package launchers and deferred and Windows shims are written once.
  - This ADR is correct without any refresh, because of requirement 9.
- An implementation plan.

## Requirements

Provisional, not yet IDs.

1. **Public `OCX_*` variables are user input only.** ocx reads them, and never writes one into a child process or into emitted shell code. The one exception is `OCX_BINARY_PIN` (requirement 2b).
2. **Exactly two variables cross from ocx to its descendants.** Each has one rule, set by its reader. One projection function, used by every spawn path, enforces them. The spawn paths are:
   - `ocx exec`, `ocx shell`, `ocx package exec`;
   - launcher re-entry and plugin dispatch;
   - records and update-check subprocesses;
   - `package test --script`;
   - the generated SDK spawner.
   One dump command shows both variables. One table-driven test pins every registry variable's behaviour.
   - **a. `__OCX_ENV_STATE`** (the existing name, widened).
     - Rule: only ocx reads it; its format may change in any release.
     - It carries a format version. A reader accepts only its own version and fails safe otherwise (requirement 8). The version bumps only on a format break. The ledger's current `v-1` revert-read arm (`crates/ocx_shell/src/shell/reconcile/ledger.rs`) is dropped.
     - It crosses every spawn. It holds three things:
       - **Policy** the parent took from its own flags: what `apply_ocx_config` forwards today (offline, frozen, remote, config, index, mirrors, patches, records, no-consent, toolchain dir, `OCX_ENV` overrides, launch identities, the credential-helper marker).
       - **Applied values.** Every value ocx applied relative to the pre-ocx environment: added path and list values, and constants with their prior value. Each value carries an owner tag: *session* (this shell's hook) or *inherited*.
       - **Session selection** (requirement 6).
     - Every spawn retags session values as inherited and adds its own composition as inherited. A hook touches only session values. `--isolate` reverts all of them.
     - It replaces about 26 forwarded public variables, plus `__OCX_EXEC_SCOPE`, `__OCX_LAUNCH_IDENTITIES`, `__OCX_CREDENTIAL_HELPER` and the `--stack` carriers.
   - **b. `OCX_BINARY_PIN`**, unchanged in name and in value shape (an absolute path).
     - Rule: frozen, because generated files on disk read it before any ocx runs (requirement 9).
     - Readers:
       - package entry-point launchers and deferred-tool shims (`${OCX_BINARY_PIN:-ocx}`);
       - toolchain trampolines (`${OCX_BINARY_PIN:-<baked>}`);
       - the Windows `.exe` shim;
       - the generated SDK runtime (`crates/ocx_sdkgen/templates/rust/runtime.rs:532`);
       - the Starlark host (`crates/ocx_script/src/ocx_module.rs`).
     - ocx itself never reads it.
     - **Value.** When the running ocx is the installed one, ocx writes the stable install path, the one trampolines already bake (`trampoline_ocx_binary`, `crates/ocx_package_manager/src/launcher/generate.rs:85`, which resolves through the `current` symlink). Otherwise ocx writes the exact running binary (a dev build, tests, Bazel, a CI tool cache).
     - Today ocx writes `current_exe()` (`crates/ocx_cli/src/app/context.rs:421`). That resolves symlinks to one build's store path, which goes stale after an upgrade.
3. **Selectors.**
   - `--global` is flag-only; the `OCX_GLOBAL` environment form is removed.
   - `OCX_PROJECT` stays a user input. Every command honours it the same way: the prompt hook, `ocx shell allow`/`revoke`/`state`, and exec. This resolves #678.
   - ocx never writes either selector.
4. **Exec modes.**
   - *Layer* (default): keep the inherited environment and put the new composition in front. Nesting gives "double env".
   - `--isolate`: revert every value `__OCX_ENV_STATE` records, then compose. It works at any nesting depth. OS and user environment stays.
   - `--clean`: hermetic.
   - `--stack` and `--clean --stack` are removed before release.
5. **Application stays idempotent**, so re-applying a composition never duplicates. This holds today and must stay pinned by tests:
   - path entries are move-to-front (`ocx_util::path::move_to_front`); shell emitters match it byte for byte (`crates/ocx_shell/src/shell.rs:123`);
   - list entries remove earlier occurrences before appending (`crates/ocx_package/src/metadata/env/modifier.rs`, `List`);
   - constants replace.
   - Known exception: the cmd.exe arm's one-time non-dedup (`crates/ocx_shell/src/shell.rs:250`).
6. **Session selection lives in `__OCX_ENV_STATE`.**
   - Follow-cwd is the default. A pinned selection names a project and/or groups, and later a named shell.
   - `ocx shell` starts a subshell that is already pinned. Switching the current session in place belongs to #363.
   - A pinned hook never follows `cd` and never mixes another environment in.
   - A pinned hook does refresh its own layer when `ocx.toml` or `ocx.lock` change: it reverts its own layer and re-applies the same selection.
   - `__OCX_EXEC_SCOPE` is removed.
7. **Long-lived descendants start clean sessions and follow upgrades.**
   - A tool started through ocx (herdr, tmux, an IDE) passes on only inherited values. Interactive shells it spawns own nothing, so their hook starts a fresh follow-cwd session.
   - Its launchers follow `ocx self update` through the stable pin path (requirement 2b).
   - `ocx exec -g x -- bash` is spelled `ocx shell -g x`.
8. **Format mismatch is fail-safe per reader.**
   - The prompt hook stands down on a state it cannot read: it never reverts blindly and never mixes. It prints one notice line telling the user to reopen the shell.
   - A nested exec ignores an unreadable state with one warning naming both versions.
   - `--isolate` warns when it cannot fully isolate.
9. **Nothing that generated files on disk read may change.**
   - Package entry-point launchers and deferred-tool shims are written once and never rewritten:
     - launchers at install (`crates/ocx_package_manager/src/tasks/pull.rs:458`);
     - shims by `prepare_lazy`, which returns an existing tree as-is.
   - Trampolines are rewritten only on a render: `ocx pull`/`add`/`remove`/`lock`/`update` (`crates/ocx_package_manager/src/mutate.rs`, `commit_and_render`). The prompt hook only heals links.
   - `.claude/artifacts/adr_package_entry_points.md:16` already records this as a one-way door. Frozen:
     - the name `OCX_BINARY_PIN`;
     - `launcher exec '<root>' --` and `launcher shim '<id>' --`;
     - `--project '<root>' exec --` and `--global exec --`;
     - every argv shape older launchers emit, per that ADR's stable-surfaces list.
   - Old trampolines' `unset OCX_GLOBAL OCX_PROJECT` line stays harmless.

## Decisions

All taken with the owner on 2026-11-06:
- **Nesting default: layer.** b goes in front of a; `--clean` is hermetic.
- **Selectors.** Drop the `OCX_GLOBAL` environment form; keep `OCX_PROJECT`, honoured everywhere the same way.
- **Long-lived descendants mark nothing** (requirement 7).
  - The owner was concerned that re-applying the global environment in such panes would duplicate list values.
  - Checked, no duplicates: on a herdr pane (0.6.5, where the login stream re-applied the global environment), PATH has 26 entries and none of ocx's are duplicated.
  - Residue: a launch-time constant can go stale. `--isolate` restores the pre-ocx value from the recorded prior value.
- **Only `ocx shell` enters an environment.** An entered shell is pinned, not idle (requirement 6). This reverses 833e8cb5's "leaving the project reconciles".
- **Exec modes: layer by default, `--isolate`, `--clean`** (requirement 4).
  - Competitors: activation-diff tools (mise since PR 9765, direnv, conda `--stack`, virtualenv, uv) replace their own previous layer. Environment builders (nix, devbox, flox) layer, with `--pure` opt-in. None isolates from the OS by default.
  - Observed on 0.6.5: a project `ocx exec` keeps `~/.ocx/toolchain/active/bin` on PATH, so global tools resolve through trampolines. Today's default isolation is only partial.
- **Transport: two variables, one rule each** (requirement 2).
  - The research council proposed three classes: an exec carrier, the hook ledger and the pin. That cut about 31 variables to 3. The owner objected that it added three rule sets.
  - **Ledger and carrier merged into `__OCX_ENV_STATE`.** Their two differences were not essential:
    - Cross-version reading yields to the owner's same-version rule plus fail-safe.
    - "Never crosses exec" becomes the owner tag on each value.
  - Accepted cost: a format break makes already-open hook shells stand down until reopened.
- **`OCX_BINARY_PIN`: keep the name, fix the value** (requirement 2b).
  - It is needed whenever a child cannot find its parent's ocx by itself:
    - `--clean`: there is no ocx on PATH. A simulated launcher exits 127.
    - The running ocx differs from the one on PATH: a dev build, acceptance tests (the runner passes the developer's PATH, `test/src/runner.py:86`), or a CI-pinned version. This is the incident behind the pin (`.claude/artifacts/adr_package_entry_points.md:994`).
    - There is no ocx on PATH at all: Bazel, a CI tool cache, a script or the generated SDK calling ocx by absolute path.
  - It does harm only through its value. A long-lived descendant keeps one build's store path after an upgrade, and the pin outranks a working ocx on PATH. Observed in this session's shell: `OCX_BINARY_PIN=…/packages/ocx.sh/sha256/85/3a43…/content/bin/ocx`. A simulated launcher exits 127 once the named build is gone.
  - Folding it into `__OCX_ENV_STATE` was rejected. Old launchers would still need `OCX_BINARY_PIN`, and a new frozen field would appear: two channels instead of one. Changing only the value needs no migration.
- **`--isolate` mechanics:** one record of applied values, not a stack. Its size is bounded by the number of distinct applied values, not by nesting depth.
- **Named shells:** noted; built in #363. The store already has `shells/<name>` and `active -> shells/default` (`crates/ocx_store/src/file_structure/toolchain_store.rs:27-30`).
- **Format mismatch:** fail-safe per reader (requirement 8).

## Threads

All closed and folded into the Decisions above:
- nesting default;
- selectors;
- long-lived descendants;
- transport shape (two variables);
- marker owner;
- isolation modes and `--isolate` mechanics;
- entered-shell hook;
- named shells;
- format mismatch;
- the binary pin;
- refreshing generated files (deferred to ocx-sh/ocx#679).

## Research

- `.agents/research/research_ocx-exec-env-transport-inventory.md` — codebase recon: every transported variable, its writers and readers, declaration mismatches, nesting traces, written contracts, cross-version promises.
- `.agents/research/research_nested-env-state-prior-art.md` — prior art: direnv, mise, conda, pixi, nix, devbox, flox, rustup, pyenv/asdf, pipenv/poetry, uv, Bazel.
- `.agents/research/research_ocx-env-transport-history.md` — repo archaeology: why each variable and scrub exists, the context of #668, #672 and #649, and agent friction in both repos.
- `.agents/research/research_env-leak-community-reports.md` — community leak reports: git, cargo, rustup, npm, Bundler, uv, direnv/tmux.
- `.agents/research/research_inherited-state-adjacent-fields.md` — adjacent fields: make/jobserver, shells, git local env, Bazel, Nix, systemd, Docker.
- `.agents/research/research_env-transport-shape-council.md` — council of premortem, operability and simplicity seats on the transport shape. It recommended three classes, which the owner later reduced to two (Decisions).

These live in the gitignored research home. Per `.agents/memory/hex.md` › Pointers, copy any the ADR cites into `.claude/artifacts/research_<topic>.md`.

## Related

- grimoire-rs/grimoire#297 — `ocx exec` composes the global toolchain when `OCX_GLOBAL` leaked in. Resolved by requirement 3 plus a release; grimoire's P-9 `env -u OCX_GLOBAL` workaround becomes unnecessary.
- ocx-sh/ocx#678 — `ocx shell allow` refuses because the hook ignores the selector while `allow` honours it. Resolved by requirement 3.
- ocx-sh/ocx#649 (closed by 451c54a8) and ed5f17f8 — the selector clears in `apply_ocx_config`.
- ocx-sh/ocx#668 — origin of `--stack`, bare `ocx shell` and `__OCX_EXEC_SCOPE`.
- ocx-sh/ocx#672 — declarative composition (separate ADR).
- ocx-sh/ocx#363 — multi-shell and per-prompt selection.
- ocx-sh/ocx#679 — generated launchers, shims and trampolines are never refreshed after an upgrade (follow-up, priority/high).
- ADRs in `.claude/artifacts/`:
  - `adr_shell_env_overhaul.md`, `adr_toolchain_activation.md`, `adr_global_toolchain_tier.md`;
  - `adr_package_entry_points.md` (launcher one-way door, pin amendment);
  - `adr_windows_exe_shim.md`;
  - `adr_live_env_reload.md` (OD-4, the dropped `__OCX_ENV` rename);
  - `adr_exec_resolution_record.md` (F10, the records `stack` field).
- Code the ADR touches:
  - `crates/ocx_env/src/vars.rs` — the two variables; `OCX_GLOBAL` env removal; removal of `__OCX_EXEC_SCOPE`, `__OCX_LAUNCH_IDENTITIES`, `__OCX_CREDENTIAL_HELPER`.
  - `crates/ocx_config/src/env.rs` — `apply_ocx_config` becomes the projection.
  - `crates/ocx_package/src/metadata/env/apply.rs` — `apply_child_env`.
  - `crates/ocx_shell/src/shell/reconcile.rs` — `inherited_env`/`stacked_env`/`stacked_clean_env` become the modes.
  - `crates/ocx_shell/src/shell/reconcile/ledger.rs` — owner tags, policy, selection, format version.
  - `crates/ocx_cli/src/command/` — `toolchain_exec.rs`, `shell.rs`, `shell_allow.rs`, `shell_revoke.rs`, `shell_state.rs`, `self_group/activate.rs`.
  - `crates/ocx_cli/src/app/` — `context.rs` (pin value), `context_options.rs`, `env_flags.rs`, `plugin_dispatch.rs`.
  - `crates/ocx_package_manager/src/launcher/generate.rs` — `trampoline_ocx_binary`, reused for the pin value.
  - `crates/ocx_package_manager/src/activation.rs`.
  - `crates/ocx_script/src/ocx_module.rs`.
  - `crates/ocx_config/src/records.rs`.
  - the `ocx_sdkgen` spawn template.
  - `crates/ocx_schema/contract/ledger.toml`.
- Docs to reconcile into one model:
  - `website/src/content/docs/reference/environment.md`, `reference/env-composition.md`;
  - `explanation/environments.mdx`, `explanation/shell-integration.mdx`, `explanation/toolchain-activation.md`, `explanation/launcher-forwarding.md`;
  - the command-line reference for `exec` and `shell`.

## Open questions

- [NEEDS CLARIFICATION: Does ocx keep writing `OCX_HOME` into children?]
  Recommended: no. `OCX_HOME` becomes user input only, and the resolved home travels in `__OCX_ENV_STATE` policy, so a `--clean` child still resolves the same root (#488, e4ce1186). The Windows shim's containment check already treats `OCX_HOME` as optional, with `launcher exec` authoritative (`crates/ocx_shim/src/main.rs:214-217`). Verify that no generated file reads it before committing.
- [NEEDS CLARIFICATION: In a child ocx, which wins: state policy, or the child's own ambient `OCX_*` export (parent `--offline` vs. an ambient `OCX_OFFLINE=0`)?]
  Recommended: `__OCX_ENV_STATE` holds only the policy the parent took from its own flags. A `--clean` child also gets the ambient values its clean environment would otherwise drop.
  - Precedence: child flags, then state, then ambient.
  - Ambient-derived values otherwise reach the child as plain inherited env. Without that limit, a long-lived descendant (herdr) would keep launch-time policy that overrides an `OCX_*` the user exports in a pane later.
  - Matches `explanation/launcher-forwarding.md` ("parsed flags are authoritative").
- [NEEDS CLARIFICATION: Encoding and size guard for `__OCX_ENV_STATE`.]
  Recommended: compressed, then base64.
  - Guard: well below the 32,767-character Windows environment block and the 128 KiB Linux per-string limit.
  - On overflow, drop the applied-value record with a warning; `--isolate` then reports it cannot fully isolate.
- [NEEDS CLARIFICATION: `__OCX_ENV_PWD` (the elvish hook's pid, `$pwd`, yield sentinels and consent-grant variables).]
  Recommended: treat it as shell-local hook state, not ocx-to-ocx state, and strip it at every spawn. Fold it into `__OCX_ENV_STATE` only if the elvish hook can read it through ocx.
- [NEEDS CLARIFICATION: Does the trampoline still need `unset OCX_PROJECT` once `OCX_GLOBAL` is gone?]
  Recommended: newly rendered trampolines keep it until a test proves the baked `--project` beats an exported `OCX_PROJECT`, then drop it. Old trampolines keep the line either way.
- [NEEDS CLARIFICATION: Contract handling for removing the `OCX_GLOBAL` env form, `--stack` and `--clean --stack`.]
  Recommended: `OCX_GLOBAL` gets a `RETIRED` row, a contract-ledger entry and a `!` commit subject. `--stack` is unreleased, so check whether the `cli.json` baseline already contains it before adding a ledger entry.
- [NEEDS CLARIFICATION: The execution record's `stack` field (833e8cb5).]
  Recommended: record the exec mode (`layer` | `isolate` | `clean`) instead.
- [NEEDS CLARIFICATION: In a pinned `ocx shell`, what happens when the user `cd`s into a different project?]
  Recommended: one notice line naming the pinned selection; nothing applied.
- [NEEDS CLARIFICATION: Plugin dispatch scrubs more than its doc promises: it strips ambient `OCX_TOOLCHAIN_DIR`, `OCX_PATCHES` and `OCX_RECORDS_*`.]
  Recommended: route it through the same projection and fix the doc to match.
- [NEEDS CLARIFICATION: Unverified items.]
  Recommended: verify these on a built main binary before the ADR rests on them:
  - `ctx.config_path.parent()` for a global exec;
  - the three nesting variants (a leaf-`env` diff);
  - that the stable install path resolves on Windows (junction, `ocx.exe`).

## Verification

How the eventual implementation is checked:
- **Unit.**
  - A table-driven test over `crates/ocx_env/src/vars.rs`. Every variable is user input, `__OCX_ENV_STATE` or `OCX_BINARY_PIN`, and the projection's child behaviour matches. It goes red on a misdeclaration.
  - `__OCX_ENV_STATE`: round trip; a spawn retags session values as inherited; a hook touches only session values; format mismatch.
  - Pin value: an installed ocx writes the stable install path; a non-installed binary writes its exact path.
- **Acceptance (`test/tests/`).** Each test is shown red against today's code before the fix.
  - Nesting `a → b` (default): b's tools in front, a's tools reachable.
  - `--isolate` at depth 1 under a hook-activated shell, and at depth 2 in an exec chain: pristine environment plus c only.
  - `--clean` is hermetic, and launchers inside it still find ocx through the pin.
  - A long-lived descendant: a shell it spawns owns no session values and activates fresh (follow-cwd).
  - Upgrade: after `current` moves to a new build, a launcher in a pre-existing environment runs the new build.
  - On-disk compatibility: a launcher, shim and trampoline body as the previous release generated it, placed on disk, still works under the new ocx.
  - Pinned `ocx shell`: `cd` into another project applies nothing; an `ocx.toml` edit refreshes the pinned layer.
  - #678 regression: an exported `OCX_PROJECT` is honoured identically by the hook and `ocx shell allow`.
  - grimoire#297 regression: an exported `OCX_GLOBAL` no longer selects the global toolchain.
  - Idempotency: applying the global composition twice yields no duplicate PATH or list entries.
  - Format mismatch: an undecodable state gives exactly one warning and the exec proceeds; the hook stands down with one notice line.
- **Exploratory hardening session** (owner, 2026-11-06). After the planned tests pass, run a deliberate session that drives the built binary through normal usage, unusual usage and edge cases, and fix what it finds on the fly.
  - Every finding gets a regression test that is shown red first, then the fix, on the same branch.
  - A finding outside this ADR's scope becomes an issue, not a silent skip.
  - The session report lists every scenario run, with its result, so a scenario that was skipped is visible.
  - Starting scenarios:
    - **Nesting:**
      - exec chains three or more deep, mixing layer, `--isolate` and `--clean`;
      - exec inside `ocx shell`, and `ocx shell` inside exec;
      - `ocx shell` inside `ocx shell`;
      - a script that calls `ocx exec` from a tool a launcher started.
    - **Long-lived and session:**
      - shells spawned by a tool launched through a trampoline (herdr, tmux);
      - `ocx self update` mid-session;
      - an `ocx.toml` or `ocx.lock` edit in a pinned shell;
      - `cd` across projects in pinned and follow-cwd shells;
      - two shells at once on one project.
    - **Selectors and inputs:**
      - an exported `OCX_PROJECT` together with `--project`, `--global` and `-g`;
      - a stale `OCX_GLOBAL` still exported;
      - ambient `OCX_*` exports that contradict the parent's flags.
    - **State abuse:**
      - `__OCX_ENV_STATE` unset, corrupted, truncated, written by another format version, or near the size guard;
      - a user who deliberately `unset`s it mid-session.
    - **Binary pin:**
      - a dev build, the acceptance-test binary, no ocx on `PATH` (Bazel-like), and `--clean` with entry-point launchers;
      - a pin that names a deleted build;
      - launcher, shim and trampoline bodies as the previous release generated them.
    - **Shells and platforms:**
      - every hook shell: bash, zsh, fish, nu, elvish, PowerShell;
      - cmd.exe and the Windows `.exe` shim;
      - paths and values with spaces, Unicode, `:`/`;` and empty `PATH` segments.
    - **Other spawn paths:**
      - plugin dispatch, `package test --script`, the generated SDK spawner, credential-helper trampolines, records subprocesses.
- **Contract.** The compat gate passes with the ledger entries named in Open questions.
- **Docs.** The six pages listed under Related describe one model, with no contradiction between "hermetic" and "inherits".
