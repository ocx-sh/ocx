# Increment 18: Canary Release Workflow

**Status**: Done (rewritten to use cargo-dist pipeline)
**Completed**: 2026-03-13
**ADR Phase**: 6 (canary pre-release pipeline)
**Depends on**: Increment 13 (canary enrichment — changelog/body format), Increment 10 (cargo-dist init — archive format reference)
**Design Spec**: `design_spec_canary_release.md`

---

## Goal

Create a dedicated `canary-release.yml` workflow that uses cargo-dist's `plan` and `build` pipeline to produce canary pre-release binaries. The build matrix, archive creation, checksums, and cross-compilation strategy are all derived from `dist-workspace.toml` — DRY with the release pipeline.

## Approach: Canary Version + dist Pipeline

The plan job computes a canary prerelease version (`{bumped}-canary.{short-sha}`), sets it in Cargo.toml via `sed`, and runs `dist plan --tag=v{version}` to produce a dynamic 8-entry build matrix. Build jobs mirror release.yml's structure exactly. A custom release job handles the `canary` tag and changelog.

### Version Strategy

```bash
BUMPED=$(git-cliff --bumped-version | sed 's/^v//')    # e.g., "0.6.0"
SHORT_SHA=$(git rev-parse --short HEAD)                  # e.g., "b4a845b"
CANARY_VERSION="${BUMPED}-canary.${SHORT_SHA}"            # e.g., "0.6.0-canary.b4a845b"
```

- `ocx version` in canary binaries shows the canary version (useful for debugging)
- dist recognizes the prerelease suffix → `announcement_is_prerelease: true`
- GitHub Release tag stays `canary` (constant)

### Workflow Structure (4 jobs, mirrors release.yml)

```
plan  →  build-local-artifacts (dynamic 8-entry matrix)  →  build-global-artifacts  →  release
```

## Tasks

### 1. Plan job

- Checkout with `fetch-depth: 0` (for git-cliff tag discovery)
- Install git-cliff, compute canary version
- Set version in Cargo.toml with `sed` (not committed)
- Install dist v0.31.0, cache as artifact
- Run `dist plan --tag=v{canary-version} --output-format=json`
- Outputs: `val` (manifest JSON), `tag-flag`, `canary-version`

### 2. build-local-artifacts job

Mirror of release.yml's job. Key differences:
- `needs: [plan]` (no verify-version)
- `if`: just check matrix non-null (no `publishing` / `pr_run_mode` gate)
- Before building: `sed` sets canary version in Cargo.toml (lightweight, no cargo-edit needed)

Matrix comes from `fromJson(needs.plan.outputs.val).ci.github.artifacts_matrix`.

### 3. build-global-artifacts job

Copy from release.yml. Sets canary version via `sed` before running `dist build --artifacts=global`.

### 4. Release job

Custom — collects artifacts, cleans up dist metadata, publishes canary pre-release:
- Remove dist manifests, source tarball, individual checksums
- Rename `sha256.sum` → `sha256sum.txt`
- Delete existing canary release, create new one with git-cliff changelog

### 5. SHA-pin all third-party actions

- `actions/checkout` — `de0fac2e4500dabe0009e67214ff5f5447ce83dd` (v6.0.2)
- `actions/upload-artifact` — `bbbca2ddaa5d8feaa63e36b76fdaad77386f024f` (v7.0.0)
- `actions/download-artifact` — `70fc10c6e5e1ce46ad2ea6f2b72d43f7d47b13c3` (v8.0.0)
- `orhun/git-cliff-action` — `c93ef52f3d0ddcdcc9bd5447d98d458a11cd4f72` (v4)
- `softprops/action-gh-release` — `a06a81a03ee405af7f2048a818ed3f03bbf83c7b` (v2.5.0)

## What's Reused from dist (DRY)

| Concern | Manual approach (before) | dist approach (after) |
|---------|------------------------|----------------------|
| Build matrix | Hardcoded 8 entries | Dynamic from `dist plan` via dist-workspace.toml |
| Runners | `*-latest` | dist-chosen: `macos-14`, `ubuntu-22.04-arm`, `windows-2022` |
| Cross-compilation | `houseabsolute/actions-rust-cross` | dist's native strategy (containers, cargo-xwin) |
| Archive creation | Manual `tar`/`zip` | `dist build` |
| Checksums | Manual `sha256sum`/`Get-FileHash` | `dist build --artifacts=global` |
| OS dependencies | None | `${{ matrix.packages_install }}` (musl-tools, etc.) |

## Verification

- [x] `canary-release.yml` triggers on push to `main` and `workflow_dispatch`
- [x] Concurrency group `canary-release` with `cancel-in-progress: true`
- [x] Build matrix derived dynamically from `dist plan` (8 entries from dist-workspace.toml)
- [x] build-local-artifacts mirrors release.yml structure
- [x] build-global-artifacts mirrors release.yml structure
- [x] Canary version embedded in binaries via `sed` + `--tag` flag
- [x] Release job cleans up dist metadata and renames checksum file
- [x] Existing canary release deleted before creating new one (`|| true` for first run)
- [x] Release body includes commit SHA (linked), date, actor, and git-cliff changelog
- [x] Release is `prerelease: true` and `make_latest: false`
- [x] All third-party actions are SHA-pinned with version comments
- [x] Permissions are `contents: write` only

## Files Changed

- `.github/workflows/canary-release.yml` (rewritten)
- `.agents/artifacts/design_spec_canary_release.md` (updated recommendation)
- `.agents/artifacts/increments/release_install/inc_18_canary_release.md` (this file)

## Notes

- The build matrix is now derived from `dist plan`, not maintained manually. Adding/removing targets in `dist-workspace.toml` automatically updates both release and canary workflows.
- Only the plan job installs git-cliff (for version computation). Build jobs use a lightweight `sed` to set the version — no cargo-edit or git-cliff needed per runner.
- The `sed -i.bak` pattern (with backup file deletion) works on both GNU and BSD sed (macOS).
- `deploy-canary.yml` is unchanged — it still handles CI verification separately.
