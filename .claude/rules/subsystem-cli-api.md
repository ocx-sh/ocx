---
paths:
  - crates/ocx_cli/src/api/**
  - crates/ocx_cli/src/command/**
---

# CLI API Data Layer Patterns

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
2. **`new()` constructor** (or named constructors for polymorphic types like `without_tags` / `with_tags`)
3. **`Printable` impl** with single `print_table` call — no conditional empty-checks, no multiple tables
4. **Static `&str` headers** in `print_table` — never `format!()` for dynamic headers; add data columns instead

Reference impls: `api/data/paths.rs`, `api/data/env.rs`.

## Output architecture — three structs, two streams (Block-tier)

OCX is a **backend tool** (`product-context.md`: automation-first, "not user-facing PM"). Output is split across three `ocx_lib::cli` structs. Pick by *what the bytes are*, never by convenience.

| Struct | Stream | Role | Knows |
|--------|--------|------|-------|
| **`Printer`** | — | Bare write primitive: `out`/`out_line`/`err`/`err_line`. Nothing else. | nothing |
| **`DataInterface`** | stdout | Machine data only: `print_json` / `print_table` / `print_tree` / `print_hint` / `print_steps`. Owns stdout color. Builds the (maybe styled) string, hands bytes to `Printer`. | `--format`, stdout color |
| **`UserInterface`** | stderr | Human diagnostics + interactive input. Owns stderr color, `interactive` (TTY), `quiet`. | quiet, TTY, stderr color |

`Context` exposes `data()` (→ `Api::report` for `Printable`) and `ui()`. `Api` wraps `DataInterface` for the report path; it is not a styling layer.

### Channel rules

- **Data → stdout, `DataInterface` only.** `--format json` = one JSON document; `--format plain` = TSV/aligned table, or **nothing** for action commands with no payload (`login`, `select`, `add`). Never interleave human text on stdout — `ocx … | jq` must not break. `Printable::print_plain` being a no-op for an action result is correct (see `api/data/login.rs`).
- **Diagnostics → `UserInterface` (`status` / `status_break` / `warn` / `success`).** Environment-adaptive routing, decided once inside `UserInterface`, never at the call site:

  | Environment | Behavior |
  |---|---|
  | `--quiet` OR non-TTY (CI/pipe) | route to `log::info!` / `log::warn!` (level-gated by `-l/--log-level`, default `info`) |
  | interactive TTY & not quiet | styled line on stderr (color per stderr setting) |

  So CI/non-interactive runs funnel diagnostics into the single `log` channel with its existing level control; decorated UI only when a human is watching.
- **Errors → typed error, never `eprintln!`.** Return a typed error; `main.rs` is the **single** boundary — `log::error!("{err:#}")` then `app::classify_error` derives the exit code from `ClassifyExitCode`. Hand-mapping (`eprintln!(msg); return Ok(ExitCode::X.into())`) is forbidden in `command/*.rs`: it skips the log layer and creates a second exit-code source that drifts.

  ```rust
  // WRONG                                  // RIGHT
  eprintln!("no ocx.toml; run `ocx init`"); return Err(UsageError::new(
  return Ok(cli::ExitCode::UsageError.into());   "no ocx.toml found … run `ocx init`").into());
  ```
- **Interactive input → `UserInterface::prompt_line` / `prompt_secret`.** Both return `Err(Unsupported)` when non-interactive; the command maps that to the usual `UsageError` (e.g. login → "requires --password-stdin"). No ad-hoc `is_terminal()` checks scattered in commands.

**Known exception:** `shell hook` / `direnv export` / `shell env` emit `# ocx: …` shell-comment lines into the shell-eval stream by design (consumer = the shell, not a human). Keep the `# ocx:` prefix so they are inert when sourced.

## Semantic intent, not display attributes (Block-tier)

Callers declare **what a line means**, never **how it looks**. `status` / `warn` / `success` are intents; `green().bold()` is an attribute. `console::style(...)`, `Style::new()…apply_to`, raw ANSI, or manual `if color {…}` **must not** appear in `command/*.rs` or `Printable` impls. `DataInterface` / `UserInterface` are the sole owners of the color/stream/route decision.

- Right: `context.ui().success("Login succeeded")` — intent; `UserInterface` decides style/stream/route.
- Wrong: `println!("{}", console::style("Login succeeded").green().bold())` — caller hard-codes presentation and stream.

New visual concept → add a named intent method + `STYLE_*` const inside `data_interface.rs` or `user_interface.rs` (whichever stream owns it). The vocabulary is small and semantic on purpose. Sole place a raw `console::Style` legitimately crosses a call site: per-tree-node `Annotation::with_style(...)`, which `DataInterface`'s tree renderer applies/strips (see `api/data/deps.rs`).

**Known exception:** `command/about.rs::print_logo` renders bespoke logo+sidebar art with manual term layout — inline styling tolerated there, not copied elsewhere; do not generalize a struct API for it (YAGNI).

## Single-Table Rule

Each `Printable::print_plain()` impl produce exactly one table. Multiple dimensions (e.g., type + path, status + content) → encode as columns, not separate tables with dynamic headers.

**Wrong:**
```rust
if !self.objects.is_empty() {
    let header = format!("Object{}", suffix);
    print_table(&[&header], &rows);
}
if !self.temp.is_empty() {
    print_table(&[&format!("Temp{}", suffix)], &rows);
}
```

**Right:**
```rust
print_table(&["Type", "Dry Run", "Path"], &rows);
```

## Report Actual Results

Commands report what happened, not echo input. Task methods return enough data for command to build accurate output.

- **Task return values drive report.** Task can be no-op (resource already absent) → return type must encode this (e.g., `Option<PathBuf>` where `None` = no-op).
- **Never build report data from `self.packages` (CLI args) alone.** Use task return value for status.
- **Preserve input order.** `_all` methods return results in same order as input `packages` slice, so caller zip with original identifiers.

## Typed Enums Over Strings

Status values, category tags, bounded sets = enums with `Display` and `Serialize` impls — never raw `String` fields.

```rust
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RemovedStatus {
    Removed,
    Absent,
}
```

## JSON Serialization

- Types wrapping `Vec<Entry>` implement custom `Serialize` to flatten to inner array (no wrapper object).
- Types using `HashMap` with `#[serde(flatten)]` produce top-level keyed objects — correct pattern for package-keyed results.
- Polymorphic types use `#[serde(untagged)]` to produce different JSON shapes per variant.

## Adding a New Report Type

1. Create `api/data/{name}.rs` with struct + doc comments + `Printable` impl
2. Add `pub mod {name};` to `api/data.rs`
3. Add `report_{name}()` method to `Api` in `api.rs` (delegates to `self.report()`)
4. Call from `command/{name}.rs` with data built from task results

## Project-Tier Commands: `Run` Pattern

`command/run.rs` follows the same `struct + execute(&self, context)` pattern as every other command but diverges from the `Printable` / `api/data/` path: it never calls `context.api().report()` because execution diverges via `child_process::exec` (the child replaces the process on Unix, or is waited synchronously on Windows). There is no structured output to emit from the parent.

Consequently, `RunFilterError` (the private error type from `filter_by_names`) stays inside `command/run.rs` and is never placed in `api/data/`. It carries CLI-shaped wording (binding names, group names) and exits 64 before any spawn — no `Printable` payload is involved.

The `Run` struct public surface:

```rust
// crates/ocx_cli/src/command/run.rs
#[derive(Parser, Clone)]
pub struct Run {
    pub groups: Vec<String>,      // -g / --group, comma-and-repeatable
    pub clean: bool,              // --clean
    pub self_view: bool,          // --self
    pub names: Vec<String>,       // binding-name filter (0.., value_terminator = "--")
    pub argv: Vec<String>,        // child argv (1.., last = true, allow_hyphen_values)
}

impl Run {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode>;
}
```

The `app/project_context.rs` module provides `load_project_with_lock` — the shared prologue consumed by both `pull.rs` and `run.rs`. See `subsystem-cli-commands.md` Semantics & Gotchas for the full `run` contract.