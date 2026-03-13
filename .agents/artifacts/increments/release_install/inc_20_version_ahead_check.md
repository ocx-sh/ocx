# Increment 20: Version-Ahead-of-Release CI Check

**Status**: Removed
**Completed**: 2026-03-13
**Removed**: 2026-03-13
**ADR Phase**: 9 (step 26)
**Depends on**: Increment 01 (version in Cargo.toml), Increment 12 (version bump PR exists)

---

## Goal

Add a CI warning that fires when the `Cargo.toml` version on `main` has not been bumped since the last release tag. This catches "forgot to merge the version bump PR" before the next release.

## Tasks

### 1. Add check to `verify-basic.yml`

Add a step to an existing job (or a new lightweight job) in `.github/workflows/verify-basic.yml`:

```yaml
- name: Check version bumped since last release
  if: github.ref == 'refs/heads/main'
  run: |
    LATEST_TAG=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || echo "v0.0.0")
    TAG_VERSION="${LATEST_TAG#v}"
    CARGO_VERSION=$(cargo metadata --format-version 1 --no-deps \
      | jq -r '.packages[] | select(.name=="ocx_cli") | .version')
    if [ "$CARGO_VERSION" = "$TAG_VERSION" ]; then
      echo "::warning::Cargo.toml version ($CARGO_VERSION) has not been bumped since $LATEST_TAG"
    fi
```

### 2. Warning, not failure

This is intentionally a **warning** (`::warning::`) not an error:
- Right after a release, before the version bump PR is merged, this will fire — that's expected
- The warning is informational, reminding the maintainer to merge the bump PR
- It should not block PRs or pushes

### 3. Scope

Only runs on `main` branch (not on PRs or feature branches) since the version bump PR only targets main.

## Verification

- [ ] Check runs only on `refs/heads/main`
- [ ] Produces a `::warning::` annotation (not failure)
- [ ] Handles the case where no tags exist (`v0.0.0` fallback)
- [ ] Correctly compares Cargo.toml version with latest git tag
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/verify-basic.yml` (add step)

## Removal Rationale

This increment was removed together with Increment 12 (post-release version bump).
The version-ahead warning only existed to catch "forgot to merge the version bump PR" —
once the post-release bump is removed (because `release:prepare` handles versioning at
release time), this warning has no purpose. See Increment 12's removal rationale for
the full reasoning.

## Original Notes

- Requires `fetch-depth: 0` (or at least enough depth to find the latest tag) in the checkout step.
- `jq` is available by default on GitHub Actions runners.
- `cargo metadata` requires a working Rust toolchain, which is already set up in `verify-basic.yml`.
- This is the last increment in the plan — it's a hardening measure that catches process failures.
