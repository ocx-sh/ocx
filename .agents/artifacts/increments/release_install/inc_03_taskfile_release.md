# Increment 03: Taskfile Release Commands

**Status**: Done
**Completed**: 2026-03-12
**ADR Phase**: 1 (step 6)
**Depends on**: Increment 02 (git-cliff config must exist)

---

## Goal

Add Taskfile commands for changelog management and release preparation, creating a reproducible release ceremony.

## Tasks

### 1. Create `taskfiles/release.taskfile.yml`

Following the existing pattern of subtaskfiles in `taskfiles/`, create a release taskfile with:

#### `changelog` task
```yaml
changelog:
  desc: Update CHANGELOG.md from git history
  cmds:
    - git-cliff --output CHANGELOG.md
```

#### `changelog:preview` task
```yaml
changelog:preview:
  desc: Preview unreleased changes
  cmds:
    - git-cliff --unreleased
```

#### `release:prepare` task
```yaml
release:prepare:
  desc: Prepare a release (compute version, update changelog, verify)
  cmds:
    - |
      NEXT=$(git-cliff --bumped-version)
      echo "Next version: $NEXT"
      cargo set-version "${NEXT#v}"
    - git-cliff --bump -o CHANGELOG.md
    - task verify
    - echo "Ready to commit, tag, and push"
```

### 2. Include in root `taskfile.yml`

Add the include to the root taskfile:
```yaml
includes:
  release:
    taskfile: ./taskfiles/release.taskfile.yml
```

Verify the namespace doesn't conflict with existing tasks.

### 3. Test the commands

- `task release:changelog` — should regenerate CHANGELOG.md
- `task release:changelog:preview` — should show unreleased changes
- `task release:prepare` — should compute version, update changelog, run verify

## Verification

- [ ] `task release:changelog` runs successfully and updates CHANGELOG.md
- [ ] `task release:changelog:preview` shows output
- [ ] `task --list` shows the new release tasks
- [ ] `task verify` still passes

## Files Changed

- `taskfiles/release.taskfile.yml` (new)
- `taskfile.yml` (add include)

## Notes

- `cargo set-version` requires `cargo-edit`: `cargo install cargo-edit`
- The `release:prepare` task is the entry point for the release ceremony described in the ADR.
- This task runs `task verify` internally, which includes format check, clippy, build, and all tests.
