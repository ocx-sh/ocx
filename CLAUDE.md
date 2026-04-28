# CLAUDE.md

This file guide Claude Code (claude.ai/code) when work in this repo.

## What is OCX

OCX = Rust package manager. Use OCI registries (Docker Hub, GHCR, private) as storage for pre-built binaries. "Backend tool" for other tools (GitHub Actions, Bazel, Python scripts), not end users. Binary named `ocx`.

## Project Identity

Full product vision, competitive positioning ("why not Homebrew / Nix / ORAS / mise"), target users, differentiators, use cases → [`product-context.md`](./.claude/rules/product-context.md). Consult when reason about project direction, scope trade-offs, ADR motivation, research framing, or anywhere product context shape technical decision. Canonical OCX product context — keep current (update protocol at bottom of same file).

## Rule Catalog

Before plan, research, or architectural decision, scan "By concern" in catalog below. Auto-loaded rules (via path globs) fire when edit matching files; catalog covers cases needing guidance *before* file open.

@.claude/rules.md

## Build & Development Commands

**Task runner**: [`task`](https://taskfile.dev) (Taskfile v3) primary runner. **Always check tasks with `task --list` before invent ad-hoc commands.** Taskfiles tree-structured: root (`taskfile.yml`), subsystem dirs (`test/`, `website/`, `.claude/`, `mirror-sdk-py/`), `taskfiles/*.taskfile.yml` for cross-cutting concerns.

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

**Cargo commands** (when need finer control):
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

**Always run `task verify` after implementation done.** Always run `cargo fmt` before commit.

**Lint tooling first-time setup** (one-off): `shellcheck`, `shfmt`, `lychee` managed by OCX itself via pinned local index at `.ocx/index/`. Run `ocx index update shellcheck shfmt lychee && ocx install --select shellcheck:0.11 && ocx install --select shfmt:3 && ocx install --select lychee:0` once. `lychee` powers `task claude:lint:links` (cross-reference check scoped to `.claude/`, `CLAUDE.md`, `AGENTS.md`), available once `mirrors/lychee/` mirror synced.

**Taskfile conventions**: subsystem verify tasks (`rust:verify`, `shell:verify`, `claude:verify`) = AI dev-loop gates — run subsystem gate for code changed. Full `task verify` runs only as final gate before commit. Verify pipeline two phases: parallel lint (`deps:`) then sequential build+test (`cmds:`). Reusable tool templates use `ocx.taskfile.yml` included with different `vars:`. Full conventions → [subsystem-taskfiles.md](./.claude/rules/subsystem-taskfiles.md).

## Architecture

**Workspace layout**: Four crates — `crates/ocx_lib` (core lib), `crates/ocx_cli` (thin CLI shell, package name `ocx`), `crates/ocx_mirror` (mirror tool), `crates/ocx_schema` (JSON schema gen, build-only). Rust edition 2024, resolver v3.

**Patched dependency**: `oci-client` patched to local git submodule at `external/rust-oci-client`.

**Subsystem context**: Each major subsystem has detailed context rule in `.claude/rules/subsystem-*.md` that auto-loads when work on matching files:

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

**Read relevant subsystem rule before work on code in that area.**

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `OCX_HOME` | Root data directory | `~/.ocx` |
| `OCX_DEFAULT_REGISTRY` | Default registry for short identifiers | `ocx.sh` |
| `OCX_INSECURE_REGISTRIES` | Comma-separated HTTP-only registries | (empty) |
| `OCX_OFFLINE` | Disable all network access; tag→digest must resolve locally or be pinned | false |
| `OCX_REMOTE` | Route mutable lookups to remote registry; pure queries never write local index | false |
| `OCX_NO_UPDATE_CHECK` | Disable update check notification | false |
| `OCX_NO_MODIFY_PATH` | Disable shell profile modification during install | false |

## Acceptance Test Structure

Tests live in `test/` using pytest + Docker Compose (registry:2 on localhost:5000). Full fixture reference + patterns → [subsystem-tests.md](./.claude/rules/subsystem-tests.md).

## Deep Context

- `.claude/rules/product-context.md` — Product vision, positioning, competitive analysis, use cases (auto-loads on website/agents/skill paths)
- `.claude/rules/arch-principles.md` — Design principles, glossary, ADR index (auto-loads on Rust files)
- `website/src/docs/user-guide.md` — Primary conceptual doc: three-store architecture, versioning, locking, auth


## Core Principles

Eight principles distill every rule, skill, standard in framework. Follow them, everything else follows.

### 1. Understand First

Read before write. Grep before create. Never modify code not read — before change function, grep all callers. Check what exists before build new.

### 2. Prove It Works

Write tests for customer use case first. Run before commit. Every bug fix get regression test. All quality gates must pass — tests, linter, types, build.

### 3. Keep It Safe

No secrets in code — use env vars or secret managers. Validate all external input. Parameterized queries only. Least privilege everywhere. Flag vulnerabilities immediately.

### 4. Keep It Simple

Small functions, single responsibility. No premature abstraction — three similar lines beat bad helper. Delete dead code. Avoid `any` types. Fix warnings before commit. Comments explain *why*, never *what* — no comments that restate code. Assume senior engineer as reader.

### 5. Don't Repeat Yourself

Check `.claude/skills/` before ad-hoc gen. Follow existing patterns in codebase. Single source of truth for business logic. Extract only when duplication real, not incidental.

### 6. Ship It

Work on branch, never main. Commit iteratively. **Never push to remote** — human decide when push. Push triggers CI, real cost.

### 7. Leave a Trail

Planning artifacts go in `./.claude/artifacts/`. Document architectural decisions in ADRs. Name things so next person understand.

### 8. Learn and Adapt

When get user feedback or corrections, evaluate if insight should persist as AI config update (rules, skills, agents) — not just memory. Patterns, conventions, quality standards belong in config so apply systematically.

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

**Commits**: Use [Conventional Commits](https://www.conventionalcommits.org/) format (e.g., `feat:`, `fix:`, `refactor:`, `ci:`, `chore:`). Scopes optional. No `Co-Authored-By` trailers. Use `chore:` for AI settings, skills, CLAUDE.md, tooling files that should not appear in changelog.

**During development**: Use `task checkpoint` to save progress. Amends all changes into single "Checkpoint" commit on feature branch.

**Landing a feature**: When feature done, run `/finalize` to clean branch history into sequence of Conventional Commits ready to fast-forward onto `main`. Two-phase model (`/commit` during dev, `/finalize` before landing) → [workflow-git.md](./.claude/rules/workflow-git.md). Manual fallback:
1. Amend checkpoint commit with proper conventional commit message: `git commit --amend -m "feat: ..."`
2. Rebase feature branch onto `main`: `git rebase main`
3. Switch to `main`: `git checkout main`
4. Fast-forward merge: `git merge --ff-only <branch>`
5. Switch back to worktree branch: `git checkout <branch>`

**Planning flow**: ADR → Design Spec → Plan → Implementation

**Artifacts**: All planning docs stored in `./.claude/artifacts/`.
**Templates**: Templates for markdown files in `./.claude/templates/artifacts/`.

> **Note:** Planning artifacts internal process, no replace proper documentation in website or code comments.

| Type | Pattern | Example |
|------|---------|---------|
| Architecture | `adr_[topic].md` | `adr_database_choice.md` |
| System Design | `system_design_[component].md` | `system_design_api.md` |
| Design | `design_spec_[component].md` | `design_spec_login_form.md` |
| Plan | `plan_[task].md` | `plan_api_refactor.md` |
| Security Audit | `security_audit_[date].md` | `security_audit_2025-01.md` |

## Skills & Personas

Persona skills (`/architect`, `/builder`, `/qa-engineer`, `/security-auditor`, `/code-check`, `/swarm-plan`, `/swarm-execute`, `/swarm-review`) and task skills live in `.claude/skills/`. Full map → "Skills by task topic" table in [.claude/rules.md](./.claude/rules.md). Check `.claude/skills/` before ad-hoc gen.

## Starting Work

Every task starts with [workflow-intent.md](./.claude/rules/workflow-intent.md) — classify work (feature, bug fix, refactoring), check GitHub for related issues/PRs, then follow appropriate workflow. Also: [workflow-feature.md](./.claude/rules/workflow-feature.md), [workflow-bugfix.md](./.claude/rules/workflow-bugfix.md), [workflow-refactor.md](./.claude/rules/workflow-refactor.md).