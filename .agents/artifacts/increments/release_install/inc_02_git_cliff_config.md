# Increment 02: git-cliff Config + Initial CHANGELOG.md

**Status**: Done
**Completed**: 2026-03-12
**ADR Phase**: 1 (steps 4-5)
**Depends on**: Increment 01 (version must exist for git-cliff `--bump` to work)

---

## Goal

Configure git-cliff for changelog generation and produce the initial `CHANGELOG.md` from existing git history.

## Tasks

### 1. Create `cliff.toml` at repo root

Create the git-cliff configuration file with:
- Keep-a-changelog format
- Conventional commit type mapping (`feat` → Added, `fix` → Fixed, etc.)
- Skip noise commits (`chore`, `ci`, `docs`, `style`, `test`, `build`)
- Tag pattern `v[0-9].*`
- Bump config: `initial_tag = "0.0.0"`, `features_always_bump_minor = true`, `breaking_always_bump_major = false`

Use the exact config from ADR Section 7 / Phase 1 step 4.

### 2. Generate initial CHANGELOG.md

Run `git-cliff --output CHANGELOG.md` to generate the changelog from existing git history.

Review the output — it should categorize existing conventional commits correctly. Non-conventional commits will be filtered out (which is correct behavior).

### 3. Verify git-cliff commands work

Test these commands:
- `git-cliff --output CHANGELOG.md` — generates full changelog
- `git-cliff --unreleased` — shows unreleased changes
- `git-cliff --bumped-version` — shows computed next version
- `git-cliff --bump --unreleased` — previews what the next release section would look like

## Verification

- [ ] `cliff.toml` exists at repo root with correct config
- [ ] `CHANGELOG.md` exists and follows keep-a-changelog format
- [ ] `git-cliff --unreleased` produces output (or empty if all commits are tagged)
- [ ] `git-cliff --bumped-version` returns a valid semver string
- [ ] `task verify` still passes (CHANGELOG.md doesn't break anything)

## Files Changed

- `cliff.toml` (new)
- `CHANGELOG.md` (new, generated)

## Notes

- git-cliff must be installed locally: `cargo install git-cliff`
- The CHANGELOG.md will initially contain all historical conventional commits. Non-conventional commits are filtered out — this is expected and correct.
- The `[bump]` section with `breaking_always_bump_major = false` keeps us in pre-1.0 semver space until intentional.
