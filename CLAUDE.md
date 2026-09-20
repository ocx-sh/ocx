# CLAUDE.md

Guide Claude Code (claude.ai/code) in this repo.

## What is OCX

OCX = Rust package manager. OCI registries (Docker Hub, GHCR, private) as storage for pre-built binaries. "Backend tool" for other tools (GitHub Actions, Bazel, Python scripts), not end users. Binary named `ocx`.

## Current State

Early stage. Core lib + CLI implemented.

### Stability tiers

**Internal code structure has no stability at all.** Crate layout, module paths, type names, function signatures, enum shapes — all free to change. Never add a compat shim, deprecation window, re-export alias, or `_v2` name for an internal refactor. Rename in place and delete the old form as if it never existed. No `ocx_*` crate is a published library; the binary is the only consumer.

**Ecosystem crates sit between the two: a crate a lockstep submodule consumer links.** `ocx_util`, `ocx_console`, `ocx_oci`, `ocx_trust`, `ocx_sign`, `ocx_config`, `ocx_index`, `ocx_package`. Breaking changes are allowed when justified — but the consumer is upgraded **in the same change series**, not afterwards, and `task satellite:verify` (a `verify-deep.yml` job) is what makes that an obligation rather than a label. A lockstep consumer does *not* promote a crate to interface. Tier table and rationale → [`adr_crate_split_workspace.md`](./.claude/artifacts/adr_crate_split_workspace.md) § "Stability tiers and the ecosystem contract".

**Interfaces are the CLI surface and every wire/persisted format** — command and flag grammar, exit codes, package metadata, OCI manifests, `ocx.lock`, the index format, `ocx.toml`. These are real contracts: other tools and published artifacts depend on them, so a change here is a decision, not a refactor.

**Even interfaces break pre-1.0.** A break is announced in the changelog and nowhere else — no migration prose in user docs, no dual-form parsing, no warning schedule. The one hard exception: already-published packages must keep resolving, so metadata and OCI manifest changes stay backward compatible on the read path.

**Batched-window carve-out.** A rename reaching dozens of files across docs, tests and downstream repos may ship one deprecation window instead of a hard break, on these terms: the old spelling stays as a *hidden* command — a clap alias is undetectable at parse time, so a warning needs its own hidden variant — it warns once on stderr and never on stdout, its removal release is named when the window opens, and every old spelling in flight lives in one `deprecated.rs` deleted whole, and is listed in that file's `RENAMED` — the authority `test/tests/test_deprecated_spellings.py` reads to sweep the repo for stale invocations, so a window opened without an entry there is a window nothing enforces. One window per release pair, not one per rename; still no migration prose in user docs. In flight, all deprecated in 0.6 and removed in 0.7: `ocx run` → `ocx exec`, `ocx package describe` → `ocx package description push`, `ocx package info` → `ocx package description pull`, and the `ocx package announce --package` flag → its positional.

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

