---
paths:
  - cliff.toml
  - CHANGELOG.md
  - .github/workflows/release.yml
  - .github/workflows/verify-version.yml
  - .github/workflows/backfill.yml
---

# Release Implementation Notes

Guidance for release + install strategy (ADR: `adr_release_install_strategy.md`). Read when working on release infra, install scripts, versioning, or changelog.

## Key Decisions

### Release = a dispatched, hand-written workflow

There is no cargo-dist. `.github/workflows/release.yml` is hand-written and runs only on `workflow_dispatch`, from `refs/heads/main` of the ocx-sh/ocx repository. Edit it like any other workflow; no generator owns it and no drift check polices it.

Inputs: `mode`, `version`, `prune_after_days` (default 30).

| Mode | `version` | Builds and publishes |
|---|---|---|
| `release` | `X.Y.Z`, equal to the `Cargo.toml` version; refused when tag `vX.Y.Z` exists | `ghcr.io/ocx-sh/ocx/cli`, then tag `vX.Y.Z`, GitHub Release, announce, docker and sdkgen |
| `candidate` | `X.Y.Z-rcN`, same `X.Y.Z` as `Cargo.toml` | the same package, as a pre-release that cascades only to its own rolling tag; GitHub pre-release, ephemeral announce |
| `dev` | ignored | `ghcr.io/ocx-sh/dev/cli` only: no git tag, no GitHub Release, no announce, no docker or sdkgen |

Jobs, in order: `plan` (guards, channel, version, tag, target package), `readiness` (release and candidate: `readiness-gate.yml` on `github.sha`), `build` (`cross.yml` with `stamp: true`, then `release_assets.py`), `scan` (`scan-binaries.yml`: `--scan`, `--exec`, `--link`), `publish`, `github-release`, `announce`, `prune`, `post`.

- **Stamped Bazel builds.** The release binaries are Bazel cross builds through `task cross:build:release` (`--define=ocx_provenance=release`, `bazel/release/`): provenance is stamped, every crate drops `__testing`, and `release_provenance_check.py --scan` fails on any `__OCX_TESTING_` string. Stamped builds use no remote cache and no cache credential (`release.yml`, `release-readiness.yml`); `cross.yml` passes write-auth only when not stamped.
- **Sign before tag.** `publish` pushes every platform with `--sign` and an SBOM, applies the cascade once after every platform is pushed and verified, and self-verifies each platform against an identity pinned in `release.yml` (`RELEASE_IDENTITY`, the literal `…/release.yml@refs/heads/main`) for a release or a candidate, and against its own `${GITHUB_SERVER_URL}/${GITHUB_WORKFLOW_REF}` for a dev build. `github-release` needs `publish`, so no tag exists before a signature does.
- **Windows names.** Bazel builds `*-pc-windows-gnullvm`. The archives carry the same name (`TARGETS` in `scripts/release_assets.py`); releases up to v0.6.6 shipped `*-pc-windows-msvc` archives.
- **No cargo-dist installers.** OCX uses the custom bootstrap at [setup.ocx.sh](https://setup.ocx.sh): it downloads a release archive, runs `ocx self setup`, and hands off to OCX package management.
- **`backfill.yml`** (dispatch, `refs/heads/main`, environment `release-backfill`) signs and attests the platforms of already published versions that lack either, and never moves a tag. Inputs: `tags` (empty = every version tag), `dry_run`.
- **Structural tests.** `.claude/tests/test_workflows.py` pins who may hold `id-token: write` together with a signing step, the main-and-repository guards, the dev-package guard, `--force` on prune only against the dev package, and `github-release` needing `publish`.

### Linux glibc Floor

The `-gnu` archives are Bazel cross builds against a glibc sysroot. `release_provenance_check.py --link` holds the documented floor (`rust-cargo.md` REL-03) with `GLIBC_FLOOR` and `NEEDED_ALLOWED`, the one place the numbers live: `scan-binaries.yml` runs it over every Linux binary of the archives (release, candidate, dev and the readiness dispatch), floored at `link-min-files`, the Linux target count. A toolchain bump or a dependency that raises the requirement reds the scan before `publish`; raise the constant in the same change, on purpose. The musl archives are static and carry neither.

### Version Source of Truth

`[workspace.package] version` in root `Cargo.toml` = single source of truth. Both crates inherit via `version.workspace = true`. Version compiled in via `env!("CARGO_PKG_VERSION")`.

### Commit Convention

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/). cocogitto validates in CI. git-cliff generates changelogs from these.

The template's `cliff.toml` renders a commit's **subject only**, with `<` and `>` escaped and no link rewriting. So a subject names no website doc path and carries no `<tag>`: `release:validate` (`changelog:validate`) reds a rendered changelog that holds either, and a repo-relative link in a subject would resolve against no page.

## Implementation Constraints

### OCX Package Directory Structure

When publishing OCX to own registry (`ocx.sh/ocx/cli`), archive must contain `bin/ocx` (or `bin/ocx.exe` on Windows) at root. `~/.ocx/env` bootstrap file resolves binary at `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/bin/ocx`. Different package layout = env file path breaks.

