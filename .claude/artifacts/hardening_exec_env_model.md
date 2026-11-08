# Hardening report: execution-environment state model

Exploratory hardening session for [`adr_exec_env_model.md`](./adr_exec_env_model.md) § Validation, run as W4–W5 of [`plan_exec_env_model.md`](./plan_exec_env_model.md) § Hardening phase.

- **Testers:** five, in parallel, one scenario family each. Each scenario is a pytest case in `test/tests/test_hardening_*.py`, and every passing scenario stays as a regression test.
- **Base:** e4b86d9f, after the I1 gate closed.
- **Host:** Linux, with bash, zsh and tmux. fish, nu, elvish and pwsh run only in the Docker shell zoo (`task test:shells`). There is no Windows host.
- **Security findings** are recorded here by location and class only.

## Summary

| Family | Module | Tests | Result | Bugs |
|---|---|---|---|---|
| H1 nesting and modes | `test_hardening_nesting.py` | 18 | 18 pass | none |
| H2 shells and terminals | `test_hardening_shells.py` | 29 | 27 pass, 2 skip (fish, pwsh absent on host) | 1, fixed in-surface |
| H3 hostile and garbled input | `test_hardening_input.py` | 94 | 92 pass, 2 xfail → HFIX step 1 | 1 (security, availability) |
| H4 limits, platforms and paths | `test_hardening_limits.py` | 18 | 18 pass | none |
| H5 secrets, exporters and integrations | `test_hardening_integrations.py` | 56 | 51 pass, 5 xfail → HFIX step 2 | 1 (security, secret exposure) |

After HFIX (W5, merged ae73bcdd), all 215 hardening acceptance tests across the five modules pass, with 0 xfails left. The Rust unit tests the fixes added pass too. The fish and pwsh skips are on this host only.

## Bugs

| # | Found by | Class | Location | Red test | Fix |
|---|---|---|---|---|---|
| B-1 | H2 | correctness | `crates/ocx_shell/src/shell/hook.rs` per-prompt gate; `reconcile/fingerprint.rs` | `test_hardening_shells.py::test_deleting_the_project_files_reverts_its_values_at_the_next_prompt` (bash, zsh × 3), red with `'q' == '__OCX_ABSENT__'`; zoo test `test_shell_reconcile.py::test_deleting_the_project_files_mid_session_reverts_at_the_next_prompt_in_every_hook_shell` (bash, zsh, fish, pwsh × 3, run 3); unit tests `a_present_member_gates_on_its_own_disappearance_and_an_absent_one_does_not`, `a_member_appearing_or_vanishing_changes_the_digest` | e8cfa266 `fix(shell)`: deleting `ocx.toml` or `ocx.lock` mid-session reverted the project's values only at the next `cd`. The gate's `-nt stamp` test is false for a vanished file. A present member now also gates on staying present (bash, zsh, fish, pwsh), and the fingerprint folds presence |
| B-2 | H3 | security: availability | `crates/ocx_config/src/exec_state/decode.rs` applied-record check; `revert.rs` | `test_a_forged_session_record_cannot_make_the_hook_drop_a_credential_variable`, `test_a_forged_record_cannot_make_an_isolate_child_lose_a_forwarded_credential` | a0be7bc5 `fix(exec)`. No writer records a credential slot:
- both record builders skip credential slots (`activation.rs:1388`, `exec_env.rs:321`);
- `--env` refuses reserved keys (`options/env_override.rs:134`);
- project `[env]` refuses them (`ocx_project/src/env.rs:110`);
- package metadata refuses them at publish and drops them at resolve.

So decode now refuses such a state whole, naming `applied.key`. Unit test `an_applied_record_on_a_credential_slot_is_invalid`, red first |
| B-3 | H5 | security: secret exposure | `crates/ocx_env/src/lib.rs` `is_secret_name` | `test_less_common_secret_names_stay_out_of_the_state` (5 params) | f1cde8e3 `fix(env)`: the classifier gains the fragments `JWT` and `WEBHOOK` and the suffixes `_PAT` and `_PWD`, under ADR §C1's rule that the heuristic may change within a major; the ADR sentence is updated. The C-002 rows are red first, and `PWD`, `OLDPWD`, `PATH`, `PATHEXT`, `XDG_*`, `PATENT` and `COMPAT` stay ordinary |

