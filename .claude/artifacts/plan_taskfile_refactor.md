# Plan: Taskfile Subsystem Refactor

## Context

`task verify` runs 9 sequential steps (~5-10 minutes) regardless of what changed. During AI review-fix loops, this runs repeatedly. The user wants:

1. **Subsystem-scoped verification** — `rust:verify`, `shell:verify`, etc.
2. **Tree-structured taskfile layout** — subsystem taskfiles in their owning directories
3. **AI-aware verification** — subsystem rules tell Claude which verify to run based on what changed; full `verify` only at the end
4. **Correctness first** — `task verify` stays as the complete gate
5. **Leverage Taskfile features fully** — templates, caching, output modes, concurrency, `.taskrc.yml`

Full feature catalog: `.claude/artifacts/research_taskfile_optimization.md`

---

## Taskfile Features We Will Use

### Caching (`sources:` + `status:` + `method:`)

Every expensive task gets `sources:` with a shared YAML anchor. Tasks self-skip when inputs haven't changed. `--force` bypasses all caching for a single run (replaces `rm -rf .task/`). `--status` checks freshness without running.

### Output Mode (`output: group` + `error_only`)

Set at root level — buffers each task's output and flushes atomically. With `error_only: true`, successful tasks produce no output; only failures are visible. Clean verify runs show nothing; broken verify runs show exactly what broke.

```yaml
output:
  group:
    error_only: true
```

For CI, add GitHub Actions collapsible groups:
```yaml
output:
  group:
    begin: '::group::{{.TASK}}'
    end: '::endgroup::'
```

### `.taskrc.yml` for Project Defaults

```yaml
# .taskrc.yml (project root)
failfast: true       # stop verify chain on first failure
```

This replaces needing `failfast:` on every task. All `deps:` and `cmds:` chains fail fast globally.

### Include-with-Vars Templates

The pattern of including the same taskfile with different `vars:` IS the Taskfile template mechanism. OCX's shell linting pattern (shellcheck + shfmt both use `ocx exec` with git ls-files) is a prime candidate:

```yaml
# taskfiles/ocx-exec.taskfile.yml (reusable template)
tasks:
  check:
    internal: true
    preconditions:
      - sh: '[ -n "$(git ls-files "{{.GLOB}}")" ]'
        msg: 'No {{.GLOB}} files found, skipping {{.TOOL}}'
    cmd: ocx exec {{.PACKAGE}} -- {{.TOOL}} {{.CHECK_ARGS}} {{.FILES}}
    vars:
      FILES:
        sh: git ls-files '{{.GLOB}}' | tr '\n' ' '

  format:
    internal: true
    preconditions:
      - sh: '[ -n "$(git ls-files "{{.GLOB}}")" ]'
        msg: 'No {{.GLOB}} files found, skipping {{.TOOL}}'
    cmd: ocx exec {{.PACKAGE}} -- {{.TOOL}} {{.FORMAT_ARGS}} {{.FILES}}
    vars:
      FILES:
        sh: git ls-files '{{.GLOB}}' | tr '\n' ' '
```

Root includes it parameterized per tool:
```yaml
includes:
  shellcheck:
    taskfile: ./taskfiles/ocx-exec.taskfile.yml
    internal: true
    vars:
      PACKAGE: 'ocx.sh/shellcheck:0.11'
      TOOL: shellcheck
      GLOB: '*.sh'
      CHECK_ARGS: ''
      FORMAT_ARGS: ''
  shfmt:
    taskfile: ./taskfiles/ocx-exec.taskfile.yml
    internal: true
    vars:
      PACKAGE: 'ocx.sh/shfmt:3'
      TOOL: shfmt
      GLOB: '*.sh'
      CHECK_ARGS: '-d -i 4 -ci'
      FORMAT_ARGS: '-w -i 4 -ci'
```

Then `shell:verify` calls `task: :shellcheck:check` and `task: :shfmt:check` via the `:` prefix (child→parent cross-reference).

