---
paths:
  - ".claude/agents/**"
  - ".claude/skills/swarm-*/**"
---

# Swarm Worker Guidelines

Rules for efficient multi-agent swarm execution.

## Context Efficiency

1. **Workers inherit session context** - CLAUDE.md and rules are loaded, but workers use focused tool sets
2. **Narrow scope** - Each worker focuses on one task
3. **Minimal tools** - Only tools needed for the task
4. **Right-sized models** - Haiku for exploration, Sonnet for implementation, Opus for architecture

## Universal Worker Protocol (Critical Steps for Every Build/Test/Review Worker)

1. **Read relevant quality rules FIRST, before any writes.** These are path-scoped and auto-load based on file type: `.claude/rules/quality-core.md` (universal, always loaded), plus the language leaf (`quality-rust.md`, `quality-python.md`, `quality-typescript.md`, `quality-bash.md`, `quality-vite.md`) that matches the files you're editing. For Python acceptance tests also read `.claude/rules/subsystem-tests.md`. Post-completion self-review is not a substitute.
2. **Grep for existing utilities before writing new code.** OCX has `DirWalker`, `PackageDir`, `ReferenceManager`, `DIGEST_FILENAME`, etc. Check `crates/ocx_lib/src/utility/` and related modules with Grep.
3. **If an existing utility doesn't fit the use case, extend it — don't work around it.** Workarounds are the #1 source of over-engineered iteration loops in prior sessions.
4. **Report deferred findings instead of oscillating.** When a fix requires human judgment or introduces a regression on re-attempt, stop and report deferred.
5. **Never auto-commit.** All commits are Michael's explicit decision. Workers report `git status` only.
6. **Flag product-level insights.** If research, architecture work, or implementation uncovers something that shifts OCX's positioning, differentiators, target users, competitive landscape, or product principles, update `.claude/rules/product-context.md` in the same commit (or flag it in the completion report if a human decision is needed). See the "Update Protocol" section at the bottom of that file for the full trigger list. This applies especially to `worker-researcher`, `worker-architect`, and `worker-doc-writer` — researchers evaluating competitors, architects making scope decisions, and doc writers framing narratives are the most likely to surface positioning shifts.

## Worker Types

| Worker | Model | Tools | Use |
|--------|-------|-------|-----|
| `worker-architecture-explorer` | sonnet | Read, Glob, Grep | Architecture discovery |
| `worker-explorer` | haiku | Read, Glob, Grep | Fast codebase search |
| `worker-builder` | sonnet (opus override for complex implementation) | Read, Write, Edit, Bash, Glob, Grep | Stubbing/implementation/refactoring (see model rationale below) |
| `worker-tester` | sonnet | Read, Write, Edit, Bash, Glob, Grep | Specification tests and validation |
| `worker-reviewer` | sonnet (default) | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped; model scales per tier via `--reviewer` overlay — see `.claude/artifacts/adr_tier_model_correlation.md`) |
| `worker-researcher` | sonnet | Read, Glob, Grep, WebFetch, WebSearch | External research |
| `worker-architect` | opus | Read, Write, Edit, Glob, Grep | Complex design decisions |
| `worker-doc-reviewer` | sonnet | Read, Glob, Grep, Bash | Documentation consistency review |
| `worker-doc-writer` | sonnet | Read, Write, Edit, Bash, Glob, Grep | Documentation writing |

## Worker Focus Modes

Orchestrators specialize workers by specifying a focus mode in the prompt.

**worker-builder focus modes:**
- `stubbing`: Create public API surface only — types, traits, signatures with `unimplemented!()`/`NotImplementedError`. Gate: `cargo check` passes. Sonnet default.
- `implementation` (default): Fill in stub bodies so specification tests pass. Sonnet default; orchestrator passes `model: opus` for architecturally complex or cross-subsystem work.
- `testing`: Write tests, cover happy path and edge cases, ensure deterministic. Sonnet default.
- `refactoring`: Extract patterns, simplify conditionals, apply SOLID/DRY. Follow Two Hats Rule (see quality-core.md). Sonnet default.

**Model selection rationale:** Opus 4.7 leads Sonnet 4.6 by 8.0pp on SWE-bench Verified at 1.67× higher input cost and lower throughput. The gap materializes on multi-step agentic chains and novel-reasoning work; it narrows to near-parity on single-pass review. OCX's policy: Opus for one-way-door architecture and max-tier complex implementation; Sonnet for standard review / testing / implementation; Haiku only for read-only exploration and narrow single-pass tasks. Per-tier overrides live in `.claude/artifacts/adr_tier_model_correlation.md` and the per-skill `overlays.md` files. Source benchmark data: `.claude/artifacts/research_model_capability_matrix.md`.

**worker-tester focus modes:**
- `specification`: Write tests from design record BEFORE implementation. Tests encode expected behavior as executable spec. Must fail against stubs.
- `validation` (default): Write tests to validate existing implementation and improve coverage

