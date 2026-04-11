---
paths:
  - taskfile.yml
  - taskfiles/**/*.yml
  - "**/taskfile.yml"
---

# Taskfiles Subsystem

[Taskfile](https://taskfile.dev) v3 is the primary task runner for OCX. The root taskfile at `taskfile.yml` includes per-subsystem taskfiles plus topical taskfiles under `taskfiles/`. Each subsystem owns its own pipeline and exposes a thin contract back to the root.

## File Layout

| Path | Owner | Loaded as |
|---|---|---|
| `taskfile.yml` | root | entry point — `task verify`, `task build`, `task ocx:*`, etc. |
| `taskfiles/*.taskfile.yml` | root, topical | included flat into root namespace (e.g. `clippy:check`, `format:check`, `lint:shell`, `mirror:*`) |
| `.claude/taskfile.yml` | `.claude/` subsystem | `dir: ./.claude`, namespace `claude:` |
| `test/taskfile.yml` | acceptance tests | `dir: ./test`, namespace `test:` |
| `website/taskfile.yml` | website | `dir: ./website`, namespace `website:` |

## OCX Conventions

These are project-specific contracts that compose with go-task's general semantics.

1. **`verify` is a subsystem contract.** Every subsystem taskfile exposes a `verify` task that runs all checks for that subsystem (tests, link-checks, lint). The root `verify` calls `<subsystem>:verify` rather than inlining individual subtasks. Example: root `verify` → `claude:verify` → `claude:tests` + `claude:lint:links`.
2. **Composite tasks aggregate subtasks.** `lint:shell` calls `shell:lint` + `shell:format:check`, not raw `shellcheck`/`shfmt`. Each linter/formatter has its own subtask so it is independently runnable. `verify` references composite tasks only — one entry per concern.
3. **Helpers are `internal: true`.** Anything only ever called as a dep (e.g. `install:nextest`, `install:hawkeye`, `install:cargo-deny`) is hidden from `task --list` so users only see entry-point tasks.
4. **Tool installs use `status:` for idempotency.** `install:*` tasks use `status: - cargo nextest --version` (or equivalent) so the install is skipped when the tool is already present. This is the cross-include deduplication pattern — `run: once` does **not** work reliably across namespaced includes (go-task issue #852).
5. **Subsystem env overrides go on the task, not the include.** When a subsystem task needs a different env from what the root taskfile sets (e.g. `claude:lint:links` needs `OCX_INDEX: '{{.HOME}}/.ocx/index'` because the root sets `OCX_INDEX` to the pinned project index), set `env:` on the task itself.

## Caching Contract

OCX currently underuses Task's built-in caching. The rules below codify the right patterns.

### `sources:` + `status:` + `method:`

| Field | Purpose | OCX recommendation |
|---|---|---|
| `sources:` | Files that, when changed, force a re-run | **Prefer broad include + targeted excludes** over enumerating extensions. `**/*` plus `exclude:` for noise (`__pycache__`, `.venv`, `.state`, `artifacts`) is shorter and survives new file types being added without rule changes. |
| `status:` | Shell command — exit 0 means "task is up-to-date, skip" | The reliable presence/freshness check. **Use this, not `generates:`, for output files.** |
| `generates:` | Output declaration (documentation only) | **Not load-bearing for cache.** Glob in `generates:` is not fingerprinted (go-task issue #2181) — deleting individual generated files does not trigger a re-run. **Still declare it** for any task that produces report/artifact files: it documents the output contract and keeps downstream consumers discoverable. Use a `status:` check on the same path if you also want skip-on-output-present semantics. |
| `method:` | `checksum` (default), `timestamp`, or `none` | **Don't write `method: checksum` explicitly — it's the default.** Specify `timestamp` only when wrapping a tool with its own incremental build (e.g. `cargo build`); `none` only to opt out of caching. |

**Sources are relative to the task's `dir:`**, not the taskfile's location. Set `dir: '{{.TASKFILE_DIR}}'` (or rely on the include's `dir:`) so source globs can use plain `**/*` without a `{{.TASKFILE_DIR}}/` prefix on every line. For files outside that dir, use a relative path like `'../CLAUDE.md'`.

**Excludes should generalize across directories.** Prefer `'**/__pycache__/**'` over `'tests/__pycache__/**'` + `'hooks/__pycache__/**'`. Per-subdirectory excludes silently miss new sibling directories. Apply this to any noise pattern that recurs (`__pycache__`, `.venv`, `node_modules`, `target`, runtime state dirs like `.state` / `.locks`).

**The two patterns OCX should use:**

```yaml
# Pattern A — fast skip when output exists, re-run when sources change
schema:generate:
  cmds:
    - cargo run -p ocx_schema --release -- metadata > {{.METADATA_SCHEMA_DIR}}/v1.json
  sources:
    - crates/ocx_schema/src/**/*.rs
    - crates/ocx_lib/src/**/*.rs
    - Cargo.lock
  status:
    - test -f {{.METADATA_SCHEMA_DIR}}/v1.json

# Pattern B — wrap a tool that has its own incremental build
build:
  cmd: cargo build --release -p ocx --locked
  sources:
    - crates/**/*.rs
    - crates/**/Cargo.toml
    - Cargo.lock
  status:
    - test target/release/ocx -nt Cargo.lock
  method: timestamp
```

### Sharing source lists across tasks

When two tasks in the same taskfile validate the same input set (e.g. structural tests + link check both reading all of `.claude/`), define the source list **once** as a YAML anchor at the top of the document under an `x-shared:` extension key, then reference it from each task with `*alias`. This keeps the list in one place and makes "shared by all tasks" visually distinct from "owned by one task that other tasks happen to reuse".

```yaml
version: '3'

x-shared:
  claude-sources: &claude-sources
    - '**/*'
    - '../CLAUDE.md'
    - '../AGENTS.md'
    - exclude: 'artifacts/**'
    - exclude: '**/__pycache__/**'
    - exclude: '**/.venv/**'
    - exclude: '**/.state/**'

tasks:
  tests:
    sources: *claude-sources
    cmd: ...
  lint:links:
    sources: *claude-sources
    cmd: ...
```

go-task accepts unknown top-level keys with the `x-` prefix (the JSON-Schema extension convention). Do **not** anchor the list under one of the consuming tasks (`sources: &foo - ...`) and reference from siblings — that implies ownership the data structure does not have.

### `.task/` cache directory

Task stores fingerprint state in `.task/` next to the taskfile. **Add `.task/` to `.gitignore`** before introducing any `sources:`/`method:` task — committing fingerprint state causes spurious conflicts and incorrect cache hits across machines.

### Reports and artifacts

Tasks that validate or check things should emit a machine-readable report so the result can be inspected after the fact and consumed by CI. OCX convention:

- **Report directory**: `target/reports/<subsystem>/` (e.g. `target/reports/claude/`). Lives under Cargo's `target/` — already gitignored, lines up with other Rust artifact dirs (`target/criterion/`, `target/llvm-cov/`), and gives CI one well-known place to scrape JUnit XML across all subsystems.
- **Layered defaulting via task vars** (not YAML anchors — anchors only expand to literal strings at parse time, but the path needs template evaluation):

  Root `taskfile.yml` exposes the **shared report root**:
  ```yaml
  vars:
    REPORTS_ROOT: '{{.ROOT_DIR}}/target/reports'
  ```

  Each subsystem taskfile resolves its own `REPORTS_DIR` with a `{{if/else}}` so the path is correct in **both** invocation modes:
  ```yaml
  vars:
    REPORTS_DIR: '{{if .REPORTS_ROOT}}{{.REPORTS_ROOT}}/claude{{else}}{{.TASKFILE_DIR}}/reports{{end}}'
  ```

  | Mode | `REPORTS_ROOT` | `REPORTS_DIR` resolves to |
  |---|---|---|
  | Included via root (`task claude:verify`) | set by root | `target/reports/claude/` (subsystem-namespaced under shared root) |
  | Standalone (`cd .claude && task verify`) | unset | `.claude/reports/` (no `/<subsystem>` suffix — already in subsystem dir) |

  Then reference as `{{.REPORTS_DIR}}` in `cmds:` and `generates:`.

  **Two load-bearing details:**
  1. **Parent and child vars must have different names** (`REPORTS_ROOT` vs `REPORTS_DIR`). If they shared a name, the child's template would resolve `.REPORTS_DIR` against the already-shadowed value and double-suffix the path (`target/reports/claude/claude/`).
  2. **`{{if/else}}` instead of `default`** because the suffix `/claude` is only correct in the included mode — when standalone, the subsystem directory already provides the namespace, so the suffix would duplicate it.

  Add the standalone fallback path to `.gitignore` (e.g. `.claude/reports/`) — `target/` is already gitignored at root for the included case.
- **Format preference**: JUnit XML when the tool supports it (pytest's `--junitxml=<path>`), JSON otherwise (lychee `--format json --output <path>`). JUnit drops straight into GitHub Actions / GitLab / Jenkins consumers; JSON is convertible later.
- **Create the directory in the task** with `mkdir -p {{.REPORTS_DIR}}` as the first command — pytest's `--junitxml` and lychee's `--output` do not auto-create parent directories.
- **Declare `generates:`** listing the report path. Not load-bearing for caching, but documents the output contract.
- **No source-exclusion needed.** Because reports live under `target/` (outside any subsystem source tree), they cannot bust the cache by being written. This is the main reason `target/reports/` beats per-subsystem `.reports/`.

```yaml
vars:
  REPORTS_DIR: '{{.ROOT_DIR}}/target/reports/claude'

tasks:
  tests:
    dir: '{{.TASKFILE_DIR}}'
    cmds:
      - mkdir -p {{.REPORTS_DIR}}
      - uv run --directory tests pytest test_ai_config.py -v --junitxml={{.REPORTS_DIR}}/tests.junit.xml
    sources: *claude-sources
    generates:
      - '{{.REPORTS_DIR}}/tests.junit.xml'
```

## Includes and `dir:` Scoping

**Three directory variables — know which to use:**

| Variable | Resolves to |
|---|---|
| `{{.ROOT_DIR}}` | Directory of the entry-point (root) taskfile |
| `{{.TASKFILE_DIR}}` | Directory of the *currently executing* taskfile |
| `{{.USER_WORKING_DIR}}` | Directory from which `task` was invoked |

**Set `dir:` on the include block** when all tasks in a sub-taskfile should run relative to its own directory (this is what OCX does for `.claude/`, `test/`, `website/`). Override per-task only when a single task in the subsystem needs a different cwd.

**Gotchas:**

- `${{.TASKFILE_DIR}}` is **wrong** — the `$` prefix turns the template into a shell variable reference and the path resolves to a literal `$/...`. Use `{{.TASKFILE_DIR}}` (no `$`).
- `{{.TASKFILE_DIR}}` historically resolved to `ROOT_DIR` in included files (go-task issue #860). Modern releases fixed this, but if a path resolution looks wrong inside a sub-taskfile, prefer `{{.ROOT_DIR}}/<absolute-path>` for safety.
- When the root sets a global `env:` (e.g. `OCX_INDEX`), included tasks inherit it. Override per-task with `env:` if a subsystem needs a different value.

## Preconditions vs Status

| Field | Use | Semantics |
|---|---|---|
| `preconditions:` + `msg:` | Guard ("don't run unless X is ready") | Failure aborts; downstream tasks do not run |
| `status:` | Cache ("skip if already done") | Exit 0 = up-to-date, skip silently. Not a failure. |

## Performance Opportunities

Highest-leverage uncached tasks in OCX: `schema:generate*` (Pattern A — slow `cargo run`), `sbom:generate:json` (Pattern A — slow `cargo cyclonedx`), root `build` (Pattern B — wraps `cargo build`), `website:install` (`status: - test -d node_modules`). **Do not cache** `catalog:generate` (live registry read) or `recordings:build` (drives real binary output). `test:build` and `recordings:ensure-binary` already use `status: - '{{eq .SKIP_BUILD "true"}}'` correctly.

## Sources

- [Taskfile schema reference](https://taskfile.dev/docs/reference/schema) — canonical field definitions
- [Taskfile usage](https://taskfile.dev/usage/) — sources/status/method, watch, run modes, env, dotenv, preconditions
- [Taskfile templating](https://taskfile.dev/docs/reference/templating) — special vars (`ROOT_DIR`, `TASKFILE_DIR`, `USER_WORKING_DIR`, `CHECKSUM`, `TIMESTAMP`)
- [go-task #2181](https://github.com/go-task/task/issues/2181) — `generates:` with globs is not fingerprinted (use `status:` instead)
- [go-task #860](https://github.com/go-task/task/issues/860) — historical `TASKFILE_DIR` resolution bug in includes
- [go-task #852](https://github.com/go-task/task/issues/852) — `run: once` does not deduplicate across namespaced includes
