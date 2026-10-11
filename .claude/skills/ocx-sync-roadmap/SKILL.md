---
name: ocx-sync-roadmap
description: Use when syncing website roadmap page against current GitHub issues and PRs. Matches open/closed issues + open/merged PRs to roadmap features, updates statuses, adds links, flags unlinked issues as candidates. Triggers: "sync roadmap", "/ocx-sync-roadmap".
disable-model-invocation: true
---

# Sync Roadmap

Sync `website/src/content/docs/roadmap.md` with current GitHub issues + PRs.

## Workflow

### Step 1: Gather GitHub State

**Prefer MCP tools** `mcp__github__list_issues` and `mcp__github__list_pull_requests` — return typed objects, no shell parsing, no subprocess cost. Request `state: "all"`, paginate as needed.

Fallback (MCP unavailable or missing field projection), run parallel:

```sh
gh issue list --state all --limit 200 --json number,title,state,labels
gh pr list --state all --limit 200 --json number,title,state,labels
```

### Step 2: Read Current Roadmap

Read `website/src/content/docs/roadmap.md`, parse:
- Each `##` theme section (title, intro paragraph)
- Each row of its `Feature | Status | Link` table (feature, status, issue and PR links)

### Step 3: Match and Update

Each roadmap row with an issue or PR link:

| GitHub State | → Roadmap Status |
|-------------|-----------------|
| Issue open, no PR | Planned |
| PR open | In progress |
| PR merged | Shipped |
| Issue closed (no PR) | Shipped |

Update the Status cell of each row.

### Step 4: Find Unlinked Issues

Scan issues + PRs with titles matching roadmap themes:

| Issue/PR title contains | Likely roadmap item |
|------------------------|-------------------|
| layer, compose, multi-layer | Composable Packages |
| depend, resolution, toolchain | Dependencies |
| require, capability, glibc, variant | System Requirements |
| referrer, sbom, signature, attestation | Referrer API |
| bazel, cmake, gradle, action, devcontainer, shim | Interoperability |
| semver, stable, schema, diagnostic, symlink, path | Hardening |

### Step 5: Propose Changes

Show summary table of all changes before apply:

```
## Roadmap Sync Proposal

### Status Updates
| Feature | Old Status | New Status | Reason |
|---------|-----------|------------|--------|
| Multi-layer artifacts | In progress | Shipped | PR #22 merged |

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

Edit `website/src/content/docs/roadmap.md` with approved changes.

## Feature Conventions

- Feature text: 1-3 words, nominative, consistent capitalization
- Rows ordered Shipped first, then In progress, then Planned
- Link cell lists issue and PR links (`[#20](https://github.com/ocx-sh/ocx/issues/20)`); `—` when none
- Status values: `Shipped`, `In progress`, `Planned`
