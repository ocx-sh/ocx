---
paths:
  - ".github/ISSUE_TEMPLATE/**"
---

# GitHub Workflow — Issues & Pull Requests

**Never create issues or PRs without user approval.** Push trigger CI, real cost.

## Tooling: MCP first

Prefer `mcp__github__*` MCP tools for typed access. `gh` CLI = fallback when MCP lack parity (label admin, org issue-type admin, ad-hoc JSON projections) or parallel session use `gh`. When `GITHUB_TOKEN` set in env, `gh` use it and ignore stored creds — prefix `env -u GITHUB_TOKEN gh ...` if need stored cred scopes (e.g., `admin:org`).

| Action | MCP tool | `gh` fallback |
|--------|----------|---------------|
| Create/update issue | `mcp__github__issue_write` (supports `type:`) | `gh issue create/edit` |
| Read issue | `mcp__github__issue_read` | `gh issue view` |
| List issues | `mcp__github__list_issues` | `gh issue list` |
| List org issue types | `mcp__github__list_issue_types` | `gh api /orgs/{org}/issue-types` |
| Create PR | `mcp__github__create_pull_request` | `gh pr create` |
| Read PR | `mcp__github__pull_request_read` | `gh pr view` |
| List PRs | `mcp__github__list_pull_requests` | `gh pr list` |
| PR review | `mcp__github__pull_request_review_write` | `gh pr review` |
| Comment | `mcp__github__add_issue_comment` | `gh issue/pr comment` |
| Threaded PR reply | `mcp__github__add_reply_to_pull_request_comment` | `gh api` |

---

## Issue Types (org-level, `ocx-sh`)

Every issue MUST have type. Types replace old `bug` / `enhancement` / `chore` labels entirely. Types do NOT apply to PRs — use labels on PRs for cross-cutting concerns.

| Type | When to use |
|------|-------------|
| **Bug** | Broken behavior, unexpected output, crash, spec deviation |
| **Feature** | New user-facing capability, command, or behavior |
| **Task** | Internal work, no user-visible capability — refactor, CI, tooling, chore, AI-config maintenance |
| **Documentation** | Docs-only: website, user guide, `--help`, ADRs, man pages |
| **Performance** | Measurable throughput / latency / memory / RSS change |
| **Security** | Vulnerability, hardening, auth, credentials, secret handling |

**When ambiguous**: perf-motivated refactor = **Performance**, not Task. Bug fix that add new flag still **Bug**. New doc site feature (beyond docs content) = **Feature**.

Discover types via `mcp__github__list_issue_types` (or `env -u GITHUB_TOKEN gh api /orgs/ocx-sh/issue-types`).

**Setting type on issue.** Prefer `mcp__github__issue_write` (supports `type:` natively). If fall back to `gh`, note `gh issue create/edit` no `--type` flag — use GraphQL `updateIssue` mutation with type's node id. Fetch ids once, then update:

```sh
# 1. fetch org type ids (one-shot)
gh api graphql -f query='query { organization(login:"ocx-sh") { issueTypes(first:20) { nodes { id name } } } }'

# 2. fetch issue node id
gh issue view <N> --json id

# 3. assign type (replace ids)
gh api graphql -f query='mutation($i:ID!,$t:ID!){ updateIssue(input:{id:$i,issueTypeId:$t}){ issue { number } } }' \
  -f i=<issue-node-id> -f t=<type-node-id>
```

Clear type: pass `issueTypeId: null` via `-F i=<id> -F t=`.

---

## Labels — Curated Taxonomy

**Do not invent labels.** If concept not covered below, propose addition to this file first, get approval, then create. Taxonomy has three axes: subsystem routing (`area/*`), priority (`priority/*`), cross-cutting concerns.

### Area labels — subsystem routing (mirrors `CLAUDE.md` subsystem table)

| Label | Scope |
|-------|-------|
| `area/oci` | OCI registry, index, push/pull |
| `area/package` | Package metadata, schema |
| `area/package-manager` | Install, resolve, lock |
| `area/file-structure` | Storage, CAS, symlinks |
| `area/cli` | CLI commands, API surface |
| `area/mirror` | Mirror tool, bundling |
| `area/tests` | Acceptance test infrastructure |
| `area/website` | VitePress docs site |
| `area/ci` | GitHub Actions, release pipeline |

### Priority labels — only ends of spectrum

| Label | Meaning |
|-------|---------|
| `priority/critical` | Blocks release or cause data loss |
| `priority/high` | Should land in next release |
| `priority/low` | Backlog, nice-to-have |

Default (untagged) = normal priority. No `priority/medium` — become dumping ground.

### Cross-cutting labels — apply on both issues AND PRs

| Label | Use |
|-------|-----|
| `performance` | PR tag for perf-impacting changes (types don't apply to PRs) |
| `security` | PR tag for security-sensitive changes |
| `breaking-change` | API or behavior change requiring version bump; surfaces in changelog |
| `regression` | Worked before, broken by recent change; elevates triage |
| `flaky-test` | CI reliability, distinct from product bugs |
| `dependencies` | Dependabot and manual dep bumps (auto-applied) |

### Domain / triage labels

- `ai-config` — `.claude/` maintenance (agents, skills, rules, hooks)
- GitHub defaults (`good first issue`, `help wanted`, `duplicate`, `wontfix`) — use as documented by GitHub

---

## Issues — Proposal-First Protocol

When user describe future work, feature idea, or tech debt to track:

1. **Draft** — Prepare issue(s) as proposal table, then ask for approval
2. **Refine** — Incorporate feedback, re-present if changes significant
3. **Create** — Only after explicit approval

### Proposal Format

Present all proposed issues in single overview table, then one body section per issue using template below, then ask "Shall I create these? Any changes?":

```
| # | Title | Type | Labels | Depends On |
|---|-------|------|--------|------------|
| 1 | short imperative title | Feature | area/cli, priority/high | — |
| 2 | another title | Bug | area/oci, regression | #1 |
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

**Titles**: don't prefix with `feat:` / `fix:` / `chore:` — issue type already classify work. Titles describe outcome in plain imperative form.

---

## Pull Requests

### PR Creation Protocol

1. **Branch ready** — all commits Conventional Commits, `task verify` pass (see `workflow-git.md`)
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

- **Title**: Conventional Commit format matching primary commit (e.g., `feat: three-tier CAS`). PR titles keep `type:` prefix; issue titles don't.
- **Labels**: apply `area/*` for routing; add `performance`, `security`, `breaking-change`, or `regression` when relevant. (Issue types don't apply to PRs — labels = only signal.)
- **One concern per PR**: match one-concern-per-issue rule
- **Link issues**: use `Closes #N` in body to auto-close on merge
- **Draft PRs**: use `--draft` for WIP needing early CI feedback
- **Review comments**: use `mcp__github__add_reply_to_pull_request_comment` for threaded replies

---

## Shared Conventions

- **User-first framing**: Value section answer "why should anyone care?" before "how does it work?"
- **Cross-reference**: Use `#N` to link related issues/PRs. Add "Depends on" section when order matters.
- **One concern per item**: Don't combine unrelated work
- **Mermaid where applicable**: Architecture, data flow, state machines. Not for simple lists.
- **No implementation details in titles**