# Increment 12: Post-Release Version Bump PR Automation

**Status**: Done
**Completed**: 2026-03-13
**ADR Phase**: 5 (step 18)
**Depends on**: Increment 10 (release workflow must exist), Increment 02 (cliff.toml must exist)

---

## Goal

After a release is published, automatically open a PR that bumps `Cargo.toml` to the next development version.

## Tasks

### 1. Add post-release job to `release.yml`

Add a job that runs after the release host job completes:

```yaml
post-release:
  needs: [host]  # or whatever cargo-dist names its final job
  runs-on: ubuntu-latest
  permissions:
    contents: write
    pull-requests: write
  steps:
    - uses: actions/checkout@v4
      with:
        fetch-depth: 0

    - name: Install tools
      run: |
        cargo install cargo-edit git-cliff

    - name: Compute next version
      run: |
        RELEASED="${GITHUB_REF_NAME#v}"
        NEXT=$(git-cliff --bumped-version)
        NEXT="${NEXT#v}"
        # Edge case: immediately after tagging, zero unreleased commits.
        # git-cliff may return the same version as just released.
        if [ "$NEXT" = "$RELEASED" ]; then
          IFS='.' read -r MAJOR MINOR PATCH <<< "$RELEASED"
          NEXT="${MAJOR}.$((MINOR + 1)).0"
        fi
        echo "NEXT_VERSION=$NEXT" >> $GITHUB_ENV

    - name: Bump version
      run: cargo set-version "${{ env.NEXT_VERSION }}"

    - name: Create version bump PR
      uses: peter-evans/create-pull-request@v6  # SHA-pin this
      with:
        title: "chore: bump version to ${{ env.NEXT_VERSION }}"
        branch: chore/bump-version-${{ env.NEXT_VERSION }}
        commit-message: "chore: bump version to ${{ env.NEXT_VERSION }}"
        body: |
          Automated version bump after release ${{ github.ref_name }}.

          This PR updates `Cargo.toml` workspace version to `${{ env.NEXT_VERSION }}` for the next development cycle.
        delete-branch: true
```

### 2. Edge case handling

The ADR calls out a specific edge case:
- After tagging `v0.5.0`, `git-cliff --bumped-version` operates on zero unreleased commits
- With `features_always_bump_minor = true`, it may return the same version as just released
- The script must detect this and default to bumping minor: `0.5.0` → `0.6.0`

### 3. PR format

- Title: `chore: bump version to X.Y.Z` (passes conventional commit check)
- Branch: `chore/bump-version-X.Y.Z`
- Body: explains what happened and why
- `delete-branch: true` cleans up after merge

## Verification

- [ ] Post-release job triggers after the host job completes
- [ ] Version computation handles the edge case (same version → bump minor)
- [ ] PR title follows conventional commit format (`chore:` prefix)
- [ ] PR body is clear and informative
- [ ] `peter-evans/create-pull-request` action is SHA-pinned
- [ ] Job has correct permissions (`contents: write`, `pull-requests: write`)
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/release.yml` (add post-release job)

## Notes

- `cargo-edit` and `git-cliff` installation in CI adds ~1-2 min. Consider caching or using pre-built binaries.
- The PR must be reviewed and merged by a human (project convention: never push directly to main).
- If the version bump PR is not merged before the next release, `release:prepare` will handle it — but the CI warning from Increment 18 will flag this.
- The `chore:` prefix ensures git-cliff filters this commit out of the changelog.
