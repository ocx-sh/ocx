---
paths:
  - crates/ocx_cli/src/**
---

# CLI Subsystem

Thin CLI shell. Use clap at `crates/ocx_cli/src/`. One file per subcommand. Output format via `Printable` trait.

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16). The command taxonomy below reflects that signed handshake. Any description of `ocx shell hook`, `ocx shell init`, `ocx shell env`, root `ocx install/uninstall/select/exec/deselect/which/deps`, or `ocx ci` (the **command**) describes the **deleted** model — do not implement it. The CI-export capability is NOT a command: it is the `--ci[=provider]` flag on `ocx env` / `ocx package env` (handshake §6, ADR `adr_ci_env_export_flag.md`).

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
- `ocx package env <ids...> [--shell[=NAME]] [--ci[=PROVIDER]] [--export-file PATH]` — composed env for the named packages (reuses `env.rs`); `--ci` writes to a CI sink (see below)
- `ocx package which <ids...>` — resolve installed packages to paths (`--candidate`/`--current` for stable symlink anchor)
- `ocx package deps <ids...>` — show dependency tree/flat/why for installed packages (`--flat`/`--why`/`--depth`/`--self`)

### Toolchain-tier — root commands
Operate on `ocx.toml` (CWD-walk / `--project` / `OCX_PROJECT`) or `$OCX_HOME/ocx.toml` under `--global`.
`--global` is a **root flag** (before the subcommand), peer of `--project`, defined once on `ContextOptions`.
Canonical form: `ocx --global <subcommand>`.
- `ocx [--global] add <id>`, `ocx [--global] remove <name>`, `ocx [--global] lock`, `ocx [--global] upgrade`
- `ocx [--global] run -- cmd` — compose toolchain env for child process only; never mutates parent shell
- `ocx [--global] env [--shell[=NAME]] [--ci[=PROVIDER]] [--export-file PATH]` — compose toolchain env. Output format is a **context-only concern** (root `--format`, default **plain** like every command — no subcommand `--format`, handshake §3 amended 2026-05-19); `--shell[=NAME]` is the ONLY eval-safe channel; `--ci` writes to a CI sink (see below)

### `ocx shell` — reduced to one survivor
- `ocx shell completion <name>` — **keep** (genuinely shell-scoped, static)
- `ocx shell hook`, `ocx shell init`, `ocx shell env` — **DELETED** (handshake §7)

### `ocx self` — installation management group
- `ocx self setup [VERSION] [--no-modify-path] [--profile PATH]... [--dry-run] [--force]` — complete a bare-binary install: bootstrap the specified or latest published ocx into the content store, write per-shell env shims (`$OCX_HOME/env.*`) and add a managed activation block to detected shell profiles. Re-running is safe (shims and blocks are diff-gated). A dirty managed block (user edits inside the fence) is skipped with exit 82; `--force` rewrites it. `--no-modify-path` (or `OCX_NO_MODIFY_PATH` truthy) writes shims only, skipping profiles. The opt-out is not remembered between runs. **`VERSION` grammar** (optional positional): `1.2.3` (tag only), `sha256:<64hex>` (digest only, written bare without `@` — no tag resolution), `1.2.3@sha256:<64hex>` (tag+digest — immutability assertion, fails closed exit 65 on mismatch; under `--frozen` uses local-index digest and hints `ocx index update` on mismatch). Malformed `VERSION` exits 64. Pin satisfied ⟺ `current` already points at the pinned digest — no re-download. Resolved digest surfaces in JSON output (`bootstrap.digest`; omitted when unpinned). ChainMode applies: `--frozen`/`--offline` + uncached tag → exit 81; digest-only pin works frozen when blobs cached.
- `ocx self activate [--shell[=NAME]] [--completion | --no-completion]` — emit eval-safe PATH prepend + completion injection + global env eval. Absent or bare `--shell` → autodetect; explicit `--shell=bash` → named shell. Completions are emitted **inline** in the activation stream (no file): the completion block leads the stream so PowerShell's `using namespace` is the first statement, which `Invoke-Expression` requires (verified on WinPS 5.1 + pwsh 7); zsh self-loads `compinit` before clap's trailing `compdef`. Gated by `options::Completion` (`options/completion.rs`): paired `--completion`/`--no-completion` flags (`overrides_with`, last-wins) → `OCX_NO_COMPLETIONS` → stderr-TTY auto fallback, resolved by `Completion::enabled(interactive)`. The `env.sh`/`env.ps1` shim decides interactivity itself (`$-`/`status is-interactive`/`[Environment]::UserInteractive`) and passes the explicit flag, so the gate never depends on probing a stderr the shim has redirected; the auto path serves only a direct in-terminal `ocx self activate`. The shim also picks the real sourcing shell (`bash`/`zsh`, not `sh` → `Dash` which has no completion backend) so the right completion backend is selected. `OCX_NO_COMPLETIONS=1` suppresses completion injection. Intended to be called from `$OCX_HOME/env.sh` at shell startup.
- `ocx self update` — check + install latest version; always bypasses throttle (explicit intent).
- `ocx self update --check` — query only, no install; always bypasses throttle.

`Self_` is in the auto-check **skip list** (`app.rs` `should_check_for_update`): `self activate` runs on every shell startup and must not trigger the background update-check. The skip applies to all `Self_` variants — including `self setup`.

**`OCX_NO_MODIFY_PATH` boolean semantics.** Read through `ocx_lib::env::flag` + `BooleanString`. Truthy values: `1`, `y`, `yes`, `on`, `true` (case-insensitive). Falsy values: `0`, `n`, `no`, `off`, `false`. Any other non-empty value emits a `WARN` log and falls back to the default (`false`). Unset → default. "Any non-empty string = true" is NOT the contract.

**`ocx self setup` 4C refresh (after `self update`).** After a successful binary swap, `refresh_shell_integration_after_swap` does two best-effort steps: (1) `setup::shims::refresh_shims` rewrites drifted `$OCX_HOME/env.*` shims; (2) `setup::refresh_profiles` re-applies the managed RC block in **heal-only** mode (`apply_target(force=false, heal_only=true)`). Heal-only never *introduces* a block (Fresh + no legacy → `NoOp`, so a `--no-modify-path` install stays untouched) and never clobbers a user-edited block (`SkippedDirty`); it only heals a drifted ocx-authored body (`FormatUpgraded`/`Migrated`). Advisories: a shim drift OR a healed block → "run `ocx self setup`" / "re-source your profile"; a `SkippedDirty` block → "run `ocx self setup --force`"; any refresh failure warns + advises. All non-fatal to the update. Drift is detected by the diff-gate / RC state machine, not a literal `SHIM_CONTRACT_VERSION` comparison. **Timing:** the refresh runs in the *old* binary, so a brand-new block body heals on the *next* update (or a `self setup` re-run), not the hop that introduces it.

**Exit-82 (`DirtyRcBlock`).** `ocx self setup` exits 82 when any shell profile contains a managed activation block that carries user edits inside the fence and `--force` was not passed. Scripts can `case $? in 82)` to detect this and re-run with `--force`.

### Removed root commands (handshake §7 — exit 64 if invoked)
- `ocx install`, `ocx uninstall`, `ocx select`, `ocx exec`, `ocx deselect`, `ocx which`, `ocx deps` → moved to `ocx package`; ocx maps clap usage errors → EX_USAGE 64 (see `app.rs:112-119`)
- `ocx ci` → removed (the **command**). The CI-export capability returns as the `--ci[=PROVIDER]` flag on `ocx env` / `ocx package env` — see "Cross-Cutting: CI Env Export (`--ci`)" below.

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

## Cross-Cutting: CI Env Export (`--ci`)

`--ci[=PROVIDER]` is a third emit branch on `ocx env` and `ocx package env`,
beside `--shell` and the structured report. It writes the composed env into a
CI system's persistence channel so tool dirs/vars reach **later** pipeline
steps (`--shell` only affects the current step). Realizes handshake §6; ADR
`adr_ci_env_export_flag.md`. Library half lives in `ocx_lib::ci` (`CiFlavor`,
`Flavor`, `github_flavor.rs`, `gitlab_flavor.rs`); the CLI wiring is two shared
helpers in `conventions.rs` (`resolve_ci_arg`, `export_ci`).

- Declared `Option<Option<CiFlavor>>` with `num_args=0..=1, require_equals=true,
  conflicts_with = "shell"` (same grammar as `--shell`). Bare `--ci` autodetects
  from `$GITHUB_ACTIONS` / `$GITLAB_CI`; usage error (exit 64) if none detected.
- Values: `github` (alias `github-actions`), `gitlab` (alias `gitlab-ci`).
- `--ci` ⟂ `--shell` (clap `conflicts_with`, exit 64). At most one applies.
- `--export-file PATH` is `requires = "ci"` (exit 64 without it). GitLab-only:
  rejected for `--ci=github` (exit 64) in `export_ci`.
- **GitHub** (`--ci=github`): inferred two-file sink — appends to `$GITHUB_PATH`
  (path entries) and `$GITHUB_ENV` (everything else). An explicit `--ci=github`
  outside GitHub Actions fails `from_env()` with exit 78 (`ConfigError`) — the
  global-lenient contract covers *resolution* failures only, not an explicit
  misconfigured channel.
- **GitLab** (`--ci=gitlab`): JSON-lines `{"name","value"}`, one per key, to
  `--export-file` or stdout. No path channel: `PATH` and every path var are
  flattened (prepend package values to existing, join with `PATH_SEPARATOR`).

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

Activation (new model): `ocx self setup` writes `$OCX_HOME/env.sh` (and per-family siblings for fish/nu/elvish/PowerShell) and appends a managed activation block to the user's login profile. The OCX install script bootstraps the binary and then delegates to `ocx self setup`; it no longer contains the profile-modification logic itself. No `$OCX_HOME/init.<shell>` static files. No `ocx shell hook`/`shell init`.

ADR: `adr_global_toolchain_tier.md`.

## Cross-Cutting: Plugin Dispatch

The `External(Vec<OsString>)` variant on `Command` routes unknown subcommand names to `ocx-<name>` binaries discovered on `$PATH`, matching the cargo / git / kubectl plugin pattern. Dispatch fires from `App::run` before `Context::try_init`, so plugins bypass `FileStructure` / OCI client init. Resolution-affecting env is forwarded via `Env::apply_ocx_config`; plugins inherit the full parent env (trust boundary, see module doc). Built-ins always shadow plugins — clap matches built-in variants first; `External` only fires for unrecognized names. Plugin not-found exits 64 (`EX_USAGE`) with a stderr install hint. Implementation: `app/plugin_dispatch.rs`. ADR: `adr_cli_plugin_pattern.md`.

## Cross-Cutting: OCX Configuration Forwarding

Any code that spawns a subprocess MUST call `env::Env::apply_ocx_config(ctx.config_view())` after building the child env and before `Command::envs()`. Resolution-affecting `ContextOptions` fields MUST appear in `OcxConfigView`, in `Env::apply_ocx_config`, and in `website/src/docs/reference/environment.md`. Presentation fields (log-level / format / color) MUST NOT propagate via env.

The forwarding set includes `OCX_BINARY_PIN`, `OCX_HOME`, `OCX_INDEX`, `OCX_CONFIG`, `OCX_NO_CONFIG`, `OCX_PROJECT`, `OCX_NO_PROJECT`, `OCX_GLOBAL`, `OCX_OFFLINE`, `OCX_REMOTE`, `OCX_FROZEN`, `OCX_MIRRORS`, and the three P1 store-zone vars: `OCX_CACHE_DIR`, `OCX_PACKAGES_DIR`, `OCX_STATE_DIR`. All appear in `env::keys` consts, `OcxConfigView` fields, `Env::apply_ocx_config`, `ContextOptions::as_view`, and `environment.md`.

## Quality Gate

During review-fix loop, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.