## Scenarios

### H1 nesting and modes (all pass)

- A three-deep chain across two projects with mixed groups layers all three scopes.
- A middle `--isolate` drops the outer group and keeps the inner ones.
- A middle `--clean`.
- An innermost `--clean` after `--isolate` leaves only its own scope.
- 20 nested scopes reach the innermost child, and the state stays within 8,169 characters and decodes. The same project and group 20 times gives one value.
- `ocx exec` inside `ocx shell`, and the reverse.
- A launcher-started tool calls `ocx exec`, both layered and under `--clean`.
- `--clean` plus a launcher inside `ocx shell` still composes the launcher's group.
- The same key in two layers: the innermost value wins, and a middle `--isolate` drops the outer value.
- The innermost exit code (42) passes unchanged through a three-deep mixed chain. A self-SIGTERM ends the chain with returncode −15, because ocx replaces its own process.
- After a `cd` into another project between layers, the inner scope follows the cwd and the outer scope stays.
- An older ocx in the middle of a chain (host release via `package exec`). A project-tier hop on the old binary is refused with "unsupported ocx.lock version 4", which is expected.
- A dev build under a host release ocx.
- **Skip condition:** the older-ocx cases skip when no release binary is on the host.

### H2 shells and terminals

Pass, on bash and zsh unless noted:
- A `cd` chain before one prompt; a `cd` in a subshell moves nothing.
- The cwd is removed under the shell.
- An `ocx.toml` edit plus relock, and a project created under the cwd mid-session.
- A garbled `ocx.toml` is never applied half-way, and the shell recovers once it is fixed.
- `ocx shell` inside `ocx shell`: the inner pins another project, and exit restores the outer.
- The same project pinned twice does not stack `PATH`.
- An unconsented project pinned by `ocx shell` contributes no values.
- `ocx exec --clean -- bash -l -i` (bash).
- A rollback of `current` under an open shell (bash).
- A hook without a session token applies and reverts like a current one, and a nested shell without a token stays coherent (bash; the residual named in D-27, now observed).
- VS Code terminal variables (simulated) do not disturb the hook.
- Two tmux panes on one project (bash): an edit reaches each pane at its own next prompt.

Bug, fixed (B-1): deleting the project files mid-session.

Not runnable:
- fish, nu, elvish and pwsh are absent on the host. The `cd`-chain cases for fish and pwsh are written and skipped here. The shell zoo carries the deletion case for all four hook shells in `test_shell_reconcile.py::test_deleting_the_project_files_mid_session_reverts_at_the_next_prompt_in_every_hook_shell`.
- zellij is absent.
- herdr would drive the running session.
- Real VS Code is not available; its terminal variables were simulated.
- No Windows host.

Observed and not filed: a pane started from a hook shell inside project P does not refresh P on edits. That is the inherited-scope rule, not a bug.

### H3 hostile and garbled input

