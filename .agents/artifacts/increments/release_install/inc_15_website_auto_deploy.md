# Increment 15: Website Auto-Deploy on Release Trigger

**Status**: Not Started
**Completed**: —
**ADR Phase**: 7 (step 22)
**Depends on**: Increment 06 (website changelog page), Increment 10 (release workflow)

---

## Goal

Automatically deploy the website after each stable release so the changelog page and install docs reflect the latest release.

## Tasks

### 1. Add `release: published` trigger to `deploy-website.yml`

Update `.github/workflows/deploy-website.yml`:

```yaml
on:
  workflow_dispatch:      # keep manual trigger
  release:
    types: [published]    # auto-deploy after release

jobs:
  deploy:
    if: ${{ github.event_name == 'workflow_dispatch' || !github.event.release.prerelease }}
    # ... existing deployment steps
```

The `if` condition ensures:
- Manual triggers always run
- Release triggers only run for stable releases (not canary pre-releases)

### 2. Verify the trigger logic

- `release: published` fires when a GitHub Release is created or moved from draft to published
- Pre-releases (canary) also fire `published` — the `!prerelease` guard filters them out
- `workflow_dispatch` bypasses the condition entirely (for manual redeploys)

## Verification

- [ ] `deploy-website.yml` has `release: types: [published]` trigger
- [ ] Pre-release (canary) does NOT trigger a deploy
- [ ] Manual `workflow_dispatch` still works
- [ ] The `if` condition is on the job level (not step level) so the whole job is skipped for pre-releases
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/deploy-website.yml` (add trigger + condition)

## Notes

- The ADR recommends this approach over `workflow_call` (simpler, self-documenting, no coupling between workflows).
- If the website deploy fails after a release, it can be manually re-triggered via `workflow_dispatch`.
- The CHANGELOG.md `@include` in the changelog page means the website will always show the latest changelog when deployed.
