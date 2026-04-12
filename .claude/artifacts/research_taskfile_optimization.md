# Research: Taskfile v3 Feature Catalog & Optimization Patterns

**Date**: 2026-04-12
**Sources**: taskfile.dev/docs/reference/{cli,schema,config,templating}, taskfile.dev/usage, taskfile.dev/docs/changelog

## Taskfile v3 Feature Catalog

### Caching & Up-to-Date Detection

| Feature | Purpose | OCX applicability |
|---------|---------|-------------------|
| `sources:` | Glob list of input files; checksum/timestamp triggers re-run | High — all expensive tasks |
| `generates:` | Output file declarations (documentation only — NOT fingerprinted for globs, go-task #2181) | Medium — document output contracts |
| `status:` | Shell commands; exit 0 = up-to-date, skip | High — presence/freshness checks |
| `method:` | `checksum` (default, SHA-256), `timestamp` (mtime), `none` | Use `timestamp` only for tools with own incremental build (cargo) |
| `--force` / `-f` | Bypass ALL caching (sources + status) for target + deps | Useful for "run everything" after taskfile changes |
| `--status` | Check up-to-date without running; exit non-zero if stale | CI gating: `task --status rust:verify` |
| `.task/` directory | Stores fingerprint state; must be in `.gitignore` | Already gitignored in OCX |

### Execution Control

| Feature | Purpose | OCX applicability |
|---------|---------|-------------------|
| `deps:` | Parallel dependencies (all run concurrently before `cmds:`) | Use for independent parallel checks |
| `cmds:` | Sequential commands (fail-fast by default) | Use for ordered verify chains |
| `--parallel` / `-p` | Run multiple CLI-named tasks concurrently | `task -p rust:verify shell:verify claude:verify` |
| `--concurrency` / `-C` | Cap max concurrent tasks (env: `TASK_CONCURRENCY`) | Prevent registry contention in tests |
| `failfast:` | Stop remaining deps on first failure (default: false since v3.46.1) | Set `true` on verify tasks |
| `run: once` | Execute at most once per session even if referenced multiple times | Unreliable across namespaces (#852) — use `status:` instead |
| `run: when_changed` | Re-run only when vars change | For parameterized tasks |
| `--dry` / `-n` | Print commands without executing (env: `TASK_DRY`) | Audit what verify will run |

### Output & Reporting

| Feature | Purpose | OCX applicability |
|---------|---------|-------------------|
| `output: group` | Buffer task output, flush atomically after exit | Essential for parallel verify — clean output |
| `group.error_only: true` | Suppress output on success; show only on failure | Noise-free verify — only see what broke |
| `group.begin` / `group.end` | Templates for CI collapsible groups | `::group::{{.TASK}}` / `::endgroup::` for GitHub Actions |
| `output: prefixed` | Prepend `[task-name]` to every line | For parallel task debugging |
| `--summary` | Print task metadata (desc, deps, commands) without executing | Documentation / discovery |
| `--list --json` | Machine-readable task listing | Tooling integration |
| `--list --json --nested` | Hierarchical JSON by namespace (v3.45.3) | Editor extensions |
| `--sort none` | Preserve YAML definition order in `--list` | Control `task --list` readability |
| `--verbose` / `-v` | Show task resolution, variable expansion, execution flow | Debug caching issues |
| `silent:` | Per-task or per-command output suppression | Quiet helpers |

### Task Organization

| Feature | Purpose | OCX applicability |
|---------|---------|-------------------|
| `includes:` with `vars:` | Include same taskfile twice with different vars — **the template pattern** | Reusable tool invocation templates |
| `internal: true` | Hide from `--list` and CLI; callable only by other tasks | Helper/setup tasks |
| `internal: true` on include | Cascade internal to all tasks in included file | Hide entire utility taskfiles |
| `aliases:` | Alternative names for tasks | Backward-compat after renames |
| `flatten: true` on include | Remove namespace prefix — tasks become root-level | Utility functions |
| `excludes:` on include | Exclude specific tasks from inclusion | Cherry-pick from shared taskfiles |
| `:task-name` (colon prefix) | Cross-reference parent tasks from child includes | Child calls root-level helpers |
| `desc:` | Short description — controls `--list` visibility | Only on user-facing entry points |
| `summary:` | Extended detail — shown via `--summary <task>` only | Document complex tasks |

### Loops & Matrices

| Feature | Purpose | OCX applicability |
|---------|---------|-------------------|
| `for: [list]` | Iterate over static list in `cmds:` | Replace duplicate command entries |
| `for: { var: NAME }` | Iterate over variable (space-split) | Dynamic file lists |
| `for: { var: NAME, split: ',' }` | Custom delimiter | CSV-style lists |
| `for: { matrix: { OS: [...], ARCH: [...] } }` | Cross-product iteration | Cross-compile, multi-platform |
| `for:` in `deps:` | Parallel iteration (deps run concurrently) | Parallel subsystem verify |
| `for: { glob: 'pattern' }` | Iterate over files matching glob (v3.43.1) | Per-file operations |

### Variables & Validation

| Feature | Purpose | OCX applicability |
|---------|---------|-------------------|
| `requires: { vars: [...] }` | Fail if variables missing | Guard release/deploy tasks |
| `requires: { vars: [{ name: X, enum: [...] }] }` | Enum validation (v3.42.0) | Restrict mode/env values |
| `vars:` with `sh:` | Dynamic variable via shell command | Git commit, dates, version |
| `ref:` in vars | Pass structured types (arrays/maps) without string coercion | Complex task parameterization |
| `map:` variables | Structured variable definitions (v3.43.1) | Config objects |
| `default` function | Fallback value in templates | Overridable include vars |

### Templating Functions (Key Subset)

| Function | Example | Purpose |
|----------|---------|---------|
| `OS` | `{{OS}}` → `linux`, `darwin`, `windows` | Platform-conditional logic |
| `ARCH` | `{{ARCH}}` → `amd64`, `arm64` | Architecture detection |
| `exeExt` | `binary{{exeExt}}` → `binary.exe` on Windows | Cross-platform binary names |
| `numCPU` | `{{numCPU}}` | Concurrency limits |
| `shellQuote` | `{{shellQuote .PATH}}` | Safe shell argument quoting |
| `joinPath` | `{{joinPath .ROOT_DIR "crates"}}` | Cross-platform path building |
| `relPath` | `{{relPath .ROOT_DIR .TASKFILE_DIR}}` | Relative path calculation |
| `toJson` / `fromJson` | `{{.DATA \| toJson}}` | JSON serialization in templates |

### Configuration (`.taskrc.yml`)

| Option | Default | Env var | Purpose |
|--------|---------|---------|---------|
| `verbose` | `false` | `TASK_VERBOSE` | Enable verbose output |
| `silent` | `false` | `TASK_SILENT` | Suppress command echoing |
| `color` | `true` | `TASK_COLOR` | Colored output |
| `concurrency` | unlimited | `TASK_CONCURRENCY` | Max concurrent tasks |
| `failfast` | `false` | `TASK_FAILFAST` | Stop on first dep failure |
| `interactive` | `false` | `TASK_INTERACTIVE` | Prompt for missing vars |
| `disable-fuzzy` | `false` | `TASK_DISABLE_FUZZY` | Disable fuzzy task matching |

File precedence: project dir → parent dirs → `$HOME` → `$XDG_CONFIG_HOME/task`.

### Special Variables

| Variable | Resolves to |
|----------|------------|
| `{{.ROOT_DIR}}` | Root taskfile directory |
| `{{.TASKFILE_DIR}}` | Current taskfile directory |
| `{{.USER_WORKING_DIR}}` | Where `task` was invoked |
| `{{.TASK}}` | Current task name |
| `{{.CLI_ARGS}}` | Extra args after `--` |
| `{{.CHECKSUM}}` | Source file checksum |
| `{{.TIMESTAMP}}` | Greatest source timestamp |

### Other Notable Features

| Feature | Purpose |
|---------|---------|
| `defer:` | Cleanup command that runs even on failure (LIFO order, like Go defer) |
| `preconditions:` + `msg:` | Guard: abort task chain if unmet |
| `platforms:` | OS/arch filter — skip task on wrong platform |
| `set:` / `shopt:` | POSIX/Bash shell options per task |
| `prompt:` | Confirmation prompt before execution |
| `watch: true` + `--watch` | File-watching re-run mode |
| `dotenv:` | Load `.env` files (first file wins) |

---

## Key Patterns for OCX

### Pattern: Reusable Tool Template via Include-with-Vars

The OCX shell linting pattern (shellcheck + shfmt via `ocx exec`) can be templatized:

```yaml
# taskfiles/tool-exec.taskfile.yml (reusable template)
tasks:
  run:
    internal: true
    preconditions:
      - sh: '[ -n "$(git ls-files "{{.GLOB}}")" ]'
        msg: 'No {{.GLOB}} files found, skipping {{.TOOL}}'
    cmd: ocx exec {{.PACKAGE}} -- {{.TOOL}} {{.ARGS}} {{.FILES}}
    vars:
      FILES:
        sh: git ls-files '{{.GLOB}}' | tr '\n' ' '
```

```yaml
# Root includes it parameterized:
includes:
  shellcheck:
    taskfile: ./taskfiles/tool-exec.taskfile.yml
    vars: { PACKAGE: 'ocx.sh/shellcheck:0.11', TOOL: shellcheck, GLOB: '*.sh', ARGS: '' }
  shfmt-check:
    taskfile: ./taskfiles/tool-exec.taskfile.yml
    vars: { PACKAGE: 'ocx.sh/shfmt:3', TOOL: shfmt, GLOB: '*.sh', ARGS: '-d -i 4 -ci' }
```

### Pattern: Child Calls Parent Helper via `:` Prefix

```yaml
# Root taskfile.yml
tasks:
  _ensure-tool:
    internal: true
    status: ['command -v {{.TOOL}}']
    cmd: cargo install --locked {{.TOOL}}

# taskfiles/rust.taskfile.yml (included as rust:)
tasks:
  test:unit:
    cmds:
      - task: :_ensure-tool
        vars: { TOOL: cargo-nextest }
      - cargo nextest run --workspace
```

### Pattern: Grouped Output for Verify

```yaml
version: '3'
output:
  group:
    error_only: true    # only show output when something fails

tasks:
  verify:
    cmds:
      - task: rust:verify
      - task: shell:verify
      - task: claude:verify
      - task: test:parallel
```

### Pattern: `.taskrc.yml` for Project Defaults

```yaml
# .taskrc.yml (project root)
failfast: true      # stop verify chain on first failure
```

### Pattern: Parallel Subsystem Verify via deps

```yaml
# Fast parallel checks (independent subsystems)
verify:fast:
  deps:
    - task: rust:format:check
    - task: rust:clippy:check
    - task: shell:verify
    - task: claude:verify
```

### Pattern: `--force` for Cache Reset

Instead of `rm -rf .task/`, use `task --force verify` to bypass all caching for one run.

---

## Sources

- https://taskfile.dev/docs/reference/cli — complete CLI flag reference
- https://taskfile.dev/docs/reference/schema — Taskfile, Task, Command, Include schema
- https://taskfile.dev/docs/reference/config — .taskrc.yml options and precedence
- https://taskfile.dev/docs/reference/templating — variables, functions, Go template syntax
- https://taskfile.dev/usage/ — usage patterns, includes, caching, loops, defer
- https://taskfile.dev/docs/changelog — version history for features
- go-task issues: #2181 (generates glob), #852 (run:once), #860 (TASKFILE_DIR)
