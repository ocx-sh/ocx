---
paths:
  - crates/ocx_cli/src/api/**
  - crates/ocx_cli/src/command/**
---

# CLI API Data Layer Patterns

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16; §3 format decision **amended 2026-05-19**). Output format is a **context-only concern**: every command — including `ocx env` and `ocx package env` — reports through the shared context `Api` whose format is `ContextOptions.format` with `.unwrap_or(Format::Plain)` applied at the single `Context::try_init` call site. No subcommand declares its own `--format` or builds its own `Api`. There is no env-specific JSON default (the former `None → Json` divergence was removed).

Standards for `ocx_cli` API reporting layer (`api/data/`, `api.rs`). Rules ensure consistent output format across all commands.

## Data Type Structure

Every file in `api/data/` follow this structure:

1. **Doc comments** on all public types — describe purpose, plain format, JSON format:
   ```rust
   /// Short description of what this represents.
   ///
   /// Plain format: N-column table (Col1 | Col2 | Col3).
   ///
   /// JSON format: shape description (array of objects, keyed object, etc.).
   ```
2. **`new()` constructor** (or named constructors for polymorphic types)
3. **`Printable` impl** with single `print_table` call — no conditional empty-checks, no multiple tables
4. **Static `&str` headers** in `print_table` — never `format!()` for dynamic headers; add data columns instead

Reference impls: `api/data/paths.rs`, `api/data/env.rs`.

## Output architecture — three structs, two streams (Block-tier)

OCX is a **backend tool** (`product-context.md`: automation-first). Output is split across three `ocx_lib::cli` structs.

| Struct | Stream | Role |
|--------|--------|------|
| **`Printer`** | — | Bare write primitive: `out`/`out_line`/`err`/`err_line` |
| **`DataInterface`** | stdout | Machine data only: `print_json` / `print_table` / `print_tree` / `print_hint` / `print_steps` |
| **`UserInterface`** | stderr | Human diagnostics + interactive input |

`Context` exposes `data()` (→ `Api::report` for `Printable`) and `ui()`.

### Channel rules

- **Data → stdout, `DataInterface` only.** `--format json` = one JSON document; `--format plain` = TSV/aligned table, or nothing for action commands with no payload. Never interleave human text on stdout.
- **Diagnostics → `UserInterface` (`status` / `status_break` / `warn` / `success`).** Environment-adaptive routing decided once inside `UserInterface`, never at call site.
- **Errors → typed error, never `eprintln!`.** Return typed error; `main.rs` is the single boundary.
- **Interactive input → `UserInterface::prompt_line` / `prompt_secret`.** Both return `Err(Unsupported)` when non-interactive.

**Known exception:** `direnv export` emits `# ocx: …` shell-comment lines into the shell-eval stream by design. Keep the `# ocx:` prefix so they are inert when sourced.

**Known exception:** `emit_lines` in `conventions.rs` emits eval-safe shell-export lines to stdout and `# ocx:` prefixed diagnostics to stderr using raw `println!` / `eprintln!`. This is deliberate: `emit_lines` produces the eval-safe channel consumed by `ocx env --shell`, `ocx package env --shell`, and `ocx direnv export`; routing through `DataInterface` would break the eval-safe contract (DataInterface is not guaranteed to produce sourceable output). Same class as the `direnv export` exception.

**Deleted exception:** `shell hook` / `shell env` emit paths are deleted. `ocx env --shell[=NAME]` and `ocx package env --shell[=NAME]` use the shared `emit_lines(shell, &[Entry])` helper in `conventions.rs` — the single emit path for all eval-safe shell output.

**Known exception:** `export_ci` in `conventions.rs` (the `--ci[=PROVIDER]` flag on `ocx env` / `ocx package env`) writes outside the `Api` entirely — through `ocx_lib::ci::CiFlavor::export`, which appends to the CI runner's files (`$GITHUB_ENV`/`$GITHUB_PATH`) or writes JSON-lines to `--export-file` / stdout. Sibling to the `emit_lines` exception: the destination is a CI persistence channel with a provider-defined wire format, not a `DataInterface` table/JSON document. The library half already does file I/O (the GitHub flavor); the GitLab writer is injectable so tests never touch real stdout/disk. ADR `adr_ci_env_export_flag.md`.

## `emit_lines` helper

`conventions.rs` contains the shared `emit_lines(shell: Shell, entries: &[Entry])` helper consumed by:
- `ocx env` (toolchain-tier)
- `ocx package env` (OCI-tier)
- `ocx direnv export` (delegates to `emit_lines`; stateless, behaviour unchanged)

`emit_lines` wraps `ocx_lib::shell::Shell::export_path` / `export_constant`. Do not inline the emit loop — delegate.

## `ContextOptions.format` = `Option<Format>` (single format authority)

`ContextOptions.format` is `Option<options::Format>`. `Api::new` applies `.unwrap_or(Format::Plain)` at the single `Context::try_init` call site — the only place a format default is decided:
- `None` → `Format::Plain` for **all** commands, with no exceptions (handshake §3 amended 2026-05-19)
- Explicit root `--format json` → `Some(Format::Json)`
- `ocx env` / `ocx package env` carry **no subcommand `--format`** and build **no local `Api`** — they report through `context.api()` like every command. The former env-specific `None → Json` divergence was removed.
- `--shell[=NAME]` is orthogonal: the only eval-safe form, unaffected by `--format`

## Semantic intent, not display attributes (Block-tier)

Callers declare **what a line means**, never **how it looks**. Raw ANSI or manual `if color {…}` **must not** appear in `command/*.rs` or `Printable` impls.

- Right: `context.ui().success("Login succeeded")` — intent.
- Wrong: `println!("{}", console::style("Login succeeded").green().bold())`.

**Known exception:** `command/about.rs::print_logo` — inline styling tolerated there, not copied elsewhere.

## Single-Table Rule

Each `Printable::print_plain()` impl produce exactly one table. Multiple dimensions → encode as columns, not separate tables.

## Report Actual Results

Commands report what happened, not echo input.

- **Task return values drive report.** Task can be no-op → return type must encode this.
- **Never build report data from `self.packages` (CLI args) alone.**
- **Preserve input order.** `_all` methods return results in same order as input slice.

## Typed Enums Over Strings

Status values, category tags, bounded sets = enums with `Display` and `Serialize` impls.

## JSON Serialization

- Types wrapping `Vec<Entry>` implement custom `Serialize` to flatten to inner array.
- Types using `HashMap` with `#[serde(flatten)]` produce top-level keyed objects.
- Polymorphic types use `#[serde(untagged)]` to produce different JSON shapes per variant.

## Adding a New Report Type

1. Create `api/data/{name}.rs` with struct + doc comments + `Printable` impl
2. Add `pub mod {name};` to `api/data.rs`
3. Add `report_{name}()` method to `Api` in `api.rs` (delegates to `self.report()`)
4. Call from `command/{name}.rs` with data built from task results

## Project-Tier Commands: `Run` Pattern

`command/run.rs` follows the same `struct + execute(&self, context)` pattern but diverges from the `Printable` / `api/data/` path: it never calls `context.api().report()` because execution diverges via `child_process::exec`. No structured output to emit from parent.

## `ocx env` / `ocx package env` Command Pattern

`command/toolchain_env.rs` is the toolchain-tier composed-env command (`ocx [--global] env`); `command/env.rs` is the OCI-tier per-package one (`ocx package env`). Both:
1. Resolve entries (toolchain: `load_project_with_lock` / global resolver → `compose_tool_set` / `resolve_env`; package: `find_or_install_all` → `resolve_env`)
2. Resolve `--ci` early via `resolve_ci_arg` (bare-flag autodetect failure surfaces before the slow entry resolution); `--ci` ⟂ `--shell` (clap `conflicts_with`)
3. For `--ci=<provider>` output: call `export_ci(provider, export_file, &entries)` — CI sink, persists for later steps (sibling emit path to `emit_lines`, outside the `Api` — see "Known exception" above)
4. For `--shell[=NAME]` output: call `emit_lines(shell, &entries)` — eval-safe, the only sourceable form
5. For default (no `--ci`/`--shell`): call `context.api().report(&env_data)` — format is the context concern (root `--format`, default plain). **No subcommand `--format`, no local `Api`, no JSON default.**

`toolchain_env.rs` never reads `ocx.toml` directly — it goes through the project resolution path via `Context`.

The `app/project_context.rs` module provides `load_project_with_lock` — the shared prologue consumed by `pull.rs`, `run.rs`, and `env.rs`.
