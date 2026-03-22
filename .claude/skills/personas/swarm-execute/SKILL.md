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
6. **Review-Fix Loop** — Iterative review and remediation (see below). Gate: no new actionable findings.
7. **Commit** — Commit all changes on the feature branch (NEVER push)

## Review-Fix Loop

After implementation passes `task verify`, enter a bounded review-fix cycle that converges on a clean codebase. The loop is **diff-scoped** and **severity-gated**.

### Design Principles

- **Fresh context**: Every reviewer and builder in the loop is a fresh subagent. Never self-review in the same context that wrote the code (Dunning-Kruger bias).
- **Diff-scoped**: Findings must relate to changed files only (`git diff main...HEAD --name-only`). No "while you're here" improvements.
- **Severity-gated**: Only Block-tier and Warn-tier findings drive the loop. Suggest-tier findings go directly to the deferred summary — they never trigger a fix round.
- **`task verify` is ground truth**: The loop is an efficiency filter. `task verify` is the real gate.

### Loop Protocol

**Round 1 (full review):**
Launch all applicable review perspectives in parallel, scoped to changed files:
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) *— optional for ≤3 files*
- `worker-reviewer` (focus: `quality`)
- `worker-reviewer` (focus: `security`) *— if touching auth, input handling, or external data*
- `worker-reviewer` (focus: `performance`) *— if touching hot paths or async code*
- `worker-doc-reviewer` *— if documentation triggers match changed files*

Each reviewer classifies findings into:
- **Actionable** (Block/Warn) — can be fixed without human input (code quality, missing tests, naming, patterns, security fixes with clear remediation)
- **Deferred** — requires human decision (design questions, scope changes, trade-offs, external dependencies)
- **Suggest** — optional improvements, go directly to deferred summary

**Round 2+ (selective re-review):**
1. `worker-builder` (fresh subagent) fixes all actionable findings from the previous round
2. Run `task verify` — must pass before re-review
3. Re-launch **only the perspectives that had actionable findings** in the previous round (not the full battery)
4. If a perspective now reports no actionable findings, drop it from future rounds

**Termination conditions** (whichever comes first):
- No actionable findings remain across all perspectives → **converged**
- Maximum **3 rounds** reached → **budget exhausted** (remaining findings deferred)
- A round produces the same findings as the previous round → **oscillation detected** (defer remaining)

**On exit:** Run `task verify` once as ground truth. Print deferred findings summary:

```
## Deferred Findings

### Auto-fixed (N rounds)
- [Finding]: [What was changed]

### Deferred: Requires human judgment
- [Finding]: [Why human judgment is needed]

### Deferred: Oscillation detected
- [Finding]: [What was tried, why it oscillated]

### Deferred: Budget exhausted
- [Finding]: [Still unresolved after 3 rounds]

### Suggestions (not actioned)
- [Finding]: [Optional improvement]
```

### Scoping Rules

- Findings MUST relate to files in `git diff main...HEAD --name-only`
- Do NOT flag pre-existing issues in unchanged code
- Do NOT expand scope to unrelated improvements in unchanged files
- Exception: if a change *introduces* a regression in an unchanged file (e.g., breaks an import), that is in scope

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