### Shell Startup: Live env eval

`ocx self setup` — invoked by the per-shell installers at [setup.ocx.sh](https://setup.ocx.sh) (repo `ocx-sh/www-setup`), or run directly after a bare-binary download — writes the thin shims `$OCX_HOME/env.{sh,fish,ps1,nu,elv}` and adds a managed `# >>> ocx v1 <hash> >>>` … `# <<< ocx <<<` block to the login profile that sources the shim. Each shim runs `eval "$(ocx self activate --shell=<sh|fish|pwsh|nushell|elvish>)"` live on every shell start — the `ocx` binary is resolved via the literal install root embedded at setup time, `[ -x ]`-guarded, with `|| true` so a broken/absent binary never breaks the shell. There is no static `~/.ocx/env` PATH export and no `ocx shell env` command (deleted — see `handshake_toolchain_cli.md` §4/§7).

### Dependency Update Config

Dependency updates run via **Renovate** (replaced Dependabot). `renovate.json` is template-owned: it extends the `ocx-sh/conventions` preset (weekly, non-major groups, `chore(deps)` / `ci(deps)`, digest pinning) and then `local>ocx-sh/ocx//.github/renovate-local`. ocx's own rules live in `.github/renovate-local.json`: `rules_rs` and `llvm` move in one PR, Dockerfile base images form the `docker-base-images` group, and the website `npm` files are `/^website/`.

### New Environment Variables

Two new env vars introduced:
- `OCX_NO_UPDATE_CHECK` — disables CLI update notification (truthy: `1`, `true`, `yes`)
- `OCX_NO_MODIFY_PATH` — disables shell profile modification during install (truthy: `1`, `true`, `yes`)

Both must add to:
- `ocx_env` doc blocks, then `task website:reference:generate` (never hand-edit `website/src/content/docs/reference/environment.md`)
- Env var parsing use same truthy-value logic as `OCX_OFFLINE` and `OCX_REMOTE`

### Installers live in ocx-sh/www-setup

The `curl … | sh` / PowerShell installers are no longer in this repo. They are the five per-shell bootstraps served at [setup.ocx.sh](https://setup.ocx.sh) (repo `ocx-sh/www-setup`): each detects the platform, resolves + verifies a release from `dist.json`, and hands off to `ocx self setup`. This repo owns only the machine-state logic those installers call — `crates/ocx_setup/src/` (shims, profile blocks, completions) behind `ocx self setup`. Installer maintenance and its CI (clean-container / Windows-runner tests) live in the www-setup repo.

### Shared Version Utility

Currently `version.rs` calls `env!("CARGO_PKG_VERSION")` directly. Update check (Phase 8) also needs value. Extract shared function (e.g., in `version` module or `app/` utility) returning compiled version string. `ocx version`, `ocx info`, update check should all call this function — no duplicated `env!()` calls.

## GitHub Operations

For release-adjacent GitHub ops, prefer MCP tools where exist:

| Operation | Prefer | Fallback |
|-----------|--------|----------|
| Read release metadata | `mcp__github__list_releases`, `mcp__github__get_release` | `gh release view` |
| Create release (manual) | `gh release create` (`release.yml` owns the normal path) | — |
| Create PR | `mcp__github__create_pull_request` | `gh pr create` |
| Read PR status | `mcp__github__get_pull_request` | `gh pr view` |

Release creation during normal ceremony is the `github-release` job of `release.yml` — do not invoke `gh release create` or MCP equivalent by hand unless patching broken release.

## Cross-References

- **Documentation rules**: See `.claude/rules/docs-style.md` for website page writing guidelines (narrative structure, headers, real-world examples, link syntax)
- **CI workflow patterns**: See `.claude/rules/subsystem-ci.md` for design principles, cost factors, authoring workflow, review checklist
- **CLI API patterns**: See `.claude/rules/subsystem-cli-api.md` for new report types
- **CLI commands reference**: See `.claude/rules/subsystem-cli-commands.md` — update when adding new behavior
- **Environment variables**: The generated `website/src/content/docs/reference/environment.md` is the canonical env var page; edit the `ocx_env` doc blocks and run `task website:reference:generate`

## Workflow: Release Ceremony

Release ceremony = human-driven process with tooling support, in four steps. No step tags or pushes for you; the release is a workflow dispatch:

```bash
# 1. Prepare. Never tags or pushes.
task release:prepare BUMP=auto   # or BUMP=patch|minor|major, or VERSION=x.y.z
# Human reviews the changes
# 2. Commit, land the release commit on main, dispatch readiness on that exact commit.
git add -A && git commit -m "release: vX.Y.Z"
gh workflow run release-readiness.yml --ref <ref holding the release commit>
# 3. Print the dispatch command, then run it yourself.
task release:tag
# gh workflow run release.yml -f mode=release -f version=X.Y.Z
# 4. release.yml takes over: readiness gate, build, scan, sign, publish, tag, GitHub Release, announce.
```

1. **`task release:prepare`** — the conventions template's task; gate, bump, changelog, validate, verify (below). The bump commit does not exist while it runs, so it cannot ask for readiness on it.
2. **Commit, land, dispatch.** `release-readiness.yml` runs the full gate on the bytes `release.yml` would publish: the stamped Bazel cross builds of all eight targets (`cross.yml`), `scan-binaries.yml`, then `acceptance.yml` on six platforms (linux and Windows x64/arm64, macOS x64/arm64) with `OCX_TEST_RELEASE_BINARY=1` against `test/limits/<os>-release`, plus `shell-activation-deep.yml`. It publishes nothing. The run-name is `readiness <sha>`. A dispatch with `bypass=true` and a `reason` skips every test job, goes green, and renames the run `readiness <sha> BYPASS: <reason>`; use it for an outage of the gate itself, never for a red result.
3. **`task release:tag`** (`taskfiles/release.taskfile.yml`) — reads the version from `Cargo.toml` and prints `gh workflow run release.yml -f mode=release -f version=<version>`. It never runs it, tags nothing and pushes nothing. `release.yml` refuses to run off `refs/heads/main`, outside the ocx-sh/ocx repository, or when tag `v<version>` already exists. Use `mode=candidate -f version=X.Y.Z-rcN` for a release candidate and `mode=dev` for a dev build (`ghcr.io/ocx-sh/dev/cli` only).
4. **`readiness-gate.yml`** — the `readiness` job of `release.yml` (release and candidate modes) calls it on `github.sha`; `publish` needs it and reads its result, so a red gate publishes nothing. `gh workflow run readiness-gate.yml --ref <ref> -f sha=<sha>` shows the gate red or green for any commit without cutting a release.

`release:prepare` is the conventions template's task (`taskfiles/baseline.taskfile.yml`, template-owned, flattened into the root taskfile). It never tags or pushes, and it calls five optional hooks that `taskfiles/release.taskfile.yml` defines, in this order:

| Step | Owner | What it does |
|---|---|---|
| `release:gate` | ocx | Refuses without a green Release Readiness run for HEAD (`scripts/release_readiness_gate.py --sha HEAD`), then runs `provenance:proof`: the release provenance scan on real bytes, green on a `__testing`-free build and red on one carrying the placeholder markers (`adr_test_speed_tiers.md` C-PROV). Defining it replaces the template's own check of the latest verify-deep run. |
| `release:bump-files` | ocx | `cargo set-version` on the workspace manifests, then `scripts/bazel_pin_check.py --set-module-version` on `MODULE.bazel`'s `module(version=)` (`bazel:pin:check` fails when the two differ), from the version the template computed with `git-cliff --bumped-version`. |
| git-cliff | template | Regenerates `CHANGELOG.md` from the commit subjects. |
| `release:validate` | ocx | `changelog:validate`: no raw HTML tag and no website doc path in the rendered changelog. |
| `release:verify` | ocx | An echo and nothing else: the template falls back to a full `task verify` when the hook is undefined, and readiness on the release commit (`release:gate`) is the test gate. |

The gate runs before any file is touched, so a refused release leaves the tree as it was. `.claude/tests/test_hooks.py::TestReleasePrepareGuard` holds the order and each hook's body against the rendered `task --dry release:prepare`; a renamed hook, a verify hook that runs the suite and a gate without its readiness or provenance step each turn it red.

After the dispatch, CI takes over: readiness gate → stamped build → provenance scan → sign and publish to the registry → tag and GitHub Release → announce → docker and sdkgen publish.

### Recovery: publish green, a later job red

Once `publish` is green, the floating tags, signatures and attestations exist. What can still be missing: the git tag, the GitHub release, the index row.

- **Rerun, do not redispatch.** Use "Re-run failed jobs" on the same run. `plan` outputs and the build tag are reused, so no new build tag, signature or attestation referrer appears.
- **Artifacts expire.** `release-dist` and `published-tags` are kept 7 days; later, `github-release` and `announce` cannot fetch their input.
- **`github-release`** creates the tag at the run's SHA, and `gh release create` fails when the release exists. Run `gh release view vX.Y.Z` before rerunning.
- **`announce` and `prune`** wait for the `ocx.sh` environment reviewer again. `announce` refuses an empty `published-tags.txt` or a missing token. `prune` runs in release mode only.
- **`post-docker` and `post-sdkgen`** start only after `github-release` is green; rerunning it unblocks them.
- **Fresh dispatch is the last resort** (artifacts expired, or the run cannot be rerun). `plan` refuses a non-main ref, the wrong repository, a malformed version and an existing `refs/tags/vX.Y.Z`; it does not refuse a published-but-untagged version. A fresh dispatch builds a new `X.Y.Z_<timestamp>` build tag, adds one more signature and attestation referrer per platform, and re-cascades the floating tags.

Afterwards check: git tag, GitHub release, index row, and both post jobs.

### Version Between Releases

Between releases, `Cargo.toml` retains version from last `release:prepare` run. The task computes the correct next version from commit history via `git-cliff --bumped-version` at release time (or takes `VERSION=`). No automated post-release version bump — `release:prepare` = single versioning mechanism.