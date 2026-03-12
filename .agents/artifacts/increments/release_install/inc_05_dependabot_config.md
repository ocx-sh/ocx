# Increment 05: Dependabot Conventional Commit Prefix + npm Ecosystem

**Status**: Not Started
**Completed**: —
**ADR Phase**: 2 (step 8)
**Depends on**: Increment 04 (cocogitto must be in CI so dependabot PRs pass the check)

---

## Goal

Configure Dependabot to produce `chore(deps):` prefixed commit messages (passing cocogitto) and add npm ecosystem for the website directory.

## Tasks

### 1. Update `.github/dependabot.yml`

Add `commit-message.prefix: "chore(deps)"` to each existing ecosystem entry and add the `npm` ecosystem for `/website`. **Preserve existing groups** (`actions`, `rust-deps`).

Final config (from ADR Section 9):

```yaml
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
    schedule:
      interval: "weekly"
    groups:
      rust-deps:
        patterns:
          - "*"
    commit-message:
      prefix: "chore(deps)"

  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
    groups:
      actions:
        patterns:
          - "*"
    commit-message:
      prefix: "chore(deps)"

  - package-ecosystem: "npm"
    directory: "/website"
    schedule:
      interval: "weekly"
    groups:
      npm-deps:
        patterns:
          - "*"
    commit-message:
      prefix: "chore(deps)"
```

### 2. Verify prefix behavior

After merging, the next Dependabot PR should have commit messages like:
- `chore(deps): Bump the rust-deps group with 3 updates`
- `chore(deps): Bump the actions group with 2 updates`

These will:
- Pass the cocogitto conventional commit check
- Be filtered out of the changelog by git-cliff (skips `chore` commits)

## Verification

- [ ] `.github/dependabot.yml` has `commit-message.prefix` on all three ecosystems
- [ ] Existing groups (`actions`, `rust-deps`) are preserved
- [ ] New `npm` ecosystem entry covers `/website`
- [ ] `task verify` still passes

## Files Changed

- `.github/dependabot.yml` (update)

## Notes

- The `chore(deps)` prefix (without colon in the config) produces `chore(deps): ...` in the commit message. Dependabot appends the colon automatically.
- git-cliff's `{ message = "^chore", skip = true }` rule filters these out of the changelog.
- The npm ecosystem will catch VitePress, Vue, and other website dependency updates.
