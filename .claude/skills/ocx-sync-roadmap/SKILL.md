---
name: ocx-sync-roadmap
description: Sync the website roadmap page with GitHub issues and PRs. Scans open/closed issues and open/merged PRs, matches them to roadmap features, updates statuses, adds missing links, and proposes new features from unlinked issues.
disable-model-invocation: true
---

# Sync Roadmap

Synchronize `website/src/docs/roadmap.md` with the current state of GitHub issues and pull requests.

## Workflow

### Step 1: Gather GitHub State

**Prefer MCP tools** `mcp__github__list_issues` and `mcp__github__list_pull_requests` for structured access — they return typed objects without shell parsing and avoid subprocess startup cost. Request `state: "all"` and paginate as needed.

Fallback (when MCP is unavailable or lacks a specific field projection), run these in parallel:

```sh
gh issue list --state all --limit 200 --json number,title,state,labels
gh pr list --state all --limit 200 --json number,title,state,labels
```

### Step 2: Read Current Roadmap

Read `website/src/docs/roadmap.md` and parse:
- Each `<RoadmapItem>` (title, accent, icon)
- Each `<RoadmapFeature>` within it (text, status, issue number, PR number)

### Step 3: Match and Update

For each roadmap feature that has an `issue` or `pr` prop:

| GitHub State | → Roadmap Status |
|-------------|-----------------|
| Issue open, no PR | `planned` |
| PR open | `active` |
| PR merged | `shipped` |
| Issue closed (no PR) | `shipped` |

Update the `status` prop on each `<RoadmapFeature>` accordingly.

### Step 4: Find Unlinked Issues

Scan issues and PRs with titles matching roadmap themes:

| Issue/PR title contains | Likely roadmap item |
|------------------------|-------------------|
| layer, compose, multi-layer | Composable Packages |
| depend, resolution, toolchain | Dependencies |
| require, capability, glibc, variant | System Requirements |
| referrer, sbom, signature, attestation | Referrer API |
| bazel, cmake, gradle, action, devcontainer, shim | Interoperability |
| semver, stable, schema, diagnostic, symlink, path | Hardening |

### Step 5: Propose Changes

Present a summary table of all changes before applying:

```
## Roadmap Sync Proposal

### Status Updates
| Feature | Old Status | New Status | Reason |
|---------|-----------|------------|--------|
| Multi-layer artifacts | active | shipped | PR #22 merged |

### New Links
| Feature | Added |
|---------|-------|
| Schema validation | issue #30 |

### Suggested New Features
| Item | Feature | Issue/PR |
|------|---------|----------|
| Hardening | Error context improvements | #35 |

Apply these changes? [y/n]
```

### Step 6: Apply

Edit `website/src/docs/roadmap.md` with the approved changes.

## Feature Conventions

- Feature text: 1-3 words, nominative style, consistent capitalization
- Shipped features listed first within their `<RoadmapFeatures>` block
- Active features next, planned last
- Both `issue` and `pr` can be set on the same feature

## Component Reference

```md
<RoadmapFeature status="active" issue="20" pr="22">Multi-layer artifacts</RoadmapFeature>
```

| Prop | Values | Description |
|------|--------|-------------|
| `status` | `shipped`, `active`, `planned` | Feature progress |
| `issue` | number | GitHub issue (links to ocx-sh/ocx/issues/{n}) |
| `pr` | number | GitHub PR (links to ocx-sh/ocx/pull/{n}) |
