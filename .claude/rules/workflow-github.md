---
paths:
  - ".github/ISSUE_TEMPLATE/**"
---

# GitHub Workflow — Issues & Pull Requests

**Never create issues or PRs without user approval.** Pushing triggers CI, which has real cost.

**Tool preference**: Prefer MCP tools `mcp__github__*` for structured, typed access. Fallback: `gh` CLI — still valid when MCP lacks parity (custom JSON projections, label management edge cases) or when a parallel session already uses `gh`.

| Action | MCP tool | `gh` fallback |
|--------|----------|---------------|
| Create issue | `mcp__github__issue_write` | `gh issue create` |
| Read issue | `mcp__github__issue_read` | `gh issue view` |
| List issues | `mcp__github__list_issues` | `gh issue list` |
| Create PR | `mcp__github__create_pull_request` | `gh pr create` |
| Read PR | `mcp__github__pull_request_read` | `gh pr view` |
| List PRs | `mcp__github__list_pull_requests` | `gh pr list` |
| PR review | `mcp__github__pull_request_review_write` | `gh pr review` |
| Comment | `mcp__github__add_issue_comment` | `gh issue/pr comment` |

---

## Issues

### Proposal-First Protocol

When the user describes future work, a feature idea, or technical debt to track:

1. **Draft** — Prepare issue(s) as a proposal table, then ask for approval
2. **Refine** — Incorporate feedback, re-present if changes are significant
3. **Create** — Only after explicit approval

### Proposal Format

Present all proposed issues in a single overview before creating any:

```
## Proposed Issues

| # | Title | Type | Depends On |
|---|-------|------|------------|
| 1 | feat: short user-facing title | Feature | — |
| 2 | fix: another title | Bug | #1 |

### Issue 1: feat: short user-facing title

**Value:** What the user/developer gains from this.
**Context:** Why this matters now, what triggered it.

**Scope:**
- Bullet list of what's in scope
- And what's explicitly out

**Acceptance Criteria:**
- [ ] Testable condition 1
- [ ] Testable condition 2

**Depends on:** None / #other-issue

---
Shall I create these issues? Any changes?
```

### Issue Body Template

```markdown
## Value
Start from the user's perspective. What problem does this solve?

## Context
Why now? Link to ADRs, discussions, or other issues. Brief.

## Scope
What's included and explicitly excluded. Bullet list.

## Design
Mermaid diagrams where they clarify relationships or flows. Skip for simple issues.

## Acceptance Criteria
- [ ] Testable, observable conditions
- [ ] Written as "X can do Y" or "When X, then Y"

## Dependencies
- #issue — one-line description of why this blocks
```

### Issue Types vs Labels

Prefer **GitHub issue types** (`Bug`, `Feature`) over labels that duplicate type information.

| Issue type | When to use | CLI flag |
|------------|-------------|----------|
| Bug | Something broken | `--label bug` (until `gh` supports `--type`) |
| Feature | New feature or capability | `--label enhancement` (until `gh` supports `--type`) |

Additional labels — use sparingly, only labels that exist on the repo: `docs`, `good first issue`.

---

## Pull Requests

### PR Creation Protocol

1. **Branch ready** — all commits are Conventional Commits, `task verify` passes (see `workflow-git.md`)
2. **Draft PR body** — present title + summary to user for approval
3. **Create** — only after explicit approval; push + create in one step

### PR Body Template

```markdown
## Summary
<1-3 bullet points describing the change>

## Test plan
- [ ] `task verify` passes
- [ ] Specific acceptance test or manual verification steps

Closes #<issue>
```

### PR Conventions

- **Title**: Conventional Commit format matching the primary commit (e.g., `feat: three-tier CAS`)
- **One concern per PR**: matches the one-concern-per-issue rule
- **Link issues**: use `Closes #N` in the body to auto-close on merge
- **Draft PRs**: use `--draft` for work-in-progress that needs early CI feedback
- **Review comments**: use `mcp__github__add_reply_to_pull_request_comment` for threaded replies

---

## Shared Conventions

- **Title format**: `type: short imperative description` — titles describe the outcome, not the approach
- **User-first framing**: Value section answers "why should anyone care?" before "how does it work?"
- **Cross-reference**: Use `#N` to link related issues/PRs. Add a "Depends on" section when order matters.
- **One concern per item**: Don't combine unrelated work
- **Mermaid where applicable**: Architecture, data flow, state machines. Not for simple lists.
- **No implementation details in titles**
- **Do not invent labels** — use only labels that exist on the repository
