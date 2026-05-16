---
paths:
  - crates/ocx_cli/src/api/**
  - crates/ocx_cli/src/command/**
---

# CLI API Data Layer Patterns

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16). The new `ocx env` and `ocx package env` commands default to **JSON** output (backend-first). All legacy commands continue to default to **plain**. The mechanism is `ContextOptions.format: Option<Format>` + `Api::new` applying `.unwrap_or(Format::Plain)`.

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

**Deleted exception:** `shell hook` / `shell env` emit paths are deleted. `ocx env --shell[=NAME]` and `ocx package env --shell[=NAME]` use the shared `emit_lines(shell, &[Entry])` helper in `conventions.rs` — the single emit path for all eval-safe shell output.

## `emit_lines` helper

`conventions.rs` contains the shared `emit_lines(shell: Shell, entries: &[Entry])` helper consumed by:
- `ocx env` (toolchain-tier)
- `ocx package env` (OCI-tier)
- `ocx direnv export` (delegates to `emit_lines`; stateless, behaviour unchanged)

`emit_lines` wraps `ocx_lib::shell::Shell::export_path` / `export_constant`. Do not inline the emit loop — delegate.

## `ContextOptions.format` = `Option<Format>` (precondition for env commands)

`ContextOptions.format` is `Option<options::Format>`. `Api::new` applies `.unwrap_or(Format::Plain)` at the single call site:
- `None` → `Format::Plain` for all legacy commands (no behaviour change)
- `ocx env` and `ocx package env` resolve `None → Format::Json` internally (backend-first)
- Explicit `--format plain` → `Some(Format::Plain)` → human inspection (NOT sourceable)

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

## `ocx env` Command Pattern

`command/env.rs` is the new toolchain-tier composed-env command. It:
1. Calls `load_project_with_lock` (from `app/project_context.rs`) or the global resolver
2. Calls `compose_tool_set` / `resolve_env` → `composer::compose`
3. For `--shell[=NAME]` output: calls `emit_lines(shell, &entries)` — eval-safe
4. For default (no `--shell`): calls `context.api().report(&env_data)` → **JSON** (resolves `None → Json`)
5. For `--format plain`: calls `context.api().report(&env_data)` → plain human inspection (NOT sourceable)

`env.rs` never reads `ocx.toml` directly — it goes through the project resolution path via `Context`.

The `app/project_context.rs` module provides `load_project_with_lock` — the shared prologue consumed by `pull.rs`, `run.rs`, and `env.rs`.
