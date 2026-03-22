---
name: swarm-execute
description: Execution orchestrator that implements plans using parallel worker swarms with quality gates. Use to execute implementation plans.
user-invocable: true
argument-hint: "plan-artifact-or-task-description"
disable-model-invocation: true
---

# Execution Orchestrator

Execute plans using parallel worker swarms with quality gates.

## Execution Workflow — Contract-First TDD

Each phase has a gate that must pass before proceeding.

1. **Discover** — Read plan artifact from `.claude/artifacts/`
2. **Stub** — Launch `worker-builder` (focus: `stubbing`) to create type signatures, trait impls, and function shells with `unimplemented!()` (Rust) or `raise NotImplementedError` (Python). No business logic. Gate: `cargo check` passes (types compile).
3. **Verify Architecture** — Launch `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) to validate stubs against the design record: API surface matches, module boundaries align, error types cover all failure modes. Gate: reviewer reports pass. *Optional for features touching ≤3 files.*
4. **Specify** — Launch `worker-tester` (focus: `specification`) to write unit tests and acceptance tests from the design record's contracts and user experience sections — NOT from the stubs. Tests should fail against the stubs. Gate: tests compile/parse and fail with `unimplemented`/`NotImplementedError`.
5. **Implement** — Launch `worker-builder` (focus: `implementation`) to fill in stub bodies. All specification tests must pass. Gate: `task verify` succeeds.
6. **Review** — Launch `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) to verify design↔tests↔implementation consistency, then `worker-reviewer` (focus: `quality`) for code quality. *Spec-compliance review optional for features touching ≤3 files.*
7. **Commit** — Commit all changes on the feature branch (NEVER push)

## Worker Assignment

See `.claude/rules/swarm-workers.md` for worker types, models, tools, and focus modes.

## Task Runner

**Always use `task` commands** — run `task --list` to discover available workflows:
- `task verify` — full quality gate (format, clippy, lint, license, build, unit tests, acceptance tests)
- `task test:quick` — acceptance tests without rebuilding
- `task checkpoint` — save work-in-progress (amends into single commit)

## Quality Gates

Run `task verify` before marking work complete. See `.claude/rules/code-quality.md` for the canonical gate list.

## Git Protocol

1. Stage and commit with descriptive conventional commit message
2. NEVER push to remote — the human decides when to push (CI has real cost)
3. Use `task checkpoint` for work-in-progress saves

## Living Design Records

Plan artifacts are living documents, not frozen specs. When implementation reveals a behavior or edge case not captured in the design record:
1. Update the plan artifact first
2. Write the corresponding test
3. Then implement

This prevents spec drift — the plan always reflects what was actually built and why.

## Constraints

- NO completing tasks without passing quality gates
- NO leaving work uncommitted locally
- NO exceeding 8 parallel workers
- NO pushing to remote
- NO running stub and test phases concurrently (sequential only — prevents context contamination)
- ALWAYS report blockers immediately
- ALWAYS validate `git status` shows clean
- ALWAYS update design record before adding tests for unspecified behaviors

## Handoff

- To Swarm Review: After implementation complete
- To QA Engineer: For acceptance testing

$ARGUMENTS
