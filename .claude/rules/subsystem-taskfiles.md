---
paths:
  - taskfile.yml
  - taskfiles/**/*.yml
  - "**/taskfile.yml"
---

# Taskfiles Subsystem

[Taskfile](https://taskfile.dev) v3 = primary task runner for OCX. Root taskfile includes per-subsystem taskfiles + reusable templates under `taskfiles/`. Each subsystem own pipeline, expose contract back to root.

## File Layout

| Path | Owner | Loaded as |
|---|---|---|
| `taskfile.yml` | root | entry point -- `task verify`, `task default`, etc. |
| `.taskrc.yml` | root | project config -- `failfast: true` |
| `taskfiles/rust.taskfile.yml` | Rust subsystem | `rust:` -- format, clippy, license, build, test:unit |
| `taskfiles/shell.taskfile.yml` | Shell subsystem | `shell:` -- verify (delegates to ocx-exec templates) |
| `taskfiles/ocx.taskfile.yml` | OCX CLI templates | included multiple times with different `vars:`; `.exec` wraps `ocx exec` |
| `taskfiles/coverage.taskfile.yml` | cross-cutting | `coverage:` |
| `taskfiles/duplo.taskfile.yml` | cross-cutting | `duplo:` |
| `taskfiles/release.taskfile.yml` | cross-cutting | `release:` |
| `taskfiles/mirror.taskfile.yml` | cross-cutting | `mirror:` |
| `.claude/taskfile.yml` | `.claude/` subsystem | `claude:` |
| `test/taskfile.yml` | acceptance tests | `test:` |
| `website/taskfile.yml` | website | `website:` (includes schema, sbom, catalog, recordings internally) |
| `website/schema.taskfile.yml` | website | `website:schema:` -- JSON schema generation |
| `website/sbom.taskfile.yml` | website | `website:sbom:` -- SBOM generation |
| `website/catalog.taskfile.yml` | website | `website:catalog:` -- catalog generation |
| `website/recordings.taskfile.yml` | website | `website:recordings:` -- terminal recordings |
| `mirror-sdk-py/taskfile.yml` | mirror SDK | `mirror-sdk:` |

## Two-Phase Verify

`task verify` run as two-phase pipeline via internal `.verify:lint` + `.verify:build-test` tasks:

1. **Phase 1 (parallel `deps:`)**: `rust:format:check`, `rust:clippy:check`, `shell:verify`, `claude:verify`
2. **Phase 2 (sequential `cmds:`)**: `rust:license:check`, `rust:license:deps`, `rust:build`, `rust:test:unit`, `test:parallel`

## AI Quality Gate Pattern

Each subsystem rule has Quality Gate section specifying which verify task to run during review-fix loops. Full `task verify` runs only as final gate before commit. Stop expensive acceptance tests running every iteration.

## Taskfile Features in Use

| Feature | Purpose |
|---|---|
| `output: group` + `error_only: true` | Clean verify output; only show failures |
| `.taskrc.yml` | Project-wide `failfast: true` (abort on first failure) |
| Include-with-vars templates | `ocx.taskfile.yml` included as `shellcheck:` and `shfmt:` with different vars; uses `requires:` for mandatory vars |
| `internal: true` on includes | Template includes hidden from `task --list` |
| `:` prefix cross-references | Child taskfiles call root helpers like `:.ensure-cargo-tool` |
| `--force` | Bypass all caching for one run (replaces `rm -rf .task/`) |
| `--status` | Check freshness without running |
| `--summary` | Show task detail (e.g., `task --summary verify`) |
| `aliases:` | Backward compat after renames |
| `for:` loops | Iteration in cmds/deps |

## OCX Conventions

1. **`verify` is a subsystem contract.** Every subsystem taskfile expose `verify` task. Root `verify` calls `<subsystem>:verify` instead of inlining subtasks.
2. **Composite tasks aggregate subtasks.** Each linter/formatter has own subtask so independently runnable. `verify` references composite tasks only -- one entry per concern.
3. **Helpers use dot-prefix + `internal: true`.** Internal tasks named with dot prefix (e.g., `.ensure-cargo-tool`, `.verify:lint`) -- like GitLab CI hidden jobs. Visually distinct + hidden from `task --list`.
4. **Tool installs use `status:` for idempotency.** `status: - cargo nextest --version` skips install when tool already present. Cross-include dedup pattern -- `run: once` does **not** work reliably across namespaced includes (go-task issue #852).
5. **Subsystem env overrides go on task, not include.** Root sets `OCX_INDEX` globally; subsystem tasks needing different value override with per-task `env:`.

## Caching Contract

### `sources:` + `status:` + `method:`

| Field | Purpose | Recommendation |
|---|---|---|
| `sources:` | Files that force re-run when changed | Broad include + targeted excludes. Survive new file types. |
| `status:` | Exit 0 = up-to-date, skip | **Load-bearing for cache.** Use for output file checks. |
| `generates:` | Output declaration (docs only) | **Not load-bearing** (go-task #2181). Declare for discoverability; add `status:` if skip-on-present needed. |
| `method:` | `checksum` (default), `timestamp`, `none` | Only specify `timestamp` for tools with own incremental build (e.g. `cargo build`). |

**Sources relative to task's `dir:`.** Set `dir: '{{.TASKFILE_DIR}}'` or rely on include's `dir:` so globs use plain `**/*`.

**Two canonical patterns:**

```yaml
# Pattern A -- skip when output exists, re-run when sources change
schema:generate:
  cmds: [cargo run -p ocx_schema --release -- metadata > {{.OUT}}]
  sources: [crates/ocx_schema/src/**/*.rs, Cargo.lock]
  status: [test -f {{.OUT}}]

# Pattern B -- wrap a tool with its own incremental build
build:
  cmd: cargo build --release -p ocx --locked
  sources: [crates/**/*.rs, Cargo.lock]
  status: [test target/release/ocx -nt Cargo.lock]
  method: timestamp
```

### Sharing source lists: `x-shared:` anchors

Multiple tasks validate same input set → define source list once as YAML anchor under `x-shared:` (JSON-Schema extension convention), reference with `*alias`. Do not anchor under consuming task -- implies ownership data structure does not have.

```yaml
x-shared:
  claude-sources: &claude-sources
    - '**/*'
    - '../CLAUDE.md'
    - exclude: 'artifacts/**'
    - exclude: '**/__pycache__/**'

tasks:
  tests:
    sources: *claude-sources
  lint:links:
    sources: *claude-sources
```

### `.task/` cache directory

Task stores fingerprint state in `.task/`. **Add to `.gitignore`** before introducing any `sources:`/`method:` task.

## Reports and Artifacts (`REPORTS_DIR`)

Tasks that validate or check should emit machine-readable reports.

- **Location**: `target/reports/<subsystem>/` (already gitignored under `target/`).
- **Root var**: `REPORTS_ROOT: '{{.ROOT_DIR}}/target/reports'` in root taskfile.
- **Subsystem var**: Each subsystem resolves own `REPORTS_DIR` with `{{if/else}}` so standalone invocation works:

  ```yaml
  vars:
    REPORTS_DIR: '{{if .REPORTS_ROOT}}{{.REPORTS_ROOT}}/claude{{else}}{{.TASKFILE_DIR}}/reports{{end}}'
  ```

  Parent + child vars must have **different names** (`REPORTS_ROOT` vs `REPORTS_DIR`) to avoid double-suffixing.
- **Format**: JUnit XML (preferred) or JSON. Create dir with `mkdir -p {{.REPORTS_DIR}}` as first command.
- **Declare `generates:`** for docs even though not load-bearing.

## Includes and `dir:` Scoping

| Variable | Resolves to |
|---|---|
| `{{.ROOT_DIR}}` | Directory of entry-point (root) taskfile |
| `{{.TASKFILE_DIR}}` | Directory of currently executing taskfile |
| `{{.USER_WORKING_DIR}}` | Directory from which `task` was invoked |

Set `dir:` on include block when all tasks should run relative to sub-taskfile's directory (`.claude/`, `test/`, `website/`). Override per-task only when needed.

**Gotchas:** `${{.TASKFILE_DIR}}` wrong (becomes shell var). When root sets global `env:`, included tasks inherit -- override per-task if needed.

## Preconditions vs Status

| Field | Use | Semantics |
|---|---|---|
| `preconditions:` + `msg:` | Guard -- abort if not ready | Failure aborts; downstream tasks don't run |
| `status:` | Cache -- skip if done | Exit 0 = up-to-date, skip silently |

## OCX-Specific Task Contracts

- **Generation tasks** (`schema:generate`, `recordings:*`, `catalog:generate`) should depend on compiled binary via `deps: [build]` + `sources: [target/release/ocx_schema]`, NOT on Rust source file lists. Cargo already tracks source deps — duplicating in Taskfile = maintenance overhead.

## Sources

- [Taskfile schema reference](https://taskfile.dev/docs/reference/schema)
- [Taskfile usage](https://taskfile.dev/usage/) -- sources/status/method, run modes, env, preconditions
- [Taskfile templating](https://taskfile.dev/docs/reference/templating) -- special vars
- [go-task #2181](https://github.com/go-task/task/issues/2181) -- `generates:` globs not fingerprinted
- [go-task #852](https://github.com/go-task/task/issues/852) -- `run: once` cross-include dedup broken