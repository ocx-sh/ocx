---
paths:
  - ".github/ISSUE_TEMPLATE/**"
---

# GitHub Workflow — Issues & Pull Requests

**Never create issues or PRs without user approval.** Pushing triggers CI, which has real cost.

## Tooling: MCP first

Prefer `mcp__github__*` MCP tools for structured, typed access. `gh` CLI is a fallback when MCP lacks parity (label admin, org issue-type admin, ad-hoc JSON projections) or when a parallel session already uses `gh`. When `GITHUB_TOKEN` is set in the environment, `gh` uses it and ignores stored credentials — prefix with `env -u GITHUB_TOKEN gh ...` if you need the stored credential's scopes (e.g., `admin:org`).

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

Every issue MUST have a type. Types replace the old `bug` / `enhancement` / `chore` labels entirely. Types do NOT apply to PRs — use labels on PRs for cross-cutting concerns.

| Type | When to use |
|------|-------------|
| **Bug** | Broken behavior, unexpected output, crash, spec deviation |
| **Feature** | New user-facing capability, command, or behavior |
| **Task** | Internal work with no user-visible capability — refactor, CI, tooling, chore, AI-config maintenance |
| **Documentation** | Docs-only: website, user guide, `--help`, ADRs, man pages |
| **Performance** | Measurable throughput / latency / memory / RSS change |
| **Security** | Vulnerability, hardening, auth, credentials, secret handling |

**When ambiguous**: a perf-motivated refactor is **Performance**, not Task. A bug fix that adds a new flag is still **Bug**. A new doc site feature (beyond docs content) is **Feature**.

Discover types programmatically via `mcp__github__list_issue_types` (or `env -u GITHUB_TOKEN gh api /orgs/ocx-sh/issue-types`).

**Setting the type on an issue.** Prefer `mcp__github__issue_write` (supports `type:` natively). If you must fall back to `gh`, note that `gh issue create/edit` has no `--type` flag — use the GraphQL `updateIssue` mutation with the type's node id. Fetch ids once, then update:

```sh
# 1. fetch org type ids (one-shot)
gh api graphql -f query='query { organization(login:"ocx-sh") { issueTypes(first:20) { nodes { id name } } } }'

# 2. fetch issue node id
gh issue view <N> --json id

# 3. assign type (replace ids)
gh api graphql -f query='mutation($i:ID!,$t:ID!){ updateIssue(input:{id:$i,issueTypeId:$t}){ issue { number } } }' \
  -f i=<issue-node-id> -f t=<type-node-id>
```

To clear a type, pass `issueTypeId: null` via `-F i=<id> -F t=`.

---

## Labels — Curated Taxonomy

**Do not invent labels.** If a concept isn't covered below, propose an addition to this file first, get approval, then create it. The taxonomy has three axes: subsystem routing (`area/*`), priority (`priority/*`), and cross-cutting concerns.

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

### Priority labels — only the ends of the spectrum

| Label | Meaning |
|-------|---------|
| `priority/critical` | Blocks a release or causes data loss |
| `priority/high` | Should land in the next release |
| `priority/low` | Backlog, nice-to-have |

Default (untagged) = normal priority. No `priority/medium` — it becomes a dumping ground.

### Cross-cutting labels — apply on both issues AND PRs

| Label | Use |
|-------|-----|
| `performance` | PR tag for perf-impacting changes (types don't apply to PRs) |
| `security` | PR tag for security-sensitive changes |
| `breaking-change` | API or behavior change requiring a version bump; surfaces in changelog |
| `regression` | Worked before, broken by a recent change; elevates triage |
| `flaky-test` | CI reliability, distinct from product bugs |
| `dependencies` | Dependabot and manual dep bumps (auto-applied) |

### Domain / triage labels

- `ai-config` — `.claude/` maintenance (agents, skills, rules, hooks)
- GitHub defaults (`good first issue`, `help wanted`, `duplicate`, `wontfix`) — use as documented by GitHub

---

## Issues — Proposal-First Protocol

When the user describes future work, a feature idea, or technical debt to track:

1. **Draft** — Prepare issue(s) as a proposal table, then ask for approval
2. **Refine** — Incorporate feedback, re-present if changes are significant
3. **Create** — Only after explicit approval

### Proposal Format

Present all proposed issues in a single overview table, then one body section per issue using the template below, then ask "Shall I create these? Any changes?":

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

**Titles**: do not prefix with `feat:` / `fix:` / `chore:` — the issue type already classifies the work. Titles describe the outcome in plain imperative form.

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

- **Title**: Conventional Commit format matching the primary commit (e.g., `feat: three-tier CAS`). PR titles keep the `type:` prefix; issue titles do not.
- **Labels**: apply `area/*` for routing; add `performance`, `security`, `breaking-change`, or `regression` when relevant. (Issue types do not apply to PRs — labels are the only signal.)
- **One concern per PR**: matches the one-concern-per-issue rule
- **Link issues**: use `Closes #N` in the body to auto-close on merge
- **Draft PRs**: use `--draft` for work-in-progress that needs early CI feedback
- **Review comments**: use `mcp__github__add_reply_to_pull_request_comment` for threaded replies

---

## Shared Conventions

- **User-first framing**: Value section answers "why should anyone care?" before "how does it work?"
- **Cross-reference**: Use `#N` to link related issues/PRs. Add a "Depends on" section when order matters.
- **One concern per item**: Don't combine unrelated work
- **Mermaid where applicable**: Architecture, data flow, state machines. Not for simple lists.
- **No implementation details in titles**