Pass:
- **Garbled carriers:** 11 forms, each one unreadable note, stood down, exit 0. They are a truncated half, a truncated last character, no dot, bad base64, a newline, a non-numeric major, a negative major, a major past u32, a non-ASCII header, an empty major, and a header past the cap. An empty-string carrier reads as absent.
- **Decoder limits:** a duplicate JSON field and JSON nested 60k deep are refused as `json`. A 300 KiB inflate bomb is refused as `inflate_limit`. Over 4,096 records, 17 scopes and 9 tiers are refused.
- **Header narrowing:** an other-major or garbled header keeps offline against `OCX_OFFLINE=0` and `OCX_REMOTE=1` (exit 81). A higher minor with unknown fields stays readable.
- **Forged fields:** 35 forged fields are each refused whole, with the field named, and a refused forgery still honours the header narrowing.
- **Secret handling:** `FromInherited` on a non-secret key is counted only. A secret-named literal or URL userinfo is presence-only. No secret appears in plain or verbose output.
- **Isolate and revert:** isolate keeps a variable against a forged `Present` record, a differing literal or `prior=withheld`. Forged `PATH` records lose no segment. Isolate under an other-major state warns and runs. The hook keeps an ordinary variable named by a forged `Present` record.
- **Pinned directory:** an unconsented pinned directory with a malformed `ocx.toml` passes. That assertion is a smoke check, the one weak check here. A missing pinned directory leaves the hook at exit 0.
- **Policy precedence:** ambient beats received, with one warning, for `insecure_registries`, `no_verify`, `default_registry`, `mirrors` and `patches`. A received `offline` survives `OCX_OFFLINE=0`, `false`, empty and `OCX_REMOTE=1`. The relaxing classification lists all six hostile fields.
- **Environment flags:** a stale `OCX_GLOBAL` gives one `--global` notice and exit 0. `OCX_PROJECT` precedence holds against `--project`, `-g`, `--global`, the walk and `OCX_NO_PROJECT`.

Bug (B-2).

Not runnable:
- The SYSTEM-scope clamp: `/etc/ocx/config.toml` is absent and not writable. It is covered by `insecure.rs` unit tests only.
- The managed-tier clamp: covered by the received-policy module.

### H4 limits, platforms and paths (all pass)

- **Rung 3:** reaches the child as the header-only marker, and the values still apply. A nested exec below a rung-3 parent stays consistent, and `--offline` survives into a third spawn.
- **Rung 2:** across three layered near-cap projects, every frame fits.
- **Symlinked project roots:** each root is composed once and counted once.
- **Unicode and space roots:** they round-trip and survive a nested exec.
- **`:` and `;` values:** they arrive verbatim, and a `;`-separated list item is kept.
- **Empty `PATH` segments:** dropped on composition (see Residuals). A fully empty `PATH` gives the project directory first.
- **Launchers:**
  - With `OCX_BINARY_PIN` set and no ocx on `PATH`, a launcher runs.
  - With neither, it fails fast and names ocx.
  - With a pin to a deleted build, it fails fast and names the pin.
- **Pins:**
  - An ambient pin to a deleted build is replaced by the running binary.
  - Invoked by absolute path, ocx gives an absolute pin.
  - Under `exec --clean`, the pin alone reaches the next ocx.

Not runnable: the cmd.exe cap, the Windows `.exe` shim and Windows key case (no Windows host; the key folding is `cfg!(windows)`). The cap value itself is covered by `test_exec_env_compat.py`.

### H5 secrets, exporters and integrations

Pass:
- **Secret values stay out of the state:**
  - secret-named `--env KEY=value` and `--env NAME`;
  - URL userinfo in three forms;
  - a userinfo project constant.
- **Secret values stay out of the output:**
  - `--log-level debug` and `trace`;
  - the `shell state` report (marked `withheld`);
  - `direnv export` stderr;
  - the `--format json env` state entry.
- **Exporter injection:** quotes, `$()`, backticks, newlines, `$HOME` and backslashes are neutralised in `env --shell=bash` and `direnv export`. `$GITHUB_ENV` and `$GITHUB_PATH` resist newline and delimiter injection.
- **Unusable ambient state:** the exporters survive garbage, other-major and empty states.
- **Plugin dispatch:**
  - Bearer credentials are never forwarded; `OCX_ANNOUNCE_TOKEN` and `OCX_TOOLCHAIN_DIR` are.
  - Dispatch survives garbled and empty states and a plugin directory with a space.
  - The exit code propagates.
- **`package test --script`:**
  - Ambient credentials reach neither the script child nor `ocx.env`.
  - Overlays of `OCX_HOME`, `OCX_OFFLINE` and `OCX_PROJECT` are refused or inert. That assertion is tolerant.
