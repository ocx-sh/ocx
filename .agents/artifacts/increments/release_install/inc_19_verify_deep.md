# Increment 19: Verify-Deep Workflow

**Status**: Not Started
**Completed**: —
**ADR Phase**: 6 (pre-release verification)
**Depends on**: Increment 18 (canary release — `snapshot-release` and `build-cross` must be removed first)

---

## Goal

Rename `deploy-canary.yml` to `verify-deep.yml` and extend it with cross-platform acceptance tests (macOS and Windows), each running against a Docker registry. This workflow becomes the comprehensive manual verification gate before releasing.

## Tasks

### 1. Rename `deploy-canary.yml` → `verify-deep.yml`

Rename the workflow file and update:
- `name:` field to `Verify Deep`
- Trigger remains `workflow_dispatch` only

### 2. Extend acceptance tests to macOS and Windows

The current `acceptance-tests` job runs only on Linux using the `services:` block for `registry:2`. GitHub Actions `services:` is Linux-only, so macOS and Windows need manual Docker registry startup.

Create a matrix for acceptance tests:

| Runner | Registry Start Method |
|--------|----------------------|
| `ubuntu-latest` | `services:` block (existing) OR `docker run -d` |
| `macos-latest` | `docker run -d -p 5000:5000 registry:2` |
| `windows-latest` | `docker run -d -p 5000:5000 registry:2` |

**Option A: Unified matrix with manual `docker run`** (preferred — simpler, consistent):
- Remove the `services:` block
- Add a step that runs `docker run -d -p 5000:5000 registry:2` on all platforms
- Wait for registry readiness (poll `http://localhost:5000/v2/`)
- This works on all three runner OSes (GitHub-hosted runners have Docker pre-installed)

**Option B: Separate jobs per platform** with platform-specific registry setup:
- More verbose but allows platform-specific customization

### 3. Update binary download per platform

Each acceptance test runner needs the correct platform binary:

| Runner | Binary Artifact |
|--------|----------------|
| `ubuntu-latest` | `ocx-x86_64-unknown-linux-gnu` |
| `macos-latest` | `ocx-aarch64-apple-darwin` |
| `windows-latest` | `ocx-x86_64-pc-windows-msvc` |

Add matrix entries mapping runner → binary artifact name.

### 4. Handle Windows-specific test considerations

- Binary extension: `ocx.exe` on Windows
- Path separator: tests should handle `\` vs `/`
- Docker Desktop must be available on the Windows runner
- Some tests may need Windows-specific adjustments (check pytest fixtures in `conftest.py`)

### 5. Handle macOS-specific test considerations

- Docker Desktop is available on GitHub-hosted macOS runners
- Verify `docker` CLI is in PATH
- No special binary permissions needed (unlike Linux where chmod +x is required — macOS artifacts from the build step should already be executable)

### 6. Update `test-results` job

The `test-results` job should collect results from all three platform acceptance test runs, not just Linux. Update the artifact download pattern if needed.

### 7. Remove main-branch guards

With `snapshot-release` and `build-cross` gone (removed in inc 18), review remaining `if: github.ref == 'refs/heads/main'` guards. The verify-deep workflow is manual-only and should work on any branch — remove main-branch restrictions.

## Verification

- [ ] Workflow file renamed to `verify-deep.yml`
- [ ] Workflow name is `Verify Deep`
- [ ] Trigger is `workflow_dispatch` only
- [ ] Unit tests run on Linux, macOS, and Windows (existing `build` job)
- [ ] Acceptance tests run on Linux, macOS, and Windows
- [ ] Docker registry starts correctly on all three platforms
- [ ] Correct platform binary is downloaded for each acceptance test runner
- [ ] Test results are collected from all platforms
- [ ] No `snapshot-release` or `build-cross` jobs remain
- [ ] No main-branch guards remain (workflow works on any branch)
- [ ] All third-party actions remain SHA-pinned
- [ ] `task verify` still passes

## Files Changed

- `.github/workflows/deploy-canary.yml` → `.github/workflows/verify-deep.yml` (rename + modify)

## Notes

- GitHub-hosted runners have Docker pre-installed on all three OSes (Ubuntu, macOS, Windows). However, Docker availability on macOS and Windows runners should be verified — if Docker is not reliably available, acceptance tests on those platforms may need to be conditional.
- The `services:` block is a GitHub Actions feature that only works on Linux. For cross-platform consistency, prefer `docker run -d` on all platforms.
- pytest's `conftest.py` starts the registry via `docker compose up -d` in `pytest_sessionstart`. When running in CI with a pre-started registry, the fixture detects the registry is already running and skips startup. Ensure the CI registry start happens before pytest runs.
- Windows acceptance tests may surface path-handling bugs — this is valuable and one of the key reasons for cross-platform testing.