### Colon-Prefix Cross-Reference (`:task-name`)

Child taskfiles can call root-level internal helpers using `:task-name`. This enables shared setup tasks at root consumed by subsystems:

```yaml
# Root taskfile.yml
tasks:
  _ensure-cargo-tool:
    internal: true
    status: ['{{.TOOL}} --version']
    cmds: ['cargo install --locked {{.CRATE}}']

# taskfiles/rust.taskfile.yml (included as rust:)
tasks:
  test:unit:
    cmds:
      - task: :_ensure-cargo-tool
        vars: { TOOL: cargo-nextest, CRATE: cargo-nextest }
      - cargo nextest run --workspace --release --locked
```

### `internal: true` on Includes

Mark entire included taskfiles as internal — their tasks are callable by other tasks but hidden from `task --list`:

```yaml
includes:
  shellcheck:
    taskfile: ./taskfiles/ocx-exec.taskfile.yml
    internal: true    # all tasks inside hidden from --list
```

### Aliases for Backward Compatibility

After renaming `lint:shell` → `shell:lint`, add alias for the old name:

```yaml
tasks:
  lint:
    aliases: [shell:lint]   # old name still works
```

### Parallel Deps for Independent Checks

Independent subsystem checks can run as parallel `deps:` instead of sequential `cmds:`:

```yaml
verify:
  deps:                          # parallel phase: independent checks
    - task: rust:format:check
    - task: rust:clippy:check
    - task: shell:verify
    - task: claude:verify
  cmds:                          # sequential phase: depends on above
    - task: rust:license:check
    - task: rust:license:deps
    - task: rust:build
    - task: rust:test:unit
    - task: test:parallel
```

Or keep it simple with two stages:
```yaml
verify:
  cmds:
    - task: verify:lint    # parallel lint phase
    - task: verify:build   # sequential build+test phase

verify:lint:
  internal: true
  deps: [rust:format:check, rust:clippy:check, shell:verify, claude:verify]

verify:build:
  internal: true
  cmds:
    - task: rust:license:check
    - task: rust:license:deps
    - task: rust:build
    - task: rust:test:unit
    - task: test:parallel
```

### Summary Field for Documentation

```yaml
tasks:
  verify:
    desc: Full quality gate
    summary: |
      Runs all quality checks in order:
      1. Parallel: format, clippy, shell lint, claude verify
      2. Sequential: license, build, unit tests, acceptance tests
      Use `task --summary verify` to see this detail.
      Use `task --force verify` to bypass all caching.
```

### `--force` Instead of `rm -rf .task/`

`task --force verify` bypasses all `sources:` and `status:` checks for one run. Cleaner than deleting the cache directory.

---

## Design

### 1. `verify` Stays as Full Gate — Two-Phase Execution

```
task verify
├── Phase 1 (parallel deps): fast independent checks
│   ├── rust:format:check
│   ├── rust:clippy:check
│   ├── shell:verify
│   └── claude:verify
└── Phase 2 (sequential cmds): ordered build + test
    ├── rust:license:check
    ├── rust:license:deps
    ├── rust:build
    ├── rust:test:unit
    └── test:parallel
```

Phase 1 runs all lint checks in parallel (~30-60s wall time instead of ~90s sequential). Phase 2 runs the build-dependent chain sequentially.

### 2. AI Quality Gate Pattern

Each `subsystem-*.md` rule gets a **Quality Gate** section:

```markdown
## Quality Gate

During review-fix loops, run `task rust:verify` (NOT full `task verify`).
Full `task verify` runs only as the final gate before commit.
```

Mapping:
- Rust crate changes → `task rust:verify`
- Shell script changes → `task shell:verify`
- AI config changes → `task claude:verify`
- Python test changes → `task test:parallel`
- Website changes → `task website:build`

### 3. Tree-Structured Taskfile Layout

