---
name: swarm-execute
description: Execution orchestrator that implements plans using parallel worker swarms with quality gates. Use to execute implementation plans.
user-invocable: true
argument-hint: "plan-artifact-or-task-description"
disable-model-invocation: true
---

# Execution Orchestrator

Execute plans using parallel worker swarms with quality gates.

## Execution Workflow

1. **Discover** — Read plan artifact from `.claude/artifacts/`
2. **Analyze** — Check task dependencies and order
3. **Execute** — Launch parallel workers for independent tasks
4. **Gate** — Run quality gates before marking tasks complete
5. **Commit** — Commit all changes on the feature branch (NEVER push)

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

## Constraints

- NO completing tasks without passing quality gates
- NO leaving work uncommitted locally
- NO exceeding 8 parallel workers
- NO pushing to remote
- ALWAYS report blockers immediately
- ALWAYS validate `git status` shows clean

## Handoff

- To Swarm Review: After implementation complete
- To QA Engineer: For acceptance testing

$ARGUMENTS
