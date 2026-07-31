# CLAUDE.md

Guide Claude Code (claude.ai/code) in this repo.

## What is OCX

OCX = Rust package manager. OCI registries (Docker Hub, GHCR, private) as storage for pre-built binaries. "Backend tool" for other tools (GitHub Actions, Bazel, Python scripts), not end users. Binary named `ocx`.

## Current State

Early stage. Core lib + CLI implemented.

### Stability tiers

**Internal code structure has no stability at all.** Crate layout, module paths, type names, function signatures, enum shapes — all free to change. Never add a compat shim, deprecation window, re-export alias, or `_v2` name for an internal refactor. Rename in place and delete the old form as if it never existed. `ocx_lib` is not a published library; the binary is the only consumer.

**Interfaces are the CLI surface and every wire/persisted format** — command and flag grammar, exit codes, package metadata, OCI manifests, `ocx.lock`, the index format, `ocx.toml`. These are real contracts: other tools and published artifacts depend on them, so a change here is a decision, not a refactor.

**Even interfaces break pre-1.0.** A break is announced in the changelog and nowhere else — no migration prose in user docs, no dual-form parsing, no warning schedule. The one hard exception: already-published packages must keep resolving, so metadata and OCI manifest changes stay backward compatible on the read path.

Practical test: if only this repo can observe the change, just make it. If a published artifact or someone's script can observe it, weigh it — then still just make it, and write the changelog line — which means the commit subject (see below), never the file.

### ⛔ Never edit `CHANGELOG.md`

`CHANGELOG.md` is **generated**, at release time, by `git-cliff` from the commit history (`task release:prepare`). Editing it by hand is always wrong: the next release overwrites the file, so a hand-written entry is deleted, and until then it is a second source of truth that reads as authoritative. No `docs(changelog):` commits. Do not create the file, append to it, or "fix" an entry in it — not for a feature, not for a breaking change, not when a plan or a rule says to write a CHANGELOG line.

**The changelog entry is the commit subject.** `cliff.toml` renders one bullet per commit from its subject line alone — the body is never read — with the scope as `*(scope)*` and `!` as **BREAKING**. So the subject is the user-facing sentence: write it for someone reading release notes, and put the reasoning in the body where it belongs. `chore:` is excluded from the changelog, which is why AI config and tooling use it.

## ⛔ MODEL POLICY — NON-NEGOTIABLE

Applies to EVERY subagent spawn (Agent tool, Workflow `agent()` incl. ultracode, swarm skills). Always set `model` explicitly — never rely on inherit (a Fable main loop would silently spawn Fable workers).

| Task | Model |
|---|---|
| **Security review, code review, adversarial/verification passes** | **Opus 5** (`opus`) |
| **Non-mechanical implementation** — multi-subsystem, async/concurrency, error + exit-code semantics, OCI/wire-format or serializer work, auth/SSRF/credential paths | **Opus 5** (`opus`) |
| ADR / architecture decisions; or Sonnet demonstrably fell short twice on the same subtask | Opus (`opus`) — may fan work back out to Sonnet workers |
| **Default** — exploration, codebase search, research, web fetch, docs, mechanical edits, test scaffolding, planning workers | **Sonnet 5** (`sonnet`) |
| Final synthesis/decision over results multiple agents prepared (research + context pre-digested) | Fable — main loop / last instance only; (near-)NEVER as subagent; prefer Opus even here |

- **Never** Fable for review, research, implementation. Scale **out** (parallel workers with crisp handovers: goal, inputs, output contract) *and* **up** on the review/correctness axis — a cheap review of a security diff is a false economy.
- "Mechanical" = local change, shape already decided (rename, doc fix, fixture, single-file edit against an existing pattern). If the *design* is still open, it is Opus.
- **Cross-model (Codex) reviews**: `luna` (trivial) / `terra` (**default** — cost-efficient, use in small review loops too, not just high tiers) / `sol` (max-tier one-way-door gates). One-shot adversarial pass, no cross-family looping. See "Cross-model model tiers" in [workflow-swarm.md](./.claude/rules/workflow-swarm.md).