| Current | Proposed | Rationale |
|---------|----------|-----------|
| `taskfiles/format.taskfile.yml` | **Removed** — into `taskfiles/rust.taskfile.yml` | Shares `&rust-sources` anchor |
| `taskfiles/clippy.taskfile.yml` | **Removed** — into `taskfiles/rust.taskfile.yml` | Same |
| `taskfiles/lint.taskfile.yml` | **Renamed** → `taskfiles/shell.taskfile.yml` | `shell:` namespace |
| `taskfiles/schema.taskfile.yml` | → `website/schema.taskfile.yml` | Outputs to website |
| `taskfiles/sbom.taskfile.yml` | → `website/sbom.taskfile.yml` | Outputs to website |
| `taskfiles/catalog.taskfile.yml` | → `website/catalog.taskfile.yml` | Outputs to website |
| `taskfiles/recordings.taskfile.yml` | → `website/recordings.taskfile.yml` | Outputs to website |
| `taskfiles/mirror-sdk.taskfile.yml` | → `mirror-sdk-py/taskfile.yml` | Belongs in its dir |
| `taskfiles/{mirror,coverage,duplo,release}` | **Stay** | Cross-cutting |
| — | **NEW** `taskfiles/ocx-exec.taskfile.yml` | Reusable `ocx exec` template |
| — | **NEW** `.taskrc.yml` | Project defaults (failfast) |

Root include map after:

```yaml
includes:
  rust: ./taskfiles/rust.taskfile.yml
  shell: ./taskfiles/shell.taskfile.yml           # renamed from lint
  coverage: ./taskfiles/coverage.taskfile.yml
  duplo: ./taskfiles/duplo.taskfile.yml
  release: ./taskfiles/release.taskfile.yml
  mirror: ./taskfiles/mirror.taskfile.yml
  # Template includes (internal — hidden from --list)
  shellcheck:
    taskfile: ./taskfiles/ocx-exec.taskfile.yml
    internal: true
    vars: { PACKAGE: 'ocx.sh/shellcheck:0.11', TOOL: shellcheck, GLOB: '*.sh' }
  shfmt:
    taskfile: ./taskfiles/ocx-exec.taskfile.yml
    internal: true
    vars: { PACKAGE: 'ocx.sh/shfmt:3', TOOL: shfmt, GLOB: '*.sh' }
  mirror-sdk:
    taskfile: ./mirror-sdk-py/taskfile.yml
    dir: ./mirror-sdk-py
  claude:
    taskfile: ./.claude/taskfile.yml
    dir: ./.claude
  test:
    taskfile: ./test/taskfile.yml
    dir: ./test
  website:                                        # now includes schema, sbom, catalog, recordings internally
    taskfile: ./website/taskfile.yml
    dir: ./website
```

### 4. `sources:` Caching

YAML anchors don't cross include boundaries — `&rust-sources` and all consumers in same file.

**`rust.taskfile.yml`:**
```yaml
x-shared:
  rust-sources: &rust-sources
    - '{{.ROOT_DIR}}/crates/**/*.rs'
    - '{{.ROOT_DIR}}/crates/**/Cargo.toml'
    - '{{.ROOT_DIR}}/Cargo.lock'
    - '{{.ROOT_DIR}}/rustfmt.toml'
    - '{{.ROOT_DIR}}/clippy.toml'
    - exclude: '{{.ROOT_DIR}}/target/**'
```

All Rust tasks get `sources: *rust-sources`. Build additionally gets `method: timestamp` + `status: [test -f ...]`.

**`test/taskfile.yml`** — `parallel` gets sources (Python files + binary).

**`website/schema.taskfile.yml`** — Pattern A (sources on schema Rust crates + status on output).

### 5. Root `taskfile.yml` Structure

