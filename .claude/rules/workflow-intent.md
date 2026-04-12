# Work-Type Router

Global rule. Classifies every task and routes to the correct workflow.

## Before Starting Any Task

1. **Classify** the work type using the table below
2. **Check GitHub context** — scan open issues and PRs for related work:
   ```
   mcp__github__list_issues (state: "open") or gh issue list --state open --json number,title,labels
   mcp__github__list_pull_requests (state: "open") or gh pr list --json number,title,labels
   ```
   Look for: duplicate work, related context, blocking dependencies, acceptance criteria to reuse.
3. **Follow the workflow** for the classified work type

## Work-Type Classification

| Signal | Type | Workflow | Template |
|--------|------|----------|----------|
| "Something is broken", error report, unexpected behavior | **Bug Fix** | [workflow-bugfix.md](./workflow-bugfix.md) | `bugfix_plan.template.md` |
| New capability, user-facing change, "add support for X" | **Feature** | [workflow-feature.md](./workflow-feature.md) | `plan.template.md` |
| Restructure, rename, extract, simplify, "clean up" | **Refactoring** | [workflow-refactor.md](./workflow-refactor.md) | `plan.template.md` |

When the type is ambiguous, ask the user. Mixed tasks (e.g., "fix bug and refactor nearby code") should be split into separate commits — bug fix first, refactor second.

## Shared Gates (All Work Types)

- **Start**: GitHub context check (above), branch confirmation (never commit on `main`)
- **End**: `task verify` passes, changes committed per [workflow-git.md](./workflow-git.md)
- **Planning artifacts**: stored in `.claude/artifacts/`, using templates from `.claude/templates/artifacts/`

## Scope Escalation

| Scope | Action |
|-------|--------|
| Trivial (< 1 hour, ≤ 3 files) | Follow the workflow inline, no plan artifact needed |
| Small–Medium (1 hour – 2 weeks) | Create a plan artifact from the appropriate template |
| Large (2+ weeks) | Use `/swarm-plan` for multi-agent planning |
