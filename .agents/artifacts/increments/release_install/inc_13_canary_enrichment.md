# Increment 13: Enrich Canary Release Body with git-cliff

**Status**: Done
**Completed**: 2026-03-13
**ADR Phase**: 6 (step 19)
**Depends on**: Increment 02 (cliff.toml must exist)

---

## Goal

Enrich the canary pre-release body with commit SHA, build date, and unreleased changelog from git-cliff.

## Tasks

### 1. Add git-cliff step to `deploy-canary.yml`

Add a step early in the workflow (after checkout):

```yaml
- name: Generate unreleased changelog
  uses: orhun/git-cliff-action@v4  # SHA-pin this
  with:
    config: cliff.toml
    args: --unreleased --strip header
  env:
    OUTPUT: UNRELEASED_CHANGES.md
```

### 2. Update the GitHub Release step

Modify the existing `softprops/action-gh-release` step to include richer metadata:

```yaml
- name: Create/update canary release
  uses: softprops/action-gh-release@v2  # SHA-pin this
  with:
    tag_name: canary
    name: "Canary (main@${{ github.sha }})"
    prerelease: true
    make_latest: false
    body: |
      **Canary build from `main`**

      | | |
      |---|---|
      | **Commit** | [`${{ github.sha }}`](https://github.com/ocx-sh/ocx/commit/${{ github.sha }}) |
      | **Date** | ${{ github.event.head_commit.timestamp || github.event.repository.pushed_at }} |
      | **Triggered by** | @${{ github.actor }} |

      ---

      ## Changes since last release

      ${{ steps.cliff.outputs.content || 'No conventional commits since last release.' }}
    files: |
      artifacts/*
```

### 3. Set step ID on git-cliff action

The git-cliff action step needs an `id` to reference its output:

```yaml
- name: Generate unreleased changelog
  id: cliff
  uses: orhun/git-cliff-action@v4
  ...
```

### 4. Update canary title format

Change from whatever the current title is to: `"Canary (main@{SHORT_SHA})"` — the first 7 chars of the commit SHA is enough for identification.

## Verification

- [ ] Canary release body includes commit SHA with link
- [ ] Canary release body includes build date
- [ ] Canary release body includes unreleased changelog (or fallback message)
- [ ] Title format: `"Canary (main@abc1234)"`
- [ ] Still marked as `prerelease: true`
- [ ] Still marked as `make_latest: false`
- [ ] git-cliff action is SHA-pinned
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/deploy-canary.yml` (modify)

## Notes

- The git-cliff `--unreleased` flag shows changes since the last version tag. If there are no conventional commits, the output will be empty — the body template handles this with a fallback message.
- The canary release continues to use the `canary` tag (overwritten on each build). No change to the tag pattern.
- `fetch-depth: 0` may be needed in the checkout step for git-cliff to find tags.