```yaml
version: '3'

output:
  group:
    error_only: true

vars:
  REPORTS_ROOT: '{{.ROOT_DIR}}/target/reports'

env:
  OCX_INDEX: '{{.ROOT_DIR}}/.ocx/index'

includes:
  # ... (as shown above)

tasks:
  default:
    deps:
      - task: rust:format:check
      - task: rust:clippy:check
      - task: check

  check:
    cmd: cargo check

  # Shared internal helpers (called by subsystems via :prefix)
  _ensure-cargo-tool:
    internal: true
    status: ['{{.TOOL}} --version']
    cmds: ['cargo install --locked {{.CRATE | default .TOOL}}']

  # Two-phase verify
  verify:
    desc: Full quality gate
    summary: |
      Phase 1 (parallel): format, clippy, shell lint, claude verify
      Phase 2 (sequential): license, build, unit tests, acceptance tests
      Use `task --force verify` to bypass caching.
      Use `task --status verify` to check freshness without running.
    cmds:
      - task: _verify:lint
      - task: _verify:build-test

  _verify:lint:
    internal: true
    deps:
      - task: rust:format:check
      - task: rust:clippy:check
      - task: shell:verify
      - task: claude:verify

  _verify:build-test:
    internal: true
    cmds:
      - task: rust:license:check
      - task: rust:license:deps
      - task: rust:build
      - task: rust:test:unit
      - task: test:parallel

  checkpoint:
    cmds:
      - if: '! [[ $(git show -s --format=%s HEAD) == "Checkpoint" ]]'
        cmd: git commit --allow-empty -m "Checkpoint"
      - cmd: git add .
      - cmd: git commit --amend --no-edit --allow-empty

  push:
    vars:
      DATE: { sh: 'date -u +"%Y-%m-%dT%H:%M:%SZ"' }
    cmds:
      - git add .
      - git commit -m "{{.DATE}}"
      - git push {{.CLI_ARGS}}
```

---

## Implementation Phases

### Phase 1: Foundation — `.taskrc.yml` + Output Mode + Shared Helpers

**Create:**
- `.taskrc.yml` — `failfast: true`
- Root `output: group` with `error_only: true`
- Root `_ensure-cargo-tool` internal helper

**Modify:**
- `taskfile.yml` — add output config, shared helpers

**Verify:** `task verify` works with grouped output, failures show output.

### Phase 2: Consolidate Rust Tasks + Caching

**Create:** `taskfiles/rust.taskfile.yml` — all Rust tasks with `x-shared: &rust-sources`, `sources:` caching, calls `:_ensure-cargo-tool` for nextest/hawkeye/cargo-deny

**Delete:** `taskfiles/format.taskfile.yml`, `taskfiles/clippy.taskfile.yml`

**Modify:** `taskfile.yml` — replace includes, move tasks, restructure `verify` into two-phase (parallel lint deps → sequential build cmds)

**Verify:** `task rust:verify` passes. `task --force verify` runs everything. After non-Rust change, Rust tasks skip.

### Phase 3: Shell Rename + OCX-Exec Template

**Create:** `taskfiles/ocx-exec.taskfile.yml` — reusable `ocx exec` template

**Rename:** `taskfiles/lint.taskfile.yml` → `taskfiles/shell.taskfile.yml`

**Modify:**
- `shell.taskfile.yml` — add `verify` task, refactor to use `:shellcheck:check` and `:shfmt:check` from template includes
- Root — add `shellcheck:` and `shfmt:` internal template includes, rename `lint:` to `shell:`

**Verify:** `task shell:verify` works. `task shell:format` applies formatting.

### Phase 4: Move Website-Adjacent Taskfiles

**Move:** `taskfiles/{schema,sbom,catalog,recordings}.taskfile.yml` → `website/`
**Move:** `taskfiles/mirror-sdk.taskfile.yml` → `mirror-sdk-py/taskfile.yml`

**Modify:** `website/taskfile.yml` — add internal includes. Root — remove old includes.

**Add:** `sources:` + `status:` caching to `website/schema.taskfile.yml` and `website/sbom.taskfile.yml`.

**Verify:** `task website:build` still works.

### Phase 5: Acceptance Test Caching

**Modify:** `test/taskfile.yml` — add `sources:` to `parallel` task (Python test files + binary).

