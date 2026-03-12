# Release Implementation Notes

Implementation guidance for the release and install strategy (ADR: `adr_release_install_strategy.md`). Read this when working on any release infrastructure, install scripts, versioning, or changelog tasks.

## Key Decisions

### cargo-dist: Builds Only, Custom Install Scripts

cargo-dist handles binary builds, archives, checksums, and GitHub Release creation. **Do NOT use cargo-dist's generated install scripts.** OCX uses a custom bootstrap pattern where the install script downloads a bootstrap binary from GitHub Releases, runs `ocx install ocx --select`, then hands off to OCX's own package management. cargo-dist's scripts know nothing about OCX's three-store architecture.

### Version Source of Truth

`[workspace.package] version` in root `Cargo.toml` is the single source of truth. Both crates inherit via `version.workspace = true`. The version is compiled in via `env!("CARGO_PKG_VERSION")`.

### Commit Convention

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/). cocogitto validates in CI. git-cliff generates changelogs from these.

## Implementation Constraints

### OCX Package Directory Structure

When publishing OCX to its own registry (`ocx.sh/ocx`), the archive must contain `bin/ocx` (or `bin/ocx.exe` on Windows) at the root. The `~/.ocx/env` bootstrap file resolves the binary at `$OCX_HOME/installs/ocx.sh/ocx/current/bin/ocx`. If the package layout differs, the env file path breaks.

### Shell Startup: Static PATH, Dynamic Completions

The `~/.ocx/env` file uses a **static `export PATH=...`** for the OCX binary (zero exec cost) and a **dynamic eval** only for shell completions (one-time-per-session, failure-tolerant). Do NOT use `eval "$(ocx shell env ...)"` for PATH — the `current` symlink path is stable and never changes, so a static export is sufficient. Only completions require invoking the binary, and they are guarded with `2>/dev/null || true`.

### Existing Dependabot Config

The current `.github/dependabot.yml` uses dependency groups (`actions`, `rust-deps`). When adding `commit-message.prefix: "chore(deps)"` and the npm ecosystem entry, **preserve the existing groups**.

### New Environment Variables

Two new env vars are introduced:
- `OCX_NO_UPDATE_CHECK` — disables CLI update notification (truthy values: `1`, `true`, `yes`)
- `OCX_NO_MODIFY_PATH` — disables shell profile modification during install (truthy values: `1`, `true`, `yes`)

Both must be added to:
- `CLAUDE.md` environment variable table
- `website/src/docs/reference/environment.md`
- The env var parsing should use the same truthy-value logic as `OCX_OFFLINE` and `OCX_REMOTE`

### Install Script Requirements (install.sh)

- Must handle both POSIX and Fish shells
- For Fish: create `~/.config/fish/conf.d/ocx.fish` (not modify a profile)
- For bash/zsh: append `. "$HOME/.ocx/env"` to profile (with idempotent existence check)
- Must honor `--no-modify-path` flag and `OCX_NO_MODIFY_PATH=1` env var
- Must verify SHA256 checksums from `sha256sum.txt`
- Must print clear success message with next steps

### Install Script Requirements (install.ps1)

- Use `$PROFILE.CurrentUserCurrentHost` for profile modification
- Create profile file if it doesn't exist
- Write a PowerShell equivalent of `~/.ocx/env` for indirection
- Same `OCX_NO_MODIFY_PATH` opt-out

### Testing Install Scripts

Install scripts must be tested in CI:
- `install.sh`: test in a clean Docker container (ubuntu, alpine, macos runner)
- `install.ps1`: test on Windows runner
- Verify: binary works, profile was modified, `ocx version` outputs correct version

### Shared Version Utility

Currently `version.rs` calls `env!("CARGO_PKG_VERSION")` directly. The update check (Phase 8) also needs this value. Extract a shared function (e.g., in a `version` module or `app/` utility) that returns the compiled version string. Both `ocx version`, `ocx info`, and the update check should call this function — no duplicated `env!()` calls.

### Post-Release Version Bump Edge Case

After tagging (e.g., `v0.5.0`), `git-cliff --bumped-version` operates on zero unreleased commits. With `features_always_bump_minor = true`, it may return the same version as the just-released tag. The CI script must detect this and default to bumping the minor version (e.g., `0.5.0` → `0.6.0`). See the implementation in Phase 5 step 18.

## Cross-References

- **Documentation rules**: See `.claude/rules/documentation.md` for website page writing guidelines (narrative structure, headers, real-world examples, link syntax)
- **CI workflow patterns**: See `.claude/rules/ci-workflows.md` (if it exists) or follow existing patterns in `.github/workflows/`
- **CLI API patterns**: See `.claude/rules/cli-api-patterns.md` for any new report types
- **CLI commands reference**: See `.claude/rules/cli-commands.md` — update when adding new behavior
- **Environment variables**: See `website/src/docs/reference/environment.md` for the canonical env var docs

## Workflow: Release Ceremony

The release ceremony is a human-driven process with tooling support:

```bash
task release:prepare    # Compute version, update CHANGELOG.md, run verify
# Human reviews the changes
git add -A && git commit -m "release: vX.Y.Z"
git tag vX.Y.Z
# Human decides when to push (never auto-push)
```

After the tag is pushed, CI takes over: build → test → GitHub Release → publish to registry → deploy website → open version bump PR.
