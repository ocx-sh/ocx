# Increment 17: Publish OCX to Own Registry (CI Job + Metadata)

**Status**: Done
**Completed**: 2026-03-13
**ADR Phase**: 8 (step 25)
**Depends on**: Increment 10 (release workflow exists)

---

## Goal

Add a release pipeline job that publishes OCX binaries to `ocx.sh/ocx` as an OCX package, enabling the self-bootstrapping install flow and self-update mechanism.

## Tasks

### 1. Create OCX package `metadata.json`

Create a metadata file for the OCX package itself (can live at repo root or in a dedicated directory):

```json
{
  "env": [
    {
      "key": "PATH",
      "value": "${installPath}/bin",
      "type": "path"
    }
  ]
}
```

This ensures `ocx shell env ocx --current` produces the correct PATH export.

### 2. Add `publish-to-registry` job to `release.yml`

```yaml
publish-to-registry:
  needs: [host]  # after GitHub Release is published
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Download release artifacts
      uses: actions/download-artifact@v4

    - name: Push to OCX registry
      run: |
        VERSION="${GITHUB_REF_NAME#v}"

        # Target triple → OCI platform mapping
        declare -A PLATFORM_MAP=(
          ["x86_64-unknown-linux-gnu"]="linux/amd64"
          ["aarch64-unknown-linux-gnu"]="linux/arm64"
          ["x86_64-unknown-linux-musl"]="linux/amd64"
          ["aarch64-unknown-linux-musl"]="linux/arm64"
          ["x86_64-apple-darwin"]="darwin/amd64"
          ["aarch64-apple-darwin"]="darwin/arm64"
          ["x86_64-pc-windows-msvc"]="windows/amd64"
          ["aarch64-pc-windows-msvc"]="windows/arm64"
        )

        for archive in artifacts/ocx-*.tar.xz artifacts/ocx-*.zip; do
          [ -f "$archive" ] || continue
          TARGET=$(basename "$archive" | sed -E 's/ocx-(.*)\.(tar\.xz|zip)/\1/')
          PLATFORM="${PLATFORM_MAP[$TARGET]}"
          if [ -z "$PLATFORM" ]; then
            echo "WARNING: Unknown target $TARGET, skipping"
            continue
          fi
          echo "Publishing $archive as ocx.sh/ocx:${VERSION} ($PLATFORM)"
          ocx package push "ocx.sh/ocx:${VERSION}" "$archive" \
            --platform "$PLATFORM" --cascade -m metadata.json
        done
      env:
        OCX_AUTH_ocx_sh_USERNAME: ${{ secrets.OCX_REGISTRY_USERNAME }}
        OCX_AUTH_ocx_sh_PASSWORD: ${{ secrets.OCX_REGISTRY_PASSWORD }}
```

### 3. Archive layout requirement

**Critical**: The archive must contain `bin/ocx` (or `bin/ocx.exe`) at its root. The `~/.ocx/env` bootstrap file resolves the binary at `$OCX_HOME/installs/ocx.sh/ocx/current/bin/ocx`.

If cargo-dist packages the binary differently (e.g., flat at root), the publish job needs to repackage it:
1. Extract the cargo-dist archive
2. Create `bin/` directory
3. Move the binary into `bin/`
4. Re-archive

Check cargo-dist's default archive layout before implementing.

### 4. Tag convention

OCX follows its own cascade convention:
- `ocx:0.5.0+build.1` (build-tagged, if applicable)
- `ocx:0.5.0`, `ocx:0.5`, `ocx:0`, `ocx:latest` via `--cascade`

### 5. Registry credentials

The job needs `OCX_REGISTRY_USERNAME` and `OCX_REGISTRY_PASSWORD` secrets. These must be configured in the GitHub repository settings before this job can run.

## Verification

- [ ] `metadata.json` declares PATH env var correctly
- [ ] Publish job runs after the host job completes
- [ ] All 8 platform binaries are pushed with correct OCI platform labels
- [ ] `--cascade` creates rolling tags
- [ ] Archive contains `bin/ocx` at root (or repackaging step handles this)
- [ ] Registry credentials are used via env vars (not hardcoded)
- [ ] `task verify` still passes

## Files Changed

- `metadata.json` or similar (OCX package metadata — new)
- `.github/workflows/release.yml` (add publish-to-registry job)

## Notes

- This job requires the OCX binary to be available on the runner (either from artifacts or pre-installed). If OCX isn't available, the job needs to install it first.
- The musl targets map to the same OCI platforms as the glibc targets (`linux/amd64`, `linux/arm64`). The OCI manifest index handles multi-variant selection.
- This is what enables `ocx install ocx --select` to work — OCX becomes a regular package in its own registry.
- The chicken-and-egg problem is solved: install scripts download from GitHub Releases (always available), then OCX manages itself from the registry.