**worker-reviewer focus modes:**
- `quality` (default): Code review checklist — naming, style, tests, patterns
- `security`: OWASP Top 10 scan, hardcoded secrets, auth/authz flows, input validation. Reference CWE IDs. See `quality-security.md`
- `performance`: N+1 queries, blocking I/O, allocations, pagination, caching. See `quality-core.md`
- `spec-compliance`: Phase-aware design record consistency review. Orchestrator specifies phase: `post-stub` (stubs ↔ design), `post-specification` (tests ↔ design), or `post-implementation` (full traceability). Knows that in early phases no implementation exists yet.

**worker-doc-reviewer**: No focus modes — always runs the full trigger matrix audit (CLI, env vars, metadata, user guide, installation, changelog).

**worker-doc-writer focus modes:**
- `reference`: Flag tables, env var entries, schema fields — facts only, no narrative
- `narrative`: User guide sections, getting started — idea→problem→solution structure
- `changelog`: Version entries with Added/Changed/Fixed/Removed sections

## Swarm Patterns

See `.claude/rules/workflow-feature.md` for the canonical contract-first TDD protocol (Stub → Verify → Specify → Implement → Review-Fix Loop). The `/swarm-execute` skill has the full detailed protocol including the review-fix loop specification.

## Tier & Overlay Vocabulary (for /swarm-plan, /swarm-execute, /swarm-review)

All three swarm skills (`/swarm-plan`, `/swarm-execute`, `/swarm-review`) take an optional tier argument (`low | auto | high | max`, default `auto`) to scale the pipeline to the scope of the feature or diff. The same pipeline shape runs at every tier — only worker count, model choice, review breadth, and Codex coverage change. Contract-first TDD (Stub → Specify → Implement → Review) is preserved at every tier.

### /swarm-plan tiers

| Tier | Intent | Defaults |
|---|---|---|
| `low` | Two-Way Door: flag/option change, doc edit, single subsystem ≤3 files | 1 explorer, research skipped, inline design, 1 reviewer single pass, Codex off |
| `auto` (default) | Classifier picks low/high/max from signals | — |
| `high` | One-Way Door Medium: new subcommand, new index/storage layout, 1–2 subsystems | `worker-architecture-explorer` + 2–4 explorers, 1 researcher, inline/sonnet architect, parallel Claude review panel (2 rounds), Codex off (auto-on for One-Way Door signals) |
| `max` | One-Way Door High: new crate, breaking API, cross-subsystem, protocol change | Same as high + mandatory opus architect, mandatory 3-axis research, mandatory Codex plan-artifact review as final gate |

### /swarm-execute tiers

Execute reads classification from the plan artifact header when present (primary signal); falls back to free-text signals otherwise. Loop rounds, builder model, and review breadth scale per tier.

| Tier | Intent | Defaults |
|---|---|---|
| `low` | Two-Way Door from plan=low: 1-round loop, minimal Stage 2 (quality only), no arch verify, no Codex | sonnet stub+impl, tester (unit only), 1 reviewer Stage 1 + 1 reviewer Stage 2 |
| `auto` (default) | Classifier reads plan header `Tier:` verbatim; falls back to free-text signals | — |
| `high` | Medium plan: 3-round loop, full Stage 2 (quality / security / perf / docs), Codex off (auto-on for One-Way Door plan signals) | sonnet stub+impl (opus override for cross-subsystem), arch-verify reviewer, unit + acceptance tests |
| `max` | Large plan: 3-round loop, adversarial Stage 2 (+ architect + SOTA + cli-ux), mandatory Codex code-diff gate | opus stub+impl (mandatory), reviewer + architect arch-verify, edge-case test coverage |

### /swarm-review tiers

Review classifies from the **diff against the configured baseline** (`--base=<ref>`, default `main`). Baseline is a pipeline input, not an overlay axis — a tight baseline produces small diffs (tier=low), a wide baseline produces large diffs (tier=high/max). Breadth, RCA, and Codex scale per tier.

| Tier | Intent | Defaults |
|---|---|---|
| `low` | ≤3 files, ≤100 lines, 1 subsystem, no structural markers | 1 reviewer (spec-compliance + quality), no RCA, no Codex |
| `auto` (default) | Classifier reads diff metrics + paths + PR labels | — |
| `high` | ≤15 files, ≤500 lines, 1–2 subsystems, no One-Way Door High signals | Stage 1 (spec-compliance + test-coverage) + Stage 2 full (quality / security / perf / docs), RCA for Block/High, Codex off (auto-on for One-Way Door signals) |
| `max` | >15 files, or cross-subsystem, or new crate, or breaking/protocol/security signals | Adversarial breadth (+ architect + SOTA + CLI-UX), RCA for all >Suggest, mandatory Codex code-diff gate |

