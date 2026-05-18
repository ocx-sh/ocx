# Handshake: Toolchain CLI & Global Activation

**Status:** Signed off (2026-05-16, all §9 boxes checked by Michael Herwig)
**Amendment 2026-05-17 (post-sign-off, user-approved):** `ocx which` moved
under `ocx package which` for tier consistency — see §2 table + §7 removed list.
**Amendment 2026-05-18 (post-sign-off, user-approved):** `ocx deps` moved
under `ocx package deps` for tier consistency — see §2 table + §7 removed list.
**Date:** 2026-05-16
**Parties:** Michael Herwig, Claude (architect seat)
**Base commit:** `a4211591` (`fix(project): fail-closed GC ledger + global/project conflict seam`)
**Discarded:** the cargo-school `--user` / `$OCX_HOME/bin` / machine-lock detour
(recoverable at git tag `backup/user-wide-detour-2026-05-16`).

This is not a design record. It is the agreement reached in this session,
written so both parties read the same thing. It supersedes
`adr_global_toolchain_activation.md` (delete) and re-anchors
`adr_global_toolchain_tier.md` (Accepted) Decisions 3, 4, 6.

---

## 1. The one principle

The global toolchain **is** the project toolchain. Same `ocx.toml` schema,
same lock, same commands, same resolver, groups allowed. `--global` only
re-targets the file to `$OCX_HOME/ocx.toml` + `$OCX_HOME/ocx.lock`.

The **only** difference between project and global is *how the composed
environment reaches a shell*:

- **project** — loaded per-directory by direnv (unchanged, untouched here).
- **global** — loaded once per shell by an exporter sourced from the login
  profile (analogous to direnv, but login-shell-scoped).

No `--user`. No `$OCX_HOME/bin`. No machine-lock-without-`ocx.toml`. No
`advisory` field. No `--system` / `/etc/profile.d`. Those were the detour.

---

## 2. Command map

Two tiers, made visible by command location.

### OCI-tier — package primitives → `ocx package <verb>`

Per-package, identifier-driven, no `ocx.toml` at any tier. Owns the
`candidate`/`current` floating symlinks.

| Command | Meaning |
|---|---|
| `ocx package install <id>` | fetch + materialise into object store |
| `ocx package uninstall <id>` | remove from object store |
| `ocx package select <id>` | set `current` symlink |
| `ocx package deselect <id>` | clear `current` symlink (new — pairs `select`) |
| `ocx package exec <id> -- cmd` | run a package binary, clean env |
| `ocx package env <ids...>` | composed env for the named packages |
| `ocx package which <ids...>` | resolve installed packages to paths (package-root, or `--candidate`/`--current` symlink anchor) — OCI-tier query, never reads `ocx.toml` (amendment 2026-05-17) |
| `ocx package deps <ids...>` | show dependency tree/flat/why for installed packages — OCI-tier query, never reads `ocx.toml` (amendment 2026-05-18) |

Devcontainer-of-a-tool stays: `ocx package install X && ocx package select X
&& ocx package env --current X`. Untouched by this handshake.

### Toolchain-tier — manifest-driven → root commands

Operate on the in-scope `ocx.toml` (CWD-walk / `--project` / `OCX_PROJECT`),
or `$OCX_HOME/ocx.toml` under `--global`.

> **`--global` is a root flag** — it must appear before the subcommand, not after it.
> Canonical form: `ocx --global <verb>`, e.g. `ocx --global add ripgrep:14`.
> The flag is defined once on `ContextOptions` (peer of `--project`); it is **not** accepted
> on any individual subcommand. `ocx add --global` → clap unknown-flag error (exit 64).

| Command | Meaning |
|---|---|
| `ocx [--global] add <id>` | record + resolve + install into the toolchain |
| `ocx [--global] remove <name>` | drop from the toolchain |
| `ocx [--global] lock` | resolve only |
| `ocx [--global] upgrade [-g grp]` | re-resolve advisory tags |
| `ocx [--global] run -- cmd` | run a command with the toolchain env |
| `ocx [--global] env` | **composed env of the in-scope toolchain** |

