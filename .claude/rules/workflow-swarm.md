---
paths:
  - ".claude/agents/**"
  - ".claude/skills/swarm-*/**"
---

# Swarm Worker Guidelines

Rules for efficient multi-agent swarm execution.

## Context Efficiency

1. **Workers inherit session context** - CLAUDE.md and rules loaded, workers use focused tool sets
2. **Narrow scope** - Each worker one task
3. **Minimal tools** - Only tools needed
4. **Opus for review and non-mechanical implementation; Sonnet for everything mechanical.** Security/code review, adversarial passes, multi-file or async/error-semantics/wire-format/credential-path implementation, and one-way-door architecture → Opus (may fan back out to Sonnet workers). Exploration, search, research, docs, fixtures, renames, test scaffolding → Sonnet. Fable (near-)never a subagent — main-loop synthesis only. Haiku only as an explicit user override

## Universal Worker Protocol (Critical Steps for Every Build/Test/Review Worker)

1. **Read relevant quality rules FIRST, before any writes.** Path-scoped, auto-load by file type: `.claude/rules/quality-core.md` (universal, always loaded), plus language leaf (`quality-rust.md`, `quality-python.md`, `quality-typescript.md`, `quality-bash.md`, `quality-vite.md`) matching files edited. Python acceptance tests also read `.claude/rules/subsystem-tests.md`. Post-completion self-review no substitute.
2. **Grep for existing utilities before writing new code.** OCX has `DirWalker`, `PackageDir`, `ReferenceManager`, `DIGEST_FILENAME`, etc. Check `crates/ocx_lib/src/utility/` and related modules with Grep.
3. **If existing utility doesn't fit, extend it — don't work around it.** Workarounds = #1 source of over-engineered iteration loops in prior sessions.
4. **Report deferred findings instead of oscillating.** Fix needs human judgment or causes regression on re-attempt → stop, report deferred.
5. **Never auto-commit.** All commits Michael's explicit decision. Workers report `git status` only.
6. **Flag product-level insights.** Research/architecture/implementation uncovers shift in OCX positioning, differentiators, target users, competitive landscape, product principles → update `.claude/rules/product-context.md` same commit (or flag in completion report if human decision needed). See "Update Protocol" section at bottom of that file for full trigger list. Applies especially to `worker-researcher`, `worker-architect`, `worker-doc-writer` — researchers eval competitors, architects make scope decisions, doc writers frame narratives most likely surface positioning shifts.

## Worker Types

| Worker | Model | Tools | Use |
|--------|-------|-------|-----|
| `worker-architecture-explorer` | sonnet | Read, Glob, Grep | Architecture discovery |
| `worker-explorer` | sonnet | Read, Glob, Grep | Fast codebase search |
| `worker-builder` | opus for non-mechanical work; sonnet for mechanical | Read, Write, Edit, Bash, Glob, Grep | Stubbing/implementation/refactoring (see model rationale below) |
| `worker-tester` | sonnet (opus at tier=max) | Read, Write, Edit, Bash, Glob, Grep | Specification tests and validation |
| `worker-reviewer` | **opus** (sonnet only as an explicit downgrade for trivial diffs) | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped; see `.claude/artifacts/adr_tier_model_correlation.md`) |
| `worker-researcher` | sonnet | Read, Glob, Grep, WebFetch, WebSearch | External research |
| `worker-architect` | opus | Read, Write, Edit, Glob, Grep | Complex design decisions |
| `worker-doc-reviewer` | sonnet | Read, Glob, Grep, Bash | Documentation consistency review |
| `worker-doc-writer` | sonnet | Read, Write, Edit, Bash, Glob, Grep | Documentation writing |

## Worker Focus Modes

Orchestrators specialize workers via focus mode in prompt.

**worker-builder focus modes:**
- `stubbing`: Public API surface only — types, traits, signatures with `unimplemented!()`/`NotImplementedError`. Gate: `cargo check` passes. Sonnet default.
- `implementation` (default): Fill stub bodies so spec tests pass. Sonnet default; orchestrator passes `model: opus` for architecturally complex / cross-subsystem work.
- `testing`: Write tests, cover happy path + edge cases, ensure deterministic. Sonnet default.
- `refactoring`: Extract patterns, simplify conditionals, apply SOLID/DRY. Follow Two Hats Rule (see quality-core.md). Sonnet default.