Task runner [`task`](https://taskfile.dev) (Taskfile v3). **Run `task --list` before invent ad-hoc commands.** Common: `task` (fast check), `task verify:scoped` (per work package; escalates to full when it must), `task verify` (full gate), `task rust:verify`, `task test`, `task checkpoint`. Cargo OK for finer control. Always `cargo fmt` before commit, `task verify` (or a green `task verify:scoped`) after implementation. Conventions → [subsystem-taskfiles.md](./.claude/rules/subsystem-taskfiles.md).

**Project toolchain.** `ocx.toml` lists `actionlint`, `bun`, `cosign`, `git-cliff`, `go-task`, `lychee`, `shellcheck`, `shfmt`, `uv`. `ocx self setup` wires a per-prompt hook that puts them on `PATH` when you `cd` in and takes them off when you leave (bash, zsh, fish, PowerShell, elvish; `ocx.toml` and `ocx.lock` are reconciled each prompt, so an edit takes effect at the next one). CI bootstraps the same set via the `setup-ocx` action. Taskfiles call the tools directly — no `ocx package exec` wrapping. For one-off overrides — e.g. testing a freshly built ocx, or invoking from a shell with no hook — prefix with `ocx exec -- <cmd>`. Details → [getting-started.md](./website/src/docs/getting-started.md) § Project Toolchain.

Single acceptance test:
```sh
cd test && uv run pytest tests/test_install.py::test_install_creates_candidate_symlink -v
```

Lint tooling setup (one-off): the first `ocx pull` (or `task` invocation) materializes the symlinks under `~/.ocx/`. The toolchain resolves from `ocx.lock` alone — this repository keeps no committed index copy, and tool bumps go through `ocx add` / `ocx lock`.

## Architecture

20 workspace members (`members = ["crates/*"]`), Rust 2024, resolver v3. `scripts/crate_map.toml` is the dependency map and the only allowed-edge table; the split that produced this layout is [`adr_crate_split_workspace.md`](./.claude/artifacts/adr_crate_split_workspace.md). Tier in brackets.

| Crate | Owns |
|---|---|
| `ocx_exit` [interface] | Process-outcome vocabulary: `ExitCode` and the `error.detail` slug — an exit code is a CLI contract |
| `ocx_util` [ecosystem] | Domain-free primitives: fs, locking, extension traits, singleflight, TLS roots, archive, compression, path-context errors |
| `ocx_console` [ecosystem] | Presentation vocabulary: rendering, printer, theme, styles, progress, data interface |
| `ocx_oci` [ecosystem] | Distribution-spec-generic registry work: references, digests, manifests, transport, referrers, SSRF guard, auth |
| `ocx_trust` [ecosystem] | Signer-identity policy: `[[trust.policy]]`, tiered resolution, compiled identity rules |
| `ocx_sign` [ecosystem] | Supply-chain signing: keyless Sigstore sign, DSSE attest, verify, cosign simplesigning, SBOM referrers |
| `ocx_config` [ecosystem] | Resolved settings from files and environment: the config tiers, the managed tier, env-var vocabulary |
| `ocx_store` [internal] | The on-disk layout: three-tier CAS, symlink namespace, package materialisation, shim blobs |
| `ocx_index` [ecosystem] | The OCX resolution-index protocol and its local collection |
| `ocx_package` [ecosystem] | Package identity, metadata, versioning, cascade, authoring, publication |
| `ocx_shell` [internal] | Shell and CI export surface: export generation, per-prompt reconciliation, hook emission |
| `ocx_project` [internal] | The project tier: `ocx.toml`/`ocx.lock`, consent, mutation, per-prompt activation |
| `ocx_package_manager` [internal] | Resolution, install, environment composition, patches, launch, execution records |
| `ocx_announce` [internal] | Index publication: announce pipeline, forge drivers, index claim |
| `ocx_script` [internal] | The Starlark host API for `ocx package test --script` |
| `ocx_setup` [internal] | Self-install: bootstrap, env shim files, managed RC blocks, profile detection |
| `ocx_test_support` [internal] | Shared unit-test fixtures and the process-environment override seam — dev-dependency only |

Three are not tier crates: `ocx_cli` [interface] is the application layer (argv, context, commands, reports, and **all** error-to-exit-code classification, pkg `ocx`); `ocx_schema` [internal] generates JSON Schema at build time; `ocx_shim` [interface] is the Windows `.exe` launcher and its wire ABI.

The mirror tool lives in its own repo: [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror) (vendors ocx as submodule). Three deps patched to submodules under `external/`: `oci-client` (`rust-oci-client`), `docker_credential`, `sigstore` (`sigstore-rs`).

Subsystem rules auto-load on path match. Read relevant one before work on that area:

| Subsystem | Rule | Scope |
|-----------|------|-------|
| OCI registry/index | [subsystem-oci.md](./.claude/rules/subsystem-oci.md) | `crates/ocx_oci/**`, `crates/ocx_index/**`, `crates/ocx_sign/**` |
| Storage/symlinks | [subsystem-file-structure.md](./.claude/rules/subsystem-file-structure.md) | `crates/ocx_store/src/**` |
| Package metadata | [subsystem-package.md](./.claude/rules/subsystem-package.md) | `crates/ocx_package/src/**` |
| Package manager | [subsystem-package-manager.md](./.claude/rules/subsystem-package-manager.md) | `crates/ocx_package_manager/src/**` |
| CLI commands/API | [subsystem-cli.md](./.claude/rules/subsystem-cli.md) | `crates/ocx_cli/src/**` |
| Script host API | [subsystem-script.md](./.claude/rules/subsystem-script.md) | `crates/ocx_script/src/**` |
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

Dev cycle: `task checkpoint` (amends single "Checkpoint" commit). Landing: `/hex-finalize` (clean → conventional commits → fast-forward onto main). Full → [workflow-git.md](./.claude/rules/workflow-git.md).

Planning flow: ADR → Design Spec → Plan → Implementation. Artifacts → `./.claude/artifacts/`; templates → `./.claude/templates/artifacts/`. Filename patterns: `adr_<topic>.md`, `system_design_<comp>.md`, `design_spec_<comp>.md`, `plan_<task>.md`, `security_audit_<date>.md`.

## Skills & Personas

Persona skills (`/builder`, `/qa-engineer`, `/security-auditor`, `/code-check`) + the vendored hex multi-agent bundle (`/hex-discuss`, `/hex-architect`, `/hex-plan`, `/hex-execute`, `/hex-review`, `/hex-finalize`) + task skills in `.claude/skills/`. Map → "Skills by task topic" in [.claude/rules.md](./.claude/rules.md). Check before ad-hoc gen.

## Starting Work

Every task starts with [workflow-intent.md](./.claude/rules/workflow-intent.md) — classify (feature/bugfix/refactor), check GitHub for related issues/PRs, route to [workflow-feature.md](./.claude/rules/workflow-feature.md) / [workflow-bugfix.md](./.claude/rules/workflow-bugfix.md) / [workflow-refactor.md](./.claude/rules/workflow-refactor.md).