### Overlays (stackable, single-axis adjustments on top of the chosen tier)

**Plan overlays:**

| Flag | Axis | Effect |
|---|---|---|
| `--architect=inline\|sonnet\|opus` | Architect model in Design phase | inline = orchestrator drafts design; sonnet/opus = `worker-architect` with named model |
| `--research=skip\|1\|3` | Research worker count | skip / 1 axis / 3 axes in parallel (tech / patterns / domain) |
| `--codex` / `--no-codex` | Plan-artifact Codex pass | Force on/off regardless of tier default |
| `--dry-run` / `--form` | Meta-plan preview UI | Gate the orchestrator behind a single approval interaction |

**Execute overlays:**

| Flag | Axis | Effect |
|---|---|---|
| `--builder=sonnet\|opus` | Builder model for Stub + Implement phases | sonnet default; opus for architecturally complex / cross-subsystem; mandatory at tier=max |
| `--loop-rounds=1\|2\|3` | Max Review-Fix Loop iterations | 1 for low, 3 for high/max |
| `--review=minimal\|full\|adversarial` | Stage 2 perspective breadth | quality only / + security/perf/docs / + architect + SOTA + CLI-UX |
| `--codex` / `--no-codex` | Code-diff Codex pass after loop converges | Force on/off regardless of tier default |
| `--dry-run` / `--form` | Meta-plan preview UI | Same semantics as plan |

**Review overlays:**

| Flag | Axis | Effect |
|---|---|---|
| `--base=<git-ref>` | Diff baseline (pipeline input, not an axis) | Default `main`; PR targets auto-resolve via `gh pr view --json baseRefName`; user flag wins |
| `--breadth=minimal\|full\|adversarial` | Stage 2 perspective breadth | quality only / + security/perf/docs / + architect + SOTA + CLI-UX |
| `--rca=on\|off` | Five Whys root-cause analysis depth | off at low; on for Block/High at high; on for >Suggest at max |
| `--codex` / `--no-codex` | Cross-model Codex code-diff pass | Force on/off regardless of tier default |
| `--dry-run` / `--form` | Meta-plan preview UI | Same semantics as plan/execute |

User-supplied flags always override classifier-inferred overlays (except tier=max's mandatory `--builder=opus` in `/swarm-execute`). Ambiguous classifications are resolved at the meta-plan gate (single approval point), never via mid-flow questions.

## Codex Plan Review (cross-model, plan-artifact scope)

Extends the cross-model adversarial pass up the lifecycle. Same entry point (`/codex-adversary`), different scope:

| Scope | When fires | Target |
|---|---|---|
| `code-diff` (default) | `/swarm-execute` final gate after Claude review loop converges; `/swarm-review` cross-model pass after Claude panel converges | Git diff (`working-tree` / `branch` / `--base`) |
| `plan-artifact` | `/swarm-plan` Phase 6 after Claude panel converges | Plan / ADR markdown file (via `--target-file`) |

Both are one-shot (no looping — prevents two-family stylistic thrash). Gating by tier:

- `low`: skipped (Two-Way Door — cost > value)
- `high`: off by default; auto-on when classifier detects One-Way Door signals (public API change, breaking change, novel algorithm); explicit via `--codex`
- `max`: mandatory final gate

Triage for plan-artifact scope mirrors the code-diff pass: Actionable → orchestrator edits plan, re-runs one `worker-reviewer` (spec-compliance) pass; Deferred → handoff; Stated-convention / Trivia → dropped with count. Unavailable path (CLAUDE_PLUGIN_ROOT unset or companion non-zero): log `Cross-model plan review skipped: <reason>` and continue.

## Coordination Protocol

1. **Orchestrator** decomposes task into clear assignments
2. **Workers** pick up assigned tasks and begin execution
3. **Workers** complete task following AGENTS.md "Session Completion" workflow
4. **Workers** report completion to orchestrator
5. **Orchestrator** integrates and verifies

### Worker Completion Requirements

When a worker completes its assigned task, it MUST follow the full completion protocol from AGENTS.md:

1. File issues for remaining work
2. Run quality gates via `task verify` (if code changed) — run `task --list` to discover available commands
3. **Commit all changes** on the feature branch
4. Report completion to orchestrator

**Critical**: NEVER push to remote — the human decides when to push (CI has real cost).

## Performance Tips

- Launch multiple explorers for broad searches
- Use worker-architect for decisions, worker-builder for execution
- Parallelize independent tasks (max 8 concurrent workers)
- Keep worker prompts under 500 tokens for fast startup

## Anti-Patterns

- NO loading full context into workers
- NO sharing state between workers
- NO workers spawning workers (single-level only)
- NO long-running workers (timeout at 5 min)
- NO opus for simple tasks (cost optimization)
- NO pushing to remote (human decides when to push — CI has real cost)
