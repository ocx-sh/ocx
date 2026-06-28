---
paths:
  - crates/ocx_cli/src/command/**
---

# OCX CLI Commands — Quick Reference

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16). The taxonomy below reflects the signed handshake. Commands listed in the **Deleted Commands (exit 64)** section do NOT exist; any description of them in older ADRs or rules is superseded. Do not implement deleted commands.

Concise index of all `ocx` CLI commands. User-facing per-command docs live in `website/src/docs/reference/command-line.md`. Implementation under `crates/ocx_cli/src/command/`.

---

## Layering: Toolchain-Tier vs OCI-Tier

The CLI surface splits into two tiers. The split is firm.

| Tier | Commands | Input | Consults `ocx.toml`? |
|------|----------|-------|----------------------|
| **Toolchain-tier** | `add`, `remove`, `lock`, `upgrade`, `run`, `env` | Binding names / OCI id (add) | **Yes** (or `$OCX_HOME/ocx.toml` under `--global`) |
| **OCI-tier** (`ocx package`) | `install`, `uninstall`, `select`, `deselect`, `exec`, `env`, `which`, `deps` | OCI identifiers | **Never** |
| **Bootstrap / mixed** | `init`, `direnv init`, `direnv export`, `about`, `version`, `shell completion` | Varies | — |
| **Low-level registry** | `package pull`, `package push`, `package describe`, `package info`, `package create`, `index update/list/catalog`, `login`, `logout` | OCI identifiers | Never |
| **Low-level local store** | `clean`, `launcher exec` | OCI identifiers | Never |

**Layer-purity rule:** `ocx run` is toolchain-tier (binding-name semantics); `ocx package exec` is OCI-tier (identifier semantics). `ocx env` is toolchain-tier; `ocx package env` is OCI-tier. No command silently switches contract based on CWD.

---

## Global Flags (all commands)

`--global` is a root flag — it must appear **before** the subcommand name (peer of `--project`), not after it.

| Flag | Env Var | Default | Purpose |
|------|---------|---------|---------|
| `--color auto\|always\|never` | `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` | auto | ANSI color output control |
| `--remote` | `OCX_REMOTE` | false | Route mutable lookups to remote registry |
| `--offline` | `OCX_OFFLINE` | false | Disable all network access |
| `--format plain\|json` | — | plain (all commands, no exceptions) | Root-only output format; no subcommand-level `--format` |
| `--index PATH` | `OCX_INDEX` | — | Override local index directory |
| `-l/--log-level` | — | — | Tracing level |
| `--global` | `OCX_GLOBAL` | false | Select `$OCX_HOME/ocx.toml` toolchain tier; affects toolchain-tier commands `add`/`remove`/`lock`/`upgrade`/`pull`/`run`/`env`; mutually exclusive with `--project` |

## Toolchain-Tier: `--global` vs Project

`--global` is a **root flag** (before the subcommand), defined once on `ContextOptions` (peer of
`--project`). It re-targets the project file to `$OCX_HOME/ocx.toml` for the toolchain-tier
commands: `add`, `remove`, `lock`, `upgrade`, `pull`, `run`, `env`. Canonical form:
`ocx --global <subcommand>` — e.g. `ocx --global add ripgrep:14`.

Project-tier commands resolve their project file in strict precedence order: `--global` (explicit) → `--project`/`OCX_PROJECT` (explicit) → CWD walk → None.

Mutually exclusive with `--project` — combining both is a clap conflict (exit 64 — ocx maps clap usage errors → EX_USAGE 64). No implicit discovery of `$OCX_HOME/ocx.toml`. `OCX_GLOBAL` is the env equivalent.

**`ocx package install --global` → clap unknown-flag error (exit 64 — ocx maps clap usage errors → EX_USAGE 64).** `--global` is NOT on any `ocx package` subcommand.

---

## Command Summary

### Toolchain-Tier Commands

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `add IDENTIFIER` | Append binding to `ocx.toml`, update lock, install | `-g/--group`, `--pull/--no-pull` |
| `init` | Create minimal `ocx.toml` in current directory | — |
| `remove IDENTIFIER` | Drop binding from `ocx.toml`, rewrite lock | — |
| `lock` | Resolve tags to digests, write `ocx.lock` | `-g/--group`, `--pull/--no-pull` |
| `upgrade PKGS...` | Re-resolve advisory tags in lock | `-g/--group`, `--pull/--no-pull` |
| `run [-g GROUP]... [NAME...] -- ARGV...` | Spawn child with composed toolchain env | `-g/--group`, `--clean`, `--self` |
| `env [--shell[=NAME]] [--ci[=PROVIDER]]` | Composed toolchain env; output via root `--format` (default plain); `--shell[=NAME]` = eval-safe; `--ci` = CI sink (later-step) | `--shell[=NAME]`, `--ci[=PROVIDER]`, `--export-file` |
| `pull` | Pre-warm package store from `ocx.lock`; re-saves lock to advance mtime for direnv re-fire (skipped under `--dry-run`) | `--dry-run` |

### OCI-Tier Commands (`ocx package`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `package install PKGS...` | Download and install packages (no `ocx.toml` touched) | `-s/--select`, `-p/--platform` |
| `package uninstall PKGS...` | Remove candidate symlink | `-d/--deselect`, `--purge` |
| `package select PKGS...` | Set `current` symlink | `-p` |
| `package deselect PKGS...` | Remove `current` symlink | — |
| `package exec PKGS... -- CMD` | Run command with package env (hermetic) | `--clean`, `-p`, `--self` |
| `package env PKGS... [--shell[=NAME]] [--ci[=PROVIDER]]` | Per-package composed env; output via root `--format` (default plain); `--shell[=NAME]` = eval-safe; `--ci` = CI sink (later-step) | `--shell[=NAME]`, `--ci[=PROVIDER]`, `--export-file`, `--self` |
| `package pull PKGS...` | Download to object store only | `-p` |
| `package create PATH` | Bundle directory into archive | `-o`, `-m`, `-l`, `-j`, `--force` |
| `package push -i ID LAYERS...` | Publish archive to registry | `-i`, `-c`, `-n`, `-m`, `-p`, `--build-timestamp` |
| `package describe ID` | Push description metadata | `--readme`, `--logo`, `--title` |
| `package info ID` | Display description metadata | `--save-readme`, `--save-logo` |
| `package test -i ID LAYERS... -- CMD` | Materialise + exec locally (no registry) | `-i`, `-p`, `-m`, `--keep`, `-o`, `--self`, `--clean` |
| `package which PKGS...` | Resolve installed packages to paths (package-root or stable symlink anchor) | `--candidate`, `--current`, `-p` |
| `package deps PKGS...` | Show dependency tree/flat/why | `--flat`, `--why`, `--depth`, `--self`, `-p` |

### Installation Management Commands (`ocx self`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `self setup [VERSION]` | Bootstrap specified or latest ocx, write env shims, add managed activation block to shell profiles | `--no-modify-path`, `--profile PATH`, `--dry-run`, `--force` |
| `self activate` | Emit eval-safe PATH prepend + completions + global env eval for the detected shell | `--shell[=NAME]` |
| `self update` | Check and install the latest released ocx version | — |
| `self update --check` | Query registry for newer version; no install | `--check` |

**`self setup` notes:**
- **`VERSION` positional (optional)**: three forms accepted — `1.2.3` (tag), `sha256:<64hex>` (digest, written bare without `@`), `1.2.3@sha256:<64hex>` (tag+digest immutability assertion). Malformed input exits 64. `tag@digest` mismatch exits 65 (fail-closed; message names both digests; under `--frozen` adds stale-index hint). Pin satisfied ⟺ `current` already points at the pinned digest — no re-download. Downgrade (pinned tag semver-older than installed) warns on stderr and proceeds. Literal `latest` = ordinary tag lookup; omit `VERSION` for latest-release semantics.
- Resolved digest surfaces in JSON output (`bootstrap.digest`; omitted when unpinned). Round-trips as a pin: `ocx self setup 0.9.2@<digest>`.
- ChainMode applies to pin resolution: `--frozen`/`--offline` + uncached tag → exit 81; digest-only pin works frozen when blobs cached.
- Exit 74 (`IoError`) — writing an env shim or shell profile failed (disk full, permission denied, etc.).
- `--no-modify-path` (or `OCX_NO_MODIFY_PATH` truthy) — writes env shims only, skips profile modification. `OCX_NO_MODIFY_PATH` is read through `ocx_lib::env::flag` + `BooleanString`: truthy = `1`/`y`/`yes`/`on`/`true`; falsy = `0`/`n`/`no`/`off`/`false`; unrecognised non-empty value → WARN + default (`false`). The opt-out is not remembered between runs.
- `--profile PATH` — override auto-detected profiles; repeatable. Explicit targets use POSIX-fence semantics regardless of file name.
- `--dry-run` — resolve but write nothing; reports `WouldPull` with resolved digest. Never returns exit 82.
- `--force` — overwrite a managed block that carries user edits (the dirty state).
- Exit 82 (`DirtyRcBlock`) — at least one profile contained a managed block with user edits and `--force` was not passed. Scripts: `case $? in 82)`.
- All `Self_` variants (including `self setup`) are in the `should_check_for_update` skip list.

**`self activate` notes:**
- `--shell` absent or bare → autodetect from `$SHELL`/parent process; exit 64 if undetectable. Differs from `ocx env --shell` where absent means "structured report path".
- Completions are emitted **inline** in the activation stream (no file): emitted first so PowerShell's `using namespace` leads the stream, which `Invoke-Expression` requires; zsh self-loads `compinit` before clap's trailing `compdef`. Gated by `options::Completion` (`crates/ocx_cli/src/options/completion.rs`): paired `--completion`/`--no-completion` flags (`overrides_with`, last-wins), then `OCX_NO_COMPLETIONS`, then a stderr-TTY auto fallback. The accessor is `Completion::enabled(interactive: bool)`.
- The `env.sh`/`env.ps1` shim decides interactivity itself (`$-`, `status is-interactive`, `[Environment]::UserInteractive`) and passes the explicit `--completion`/`--no-completion` flag, so the gate never depends on the binary probing a stderr the shim has redirected. The auto stderr-TTY path only serves a direct `ocx self activate` with neither flag. The shim also selects the real sourcing shell (`bash`/`zsh`, never `sh` → `Dash`, which has no completion backend) so the correct extension is generated.
- `OCX_NO_COMPLETIONS=1` → suppress completion injection.
- `Self_` variants are in the `should_check_for_update` skip list — `self activate` runs on every shell start and must not trigger the background update-check.

**`self update` / `self update --check` notes:**
- Both always bypass the auto-check throttle (explicit user intent).
- Both are **ChainMode-aware** (`TagProbe::Index`): version discovery resolves the latest tag through the configured local index, honouring `--offline`/`--frozen`/`--remote` + `OCX_INDEX` like every other tag-resolving command. Plain `ocx self update` in default mode reads the local index; `ocx --remote self update` forces a live registry probe. (The background auto-check `app/update_check.rs` is the only caller that passes `TagProbe::Remote` — it must surface fresh upstream releases regardless of ambient ChainMode.)
- `--check` calls `self_check_update(Some(Duration::ZERO), TagProbe::Index)` and reports without installing.
- Without `--check` calls `self_update()` which routes the install through `install_all`.
- A `sha256:` digest pin in `self setup` selects a platform-specific package digest; the same tag resolves to a different digest per OS/arch. For CI matrices, pin by tag (each runner resolves its own platform digest) rather than sharing a single digest across platforms.

### Other Commands

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `login [REGISTRY]` | Authenticate to a registry | `-u/--username`, `--password-stdin`, `--insecure` |
| `logout [REGISTRY]` | Remove stored credentials | — |
| `clean` | GC unreferenced objects | `--dry-run`, `--force` |
| `shell completion` | Generate shell completions | `--shell` |
| `direnv init` | Write `.envrc` wiring `ocx direnv export` | `--force` |
| `direnv export` | Stateless bash export generator for direnv `.envrc` | — |
| `index catalog` | List known repositories | `--tags` |
| `index list PKGS...` | List tags for packages | `--platforms`, `--variants` |
| `index update PKGS...` | Sync local index from remote | — |
| `version` | Print version | — |
| `about` | Print version + registry + platform + shell + home | — |

### Deleted Commands (exit 64 if invoked)

These commands **do not exist** in the current model. Any invocation returns exit 64 (ocx maps clap usage errors → EX_USAGE 64; see `app.rs:112-119`):

| Deleted command | Replacement |
|-----------------|-------------|
| `ocx install` | `ocx package install` |
| `ocx uninstall` | `ocx package uninstall` |
| `ocx select` | `ocx package select` |
| `ocx deselect` | `ocx package deselect` |
| `ocx exec` | `ocx package exec` |
| `ocx which` | `ocx package which` |
| `ocx deps` | `ocx package deps` |
| `ocx ci` | Removed as a command; CI export is the `--ci[=PROVIDER]` flag on `ocx env` / `ocx package env` |
| `ocx shell hook` | Removed (activation via `$OCX_HOME/env.sh` + `ocx --global env --shell=sh`) |
| `ocx shell init` | Removed (`ocx self setup` owns profile modification) |
| `ocx shell env` | `ocx env` (toolchain) or `ocx package env` (per-package) |

---

## `ocx env` vs `ocx package env`

| Want | Invocation | Output |
|------|------------|--------|
| Toolchain env, default | `ocx [--global] env` | plain table (context default) |
| Toolchain env, machine-readable | `ocx --format json [--global] env` | JSON |
| Toolchain env, eval-safe | `ocx [--global] env --shell[=NAME]` | Shell export lines |
| Per-package env, default | `ocx package env <ids...>` | plain table (context default) |
| Per-package env, machine-readable | `ocx --format json package env <ids...>` | JSON |
| Per-package env, eval-safe | `ocx package env <ids...> --shell[=NAME]` | Shell export lines |
| Either, CI sink (later-step) | `ocx [--global] env --ci=github` / `ocx package env <ids...> --ci=gitlab [--export-file PATH]` | GitHub two-file sink / GitLab JSON-lines |

Rules:
- `--shell` is the **only eval-safe form**. Plain/JSON are NOT sourceable.
- `eval "$(ocx env)"` is a user error. `eval "$(ocx env --shell=bash)"` is correct.
- `--shell=sh` ≡ `--shell=dash` (POSIX strict; `sh` is a `PossibleValue` alias on `Shell::Dash` — no new enum variant).
- `ocx package env` reuses `env.rs::execute` which auto-installs via `find_or_install_all` — deliberate (handshake §2).
- `--ci` writes to a CI persistence channel for **later** steps (vs `--shell` = current step). `--ci` ⟂ `--shell` (exit 64). `--ci=github` infers `$GITHUB_ENV`/`$GITHUB_PATH` (rejects `--export-file`, exit 64); `--ci=gitlab` writes JSON-lines to `--export-file` or stdout. Bare `--ci` autodetects (`$GITHUB_ACTIONS`/`$GITLAB_CI`); exit 64 if undetected. Explicit `--ci=github` outside GitHub Actions → exit 78. ADR `adr_ci_env_export_flag.md`.

---

## Task Method Quick Reference

| Manager Method | Auto-Install | Symlink | Use In |
|----------------|-------------|---------|--------|
| `find_all()` | No | No | `package which`, `package deps` |
| `resolver().build_graph()` | No | No | `package deps` |
| `find_symlink_all(kind)` | No | Yes (candidate/current) | `package which --candidate` |
| `find_or_install_all()` | **Yes** | No | `package env`, `package exec` |
| `install_all(candidate=true)` | N/A | Creates candidate | `package install` |
| `install_all(candidate=false)` | N/A | No | `package pull` |
| `deselect_all()` | No | Removes current | `package deselect` |
| `uninstall_all()` | No | Removes candidate | `package uninstall` |
| `clean()` | No | — | `clean` |

---

## Semantics & Gotchas

- **`ocx run` semantics** — `--` mandatory, exit 64 if missing (ocx maps clap usage errors → EX_USAGE 64); default scope = `[tools]` only; `ocx --global run` = compose global toolchain env for child only, never mutates parent; `ocx run` (no `--global`) never reads `$OCX_HOME/ocx.toml`; missing `ocx.toml` → exit 64; missing `ocx.lock` → exit 78.
- **`ocx env` output format is a context-only concern** — root `--format` (default plain, same as every command); no subcommand `--format`; no env-specific JSON default (handshake §3 amended 2026-05-19, reversing the original backend-first JSON default). JSON via `ocx --format json env`. Plain and JSON are both NOT sourceable; `--shell[=NAME]` only eval-safe channel.
- **`package env` auto-installs** — `ocx package env` uses `find_or_install_all` (unlike the deleted `shell env` which used `find_all`). Do NOT assert no-download semantics against `ocx package env`.
- **`login`/`logout` registry optional** — falls back to `OCX_DEFAULT_REGISTRY` (default `ocx.sh`).
- **`logout` is always exit 0** — matches `docker`/`oras`/`helm`; CI cleanup must not fail.
- **`--password VALUE` does not exist** — argv-visible secrets leak via `ps`. Use `--password-stdin`.
- **`index list <pkg>@<digest>` rejected** — tag-only identifiers still work.
- **`shell hook` vs `direnv export`** — `shell hook` is deleted; `direnv export` is stateless bash export generator for direnv `.envrc` (still alive, untouched).
- **`package test` tempdir lifecycle** — without `--keep` or `--output`, temp dir deleted on any exit. `--keep` + `--output` are mutually exclusive.
- **`launcher exec` internal subcommand** — hidden from `--help`. Wire ABI: `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Forces `self_view=true`. Resolves `${installPath}` in baked entrypoint `args` and prepends them before user args (wire ABI unchanged).
