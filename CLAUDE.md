# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is OCX

OCX is a Rust-based package manager that uses OCI registries (Docker Hub, GHCR, private registries) as storage for pre-built binaries. It is a "backend tool" designed for use by other tools (GitHub Actions, Bazel, Python scripts) rather than end users directly. The binary is named `ocx`.

## Project Identity

For the full product vision, competitive positioning ("why not Homebrew / Nix / ORAS / mise"), target users, differentiators, and use cases, read [`product-context.md`](./.claude/rules/product-context.md). Consult it when reasoning about project direction, scope trade-offs, ADR motivation, research framing, or anything where product context shapes a technical decision. It is the canonical OCX product context — keep it current (the update protocol is at the bottom of the same file).

## Rule Catalog

Before planning, researching, or making architectural decisions, consult [[.claude/rules.md](./.claude/rules.md)](.claude/rules.md) — the authoritative catalog of what rules exist in this project and when each applies. Auto-loaded rules (via path globs) only fire when you edit matching files; the catalog covers cases where you need guidance *before* any file is open. Always scan "By concern" in plan and research phases.

## Build & Development Commands

**Task runner**: [`task`](https://taskfile.dev) (Taskfile v3) is the primary task runner. **Always check available tasks with `task --list` before inventing ad-hoc commands.** Taskfiles are tree-structured: root (`taskfile.yml`), subsystem dirs (`test/`, `website/`, `.claude/`, `mirror-sdk-py/`), and `taskfiles/*.taskfile.yml` for cross-cutting concerns.

**Key workflows:**
```sh
task                           # fast check (format + clippy + cargo check)
task verify                    # full quality gate (parallel lint, then build + tests)
task --force verify            # bypass caching — run everything
task rust:verify               # Rust-only gate (format, clippy, license, build, unit tests)
task shell:verify              # shell-only gate (shellcheck + shfmt)
task checkpoint                # save progress (amends into single "Checkpoint" commit)
task rust:build                # release binary
task test                      # build + registry + all acceptance tests
task test:quick                # skip binary rebuild
task test:parallel             # pytest-xdist (-n auto)
task coverage                  # LLVM coverage report
task coverage:open             # open HTML report in browser
task website:serve             # VitePress dev server
task website:build             # full website build (schema + recordings + sbom + catalog + vitepress)
```

**Cargo commands** (when you need finer control):
```sh
cargo check                    # fast check
cargo build --release -p ocx   # release CLI binary
cargo fmt                      # format (max_width=120, see rustfmt.toml)
cargo clippy --workspace       # lint
cargo nextest run --workspace  # all unit tests
cargo nextest run -p ocx_lib <test_name>         # single test by name
cargo test -p ocx_lib -- <test_name> --nocapture # with output
```

**Single acceptance test:**
```sh
cd test && uv run pytest tests/test_install.py::test_install_creates_candidate_symlink -v --no-build
```

**Always run `task verify` after completing an implementation.** Always run `cargo fmt` before committing.

**Lint tooling first-time setup** (one-off): `shellcheck`, `shfmt`, and `lychee` are managed by OCX itself via the pinned local index at `.ocx/index/`. Run `ocx index update shellcheck shfmt lychee && ocx install --select shellcheck:0.11 && ocx install --select shfmt:3 && ocx install --select lychee:0` once. `lychee` powers `task claude:lint:links` (cross-reference check scoped to `.claude/`, `CLAUDE.md`, `AGENTS.md`) and becomes available once the `mirrors/lychee/` mirror is synced.

**Taskfile conventions**: subsystem verify tasks (`rust:verify`, `shell:verify`, `claude:verify`) are the AI development-loop gates — run the subsystem gate for the code you changed. Full `task verify` runs only as the final gate before commit. The verify pipeline has two phases: parallel lint (`deps:`) then sequential build+test (`cmds:`). Reusable tool templates use `ocx.taskfile.yml` included with different `vars:`. See [subsystem-taskfiles.md](./.claude/rules/subsystem-taskfiles.md) for full conventions.

## Architecture

**Workspace layout**: Four crates — `crates/ocx_lib` (core library), `crates/ocx_cli` (thin CLI shell, package name `ocx`), `crates/ocx_mirror` (mirror tool), `crates/ocx_schema` (JSON schema generation, build-only). Rust edition 2024, resolver v3.

**Patched dependency**: `oci-client` is patched to a local git submodule at `external/rust-oci-client`.

**Subsystem context**: Each major subsystem has a detailed context rule in `.claude/rules/subsystem-*.md` that loads automatically when working on matching files:

| Subsystem | Rule | Scope |
|-----------|------|-------|
| OCI registry/index | [subsystem-oci.md](./.claude/rules/subsystem-oci.md) | `crates/ocx_lib/src/oci/**` |
| Storage/symlinks | [subsystem-file-structure.md](./.claude/rules/subsystem-file-structure.md) | `crates/ocx_lib/src/file_structure/**` |
| Package metadata | [subsystem-package.md](./.claude/rules/subsystem-package.md) | `crates/ocx_lib/src/package/**` |
| Package manager | [subsystem-package-manager.md](./.claude/rules/subsystem-package-manager.md) | `crates/ocx_lib/src/package_manager/**` |
| CLI commands/API | [subsystem-cli.md](./.claude/rules/subsystem-cli.md) | `crates/ocx_cli/src/**` |
| Mirror tool | [subsystem-mirror.md](./.claude/rules/subsystem-mirror.md) | `crates/ocx_mirror/**` |
| Acceptance tests | [subsystem-tests.md](./.claude/rules/subsystem-tests.md) | `test/**` |
| Website/docs | [subsystem-website.md](./.claude/rules/subsystem-website.md) | `website/**` |

**Read the relevant subsystem rule before working on code in that area.**

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

Tests live in `test/` using pytest + Docker Compose (registry:2 on localhost:5000). See [subsystem-tests.md](./.claude/rules/subsystem-tests.md) for full fixture reference and patterns.

## Deep Context

- `.claude/rules/product-context.md` — Product vision, positioning, competitive analysis, use cases (auto-loads on website/agents/skill paths)
- `.claude/rules/arch-principles.md` — Design principles, glossary, ADR index (auto-loads on Rust files)
- `website/src/docs/user-guide.md` — Primary conceptual doc: three-store architecture, versioning, locking, auth


## Core Principles

These eight principles distill every rule, skill, and standard in this framework. Follow them and everything else follows.

### 1. Understand First

Read before writing. Grep before creating. Never modify code you haven't read. Check what exists before building something new.

### 2. Prove It Works

Write tests for the customer use case first. Run them before committing. Every bug fix gets a regression test. All quality gates must pass — tests, linter, types, build.

### 3. Keep It Safe

No secrets in code — use env vars or secret managers. Validate all external input. Parameterized queries only. Least privilege everywhere. Flag vulnerabilities immediately.

### 4. Keep It Simple

Small functions, single responsibility. No premature abstraction — three similar lines beat a bad helper. Delete dead code. Avoid `any` types. Fix warnings before committing. Comments explain *why*, never *what* — do not add comments that restate code. Assume a senior engineer as the reader.

### 5. Don't Repeat Yourself

Check `.claude/skills/` before ad-hoc generation. Follow existing patterns in the codebase. Single source of truth for business logic. Extract only when duplication is real, not incidental.

### 6. Ship It

Work on a branch, never main. Commit iteratively. **Never push to remote** — the human decides when to push. Pushing triggers CI, which has real cost.

### 7. Leave a Trail

Planning artifacts go in `./.claude/artifacts/`. Document architectural decisions in ADRs. Name things so the next person understands.

### 8. Learn and Adapt

When receiving user feedback or corrections, evaluate whether the insight should also be persisted as an AI config update (rules, skills, agents) — not just a memory. Patterns, conventions, and quality standards belong in the config so they apply systematically.

## Tech Stack

@.claude/rules/product-tech-strategy.md

## Workflow

**Worktrees**: Four git worktrees with fixed branch names:

| Directory | Branch |
|-----------|--------|
| `ocx` | `goat` |
| `ocx-evelynn` | `evelynn` |
| `ocx-sion` | `sion` |
| `ocx-soraka` | `soraka` |

**Commits**: Use [Conventional Commits](https://www.conventionalcommits.org/) format (e.g., `feat:`, `fix:`, `refactor:`, `ci:`, `chore:`). Scopes are optional. Do not add `Co-Authored-By` trailers to commit messages. Use `chore:` for changes to AI settings, skills, CLAUDE.md, and other tooling files that should not appear in the changelog.

**During development**: Use `task checkpoint` to save progress. This amends all changes into a single "Checkpoint" commit on the feature branch.

**Landing a feature**: When a feature is finished, run `/finalize` to clean up the branch history into a sequence of Conventional Commits ready to fast-forward onto `main`. See [workflow-git.md](./.claude/rules/workflow-git.md) for the two-phase model (`/commit` during development, `/finalize` before landing). Manual fallback:
1. Amend the checkpoint commit with a proper conventional commit message: `git commit --amend -m "feat: ..."`
2. Rebase the feature branch onto `main`: `git rebase main`
3. Switch to `main`: `git checkout main`
4. Fast-forward merge: `git merge --ff-only <branch>`
5. Switch back to the worktree branch: `git checkout <branch>`

**Planning flow**: ADR → Design Spec → Plan → Implementation

**Artifacts**: All planning docs stored in `./.claude/artifacts/`.
**Templates**: Templates for markdown files in `./.claude/templates/artifacts/`.

> **Note:** Planning artifacts are an internal process and do not replace proper documentation in the website or code comments.

| Type | Pattern | Example |
|------|---------|---------|
| Architecture | `adr_[topic].md` | `adr_database_choice.md` |
| System Design | `system_design_[component].md` | `system_design_api.md` |
| Design | `design_spec_[component].md` | `design_spec_login_form.md` |
| Plan | `plan_[task].md` | `plan_api_refactor.md` |
| Security Audit | `security_audit_[date].md` | `security_audit_2025-01.md` |

## Skills & Personas

Persona skills (`/architect`, `/builder`, `/qa-engineer`, `/security-auditor`, `/code-check`, `/swarm-plan`, `/swarm-execute`, `/swarm-review`) and task skills live in `.claude/skills/`. See the "Skills by task topic" table in [[.claude/rules.md](./.claude/rules.md)](.claude/rules.md) for the full map. Skills are auto-suggested via `.claude/skills/skill-rules.json`; check `.claude/skills/` before ad-hoc generation.

## Starting Work

Every task starts with [workflow-intent.md](./.claude/rules/workflow-intent.md) — classify the work (feature, bug fix, or refactoring), check GitHub for related issues/PRs, then follow the appropriate workflow. See also: [workflow-feature.md](./.claude/rules/workflow-feature.md), [workflow-bugfix.md](./.claude/rules/workflow-bugfix.md), [workflow-refactor.md](./.claude/rules/workflow-refactor.md).
