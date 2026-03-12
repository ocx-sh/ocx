# Increment 11: Release Notes + Version Verification in Release Workflow

**Status**: Not Started
**Completed**: —
**ADR Phase**: 5 (steps 16-17)
**Depends on**: Increment 10 (release workflow must exist), Increment 02 (cliff.toml must exist)

---

## Goal

Add git-cliff release notes generation and Cargo.toml-tag version verification to the release workflow.

## Tasks

### 1. Add git-cliff release notes step

In `.github/workflows/release.yml`, add a step (in the host/publish job, after builds complete, before GitHub Release creation):

```yaml
- name: Generate release notes
  uses: orhun/git-cliff-action@v4  # SHA-pin this
  with:
    config: cliff.toml
    args: --latest --strip header
  env:
    OUTPUT: RELEASE_NOTES.md
```

Then modify the GitHub Release creation step to use the generated notes:

```yaml
- name: Create GitHub Release
  uses: softprops/action-gh-release@v2  # SHA-pin this
  with:
    body_path: RELEASE_NOTES.md
```

If cargo-dist creates the release automatically, instead pass the release notes body to cargo-dist's mechanism (check cargo-dist docs for custom release body support).

### 2. Add version verification step

Add early in the release workflow (before builds start):

```yaml
- name: Verify Cargo.toml version matches tag
  run: |
    TAG_VERSION="${GITHUB_REF_NAME#v}"
    CARGO_VERSION=$(cargo metadata --format-version 1 --no-deps \
      | jq -r '.packages[] | select(.name=="ocx_cli") | .version')
    if [ "$TAG_VERSION" != "$CARGO_VERSION" ]; then
      echo "ERROR: Cargo.toml version ($CARGO_VERSION) does not match tag ($TAG_VERSION)"
      exit 1
    fi
```

This catches the case where someone tags without updating Cargo.toml (or updates Cargo.toml but uses the wrong version in the tag).

### 3. Integrate with cargo-dist's release mechanism

cargo-dist may handle GitHub Release creation itself. In that case:
- Check if cargo-dist supports a `release-body` or `release-notes-path` option
- If not, the git-cliff step should run in a post-release job that updates the existing release body via `gh release edit`

Fallback approach:
```yaml
- name: Update release notes
  run: |
    gh release edit "${{ github.ref_name }}" --notes-file RELEASE_NOTES.md
  env:
    GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

## Verification

- [ ] Release notes are generated from git-cliff in keep-a-changelog format
- [ ] `--strip header` removes the "# Changelog" header (we want just the version section)
- [ ] `--latest` extracts only the current release's changes
- [ ] Version mismatch between tag and Cargo.toml fails the workflow
- [ ] git-cliff action is SHA-pinned
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/release.yml` (modify)

## Notes

- The git-cliff `--latest` flag extracts changes between the current tag and the previous tag. This requires `fetch-depth: 0` in the checkout step.
- The version verification step requires `jq` — available by default on GitHub Actions runners.
- If cargo-dist handles the release body, we need to either hook into its mechanism or update the release after cargo-dist creates it.