**Model selection rationale (owner policy 2026-07-24, Opus 5 release — supersedes the 2026-07-16 Sonnet-floor policy):** the axis is *nature of the work*, not tier alone. **Opus** for security and code review, adversarial/verification passes, and non-mechanical implementation — multi-file, async/concurrency, error and exit-code semantics, OCI/wire-format or serializer work, auth/SSRF/credential paths, ADR and one-way-door architecture; Opus may itself fan work back out to Sonnet workers. **Sonnet** for mechanical and breadth work — exploration, codebase search, research, web fetch, docs, fixtures, renames, test scaffolding, planning workers. "Mechanical" = local change whose shape is already decided; if the design is still open, it is Opus. **Fable** is the main-loop/last-instance decider over pre-digested multi-agent results, (near-)never a subagent. **Haiku** only as an explicit user override, never on security paths. Per-tier defaults in each skill's `overlays.md`; benchmark background in `.claude/artifacts/research_model_capability_matrix.md` and `adr_tier_model_correlation.md` (both predate Opus 5 — treat their per-tier tables as superseded by this rule where they disagree).

**worker-tester focus modes:**
- `specification`: Write tests from design record BEFORE implementation. Tests encode expected behavior as executable spec. Must fail against stubs.
- `validation` (default): Write tests to validate existing implementation, improve coverage

**worker-reviewer focus modes:**
- `quality` (default): Code review checklist — naming, style, tests, patterns
- `security`: OWASP Top 10 scan, hardcoded secrets, auth/authz flows, input validation. Reference CWE IDs. See `quality-security.md`
- `performance`: N+1 queries, blocking I/O, allocations, pagination, caching. See `quality-core.md`
- `spec-compliance`: Phase-aware design record consistency review. Orchestrator specifies phase: `post-stub` (stubs ↔ design), `post-specification` (tests ↔ design), `post-implementation` (full traceability). Knows early phases have no implementation yet.

**worker-doc-reviewer**: No focus modes — always runs full trigger matrix audit (CLI, env vars, metadata, user guide, installation, changelog).

**worker-doc-writer focus modes:**
- `reference`: Flag tables, env var entries, schema fields — facts only, no narrative
- `narrative`: User guide sections, getting started — idea→problem→solution structure
- `changelog`: Version entries with Added/Changed/Fixed/Removed sections

## Swarm Patterns

See `.claude/rules/workflow-feature.md` for canonical contract-first TDD protocol (Stub → Verify → Specify → Implement → Review-Fix Loop). `/swarm-execute` skill has full detailed protocol incl. review-fix loop spec.

## Review-Fix Loop

Canonical protocol used by `/swarm-execute`, `/swarm-review`, bug-fix workflow Phase 6, refactor workflow Phase 5. Byte-identical copies ship in `workflow-bugfix.md` and `workflow-refactor.md` so protocol auto-loads from all three worker-relevant path scopes.

<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->
Diff-scoped, bounded iterative review. Tier-scaled: 1 round at `low`, up to 3 rounds at `high`/`max`.

**Round 1** — run every perspective on diff. Perspectives most likely find blockers run first (e.g. spec-compliance, correctness, behavior-preservation); if surface actionable findings, fix before remaining perspectives in same round.

Classify each finding:

- **Actionable** — fix automatically, re-run affected perspectives next round.
- **Deferred** — needs human judgment; surface in commit summary with context.

**Subsequent rounds** — re-run only perspectives with actionable findings prior round. Loop exits when no actionable findings remain or tier's round cap hit. Oscillating findings (same issue surfaced two rounds) auto-defer.

**Cross-model adversarial pass** (optional, tier-scaled): after Claude loop converges, run single Codex adversarial review against diff as final gate. One-shot, no looping — two-family stylistic thrash = failure mode. Codex model scales with tier (`low→terra` opt-in, `high→terra` default-on, `max→sol`) — see "Cross-model model tiers" in `workflow-swarm.md`. Skipped gracefully if Codex unavailable.

**Gate to exit**: no actionable findings remain, verification passes on final state, deferred findings documented for handoff.
<!-- REVIEW_FIX_LOOP_CANONICAL_END -->

## Tier & Overlay Vocabulary (for /swarm-plan, /swarm-execute, /swarm-review)

All three swarm skills (`/swarm-plan`, `/swarm-execute`, `/swarm-review`) take optional tier arg (`low | auto | high | max`, default `auto`) to scale pipeline to scope of feature/diff. Same pipeline shape every tier — only worker count, model choice, review breadth, Codex coverage change. Contract-first TDD (Stub → Specify → Implement → Review) preserved every tier.

### /swarm-plan tiers

