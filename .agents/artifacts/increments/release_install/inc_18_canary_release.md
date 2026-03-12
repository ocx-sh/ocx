# Increment 18: Canary Release Workflow

**Status**: Not Started
**Completed**: —
**ADR Phase**: 6 (canary pre-release pipeline)
**Depends on**: Increment 13 (canary enrichment — changelog/body format), Increment 10 (cargo-dist init — archive format reference)
**Design Spec**: `design_spec_canary_release.md`

---

## Goal

Create a dedicated `canary-release.yml` workflow that builds all 8 targets and publishes a `canary` pre-release on every push to `main`. This separates the release concern from the verification concern currently combined in `deploy-canary.yml`.

## Tasks

### 1. Create `.github/workflows/canary-release.yml`

Create the new workflow with the following structure:

**Trigger and concurrency:**
```yaml
name: Canary Release

on:
  push:
    branches: [main]
  workflow_dispatch:

concurrency:
  group: canary-release
  cancel-in-progress: true

permissions:
  contents: write
```

### 2. Build job — 8-target matrix

Define a matrix strategy covering all 8 targets from `dist-workspace.toml`. Split into native builds (using `.github/actions/build-rust`) and cross builds (using `houseabsolute/actions-rust-cross`):

| Target | Runner | Method |
|--------|--------|--------|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | Native (build-rust action) |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest` | Cross (actions-rust-cross) |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | Cross (actions-rust-cross) |
| `aarch64-unknown-linux-musl` | `ubuntu-latest` | Cross (actions-rust-cross) |
| `x86_64-apple-darwin` | `macos-latest` | Cross (actions-rust-cross) |
| `aarch64-apple-darwin` | `macos-latest` | Native (build-rust action) |
| `x86_64-pc-windows-msvc` | `windows-latest` | Native (build-rust action) |
| `aarch64-pc-windows-msvc` | `windows-latest` | Cross (actions-rust-cross) |

Use a matrix include with a `cross: true/false` field to select the build method. Each matrix entry needs: `os`, `target`, `cross`.

Steps per build job:
1. Checkout with `submodules: true`
2. Conditional: if `cross == false`, use `.github/actions/build-rust`; if `cross == true`, use `houseabsolute/actions-rust-cross`
3. Create archive — `.tar.xz` for Unix targets, `.zip` for Windows targets
4. Generate SHA-256 checksum file for the archive
5. Upload archive + checksum as artifact

### 3. Archive creation steps

**Archive naming convention** (matches cargo-dist): `ocx-{target}.tar.xz` or `ocx-{target}.zip`.

**Binary at archive root** — no nested directories. This matches cargo-dist's layout and is what the install scripts expect.

**Unix targets (.tar.xz):**
```bash
mkdir -p staging
cp "target/${TARGET}/release/ocx" staging/
tar -cJf "ocx-${TARGET}.tar.xz" -C staging ocx
sha256sum "ocx-${TARGET}.tar.xz" > "ocx-${TARGET}.tar.xz.sha256"
```

For cross builds using `actions-rust-cross`, the binary location may differ — check the output path and adjust accordingly.

**Windows targets (.zip):**
```powershell
New-Item -ItemType Directory -Path staging
Copy-Item "target\${env:TARGET}\release\ocx.exe" staging\
Compress-Archive -Path staging\ocx.exe -DestinationPath "ocx-${env:TARGET}.zip"
$hash = (Get-FileHash "ocx-${env:TARGET}.zip" -Algorithm SHA256).Hash.ToLower()
"${hash}  ocx-${env:TARGET}.zip" | Out-File "ocx-${env:TARGET}.zip.sha256" -Encoding ascii
```

Upload both the archive and `.sha256` file as a single artifact named `archive-{target}`.

### 4. Release job

Depends on the build job completing. Steps:

1. **Checkout** with `fetch-depth: 0` (needed for git-cliff to find tags)
2. **Generate changelog** with `orhun/git-cliff-action` (SHA-pinned `c93ef52f`):
   ```yaml
   - name: Generate unreleased changelog
     id: cliff
     uses: orhun/git-cliff-action@c93ef52f3d0ddcdcc9bd5447d98d458a11cd4f72 # v4
     with:
       config: cliff.toml
       args: --unreleased --strip header
     env:
       OUTPUT: UNRELEASED_CHANGES.md
   ```
3. **Download all artifacts** using `actions/download-artifact` with `pattern: archive-*` and `merge-multiple: true`
4. **Merge checksums** into a single `sha256sum.txt`:
   ```bash
   cat artifacts/*.sha256 > artifacts/sha256sum.txt
   rm artifacts/*.sha256
   ```
5. **Delete existing canary release** (idempotent):
   ```bash
   gh release delete canary --yes --cleanup-tag || true
   ```
6. **Create new pre-release** with `softprops/action-gh-release` (SHA-pinned):
   ```yaml
   - uses: softprops/action-gh-release@a06a81a03ee405af7f2048a818ed3f03bbf83c7b # v2.5.0
     with:
       tag_name: canary
       name: "Canary (main@${{ github.sha }})"
       body: |
         **Canary build from `main`**

         | | |
         |---|---|
         | **Commit** | [`${{ github.sha }}`](${{ github.server_url }}/${{ github.repository }}/commit/${{ github.sha }}) |
         | **Date** | ${{ github.event.head_commit.timestamp }} |
         | **Triggered by** | @${{ github.actor }} |

         ---

         ## Changes since last release

         ${{ steps.cliff.outputs.content || 'No conventional commits since last release.' }}
       prerelease: true
       make_latest: false
       files: |
         artifacts/*.tar.xz
         artifacts/*.zip
         artifacts/sha256sum.txt
   ```

### 5. SHA-pin all third-party actions

Every third-party action must be SHA-pinned with a version comment:
- `actions/checkout` — `de0fac2e4500dabe0009e67214ff5f5447ce83dd` (v6.0.2)
- `houseabsolute/actions-rust-cross` — `9a1618ffb70e8374ab5f48fcccea3ebeacf57971` (v1.0.5)
- `actions/upload-artifact` — `bbbca2ddaa5d8feaa63e36b76fdaad77386f024f` (v7.0.0)
- `actions/download-artifact` — `70fc10c6e5e1ce46ad2ea6f2b72d43f7d47b13c3` (v8.0.0)
- `orhun/git-cliff-action` — `c93ef52f3d0ddcdcc9bd5447d98d458a11cd4f72` (v4)
- `softprops/action-gh-release` — `a06a81a03ee405af7f2048a818ed3f03bbf83c7b` (v2.5.0)

### 6. Remove `snapshot-release` and `build-cross` jobs from `deploy-canary.yml`

Remove these two jobs from `.github/workflows/deploy-canary.yml`:
- `build-cross` — cross-compilation builds
- `snapshot-release` — canary release publishing

These concerns are now handled by `canary-release.yml`. The remaining jobs in `deploy-canary.yml` should be:
- `build` — native build + test (3 targets)
- `acceptance-tests` — pytest acceptance tests
- `test-results` — publish test results

Update the `needs` arrays on remaining jobs to remove references to deleted jobs.

**Do NOT rename `deploy-canary.yml` in this increment** — that happens in increment 19 (verify-deep).

### 7. Add comment referencing `dist-workspace.toml`

Add a comment in the matrix section of `canary-release.yml`:
```yaml
# Targets must match dist-workspace.toml — sync when adding/removing platforms
```

## Verification

- [ ] `canary-release.yml` triggers on push to `main` and `workflow_dispatch`
- [ ] Concurrency group `canary-release` with `cancel-in-progress: true`
- [ ] All 8 targets from `dist-workspace.toml` are present in the build matrix
- [ ] Native builds use `.github/actions/build-rust` action
- [ ] Cross builds use `houseabsolute/actions-rust-cross` (SHA-pinned)
- [ ] Unix archives are `.tar.xz`, Windows archives are `.zip`
- [ ] Archive naming follows `ocx-{target}.tar.xz` / `ocx-{target}.zip`
- [ ] Binary is at archive root (no nested directory)
- [ ] Individual `.sha256` files merged into `sha256sum.txt` in release job
- [ ] Existing canary release is deleted before creating a new one (`|| true` for first run)
- [ ] Release body includes commit SHA (linked), date, actor, and git-cliff changelog
- [ ] Release is `prerelease: true` and `make_latest: false`
- [ ] All third-party actions are SHA-pinned with version comments
- [ ] `snapshot-release` and `build-cross` jobs removed from `deploy-canary.yml`
- [ ] Remaining `deploy-canary.yml` jobs still function correctly
- [ ] Permissions are `contents: write` only
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/canary-release.yml` (create)
- `.github/workflows/deploy-canary.yml` (modify — remove `snapshot-release` and `build-cross` jobs)

## Notes

- The build matrix is maintained separately from `dist-workspace.toml` targets. This is intentional: targets change rarely, and the canary workflow must not depend on cargo-dist's plan/build/host pipeline. A comment points to `dist-workspace.toml` as the source of truth.
- The `gh release delete canary --yes --cleanup-tag || true` approach avoids race conditions — `--cleanup-tag` deletes both the release and tag in one operation.
- On Windows runners, `sha256sum` is not available. Use PowerShell `Get-FileHash` instead, formatting output as `{hash}  {filename}` to match `sha256sum` format.
- The release job gets `GITHUB_TOKEN` automatically from the `permissions: contents: write` block.