## Project Identity

Vision/positioning/competitors/users/use cases → [`product-context.md`](./.claude/rules/product-context.md). Canonical product context — keep current (update protocol at bottom of same file).

## Rule Catalog

Before plan/research/architectural decision, scan "By concern" in catalog. Auto-loaded rules fire on file edit; catalog covers cases needing guidance *before* file open.

@.claude/rules.md

## Build & Development

Task runner [`task`](https://taskfile.dev) (Taskfile v3). **Run `task --list` before invent ad-hoc commands.** Common: `task` (fast check), `task verify` (full gate), `task rust:verify`, `task test`, `task checkpoint`. Cargo OK for finer control. Always `cargo fmt` before commit, `task verify` after implementation. Conventions → [subsystem-taskfiles.md](./.claude/rules/subsystem-taskfiles.md).

**Project toolchain.** `ocx.toml` lists `go-task`, `shellcheck`, `shfmt`, `lychee`, `bun`, `uv`, `git-cliff`. [direnv](https://direnv.net) loads them onto `PATH` via `.envrc` (`eval "$(ocx direnv export)"`); CI bootstraps the same set via the `setup-ocx` action. Taskfiles call the tools directly — no `ocx package exec` wrapping. After editing `ocx.toml`, direnv reloads automatically (`watch_file ocx.toml ocx.lock`). For one-off overrides — e.g. testing a freshly built ocx, or invoking from a shell that hasn't allowed direnv — prefix with `direnv exec . <cmd>` or `ocx run -- <cmd>`.

Single acceptance test:
```sh
cd test && uv run pytest tests/test_install.py::test_install_creates_candidate_symlink -v
```

Lint tooling setup (one-off): `task ocx:index-update` populates `.ocx/index/` for every tool in `ocx.toml`; first `direnv allow` (or `task` invocation) materializes the symlinks under `~/.ocx/`.

## Architecture

Four crates: `crates/ocx_lib` (core), `crates/ocx_cli` (thin CLI, pkg `ocx`), `crates/ocx_schema` (build-only JSON schema), `crates/ocx_shim` (Windows launcher shim). The mirror tool lives in its own repo: [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror) (vendors ocx as submodule). Rust 2024, resolver v3. `oci-client` patched to `external/rust-oci-client`.

Subsystem rules auto-load on path match. Read relevant one before work on that area:

| Subsystem | Rule | Scope |
|-----------|------|-------|
| OCI registry/index | [subsystem-oci.md](./.claude/rules/subsystem-oci.md) | `crates/ocx_lib/src/oci/**` |
| Storage/symlinks | [subsystem-file-structure.md](./.claude/rules/subsystem-file-structure.md) | `crates/ocx_lib/src/file_structure/**` |
| Package metadata | [subsystem-package.md](./.claude/rules/subsystem-package.md) | `crates/ocx_lib/src/package/**` |
| Package manager | [subsystem-package-manager.md](./.claude/rules/subsystem-package-manager.md) | `crates/ocx_lib/src/package_manager/**` |
| CLI commands/API | [subsystem-cli.md](./.claude/rules/subsystem-cli.md) | `crates/ocx_cli/src/**` |
| Script host API | [subsystem-script.md](./.claude/rules/subsystem-script.md) | `crates/ocx_lib/src/script/**` |
| Acceptance tests | [subsystem-tests.md](./.claude/rules/subsystem-tests.md) | `test/**` |
| Website/docs | [subsystem-website.md](./.claude/rules/subsystem-website.md) | `website/**` |

## Environment Variables

Canonical reference → [`website/src/docs/reference/environment.md`](./website/src/docs/reference/environment.md).

## Deep Context

- [`product-context.md`](./.claude/rules/product-context.md) — vision, competitors, use cases
- [`arch-principles.md`](./.claude/rules/arch-principles.md) — design principles, glossary, ADR index (auto-loads on Rust)
- [`website/src/docs/user-guide.md`](./website/src/docs/user-guide.md) — three-store architecture, versioning, locking, auth

## Core Principles

Eight principles distill every rule, skill, standard. Deep dive: [`quality-core.md`](./.claude/rules/quality-core.md) (SOLID/DRY/KISS/YAGNI).

### 1. Understand First
Read before write. Grep before create. Never modify unread code — grep all callers before change function.

### 2. Prove It Works
Tests for customer use case first. Run before commit. Regression test per bug fix. All gates pass — tests, linter, types, build.

### 3. Keep It Safe
No secrets in code — env vars / secret managers. Validate external input. Parameterized queries only. Least privilege. Flag vulnerabilities immediately.

### 4. Keep It Simple
Small functions, single responsibility. No premature abstraction — three similar lines beat bad helper. Delete dead code. Avoid `any` types. Fix warnings. Comments explain *why*, never *what*.

### 5. Don't Repeat Yourself
Check `.claude/skills/` before ad-hoc gen. Follow existing patterns. Single source of truth for business logic. Extract on real duplication, not incidental.

### 6. Ship It
Work on branch, never main. Commit iteratively. **Never push to remote** — human decides. Push triggers CI, real cost.

### 7. Leave a Trail
Planning artifacts → `./.claude/artifacts/`. ADRs for architectural decisions. Name things so next person understand.

### 8. Learn and Adapt
On user feedback or corrections, evaluate if insight should persist as AI config update (rules/skills/agents) — not just memory.

## Tech Stack

@.claude/rules/product-tech-strategy.md

## Workflow

**Worktrees**: Four git worktrees, fixed branches:

| Directory | Branch |
|-----------|--------|
| `ocx` | `goat` |
| `ocx-evelynn` | `evelynn` |
| `ocx-sion` | `sion` |
| `ocx-soraka` | `soraka` |

**Parallel Agents**: Implementation agents run in git worktrees under `.agents/worktrees/<slug>` (gitignored). Plans MUST be parallel-capable by design: file-disjoint work packages, explicit dependency DAG, contract-first stubs. Details → [workflow-swarm.md](./.claude/rules/workflow-swarm.md) "Parallel Worktree Execution".

Commits: [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `refactor:`, `ci:`, `chore:`). No `Co-Authored-By` trailers. `chore:` for AI settings/CLAUDE.md/tooling (no changelog).

Dev cycle: `task checkpoint` (amends single "Checkpoint" commit). Landing: `/finalize` (clean → conventional commits → fast-forward onto main). Full → [workflow-git.md](./.claude/rules/workflow-git.md).

Planning flow: ADR → Design Spec → Plan → Implementation. Artifacts → `./.claude/artifacts/`; templates → `./.claude/templates/artifacts/`. Filename patterns: `adr_<topic>.md`, `system_design_<comp>.md`, `design_spec_<comp>.md`, `plan_<task>.md`, `security_audit_<date>.md`.

## Skills & Personas

Persona skills (`/architect`, `/builder`, `/qa-engineer`, `/security-auditor`, `/code-check`, `/swarm-plan`, `/swarm-execute`, `/swarm-review`) + task skills in `.claude/skills/`. Map → "Skills by task topic" in [.claude/rules.md](./.claude/rules.md). Check before ad-hoc gen.

## Starting Work

Every task starts with [workflow-intent.md](./.claude/rules/workflow-intent.md) — classify (feature/bugfix/refactor), check GitHub for related issues/PRs, route to [workflow-feature.md](./.claude/rules/workflow-feature.md) / [workflow-bugfix.md](./.claude/rules/workflow-bugfix.md) / [workflow-refactor.md](./.claude/rules/workflow-refactor.md).