| Tier | Intent | Defaults |
|---|---|---|
| `low` | Two-Way Door: flag/option change, doc edit, single subsystem ≤3 files | 1 explorer, research skipped, inline design, 1 reviewer single pass, Codex off |
| `auto` (default) | Classifier picks low/high/max from signals | — |
| `high` | One-Way Door Medium: new subcommand, new index/storage layout, 1–2 subsystems | `worker-architecture-explorer` + 2–4 explorers, 1 researcher, inline/sonnet architect, parallel Claude review panel (2 rounds), Codex off (auto-on for One-Way Door signals) |
| `max` | One-Way Door High: new crate, breaking API, cross-subsystem, protocol change | Same as high + mandatory opus architect, mandatory 3-axis research, mandatory Codex plan-artifact review as final gate |

### /swarm-execute tiers

Execute reads classification from plan artifact header when present (primary signal); falls back to free-text signals otherwise. Loop rounds, builder model, review breadth scale per tier.

| Tier | Intent | Defaults |
|---|---|---|
| `low` | Two-Way Door from plan=low: 1-round loop, minimal Stage 2 (quality only), no arch verify, no Codex | sonnet stub+impl, tester (unit only), 1 reviewer Stage 1 + 1 reviewer Stage 2 |
| `auto` (default) | Classifier reads plan header `Tier:` verbatim; falls back to free-text signals | — |
| `high` | Medium plan: 3-round loop, full Stage 2 (quality / security / perf / docs), Codex off (auto-on for One-Way Door plan signals) | sonnet stub+impl (opus override for cross-subsystem), arch-verify reviewer, unit + acceptance tests |
| `max` | Large plan: 3-round loop, adversarial Stage 2 (+ architect + SOTA + cli-ux), mandatory Codex code-diff gate | opus stub+impl (mandatory), reviewer + architect arch-verify, edge-case test coverage |

### /swarm-review tiers

Review classifies from **diff against configured baseline** (`--base=<ref>`, default `main`). Baseline = pipeline input, not overlay axis — tight baseline → small diffs (tier=low), wide baseline → large diffs (tier=high/max). Breadth, RCA, Codex scale per tier.

| Tier | Intent | Defaults |
|---|---|---|
| `low` | ≤3 files, ≤100 lines, 1 subsystem, no structural markers | 1 reviewer (spec-compliance + quality), no RCA, no Codex |
| `auto` (default) | Classifier reads diff metrics + paths + PR labels | — |
| `high` | ≤15 files, ≤500 lines, 1–2 subsystems, no One-Way Door High signals | Stage 1 (spec-compliance + test-coverage) + Stage 2 full (quality / security / perf / docs), RCA for Block/High, Codex off (auto-on for One-Way Door signals) |
| `max` | >15 files, or cross-subsystem, or new crate, or breaking/protocol/security signals | Adversarial breadth (+ architect + SOTA + CLI-UX), RCA for all >Suggest, mandatory Codex code-diff gate |

### Overlays (stackable, single-axis adjustments on top of chosen tier)

**Plan overlays:**

| Flag | Axis | Effect |
|---|---|---|
| `--architect=inline\|sonnet\|opus` | Architect model in Design phase | inline = orchestrator drafts design; sonnet/opus = `worker-architect` with named model |
| `--research=skip\|1\|3` | Research worker count | skip / 1 axis / 3 axes parallel (tech / patterns / domain) |
| `--codex` / `--no-codex` | Plan-artifact Codex pass | Force on/off regardless of tier default |
| `--dry-run` / `--form` | Meta-plan preview UI | Gate orchestrator behind single approval interaction |

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
| `--base=<git-ref>` | Diff baseline (pipeline input, not axis) | Default `main`; PR targets auto-resolve via `gh pr view --json baseRefName`; user flag wins |
| `--breadth=minimal\|full\|adversarial` | Stage 2 perspective breadth | quality only / + security/perf/docs / + architect + SOTA + CLI-UX |
| `--rca=on\|off` | Five Whys root-cause analysis depth | off at low; on for Block/High at high; on for >Suggest at max |
| `--codex` / `--no-codex` | Cross-model Codex code-diff pass | Force on/off regardless of tier default |
| `--dry-run` / `--form` | Meta-plan preview UI | Same semantics as plan/execute |