### `ocx shell` — reduced to one survivor

| Command | Fate |
|---|---|
| `ocx shell completion <name>` | **keep** (genuinely shell-scoped, static) |
| `ocx shell hook` | **delete** — stateful per-prompt `_OCX_APPLIED` diff; redundant with direnv (project) + the login exporter (global). The "BS" byproduct. |
| `ocx shell init` | **delete** — only existed to wire the hook / static render. The OCX installer now owns profile wiring (§4). |
| `ocx shell env` | **delete** — ambiguous cut. Replaced by `ocx env` (toolchain) and `ocx package env` (per-package). |

### `ocx direnv …` — untouched

Project-only direnv adapter. Out of scope for this handshake.

### Removed root commands

- `ocx install`, `ocx uninstall`, `ocx select`, `ocx exec` → moved under
  `ocx package`. Pre-stable CLI; clean break, no aliases, no shim
  (`project_breaking_compat_next_version.md`). This is the only real UX
  cost; accepted.
- `ocx which` → moved under `ocx package` (amendment 2026-05-17). Same
  rationale: identifier-driven OCI-tier query that never reads `ocx.toml`;
  leaving it at root contradicted the tier split. Same clean-break terms.
- `ocx deps` → moved under `ocx package` (amendment 2026-05-18). Same
  rationale: identifier-driven OCI-tier query that never reads `ocx.toml`;
  leaving it at root contradicted the tier split. Same clean-break terms.
- `ocx ci` → removed. CI export is a deferred extension point (§6), not a
  command.
- Implicit `$OCX_HOME/ocx.toml` discovery → stays deleted (`a4211591`).

---

## 3. The `env` command (both tiers)

Two orthogonal axes. Scope = command location. Format = flag. There is no
`--format shell`.

| Want | Invocation |
|---|---|
| Toolchain env, machine-readable | `ocx [--global] env` (default: global `--format`, JSON or plain) |
| Toolchain env, sourceable | `ocx [--global] env --shell[=NAME]` |
| Per-package env, machine-readable | `ocx package env <ids...>` |
| Per-package env, sourceable | `ocx package env <ids...> --shell[=NAME]` |

Rules:

- **Default output** = whatever the global `--format` selects (JSON default,
  or plain). For inspection / tooling. **Backend-first: JSON is the default
  channel.**
- **`--shell` is the only eval-safe form.** Plain/JSON are *not* sourceable
  (no quoting/escaping → breaks on paths with spaces). Documented explicitly;
  `eval "$(ocx env)"` is a user error, `eval "$(ocx env --shell=bash)"` is
  correct.
- **`--shell` value via equals form only.** Bare `--shell` = autodetect from
  `$SHELL`/parent; error if undetectable. `--shell=bash` = explicit. Equals
  form mandated so a following positional (`ocx package env --shell ripgrep`)
  is never swallowed. Flags precede positionals (project convention).
- Implementation reuses the existing `resolve_env` → `composer::compose`
  path. It already composes **both** entrypoint-launcher packages **and**
  plain no-entrypoint packages that declare a `Modifier::Path` into
  `content/`. No new resolution machinery.

`ocx --global env --shell=bash` is the single command the login exporter
runs (§4). `--global` (root flag) and `--shell` are independent and combine freely.

---

## 4. Activation: how global env reaches a shell

1. **One file per shell *family*, not per shell.** POSIX-compatible shells
   (bash, zsh, sh, dash, …) share **one** `$OCX_HOME/env.sh` using POSIX
   `.`/`eval`. A separate file exists *only* where the syntax genuinely
   differs — fish, PowerShell, nushell, elvish. Minimise files floating
   around; default is the single POSIX file. Each is **fixed text**:

   ```sh
   # $OCX_HOME/env.sh   (POSIX: bash / zsh / sh / dash)
   eval "$(ocx --global env --shell=sh)"
   ```

2. The **in-repo OCX install script** appends one `source`/`.` line to the
   user's login profile (`.profile`/`.zshrc`/PowerShell profile/…) pointing
   at the matching file, guarded so re-running the installer is idempotent.
   There is **no separate `setup.ocx.sh` website repo** — that does not
   exist and is not planned now; the install script in this repo owns the
   profile modification.

