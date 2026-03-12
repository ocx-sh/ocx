# Increment 09: Install Script CI Tests

**Status**: Done
**Completed**: 2026-03-12
**ADR Phase**: 4 (step 13)
**Depends on**: Increments 07 + 08 (install scripts must exist), Increment 10 (needs a GitHub Release to test against)

---

## Goal

Add CI workflow to test install scripts against real platforms, verifying the full bootstrap flow works.

## Tasks

### 1. Create `.github/workflows/test-install-scripts.yml`

A manual (`workflow_dispatch`) workflow that tests both install scripts. Can be extended to run automatically after releases later.

```yaml
name: Test Install Scripts
on:
  workflow_dispatch:
    inputs:
      release_tag:
        description: 'Release tag to test (e.g., v0.1.0). Defaults to latest.'
        required: false

jobs:
  test-install-sh-ubuntu:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run install.sh
        run: |
          chmod +x website/src/public/install.sh
          OCX_NO_MODIFY_PATH=1 bash website/src/public/install.sh
      - name: Verify installation
        run: |
          export PATH="$HOME/.ocx/installs/ocx.sh/ocx/current/bin:$PATH"
          ocx version

  test-install-sh-alpine:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run in Alpine container
        run: |
          docker run --rm -v "$PWD/website/src/public/install.sh:/install.sh" \
            alpine:latest sh -c "apk add curl tar xz && sh /install.sh && ocx version"

  test-install-ps1-windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run install.ps1
        shell: pwsh
        run: |
          $env:OCX_NO_MODIFY_PATH = "1"
          & ./website/src/public/install.ps1
      - name: Verify installation
        shell: pwsh
        run: |
          $env:PATH = "$env:USERPROFILE\.ocx\installs\ocx.sh\ocx\current\bin;$env:PATH"
          ocx version
```

### 2. Test matrix considerations

- **Ubuntu**: tests bash + standard Linux environment
- **Alpine**: tests POSIX shell (ash/dash), musl libc, minimal environment
- **Windows**: tests PowerShell + Windows symlinks
- **macOS**: could add `macos-latest` runner for completeness (tests `shasum` instead of `sha256sum`)

### 3. Verification checks

Each platform job should verify:
- Binary exists at the expected path
- `ocx version` runs and outputs the expected version string
- The `~/.ocx/env` file (or `env.ps1`) was created

## Verification

- [ ] Workflow file is valid YAML
- [ ] Jobs cover Linux (Ubuntu + Alpine) and Windows
- [ ] Each job verifies `ocx version` runs after install
- [ ] `workflow_dispatch` trigger works (manual testing)
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/test-install-scripts.yml` (new)

## Notes

- This workflow is initially manual (`workflow_dispatch`). After the release pipeline (Stage 6) is in place, it can be extended to run automatically post-release.
- The Alpine test validates POSIX compliance (no bash-isms in install.sh).
- macOS test is optional in the first iteration — the main concern is `shasum -a 256` vs `sha256sum`, which can be unit-tested in the script itself.
- These tests require an existing GitHub Release with binaries. They will fail until Increment 10+ creates the first release.
