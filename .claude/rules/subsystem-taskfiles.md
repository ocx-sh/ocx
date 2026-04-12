---
paths:
  - taskfile.yml
  - taskfiles/**/*.yml
  - "**/taskfile.yml"
---

# Taskfiles Subsystem

[Taskfile](https://taskfile.dev) v3 is the primary task runner for OCX. The root taskfile includes per-subsystem taskfiles and reusable templates under `taskfiles/`. Each subsystem owns its own pipeline and exposes a contract back to the root.

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

`task verify` runs as a two-phase pipeline via internal `.verify:lint` and `.verify:build-test` tasks:

1. **Phase 1 (parallel `deps:`)**: `rust:format:check`, `rust:clippy:check`, `shell:verify`, `claude:verify`
2. **Phase 2 (sequential `cmds:`)**: `rust:license:check`, `rust:license:deps`, `rust:build`, `rust:test:unit`, `test:parallel`

## AI Quality Gate Pattern

Each subsystem rule has a Quality Gate section specifying which verify task to run during review-fix loops. Full `task verify` runs only as the final gate before commit. This prevents expensive acceptance tests from running during every iteration.

## Taskfile Features in Use

| Feature | Purpose |
|---|---|
| `output: group` + `error_only: true` | Clean verify output; only shows failures |
| `.taskrc.yml` | Project-wide `failfast: true` (abort on first failure) |
| Include-with-vars templates | `ocx.taskfile.yml` included as `shellcheck:` and `shfmt:` with different vars; uses `requires:` for mandatory vars |
| `internal: true` on includes | Template includes hidden from `task --list` |
| `:` prefix cross-references | Child taskfiles call root helpers like `:.ensure-cargo-tool` |
| `--force` | Bypasses all caching for one run (replaces `rm -rf .task/`) |
| `--status` | Check freshness without running |
| `--summary` | Show task detail (e.g., `task --summary verify`) |
| `aliases:` | Backward compatibility after renames |
| `for:` loops | Iteration in cmds/deps |

## OCX Conventions

1. **`verify` is a subsystem contract.** Every subsystem taskfile exposes a `verify` task. The root `verify` calls `<subsystem>:verify` rather than inlining individual subtasks.
2. **Composite tasks aggregate subtasks.** Each linter/formatter has its own subtask so it is independently runnable. `verify` references composite tasks only -- one entry per concern.
3. **Helpers use dot-prefix + `internal: true`.** Internal tasks are named with a dot prefix (e.g., `.ensure-cargo-tool`, `.verify:lint`) -- like GitLab CI hidden jobs. This makes them visually distinct and hidden from `task --list`.
4. **Tool installs use `status:` for idempotency.** `status: - cargo nextest --version` skips the install when the tool is already present. This is the cross-include deduplication pattern -- `run: once` does **not** work reliably across namespaced includes (go-task issue #852).
5. **Subsystem env overrides go on the task, not the include.** The root sets `OCX_INDEX` globally; subsystem tasks that need a different value override with per-task `env:`.

## Caching Contract

### `sources:` + `status:` + `method:`

| Field | Purpose | Recommendation |
|---|---|---|
| `sources:` | Files that force re-run when changed | Broad include + targeted excludes. Survives new file types. |
| `status:` | Exit 0 = up-to-date, skip | **Load-bearing for cache.** Use for output file checks. |
| `generates:` | Output declaration (documentation only) | **Not load-bearing** (go-task #2181). Declare for discoverability; add `status:` if skip-on-present is needed. |
| `method:` | `checksum` (default), `timestamp`, `none` | Only specify `timestamp` for tools with their own incremental build (e.g. `cargo build`). |

**Sources are relative to the task's `dir:`.** Set `dir: '{{.TASKFILE_DIR}}'` or rely on the include's `dir:` so globs use plain `**/*`.

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

When multiple tasks validate the same input set, define the source list once as a YAML anchor under `x-shared:` (the JSON-Schema extension convention), then reference with `*alias`. Do not anchor under a consuming task -- that implies ownership the data structure does not have.

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
- **Subsystem var**: Each subsystem resolves its own `REPORTS_DIR` with `{{if/else}}` so standalone invocation works:

  ```yaml
  vars:
    REPORTS_DIR: '{{if .REPORTS_ROOT}}{{.REPORTS_ROOT}}/claude{{else}}{{.TASKFILE_DIR}}/reports{{end}}'
  ```

  Parent and child vars must have **different names** (`REPORTS_ROOT` vs `REPORTS_DIR`) to avoid double-suffixing.
- **Format**: JUnit XML (preferred) or JSON. Create dir with `mkdir -p {{.REPORTS_DIR}}` as first command.
- **Declare `generates:`** for documentation even though it is not load-bearing.

## Includes and `dir:` Scoping

| Variable | Resolves to |
|---|---|
| `{{.ROOT_DIR}}` | Directory of the entry-point (root) taskfile |
| `{{.TASKFILE_DIR}}` | Directory of the currently executing taskfile |
| `{{.USER_WORKING_DIR}}` | Directory from which `task` was invoked |

Set `dir:` on the include block when all tasks should run relative to the sub-taskfile's directory (`.claude/`, `test/`, `website/`). Override per-task only when needed.

**Gotchas:** `${{.TASKFILE_DIR}}` is wrong (becomes a shell var). When the root sets a global `env:`, included tasks inherit it -- override per-task if needed.

## Preconditions vs Status

| Field | Use | Semantics |
|---|---|---|
| `preconditions:` + `msg:` | Guard -- abort if not ready | Failure aborts; downstream tasks do not run |
| `status:` | Cache -- skip if done | Exit 0 = up-to-date, skip silently |

## Sources

- [Taskfile schema reference](https://taskfile.dev/docs/reference/schema)
- [Taskfile usage](https://taskfile.dev/usage/) -- sources/status/method, run modes, env, preconditions
- [Taskfile templating](https://taskfile.dev/docs/reference/templating) -- special vars
- [go-task #2181](https://github.com/go-task/task/issues/2181) -- `generates:` globs not fingerprinted
- [go-task #852](https://github.com/go-task/task/issues/852) -- `run: once` cross-include dedup broken
