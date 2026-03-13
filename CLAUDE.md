# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is OCX

OCX is a Rust-based package manager that uses OCI registries (Docker Hub, GHCR, private registries) as storage for pre-built binaries. It is a "backend tool" designed for use by other tools (GitHub Actions, Bazel, Python scripts) rather than end users directly. The binary is named `ocx`.

## Build & Development Commands

**Task runner**: [`task`](https://taskfile.dev) (Taskfile v3) is the primary task runner.

```sh
cargo check                    # fast check (also: `task`)
cargo build                    # debug build
cargo build --release -p ocx      # release CLI binary
cargo fmt                      # format (max_width=120, see rustfmt.toml)
cargo clippy --workspace       # lint
```

**Rust tests** (cargo-nextest is used in CI):
```sh
cargo nextest run --workspace                    # all tests
cargo nextest run -p ocx_lib <test_name>         # single test by name
cargo test -p ocx_lib -- <test_name> --nocapture # with output
```

**Acceptance tests** (Python/pytest, requires Docker for registry:2):
```sh
task test              # build binary + start registry + run all pytest tests
task test:quick        # skip binary rebuild
task test:parallel     # run with pytest-xdist (-n auto)

# Single test:
cd test && uv run pytest tests/test_install.py::test_install_creates_candidate_symlink -v --no-build
```

**Coverage**: `task coverage` (cargo-llvm-cov), `task coverage:open` to view HTML report.

**Verification**: `task verify` runs format check, clippy, build, unit tests, and acceptance tests. **Always run `task verify` after completing an implementation** to confirm nothing is broken.

**Before committing**: Always run `cargo fmt` before creating a commit to ensure code is properly formatted.

## Architecture

**Workspace layout**: Two crates — `crates/ocx_lib` (core library) and `crates/ocx_cli` (thin CLI shell using clap, package name `ocx`). Rust edition 2024, resolver v3.

**Patched dependency**: `oci-client` is patched to a local git submodule at `external/rust-oci-client`.

**Key subsystems in `ocx_lib`**:

- **`file_structure`** — Content-addressed local storage layout under `~/.ocx` (configurable via `OCX_HOME`). Composed of `ObjectStore`, `IndexStore`, `InstallStore`, `TempStore`. All OCI identifier components are slugified via `to_relaxed_slug()` before becoming filesystem paths.
- **`oci`** — OCI registry client, digest types, platform matching, identifiers. `index/` contains `RemoteIndex` (in-memory cached), `LocalIndex` (filesystem-backed), and the public `Index` wrapper with `select()` returning `SelectResult` enum.
- **`package_manager`** — Facade over `FileStructure` + `Index` + `Client`. Task methods in `package_manager/tasks/` (find, install, uninstall, select, deselect, find_or_install). Three-layer error model: `Error` → `PackageError` → `PackageErrorKind`. All package-specific errors flow through `PackageErrorKind`.
- **`reference_manager`** — Manages install symlinks + back-references for GC. Always use `ReferenceManager` for install symlinks (not raw `symlink::update`).

**CLI layer** (`ocx_cli`):
- `app/context.rs` — `Context` struct: holds `FileStructure`, `Index`, `PackageManager`, `Api`, OCI client. Created once per command invocation.
- `command/` — One file per CLI subcommand. Commands call `context.manager()` methods and build report data from task return values (never from raw CLI args alone).
- `api/` — Output formatting (JSON vs plain text) via `context.api().report_*()`. Each `api/data/` type implements `Reportable` with a single `print_table` call. See @.claude/rules/cli-api-patterns.md for the full contract.

**CLI command reference**: For any task involving CLI commands, user workflows, flags, or command behavior, see @.claude/rules/cli-commands.md.

**Patterns**:
- Commands use `context.manager().find_or_install_all(...)` with auto-install on `PackageNotFound` (unless offline).
- Environment resolution: `env::Env::clean()` + `metadata_env.resolve_into_env(content_path, &mut env)`.
- Progress reporting: `tracing` `info_span!` + `tracing-indicatif` `IndicatifLayer`. No custom progress abstraction.
- Error handling: `ocx_lib::Error` with `Error::InternalFile(path, e)` for file errors.
- Async runtime: tokio with `#[tokio::main]`, `JoinSet` for parallel tasks.

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `OCX_HOME` | Root data directory | `~/.ocx` |
| `OCX_DEFAULT_REGISTRY` | Default registry for short identifiers | `ocx.sh` |
| `OCX_INSECURE_REGISTRIES` | Comma-separated HTTP-only registries | (empty) |
| `OCX_OFFLINE` | Offline mode flag | false |
| `OCX_REMOTE` | Use remote index directly | false |
| `OCX_NO_UPDATE_CHECK` | Disable update check notification | false |
| `OCX_NO_MODIFY_PATH` | Disable shell profile modification during install | false |

## Acceptance Test Structure

Tests live in `test/` using pytest + Docker Compose (registry:2 on localhost:5000). Test isolation via UUID-prefixed repo names and isolated `OCX_HOME` per test (`tmp_path`). Binary build and registry startup happen in `pytest_sessionstart`. Key fixtures: `ocx` (runner), `published_package`, `published_two_versions`, `unique_repo`.

## Documentation (website/src/docs/)

The `website/` directory contains a VitePress docs site. Key pages for understanding the product design:

- **[user-guide.md](website/src/docs/user-guide.md)** — The primary conceptual document. Covers:
  - **Three-store architecture**: `objects/` (immutable, content-addressed binaries — analogous to Nix store/Git objects), `index/` (local snapshot of registry metadata for offline/reproducibility), `installs/` (stable symlinks: `candidates/{tag}` for pinned versions, `current` as a floating pointer set by `ocx select`).
  - **Path resolution modes**: default (object store, auto-installs), `--candidate` (symlink, no auto-install), `--current` (symlink, no auto-install).
  - **Versioning**: semver-inspired tag hierarchy (build-tagged → rolling patch → minor → major → latest), cascading pushes via `--cascade`, OCI multi-platform manifests for cross-arch.
  - **Locking strategy**: digest references for absolute reproducibility; local index snapshot as implicit lock; bundled index inside GitHub Actions/Bazel rules as a two-level lock.
  - **Authentication**: layered approach — `OCX_AUTH_<REGISTRY>_*` env vars checked first, then Docker credentials (`~/.docker/config.json`).
- **[faq.md](website/src/docs/faq.md)** — Platform-specific behavior: macOS ad-hoc code signing (auto-applied to Mach-O binaries after extraction), Windows executable resolution via `PATHEXT`.
- **[reference/command-line.md](website/src/docs/reference/command-line.md)** — Full CLI command reference with all flags and options.
- **[reference/environment.md](website/src/docs/reference/environment.md)** — Complete environment variable reference including auth vars and truthy value parsing.


## Core Principles

These seven principles distill every rule, skill, and standard in this framework. Follow them and everything else follows.

### 1. Understand First

Read before writing. Grep before creating. Never modify code you haven't read. Check what exists before building something new.

### 2. Prove It Works

Write tests for the customer use case first. Run them before committing. Every bug fix gets a regression test. All quality gates must pass — tests, linter, types, build.

### 3. Keep It Safe

No secrets in code — use env vars or secret managers. Validate all external input. Parameterized queries only. Least privilege everywhere. Flag vulnerabilities immediately.

### 4. Keep It Simple

Small functions, single responsibility. No premature abstraction — three similar lines beat a bad helper. Delete dead code. Avoid `any` types. Fix warnings before committing.

### 5. Don't Repeat Yourself

Check `.claude/skills/` before ad-hoc generation. Follow existing patterns in the codebase. Single source of truth for business logic. Extract only when duplication is real, not incidental.

### 6. Ship It

Work on a branch, never main. Commit iteratively. **Never push to remote** — the human decides when to push. Pushing triggers CI, which has real cost.

### 7. Leave a Trail

Planning artifacts go in `./.agents/artifacts/`. Track work with Beads (`bd` CLI). Document architectural decisions in ADRs. Name things so the next person understands.

## Tech Stack

@.claude/rules/tech-strategy.md

## Workflow

**Branching**: Always branch from `main`. Never commit directly to `main`.

**Commits**: Use [Conventional Commits](https://www.conventionalcommits.org/) format (e.g., `feat:`, `fix:`, `refactor:`, `ci:`, `chore:`). Scopes are optional. Do not add `Co-Authored-By` trailers to commit messages.

**Planning flow**: PR-FAQ → PRD → ADR → Design Spec → Plan → Implementation Beads

**Artifacts**: All planning docs stored in `./.agents/artifacts/`.
**Templates**: Templates for markdown files in `./.agents/templates/artifacts/`.

> **Note:** Planning artifacts are an internal process and do not replace proper documentation in the website or code comments.

| Type | Pattern | Example |
|------|---------|---------|
| Vision | `pr_faq_[feature].md` | `pr_faq_user_auth.md` |
| Requirements | `prd_[feature].md` | `prd_user_auth.md` |
| Architecture | `adr_[topic].md` | `adr_database_choice.md` |
| System Design | `system_design_[component].md` | `system_design_api.md` |
| Design | `design_spec_[component].md` | `design_spec_login_form.md` |
| Roadmap | `roadmap_[project].md` | `roadmap_mvp.md` |
| Plan | `plan_[task].md` | `plan_api_refactor.md` |
| Security Audit | `security_audit_[date].md` | `security_audit_2025-01.md` |
| Post-Mortem | `postmortem_[incident-id].md` | `postmortem_inc-2025-001.md` |

**Beads** (issue tracking — CLI saves 98% tokens vs MCP):

```bash
bd create "Task"                        # Create
bd ready                                # Find unblocked work
bd show <id>                            # View details
bd update <id> --status in_progress     # Claim
bd close <id>                           # Complete
bd sync                                 # Sync with git
```

See `beads-workflow` skill for complete command reference.

## Personas

| Command | Role | Use |
|---------|------|-----|
| `/architect` | Principal Architect | System design, ADRs |
| `/builder` | Software Engineer | Implementation, debugging, testing |
| `/qa-engineer` | QA Engineer | Test strategy, E2E, accessibility |
| `/security-auditor` | Security Auditor | Threat modeling, audits |
| `/ui-ux-designer` | UI/UX Designer | Interface design, a11y |
| `/code-check` | Codebase Auditor | SOLID, DRY, consistency audits |
| `/swarm-plan` | Planning Orchestrator | Parallel exploration, decomposition |
| `/swarm-execute` | Execution Orchestrator | Parallel workers, quality gates |
| `/swarm-review` | Adversarial Reviewer | Multi-perspective code review |

## Skills

Check `.claude/skills/` before ad-hoc generation. Skills are auto-suggested based on context via `.claude/skills/skill-rules.json`.
