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
| `env [--shell[=NAME]]` | Composed toolchain env; output via root `--format` (default plain); `--shell[=NAME]` = eval-safe | `--shell[=NAME]` |
| `pull` | Pre-warm package store from `ocx.lock`; re-saves lock to advance mtime for direnv re-fire (skipped under `--dry-run`) | `--dry-run` |

### OCI-Tier Commands (`ocx package`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `package install PKGS...` | Download and install packages (no `ocx.toml` touched) | `-s/--select`, `-p/--platform` |
| `package uninstall PKGS...` | Remove candidate symlink | `-d/--deselect`, `--purge` |
| `package select PKGS...` | Set `current` symlink | `-p` |
| `package deselect PKGS...` | Remove `current` symlink | — |
| `package exec PKGS... -- CMD` | Run command with package env (hermetic) | `--clean`, `-p`, `--self` |
| `package env PKGS... [--shell[=NAME]]` | Per-package composed env; output via root `--format` (default plain); `--shell[=NAME]` = eval-safe | `--shell[=NAME]`, `--self` |
| `package pull PKGS...` | Download to object store only | `-p` |
| `package create PATH` | Bundle directory into archive | `-o`, `-m`, `-l`, `-j`, `--force` |
| `package push -i ID LAYERS...` | Publish archive to registry | `-i`, `-c`, `-n`, `-m`, `-p`, `--build-timestamp` |
| `package describe ID` | Push description metadata | `--readme`, `--logo`, `--title` |
| `package info ID` | Display description metadata | `--save-readme`, `--save-logo` |
| `package test -i ID LAYERS... -- CMD` | Materialise + exec locally (no registry) | `-i`, `-p`, `-m`, `--keep`, `-o`, `--self`, `--clean` |
| `package which PKGS...` | Resolve installed packages to paths (package-root or stable symlink anchor) | `--candidate`, `--current`, `-p` |
| `package deps PKGS...` | Show dependency tree/flat/why | `--flat`, `--why`, `--depth`, `--self`, `-p` |

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
| `ocx ci` | Removed (deferred extension point) |
| `ocx shell hook` | Removed (activation via `$OCX_HOME/env.sh` + `ocx --global env --shell=sh`) |
| `ocx shell init` | Removed (OCX install script owns profile modification) |
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

Rules:
- `--shell` is the **only eval-safe form**. Plain/JSON are NOT sourceable.
- `eval "$(ocx env)"` is a user error. `eval "$(ocx env --shell=bash)"` is correct.
- `--shell=sh` ≡ `--shell=dash` (POSIX strict; `sh` is a `PossibleValue` alias on `Shell::Dash` — no new enum variant).
- `ocx package env` reuses `env.rs::execute` which auto-installs via `find_or_install_all` — deliberate (handshake §2).

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
- **`launcher exec` internal subcommand** — hidden from `--help`. Wire ABI: `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Forces `self_view=true`.
