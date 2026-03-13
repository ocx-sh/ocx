---
description: Execute implementation plans with parallel worker swarm
argument-hint: [plan-artifact-or-task-description]
---

# Execution Orchestrator

Execute plans using parallel worker swarms with quality gates.

## MCP Tools

**Context7** (documentation):
- Research implementation patterns
- Verify API usage

## CLI Tools

**gh** (GitHub CLI):
- Use `gh pr create` for creating pull requests
- Use `gh pr view` to check PR status
- Use `gh issue list` for issue tracking

## Execution Workflow

1. **Discover** — Identify available tasks from the plan artifact
2. **Analyze** — Check task dependencies and order
3. **Execute** — Launch parallel workers for independent tasks
4. **Gate** — Run quality gates before marking tasks complete
5. **Commit** — Commit all changes on the feature branch (NEVER push)

## Worker Assignment

See `swarm-workers.md` for worker types and focus modes:
- worker-builder (implementation, testing, refactoring)
- worker-reviewer (code review, security analysis, performance review)
- worker-explorer (research/spike)
- worker-researcher (external documentation/API research)
- worker-architect (complex design decisions)

## Quality Gates

Run quality gates per `code-quality.md` — all must pass:
- Test suite passes
- Linter passes
- Type checker passes (if applicable)
- Build succeeds
- Security audit passes

No exceptions.

## Git Push Protocol

Follow git protocol in `swarm-workers.md`:
1. Stage and commit with descriptive message
2. NEVER push to remote — the human decides when to push (CI has real cost)

## Checkpointing

For long-running tasks, report progress milestones back to the orchestrator as each step completes.

## Error Handling

If a worker fails, report the blocker to the orchestrator with a safe description (no secrets). The orchestrator will determine whether to retry, reassign, or create a follow-up task.

## Rollback

If quality gates fail: stash changes, mark task as blocked, add comment with reason.

## Constraints

- NO completing tasks without passing quality gates
- NO leaving work uncommitted locally
- NO exceeding 8 parallel workers
- NO pushing to remote (human decides when to push)
- NO exposing secrets in error messages or reports
- ALWAYS report blockers immediately to the orchestrator
- ALWAYS verify `git status` shows up to date
- ALWAYS validate inputs before executing commands

## Definition of Done

- [ ] Code implemented per specification
- [ ] Tests written and passing
- [ ] Linter passes
- [ ] Types check
- [ ] Build succeeds
- [ ] Changes committed on feature branch (NOT pushed)

## Related Skills

`testing`

## Handoff

- To Swarm Review: After implementation complete, create PR
- To QA Engineer: For acceptance testing
- To Planner: When scope changes discovered

$ARGUMENTS