3. Every new login shell sources it → `ocx --global env --shell=sh` runs
   fresh → the global toolchain is on `PATH`.

4. **rc/profile content is single-sourced through `ocx`, not hand-written
   in the installer.** Preferred: `ocx env` (or an `ocx env`-adjacent
   subcommand) emits the rc/profile snippet for a given shell (the concept
   of "rc" generalises to a PowerShell profile script, fish conf, etc.) so
   the installer just calls `ocx` and writes the output — one source of
   truth, no shell logic duplicated in the installer. **This generator MAY
   be deferred** (installer ships the fixed `eval` line directly at first);
   recorded so it is designed-for, not invented later ad-hoc.

The env file is fixed; the `eval` runs each shell start, so it is **always
current** — there is no stale per-tool static render, no `$OCX_HOME/init.*`,
no `project_guarded` shell-script `ocx.toml` walk. Those are deleted (§7).

---

## 5. Isolation & `run`

- **Isolation by PATH precedence, no in-shell strip.** Inside a project,
  direnv prepends the project toolchain ahead of the ambient global. Normal
  precedence. `emit_global_path_strip` / `strip_global` are deleted.
- **`ocx run` (bare)** = project tier; error if no project/lock.
- **`ocx --global run -- cmd`** = compose the global toolchain env for that
  child process only; never mutates the parent shell. Explicit tier
  selection, single tier, no implicit merge — not a leak.
- Builds stay reproducible because `run` / `package exec` compose exactly
  the explicitly selected tier, independent of interactive PATH.

---

## 6. On the radar — designed for, not built (YAGNI)

Recorded so the grammar stays forward-compatible; **no code now**:

- **CI export.** GitHub Actions (`$GITHUB_ENV`/`$GITHUB_PATH`, `KEY=VALUE`,
  autodetected sink, current-job) and GitLab CI/CD Functions (JSON-lines
  `{"name","value"}`, path from `${{ export_file }}` passed explicitly,
  no path channel, string/number/boolean only, *later-step* scope) differ
  enough that the destination/provider must be explicit value-flags
  (`--ci=<provider>`, an explicit out-path), never positionals or
  subcommands (package tier has variadic positional ids). **GitLab not
  supported now.** Same-step tool availability is always `--shell`, not CI.
- **File / profile export.** A future destination axis: write the exporter
  output to a file/profile instead of stdout. Not named `--ci`/`--out`
  today; added only when built. No dead surface now.

Two axes reserved: **format** (json/plain | shell | future ci) ×
**destination** (stdout | future file). Today only stdout × {default,
shell}. Equals-form value-flags only, so the rest slots in without a
grammar break.

---

## 7. Code delta vs `a4211591` (activation + taxonomy only)

**Delete:**

- `shell_init.rs::project_guarded` (duplicated POSIX `ocx.toml` walk)
- `shell_init.rs::write_static_global_entrypoint` (per-tool static render) +
  the static-file write + `home_init_path` usage from `shell_init`
- `shell_hook.rs::emit_global_path_strip` + the `strip_global` parameter
- `shell_hook.rs::emit_global_current_set` (the global prompt arm)
- the whole `ocx shell hook` / `ocx shell init` / `ocx shell env` commands
- the `install.rs` `global: bool` field + its delegation to `Add::execute`
  (the removed `install --global` sugar — the one `--global` site from
  `a4211591` that does NOT survive)
- detour remnants if still present: `--user`, `UserBinStore`/`$OCX_HOME/bin`,
  machine-lock-without-toml, `advisory` field, `--system`,
  `resolve_lock_tools` (confirm unused; else keep harmless)

**Add:**

- `ocx env` (toolchain tier, root) — stateless; reuses
  `resolve_global_current_env` + `resolve_env`/`composer::compose`
- `ocx package` command group with `install`/`uninstall`/`select`/
  `deselect`/`exec`/`env` (move the four root commands in; add `deselect`)
