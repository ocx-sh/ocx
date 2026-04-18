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
| `worker-reviewer` | sonnet | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped, Sonnet 4.6 within 1.2pp of Opus on SWE-bench at 5× lower cost) |
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

**Model selection rationale:** Sonnet 4.6 is within 1.2pp of Opus 4.6 on SWE-bench at 5× lower cost. Reserve Opus for deep reasoning (architecture, complex implementation). Use Sonnet as default for review, testing, and mechanical refactoring.

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
