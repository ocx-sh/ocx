# Increment 04: Conventional Commit CI Enforcement (cocogitto)

**Status**: Done
**Completed**: 2026-03-12
**ADR Phase**: 2 (step 7)
**Depends on**: Nothing (can start independently, but best after Stage 1)

---

## Goal

Enforce conventional commit format on all PR commits using cocogitto in the CI pipeline.

## Tasks

### 1. Add conventional commit check to `verify-basic.yml`

Add a new job `conventional-commits` to `.github/workflows/verify-basic.yml`:

```yaml
conventional-commits:
  runs-on: ubuntu-latest
  if: github.event_name == 'pull_request'
  steps:
    - uses: actions/checkout@v4
      with:
        fetch-depth: 0
        ref: ${{ github.event.pull_request.head.sha }}

    - name: Check conventional commits
      uses: cocogitto/cocogitto-action@v4
      with:
        command: check
        args: "${{ github.event.pull_request.base.sha }}..${{ github.event.pull_request.head.sha }}"
```

Key details:
- `fetch-depth: 0` — full history needed for range validation
- `ref: head.sha` — check the PR branch, not the merge commit
- Only runs on `pull_request` events (not push to main)
- SHA-pin the action (look up latest cocogitto-action SHA)

### 2. Verify existing commits

Before enforcing, check that recent commits on main follow conventional format:
```bash
cog check
```

If there are violations in history, that's OK — the CI check only validates PR commits going forward.

### 3. Document allowed commit types

Allowed types (from ADR): `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `ci`, `chore`, `style`, `build`, `revert`. Scopes are optional and freeform.

This is already documented in the ADR but should be mentioned in the PR that adds this check.

## Verification

- [ ] `verify-basic.yml` has the new `conventional-commits` job
- [ ] The job only triggers on `pull_request` events
- [ ] Action is SHA-pinned (not just `@v4`)
- [ ] `task verify` still passes locally
- [ ] Test by creating a PR with a non-conventional commit message (should fail)

## Files Changed

- `.github/workflows/verify-basic.yml` (add job)

## Notes

- The cocogitto action uses the musl-static binary, so no Rust compilation in CI.
- This does NOT block pushes to main — only PR checks. Commits that land on main via merge must have already passed the PR check.
- If a rebase-merge workflow is used, every commit is validated individually (which is correct behavior — each commit on main should be conventional).
