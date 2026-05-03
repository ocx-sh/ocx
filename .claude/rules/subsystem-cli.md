---
paths:
  - crates/ocx_cli/src/**
---

# CLI Subsystem

Thin CLI shell. Use clap at `crates/ocx_cli/src/`. One file per subcommand. Output format via `Printable` trait.

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
| `command/*.rs` | One file per subcommand (30 files) |
| `api.rs` | `Api` facade: dispatches JSON vs plain text |
| `api/data/*.rs` | Report data types implementing `Printable` |
| `options/*.rs` | Shared arg parsing helpers (Identifier, ContentPath, Platform, PackageRef + `validate_package_root`) |

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
    default_index: Index,       // Local (default) or Remote (--remote flag)
    manager: PackageManager,
}
```

Accessors: `manager()`, `api()`, `file_structure()`, `default_index()`, `local_index()`, `remote_client()`.

## Command Pattern

Every command same flow:

1. **Transform identifiers** — `options::Identifier::transform_all(packages, default_registry)`
2. **Call manager task** — `context.manager().task_all(identifiers, ...)`
3. **Build report data** — from task return values (never from CLI args alone)
4. **Report** — `context.api().report(&data_type)?`

## Routing intent

Commands that hit `Index::fetch_manifest{,_digest}` / `Index::select` / `Index::fetch_candidates` must declare caller intent via `IndexOperation::{Query, Resolve}`. Pure-read commands (`index list`, `index catalog`, `package info`) pass `Op::Query` so a cache miss returns `None` instead of triggering a chain walk + write-through. Install/pull paths in `package_manager::tasks::resolve` pass `Op::Resolve`. Misuse silently leaks writes through query paths — the structural test `chain_refs_tests::op_query_never_walks_source_in_any_mode` catches the most common regression mode. Full contract → [`subsystem-oci.md`](./subsystem-oci.md) and [`adr_index_routing_semantics.md`](../artifacts/adr_index_routing_semantics.md).

## API Reporting Layer

### Printable Trait

```rust
pub trait Printable: serde::Serialize {
    fn print_plain(&self, printer: &Printer);
    fn print_json(&self, printer: &Printer) -> anyhow::Result<()> { ... }
}
```

### Rules

- **Single table**: Each `print_plain()` make exactly one `print_table()` call
- **Static headers**: Use `&str` array, never `format!()` for dynamic headers
- **Typed enums**: Status values are enums with `Display` + `Serialize`, not raw strings
- **Report actual results**: Build data from task return values, not echoed CLI args
- **Preserve input order**: Zip task results with original identifiers for reporting

### Adding a New Report Type

1. Make `api/data/{name}.rs` with struct + doc comments + `Printable` impl
2. Add `pub mod {name};` to `api/data.rs`
3. Add `report_{name}()` method to `Api` (delegate to `self.report()`)
4. Call from `command/{name}.rs` with data built from task results

See `subsystem-cli-api.md` for full contract. `subsystem-cli-commands.md` for quick reference. User-facing per-command docs at `website/src/docs/reference/command-line.md`.

## Cross-Cutting: `--self` Flag

Six env-consuming commands (`exec`, `env`, `shell env`, `shell profile load`, `ci export`, `deps`) share a single boolean `--self` flag (default off) that selects which env surface `ocx exec` emits:

- **Off (default)** — interface surface: emits the consumer-visible env (vars where `has_interface()` is true). What a human or CI script sees when using a package.
- **On (`--self`)** — private surface: emits the package's own runtime env (vars where `has_private()` is true). The `ocx launcher exec` subcommand forces `self_view=true` internally so generated launchers do not need to bake any flag.

`ExecMode` and `ExecModeFlag` no longer exist. The lib accepts a plain `self_view: bool`. Pattern:

- Add `#[clap(long = "self", default_value_t = false)] self_view: bool` to the command's clap `Args` struct.
- Pass `self_view` to the manager task: `resolve_env(packages, self_view)`.

The manager calls `composer::compose(roots, store, self_view)` which gates TC entry emission via `tc_entry.visibility.has_interface()` (false) or `has_private()` (true).

## Cross-Cutting: OCX Configuration Forwarding

Any code that spawns a subprocess MUST call `env::Env::apply_ocx_config(ctx.config_view())` after building the child env (whether `Env::new()` or `Env::clean()`) and before `Command::envs()`. `apply_ocx_config` is the **sole** path that lands `OCX_*` keys on a child env — the running ocx's parsed config is authoritative, never ambient parent-shell exports.

New resolution-affecting `ContextOptions` fields (offline / remote / config / index / similar) MUST appear in `OcxConfigView`, in `Env::apply_ocx_config`, and in `website/src/docs/reference/environment.md`. Presentation fields (log-level / format / color) MUST NOT propagate via env — they leak into entrypoint child streams. PR review:
- Adding a resolution flag without all three updates → **Block-tier**
- Routing a presentation flag through `apply_ocx_config` → **Block-tier**

The `--interactive` flag on `ocx exec` was removed: stdin always inherits, matching shell exec semantics. Do not reintroduce a stdin-gating flag.

## Quality Gate

During review-fix loop, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.