**Verify:** Cache skip works for non-test changes.

### Phase 6: AI Configuration — Subsystem Quality Gates

**Modify:**
- `subsystem-oci.md`, `subsystem-package.md`, `subsystem-package-manager.md`, `subsystem-cli.md`, `subsystem-mirror.md` → Quality Gate: `task rust:verify`
- `subsystem-tests.md` → Quality Gate: `task test:parallel`
- `subsystem-website.md` → Quality Gate: `task website:build`
- `subsystem-taskfiles.md` — comprehensive update: tiered model, tree structure, feature catalog reference, output modes, `.taskrc.yml`, template pattern, quality gate pattern
- `workflow-feature.md` — subsystem verify during implementation, full verify at final gate
- `CLAUDE.md` — update key workflows with `rust:verify`, `shell:verify`, `--force`

**Verify:** `task claude:tests` passes.

### Phase 7: Integration Verification

- `task --force verify` — full gate bypassing cache
- `task rust:verify` — Rust-only
- `task shell:verify` — shell-only
- Touch only `.py` → `task verify` → Rust tasks skip
- Touch only `CLAUDE.md` → `task verify` → only claude:verify runs
- `task --list` — clean, grouped by namespace, only entry points shown
- `task --summary verify` — shows two-phase detail
- `task --status rust:verify` — checks freshness without running
- `task website:build` — works after moves

---

## Key Files

| File | Change |
|------|--------|
| `.taskrc.yml` | **NEW** — project defaults (failfast) |
| `taskfile.yml` | Restructure: output mode, shared helpers, two-phase verify, updated includes |
| `taskfiles/rust.taskfile.yml` | **NEW** — consolidated Rust tasks with `&rust-sources` caching |
| `taskfiles/shell.taskfile.yml` | **RENAMED** from lint — verify task, uses template includes |
| `taskfiles/ocx-exec.taskfile.yml` | **NEW** — reusable `ocx exec` tool invocation template |
| `taskfiles/format.taskfile.yml` | **DELETE** |
| `taskfiles/clippy.taskfile.yml` | **DELETE** |
| `website/taskfile.yml` | Add internal includes for moved taskfiles |
| `website/{schema,sbom,catalog,recordings}.taskfile.yml` | **MOVED** + caching where applicable |
| `mirror-sdk-py/taskfile.yml` | **MOVED** |
| `test/taskfile.yml` | Add `sources:` to `parallel` |
| `.claude/rules/subsystem-*.md` (7 files) | Add Quality Gate sections |
| `.claude/rules/subsystem-taskfiles.md` | Major update: tiered model, features, templates |
| `.claude/rules/workflow-feature.md` | Subsystem verify during implementation |
| `CLAUDE.md` | Update key workflows |

---

## Acceptance Criteria

- [ ] `task rust:verify` runs all Rust checks with `sources:` caching
- [ ] `task shell:verify` runs shell checks using `ocx-exec` template
- [ ] `task verify` is the full gate — parallel lint phase + sequential build phase
- [ ] `output: group` with `error_only: true` — clean output on success, detail on failure
- [ ] `.taskrc.yml` sets `failfast: true` project-wide
- [ ] `task --force verify` bypasses all caching
- [ ] `task --status rust:verify` checks freshness without running
- [ ] `task --summary verify` shows two-phase execution detail
- [ ] After non-Rust change, `task verify` skips Rust tasks via cache
- [ ] After CLAUDE.md-only change, only `claude:verify` runs
- [ ] Reusable `ocx-exec` template used by shellcheck + shfmt
- [ ] Internal helpers called via `:` prefix from subsystem taskfiles
- [ ] Website-adjacent taskfiles live in `website/` directory
- [ ] Schema and SBOM use `sources:` + `status:` caching
- [ ] Each subsystem rule has a Quality Gate section
- [ ] `task --list` shows clean subsystem entry points (no internal tasks)
- [ ] `task verify` passes on clean checkout