User-supplied flags always override classifier-inferred overlays (except tier=max's mandatory `--builder=opus` in `/swarm-execute`). Ambiguous classifications resolved at meta-plan gate (single approval point), never via mid-flow questions.

## Codex Plan Review (cross-model, plan-artifact scope)

Extends cross-model adversarial pass up lifecycle. Same entry point (`/codex-adversary`), different scope:

| Scope | When fires | Target |
|---|---|---|
| `code-diff` (default) | `/swarm-execute` final gate after Claude review loop converges; `/swarm-review` cross-model pass after Claude panel converges | Git diff (`working-tree` / `branch` / `--base`) |
| `plan-artifact` | `/swarm-plan` Phase 6 after Claude panel converges | Plan / ADR markdown file (via `--target-file`) |

Both one-shot (no looping — prevents two-family stylistic thrash). Gating by tier:

- `low`: skipped (Two-Way Door — cost > value); runs when `--codex` forced by user
- `high`: **default ON** — one-shot `terra` pass; `--codex=off` or user override disables
- `max`: mandatory final gate

### Cross-model model tiers

When the pass fires, the Codex model scales with tier — heavier review
earns a stronger reviewer (mirrors the Claude `--reviewer` haiku→sonnet→
opus ladder). `/codex-adversary` resolves these aliases to slugs (see its
"Model selection"); the GPT-5.6 tiers map onto the Claude analogues:

| Tier | Codex model | Slug | Claude analogue |
|---|---|---|---|
| `low` (only when `--codex` forced) | `terra` (`luna` = explicit cheap override) | `gpt-5.6-terra` | Sonnet |
| `high` (default-on) | `terra` | `gpt-5.6-terra` | Sonnet |
| `max` | `sol` | `gpt-5.6-sol` | Opus |

Override per run with `--codex-model=luna|terra|sol` (swarm skills) or
`--model` (`/codex-adversary` direct); the user value wins over the tier
default, exactly like every other overlay.

Triage for plan-artifact scope mirrors code-diff pass: Actionable → orchestrator edits plan, re-runs one `worker-reviewer` (spec-compliance) pass; Deferred → handoff; Stated-convention / Trivia → dropped with count. Unavailable path (CLAUDE_PLUGIN_ROOT unset or companion non-zero): log `Cross-model plan review skipped: <reason>` and continue.

## Plan Status Tracking

Every `.claude/state/plans/plan_*.md` carries a `## Status` block at top: `Plan` / `Active phase` / `Step` / `Last update`. Swarm skills mutate it on phase entry, round entry, verdict, commit. Global pointer `.claude/state/current_plan.md` (gitignored) names the active plan. `/next` reads block as primary signal; `/finalize` refuses if any phase still active. Schema + per-skill mutation table → [`meta-ai-config.md`](./meta-ai-config.md) "Plan Status Protocol".

## Coordination Protocol

1. **Orchestrator** decomposes task into clear assignments
2. **Workers** pick up assigned tasks, begin execution
3. **Workers** complete task following AGENTS.md "Session Completion" workflow
4. **Workers** report completion to orchestrator
5. **Orchestrator** integrates and verifies

### Worker Completion Requirements

When worker completes assigned task, MUST follow full completion protocol from AGENTS.md:

1. File issues for remaining work
2. Run quality gates via `task verify` (if code changed) — run `task --list` to discover available commands
3. **Commit all changes** on feature branch
4. Report completion to orchestrator

**Critical**: NEVER push to remote — human decides when to push (CI has real cost).

## Parallel Worktree Execution

Plans MUST be parallel-capable by design when work packages (WPs) are file-disjoint. This section governs the mechanics of running WPs concurrently in separate git worktrees.

### Lifecycle

- One worktree per WP, at `.agents/worktrees/<wp-slug>` (gitignored)
- Worktree created from the **DAG-designated base tip** — NOT always main/feature-root; a WP may base on another WP's tip when the dependency DAG says so
- One branch per WP
- Merge back in DAG order
- Run `cargo check` after **every** merge — catches cross-file interactions per-file verify misses
- Remove the worktree after its WP merges

### Plan Requirements

Plans must define per WP: name, owned files (disjoint across concurrently-running WPs), size, dependencies, base tip, merge order. See `swarm-plan` "Parallelization" and `plan.template.md`.

### Anti-Patterns

| Anti-pattern | Why it breaks |
|---|---|
| Two concurrent WPs touch the same file | Merge conflicts, lost work, non-deterministic outcome |
| Branch from main when the DAG says branch from a WP tip | Missing the dependency's changes, broken build after merge |
| Reuse a worktree for a second WP | Stale branch state, cross-contamination between WPs |
| Skip the post-merge `cargo check` | Cross-file interactions surface only after merge — per-file verify misses them |

## Performance Tips

- Launch multiple explorers for broad searches
- Use worker-architect for decisions, worker-builder for execution
- Send all Agent calls in single message with multiple tool invocations → run concurrently (max 8 workers)
- Keep worker prompts under 500 tokens for fast startup

## Anti-Patterns

- NO loading full context into workers
- NO sharing state between workers
- NO workers spawning workers (single-level only)
- NO long-running workers (timeout at 5 min)
- NO opus for genuinely mechanical tasks (renames, fixtures, doc fixes, codebase search)
- NO sonnet review on security, auth/credential, SSRF/network-policy, exit-code, or wire-format diffs
- NO pushing to remote (human decides when to push — CI has real cost)