- **SDK spawner:** covered by `spawn_conformance` (10 cases).

Bug (B-3).

Covered elsewhere: credential-helper re-entry (`test_credential_helper_reentry.py`) and the self-update hand-off (`test_self_update.py`).

## Residuals

- **The previous-release middle hop** is proven against the host's installed release. It skips on hosts without one, so CI does not carry it.
- **fish, nu, elvish and pwsh:** the hardening scenarios for these shells run only in the Docker shell zoo. B-1's fish and pwsh legs are proven by the zoo deletion test above. The full zoo on 2a061da4 passed 519, with 0 failures: fish 40 and pwsh 41 passed. nushell skips by D-26.
- **Windows:** no Windows-specific scenario ran (no host).
- **Empty `PATH` segments:** dropping them on composition is pre-existing, not part of this series. `ocx_util::path::retained` filters empty segments, `Env::add_path` reaches it through `move_to_front`, and both are unchanged since 833e8cb5; the behaviour dates from 576e54ad. An empty segment means the cwd on POSIX, so dropping it is the safer reading. The H4 test pins it.
- **Not covered:** `package test` or `patch test` inside an exec layer (H1), and the SYSTEM and managed clamps against crafted policy (H3).

## R2 follow-through (execute run 3)

Review call 2 (R2) asked whether the hardening modules prove what their names claim. Two fix rounds tightened them. Every tightened assertion was shown red by a planted wrong value or input and then green. No product bug surfaced.

| Module | Change | Commit |
|---|---|---|
| `test_hardening_nesting.py` | `import pwd` moved to its use site, so Windows collection no longer aborts | 3e1d2384 |
| `test_hardening_input.py` | pinned-dir smoke now asserts exit code, notice and environment, with a malformed-fixture control; the vanished-pin test asserts quietness; secret-classed fixtures are sentinels; the four-case probe is POSIX-only | 3e1d2384 |
| `test_hardening_input.py` | a vanished pin keeps a recorded project layer; an unpinned prompt retires the same layer (control) | 52d879f4 |
| `test_hardening_limits.py` | launcher failures assert exit 127 and the shell's `exec: … not found` | 3e1d2384 |
| `test_hardening_integrations.py` | `$GITHUB_PATH`, `package test` overlay and credential-absence tests gain positive controls and exact refusals; the plugin credential test lists a control variable from the same run | 89047c1d, 52d879f4 |
| `test_hardening_shells.py` | rollback asserts one outcome; missing ocx or tmux escalates under `__OCX_TESTING_REQUIRE_LIVE_SHELLS` | 89047c1d |
| `test_shell_reconcile.py` (zoo) | `test_deleting_the_project_files_mid_session_reverts_at_the_next_prompt_in_every_hook_shell`, bash, zsh, fish and pwsh × 3 victim sets, carries B-1's fish and pwsh legs | 89047c1d, 0cb574b4 |
| `test_shell_reconcile.py` (zoo) | a pwsh watched path holding a backslash no longer reconciles every prompt (red on the prior binary) | 133ef058 |

B-1's fix gained two follow-ups. Watched-file presence is now sampled once, off the async runtime, for both the digest and the baked gate (d6381a52). A non-UTF-8 member gets no presence term (d6381a52), and pwsh tests presence with `[System.IO.File]::Exists` or `[System.IO.Directory]::Exists` (133ef058).

Residuals added by R2:
- pwsh's binary-presence check in `activate.rs` and `test_self_activate.py` still uses `Test-Path`, which shares the backslash reading. The hook's own binary term moved to the ASCII-literal form in abc97bd0.
- PowerShell watch-term literals are now ASCII only (abc97bd0), closing a security finding recorded by location and class in the plan log. Other PowerShell emitters carry the same class and are deferred to one design decision: R2-46, and R2-47 at `reconcile/plan.rs`.
- A vanished pinned directory keeps its layer silently, while an unconsented pin prints a notice. The ADR does not require a notice, so this is a product call (R2-41).

## Proposed issues

None from the testers.
