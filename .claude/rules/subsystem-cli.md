---
paths:
  - crates/ocx_cli/src/**
---

# CLI Subsystem

Thin CLI shell. Use clap at `crates/ocx_cli/src/`. One file per subcommand. Output format via `Printable` trait.

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16). The command taxonomy below reflects that signed handshake. Any description of `ocx shell hook`, `ocx shell init`, `ocx shell env`, root `ocx install/uninstall/select/exec/deselect/which/deps`, or `ocx ci` describes the **deleted** model — do not implement it.

## High-Level vs OCI-Tier Layering

The CLI surface divides into two tiers. **Toolchain-tier (project-tier)** commands operate on `ocx.toml` + `ocx.lock` — the unit of work is a lock-resolved binding name. **OCI-tier (low-level)** commands operate on OCI identifiers directly and never consult `ocx.toml`. The boundary is firm: missing `ocx.toml` is a usage error (exit 64) for toolchain-tier commands; `ocx.toml` is irrelevant and never consulted by OCI-tier commands.

`ocx run` is the toolchain-tier child-spawn command; `ocx package exec` is its OCI-tier counterpart. `ocx env` is the new toolchain-tier composed-env command. For the full command taxonomy, see `subsystem-cli-commands.md`.

## Command Taxonomy (Signed Handshake Model)

### OCI-tier — `ocx package <verb>`
Per-package, identifier-driven, no `ocx.toml` at any tier:
- `ocx package install <id>` — fetch + materialise into object store
- `ocx package uninstall <id>` — remove from object store
- `ocx package select <id>` — set `current` symlink
- `ocx package deselect <id>` — clear `current` symlink
- `ocx package exec <id> -- cmd` — run package binary, clean env
- `ocx package env <ids...> [--shell[=NAME]]` — composed env for the named packages (reuses `env.rs`)
- `ocx package which <ids...>` — resolve installed packages to paths (`--candidate`/`--current` for stable symlink anchor)
- `ocx package deps <ids...>` — show dependency tree/flat/why for installed packages (`--flat`/`--why`/`--depth`/`--self`)

### Toolchain-tier — root commands
Operate on `ocx.toml` (CWD-walk / `--project` / `OCX_PROJECT`) or `$OCX_HOME/ocx.toml` under `--global`.
`--global` is a **root flag** (before the subcommand), peer of `--project`, defined once on `ContextOptions`.
Canonical form: `ocx --global <subcommand>`.
- `ocx [--global] add <id>`, `ocx [--global] remove <name>`, `ocx [--global] lock`, `ocx [--global] upgrade`
- `ocx [--global] run -- cmd` — compose toolchain env for child process only; never mutates parent shell
- `ocx [--global] env [--shell[=NAME]]` — compose toolchain env. Output format is a **context-only concern** (root `--format`, default **plain** like every command — no subcommand `--format`, handshake §3 amended 2026-05-19); `--shell[=NAME]` is the ONLY eval-safe channel

### `ocx shell` — reduced to one survivor
- `ocx shell completion <name>` — **keep** (genuinely shell-scoped, static)
- `ocx shell hook`, `ocx shell init`, `ocx shell env` — **DELETED** (handshake §7)

### `ocx self` — installation management group
- `ocx self activate [--shell[=NAME]]` — emit eval-safe PATH prepend + completion injection + global env eval. Absent or bare `--shell` → autodetect; explicit `--shell=bash` → named shell. `OCX_NO_COMPLETIONS=1` suppresses completion injection. Intended to be called from `$OCX_HOME/env.sh` at shell startup.
- `ocx self update` — check + install latest version; always bypasses throttle (explicit intent).
- `ocx self update --check` — query only, no install; always bypasses throttle.

`Self_` is in the auto-check **skip list** (`app.rs` `should_check_for_update`): `self activate` runs on every shell startup and must not trigger the background update-check. The skip applies to all `Self_` variants.

### Removed root commands (handshake §7 — exit 64 if invoked)
- `ocx install`, `ocx uninstall`, `ocx select`, `ocx exec`, `ocx deselect`, `ocx which`, `ocx deps` → moved to `ocx package`; ocx maps clap usage errors → EX_USAGE 64 (see `app.rs:112-119`)
- `ocx ci` → removed

## Design Rationale

CLI thin on purpose — all business logic in `ocx_lib` so other consumer reuse (mirror tool, future SDK). Single `Context` struct with lazy init. No build unused client/index. `Printable` trait give each report type own formatting (plain + JSON). Enforce single-table rule without central formatter. See `arch-principles.md` for full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `main.rs` | Entry point (tokio runtime) |
| `app.rs` | CLI parser root + `Cli` struct |
| `command.rs` | `Command` enum dispatching to subcommands; `External(Vec<OsString>)` variant routes unknown names to plugin dispatch |
| `app/context.rs` | `Context`: per-invocation state (FileStructure, Index, PackageManager, Api) |
| `app/context_options.rs` | `ContextOptions`: global flags (offline, remote, format, color, log-level) |
| `app/update_check.rs` | GitHub release update notification |
| `app/version.rs` | Version string accessor |
| `app/plugin_dispatch.rs` | Git/cargo-style external subcommand dispatch; see "Cross-Cutting: Plugin Dispatch" below |
| `command/*.rs` | One file per subcommand |
| `command/env.rs` | Toolchain-tier composed-env command (`ocx env`) |
| `command/run.rs` | Toolchain-tier child-spawn command (`ocx run`) |
| `command/package.rs` | OCI-tier `ocx package` group dispatcher |
| `api.rs` | `Api` facade: dispatches JSON vs plain text |
| `api/data/*.rs` | Report data types implementing `Printable` |
| `options/*.rs` | Shared arg parsing helpers |

## `--shell` Flag Convention

`--shell` is declared as `Option<Option<Shell>>` with clap `num_args=0..=1, require_equals=true, default_missing_value=…` (pattern from `package_push.rs`):
- `--shell` absent → structured-report path through the context `Api` (format = root `--format`, default plain — same for `ocx env` / `ocx package env` as every command)
- `--shell` bare (equals form, no value) → autodetect from `$SHELL`/parent; error (exit 64) if undetectable
- `--shell=bash` → explicit shell

`require_equals=true` guarantees a following positional (`ocx package env --shell ripgrep`) is never swallowed.

`--shell=sh` resolves to `Shell::Dash` via a `PossibleValue::new("sh")` alias — **no new enum variant**, zero new match arms (handshake C5).

## `ContextOptions.format` — `Option<Format>` (single format authority)

`ContextOptions.format` is `Option<options::Format>`. The single `Api::new` call site in `Context::try_init` applies `.unwrap_or(Format::Plain)`. This is the **only** place a format default is decided. **No subcommand declares its own `--format` or builds its own `Api`** — `ocx env` and `ocx package env` report through `context.api()` exactly like every other command (handshake §3 amended 2026-05-19: format is a context-only concern; the former env-specific `None → Json` divergence was removed).

## Context Struct

Made once per command invocation via `Context::try_init(options, color_config)`:

```rust
pub struct Context {
    offline: bool,
    remote_client: Option<oci::Client>,
    remote_index: Option<RemoteIndex>,
    local_index: LocalIndex,
    file_structure: FileStructure,
    api: Api,
    default_index: Index,
    manager: PackageManager,
}
```

## Command Pattern

Every command same flow:

1. **Transform identifiers** — `options::Identifier::transform_all(packages, default_registry)`
2. **Call manager task** — `context.manager().task_all(identifiers, ...)`
3. **Build report data** — from task return values (never from CLI args alone)
4. **Report** — `context.api().report(&data_type)?`

## Cross-Cutting: `--global` Toolchain Tier

`--global` is a **ROOT flag** (must appear before the subcommand), peer of `--project`, defined
**once** on `ContextOptions` as `pub global: bool` with `conflicts_with = "project"`. It selects
`$OCX_HOME/ocx.toml` as the project file for toolchain-tier commands. Per-command `--global` flags
and the `with_command_global` seam no longer exist.

Canonical invocation: `ocx --global <subcommand>` — e.g. `ocx --global add ripgrep:14`.
Toolchain-tier commands affected: `add`, `remove`, `lock`, `upgrade`, `pull`, `run`, `env`.

Strict isolation rules:
- `--global` and `--project` together → clap `conflicts_with` conflict (exit 64 — ocx maps clap usage errors → EX_USAGE 64). No precedence guessing.
- `check_global_project_exclusivity` (in `app/context.rs`, called from `Context::try_init`) closes env-sourced gaps (`OCX_GLOBAL` default value, `OCX_PROJECT` env) that clap `conflicts_with` cannot see.
- `ocx run` is hermetic: without `--global`, reads only the in-effect project file; global file never consulted.
- `ocx --global run -- cmd` composes global toolchain env for child process only; never mutates parent shell.
- `OCX_GLOBAL` is the env-var equivalent (resolution-affecting; forwarded to child ocx via `apply_ocx_config`).
- No implicit `$OCX_HOME/ocx.toml` discovery: project resolution is explicit `--project`/`OCX_PROJECT` → CWD walk → None.
- `ocx package install --global` → clap unknown-flag error (exit 64 — ocx maps clap usage errors → EX_USAGE 64). `--global` is NOT on any `ocx package` subcommand.

Activation (new model): the OCX install script writes `$OCX_HOME/env.sh` containing `eval "$(ocx --global env --shell=sh)"` and appends a block-marker idempotent line to the user's login profile. No `$OCX_HOME/init.<shell>` static files. No `ocx shell hook`/`shell init`.

ADR: `adr_global_toolchain_tier.md`.

## Cross-Cutting: Plugin Dispatch

The `External(Vec<OsString>)` variant on `Command` routes unknown subcommand names to `ocx-<name>` binaries discovered on `$PATH`, matching the cargo / git / kubectl plugin pattern. Dispatch fires from `App::run` before `Context::try_init`, so plugins bypass `FileStructure` / OCI client init. Resolution-affecting env is forwarded via `Env::apply_ocx_config`; plugins inherit the full parent env (trust boundary, see module doc). Built-ins always shadow plugins — clap matches built-in variants first; `External` only fires for unrecognized names. Plugin not-found exits 64 (`EX_USAGE`) with a stderr install hint. Implementation: `app/plugin_dispatch.rs`. ADR: `adr_cli_plugin_pattern.md`.

## Cross-Cutting: OCX Configuration Forwarding

Any code that spawns a subprocess MUST call `env::Env::apply_ocx_config(ctx.config_view())` after building the child env and before `Command::envs()`. Resolution-affecting `ContextOptions` fields MUST appear in `OcxConfigView`, in `Env::apply_ocx_config`, and in `website/src/docs/reference/environment.md`. Presentation fields (log-level / format / color) MUST NOT propagate via env.

## Quality Gate

During review-fix loop, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.
