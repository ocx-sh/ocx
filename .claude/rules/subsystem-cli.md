---
paths:
  - crates/ocx_cli/src/**
---

# CLI Subsystem

Thin CLI shell. Use clap at `crates/ocx_cli/src/`. One file per subcommand. Output format via `Printable` trait.

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16). The command taxonomy below reflects that signed handshake. Any description of `ocx shell hook`, `ocx shell init`, `ocx shell env`, root `ocx install/uninstall/select/exec/deselect`, or `ocx ci` describes the **deleted** model — do not implement it.

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

### Toolchain-tier — root commands
Operate on `ocx.toml` (CWD-walk / `--project` / `OCX_PROJECT`) or `$OCX_HOME/ocx.toml` under `--global`:
- `ocx add [--global] <id>`, `ocx remove [--global] <name>`, `ocx lock [--global]`, `ocx upgrade [--global]`
- `ocx run [--global] -- cmd` — compose toolchain env for child process only; never mutates parent shell
- `ocx env [--global] [--shell[=NAME]]` — compose toolchain env: **JSON by default** (backend-first, handshake §3); `--format plain` for human inspection (NOT sourceable); `--shell[=NAME]` is the ONLY eval-safe channel

### `ocx shell` — reduced to one survivor
- `ocx shell completion <name>` — **keep** (genuinely shell-scoped, static)
- `ocx shell hook`, `ocx shell init`, `ocx shell env` — **DELETED** (handshake §7)

### Removed root commands (handshake §7 — exit 2 from clap if invoked)
- `ocx install`, `ocx uninstall`, `ocx select`, `ocx exec`, `ocx deselect` → moved to `ocx package`
- `ocx ci` → removed

## Design Rationale

CLI thin on purpose — all business logic in `ocx_lib` so other consumer reuse (mirror tool, future SDK). Single `Context` struct with lazy init. No build unused client/index. `Printable` trait give each report type own formatting (plain + JSON). Enforce single-table rule without central formatter. See `arch-principles.md` for full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `main.rs` | Entry point (tokio runtime) |
| `app.rs` | CLI parser root + `Cli` struct |
| `command.rs` | `Command` enum dispatching to subcommands |
| `app/context.rs` | `Context`: per-invocation state (FileStructure, Index, PackageManager, Api) |
| `app/context_options.rs` | `ContextOptions`: global flags (offline, remote, format, color, log-level) |
| `app/update_check.rs` | GitHub release update notification |
| `app/version.rs` | Version string accessor |
| `command/*.rs` | One file per subcommand |
| `command/env.rs` | Toolchain-tier composed-env command (`ocx env`) |
| `command/run.rs` | Toolchain-tier child-spawn command (`ocx run`) |
| `command/package.rs` | OCI-tier `ocx package` group dispatcher |
| `api.rs` | `Api` facade: dispatches JSON vs plain text |
| `api/data/*.rs` | Report data types implementing `Printable` |
| `options/*.rs` | Shared arg parsing helpers |

## `--shell` Flag Convention

`--shell` is declared as `Option<Option<Shell>>` with clap `num_args=0..=1, require_equals=true, default_missing_value=…` (pattern from `package_push.rs`):
- `--shell` absent → default-format path (JSON for `ocx env` / `ocx package env`)
- `--shell` bare (equals form, no value) → autodetect from `$SHELL`/parent; error (exit 64) if undetectable
- `--shell=bash` → explicit shell

`require_equals=true` guarantees a following positional (`ocx package env --shell ripgrep`) is never swallowed.

`--shell=sh` resolves to `Shell::Dash` via a `PossibleValue::new("sh")` alias — **no new enum variant**, zero new match arms (handshake C5).

## `ContextOptions.format` — `Option<Format>` Precondition

`ContextOptions.format` is `Option<options::Format>`. The `Api::new` call site applies `.unwrap_or(Format::Plain)` so **all legacy commands keep Plain default unchanged**. `ocx env` and `ocx package env` resolve `None → Json` for their own output.

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

`--global` selects `$OCX_HOME/ocx.toml` as the project file for toolchain-tier commands (`add`, `remove`, `lock`, `upgrade`, `run`, `env`). Defined in `ContextOptions` as `pub global: bool` with `conflicts_with = "project"`.

Strict isolation rules:
- `--global` and `--project` together → clap `conflicts_with` conflict (exit 2). No precedence guessing.
- `ocx run` is hermetic: without `--global`, reads only the in-effect project file; global file never consulted.
- `ocx run --global -- cmd` composes global toolchain env for child process only; never mutates parent shell.
- `OCX_GLOBAL` is the env-var equivalent (resolution-affecting; forwarded to child ocx via `apply_ocx_config`).
- No implicit `$OCX_HOME/ocx.toml` discovery: project resolution is explicit `--project`/`OCX_PROJECT` → CWD walk → None.
- `ocx package install --global` → clap unknown-flag error (exit 2). `--global` is NOT on `ocx package install`.

Activation (new model): the OCX install script writes `$OCX_HOME/env.sh` containing `eval "$(ocx env --global --shell=sh)"` and appends a block-marker idempotent line to the user's login profile. No `$OCX_HOME/init.<shell>` static files. No `ocx shell hook`/`shell init`.

ADR: `adr_global_toolchain_tier.md`.

## Cross-Cutting: OCX Configuration Forwarding

Any code that spawns a subprocess MUST call `env::Env::apply_ocx_config(ctx.config_view())` after building the child env and before `Command::envs()`. Resolution-affecting `ContextOptions` fields MUST appear in `OcxConfigView`, in `Env::apply_ocx_config`, and in `website/src/docs/reference/environment.md`. Presentation fields (log-level / format / color) MUST NOT propagate via env.

## Quality Gate

During review-fix loop, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.
