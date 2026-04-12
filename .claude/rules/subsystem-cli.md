---
paths:
  - crates/ocx_cli/src/**
---

# CLI Subsystem

Thin CLI shell using clap at `crates/ocx_cli/src/`. One file per subcommand, output formatting via `Printable` trait.

## Design Rationale

The CLI is intentionally thin — all business logic lives in `ocx_lib` so it can be reused by other consumers (mirror tool, future SDK). Single `Context` struct with lazy init avoids constructing unused clients/indices. The `Printable` trait gives each report type ownership of its own formatting (plain + JSON), enforcing the single-table rule without a central formatter. See `arch-principles.md` for the full pattern catalog.

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
| `command/*.rs` | One file per subcommand (30 files) |
| `api.rs` | `Api` facade: dispatches JSON vs plain text |
| `api/data/*.rs` | Report data types implementing `Printable` |
| `options/*.rs` | Shared arg parsing helpers (Identifier, ContentPath, Platform) |

## Context Struct

Created once per command invocation via `Context::try_init(options, color_config)`:

```rust
pub struct Context {
    offline: bool,
    remote_client: Option<oci::Client>,
    remote_index: Option<RemoteIndex>,
    local_index: LocalIndex,
    file_structure: FileStructure,
    api: Api,
    default_index: Index,       // Local (default) or Remote (--remote flag)
    manager: PackageManager,
}
```

Accessors: `manager()`, `api()`, `file_structure()`, `default_index()`, `local_index()`, `remote_client()`.

## Command Pattern

Every command follows the same flow:

1. **Transform identifiers** — `options::Identifier::transform_all(packages, default_registry)`
2. **Call manager task** — `context.manager().task_all(identifiers, ...)`
3. **Build report data** — from task return values (never from CLI args alone)
4. **Report** — `context.api().report(&data_type)?`

## API Reporting Layer

### Printable Trait

```rust
pub trait Printable: serde::Serialize {
    fn print_plain(&self, printer: &Printer);
    fn print_json(&self, printer: &Printer) -> anyhow::Result<()> { ... }
}
```

### Rules

- **Single table**: Each `print_plain()` produces exactly one `print_table()` call
- **Static headers**: Use `&str` array, never `format!()` for dynamic headers
- **Typed enums**: Status values are enums with `Display` + `Serialize`, not raw strings
- **Report actual results**: Build data from task return values, not echoed CLI args
- **Preserve input order**: Zip task results with original identifiers for reporting

### Adding a New Report Type

1. Create `api/data/{name}.rs` with struct + doc comments + `Printable` impl
2. Add `pub mod {name};` to `api/data.rs`
3. Add `report_{name}()` method to `Api` (delegates to `self.report()`)
4. Call from `command/{name}.rs` with data built from task results

See `subsystem-cli-api.md` for the full contract and `subsystem-cli-commands.md` for the quick reference. User-facing per-command docs live at `website/src/docs/reference/command-line.md`.

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` is the final gate before commit.