- one shared `emit_lines(shell, &[Entry])` helper, extracted from the
  inline copies, consumed by `ocx env` and `ocx direnv export`

**Re-point:**

- `direnv_export.rs` → delegates to the shared `emit_lines` (behaviour
  unchanged: project, stateless)
- the OCX installer → generates the per-shell `$OCX_HOME/env.<shell>` file
  and appends the profile `source` line (replaces `ocx shell init`)

**Keep unchanged from `a4211591`:**

- **[Updated 2026-05-17]** `--global` is a single root flag on `ContextOptions`
  (peer of `--project`), enforced by clap `conflicts_with = "project"`.
  `ProjectConfig::resolve` global arm and `--global` ⟂ `--project` = exit 64
  survive. Per-command `--global` flags and the `with_command_global` reconcile
  seam are **deleted**; `check_global_project_exclusivity` (in
  `app/context.rs`, called from `Context::try_init`) closes the env-sourced
  gaps (`OCX_GLOBAL` default value, `OCX_PROJECT` env) that clap
  `conflicts_with` cannot see.
- `resolve_global_current_env`, the GC / no-self-link / conflict seam
- `candidate`/`current` floating symlinks and their commands (now under
  `ocx package`)

---

## 7a. Stale-doc safeguard (mandatory, not optional)

Cross-model adversary reviews mine design records and implement items
found there without further review. So stale records are a correctness
hazard, not just tidiness. **Already done in this session:**
`adr_global_toolchain_activation.md` deleted; `adr_global_toolchain_tier.md`
Decisions 3/4/6/7 carry inline `[SUPERSEDED] / DO NOT IMPLEMENT` markers +
a top banner pointing here.

**The implementation plan MUST, before any review/Codex pass, reconcile
(rewrite or inline-mark `[SUPERSEDED → handshake]`) every doc that still
describes the rejected model** — so no adversary can act on stale text:

- `adr_cli_high_low_layering.md` (global-tier layer-table row; `shell hook`
  per-prompt evaluator framing)
- `adr_project_toolchain_config.md` (Amendments C/F home-tier clauses)
- `arch-principles.md` (ADR index + Key Concepts: `$OCX_HOME/bin`,
  user-wide, `shell init`/`shell hook` rows)
- `subsystem-cli.md`, `subsystem-cli-commands.md`, `subsystem-cli-api.md`
  (auto-load on CLI edits — highest blind-implementation risk: must show
  the `ocx package` group, root `ocx env`, deleted `shell hook/init/env`)
- project memory `project_setup_ocx_canonical_install.md` (the
  `setup.ocx.sh` website-repo split is invalid — correct to in-repo
  installer owning profile modification)

This list is part of the handshake scope, gated in §9.

## 8. Mandated test fixture

Add a test fixture for a package with **no entrypoints** that exports a
`Modifier::Path` into `content/bin/`, exercised end-to-end through
`ocx --global env --shell=bash` (install via `ocx --global add`, source the
exporter, resolve the binary). This closes the `make_package`-always-emits-
entrypoints blind spot that hid the detour's entrypoint-only bug.

---

## 9. Sign-off

- [x] §1 principle correct (global = project toolchain; only diff = load site)
- [x] §2 command map correct (`ocx package` group; `shell` → `completion`;
      deletions; `ocx env` at root)
- [x] §3 `env` cut correct (scope=location, format=flag, `--shell=` equals,
      JSON default, plain not sourceable)
- [x] §4 activation correct (one POSIX env file + per-family only where
      syntax differs; in-repo installer owns profile mod, no website repo;
      rc content single-sourced via `ocx`, generator may defer)
- [x] §5 isolation/`run` correct (precedence not strip; `--global run`
      child-only)
- [x] §6 deferred scope acceptable (CI/file export designed-for, not built;
      GitLab not now)
- [x] §7 code delta acceptable
- [x] §7a stale-doc safeguard scope accepted (plan must reconcile listed
      records before any review/Codex pass)
- [x] §8 no-entrypoint fixture mandated

On sign-off → `/swarm-plan` for the revert-aware implementation plan
(stopping there again for plan approval before any execute).
