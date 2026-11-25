# Research: rules_ocx as a repository — its ocx binary, tests, CI, release and rules

## Metadata

**Date:** 2026-11-25
**Lane:** codebase read of the sibling checkout, plus host checks (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ by a subagent from the rules_ocx checkout at `9648f93` (version 0.5.1), with `path:line` relative to that repository. RUN by the orchestrator on this host: docker, the `gh` token's scopes and push rights, the open issues and pull requests, and the state of the checkout. READ by the orchestrator on ocx main at `f41c6684`: `.claude/rules/workflow-release.md`. Nothing was built.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Not read: `.bcr/metadata.template.json`, the runner operating system of five workflows (`update-dist.yml`, `deploy-site.yml`, `pages.yml`, `issues.yml`, `claude.yml`), and the per-test details of `examples/*` and `ocx/tests/*_test.bzl`. Whether `rctx.download` in `ocx/private/download.bzl` accepts a `file://` source was not checked.

## 1. Where rules_ocx gets its `ocx`

- `ocx_download`, a repository rule (`ocx/private/download.bzl:61`). `ocx/extensions.bzl:249-254` always creates `@ocx_tool` from it.
- Pins: `DEFAULT_OCX_VERSION = "0.6.6"` (`ocx/private/versions.bzl:15`) and `MIN_OCX_VERSION = "0.6.1"` (`:18`).
- The release manifest is a vendored `dist/dist.json`, a snapshot of setup.ocx.sh/dist.json with sha256 values that are enforced (`download.bzl:6-9`, attribute default `//dist:dist.json` at `:83`). The 0.6.6 rows cover Linux gnu and musl, macOS and Windows msvc, each for aarch64 and x86_64 (`dist/dist.json:6-13`).
- Host mapping: Linux to musl, macOS to apple-darwin, Windows to gnullvm and then msvc (`ocx/private/platforms.bzl:73-116`).
- What can be overridden is the source of the download only: the tag `ocx.download(version, dist_manifest, triple)`, which only the root module may use and at most once (`extensions.bzl:19-36`, `:237-247`), and the variables `OCX_INSTALL_DIST_URL` and `OCX_INSTALL_MIRROR_URL` (`download.bzl:36,44`).
- A way to name a local binary: not found. `OCX_BIN` is a test variable only (`ocx/tests/BUILD.bazel:28`).
- Rules for the pin: bump `DEFAULT_OCX_VERSION` and `dist/dist.json` together (`AGENTS.md:68`); never edit a `dist.json` row by hand (`.claude/rules/dist-snapshot.md:83`). `scripts/bump_ocx.py` and `.github/workflows/update-dist.yml` (weekly, by dispatch, and by `repository_dispatch`) do the refresh.

## 2. Tests

- `task test` is `bazel test //...` (`taskfile.yml:251-254`): unit suites for platforms, manifest, launcher, package and policy (`ocx/tests/BUILD.bazel:11-19`), two smoke tests, and the docs freshness test.
- `ci:test` is `bazel test --config=basic` (`taskfile.yml:45-49`); the basic configuration leaves out the `deep` tag (`bazel/baseline.bazelrc:13`).
- `ci:deep` is the nightly tier (`taskfile.yml:63-75`): Bazel 8.7.0, 9.x and rolling (`:15`); the three example workspaces against the live public registry (`:88-106`); the BCR workspace `e2e/bzlmod` per Bazel version (`:110-127`); `pins:check` (`:144`); `deep:offline` on Linux (`:164-185`).
- Every example is a module of its own with `local_path_override` and pulls from the live registry (`AGENTS.md:277-278`).

## 3. CI

- `verify-basic.yml`: on pull requests, pushes to main, merge groups and by dispatch; Linux only. Its `basic-gate` job is the one required check (`:109`).
- `verify-deep.yml`: nightly, by dispatch, by `workflow_call`, and on a pull request labelled `run-deep` (`:4-24`). It runs `task ci:deep` on Linux, macOS and Windows (`:32`).
- `release.yml` on a pushed tag `v*`; `publish.yaml` by call or dispatch; `update-dist.yml` as above.
- By hand: verify-basic, verify-deep, publish, update-dist, deploy-site, pages and issues can be dispatched.

## 4. Release

- The flow is in `.claude/rules/release.md:1-13`: `git-cliff --bump` gives the version, `MODULE.bazel` and the README change, a commit `chore(release): vX.Y.Z`, a tag, a push of the tag.
- `release.yml:25-53` refuses a tag that differs from the `MODULE.bazel` version, builds the source archive and creates the GitHub release. Its `publish-to-bcr` job then calls `publish.yaml`, and skips a tag that holds `-` (`:55-64`).
- `.bcr/presubmit.yml:1-16`: module path `e2e/bzlmod`; debian11, ubuntu2204, macOS and Windows; Bazel `8.x` and `9.x`.
- `MODULE.bazel:8-18`: `rules_ocx` 0.5.1, `bazel_compatibility >= 8.0.0`, dependencies `bazel_skylib` 1.9.0 and `platforms` 1.0.0.
- `release.md:11` says the BCR submission is manual; `release.yml:55-64` automates it.

## 5. Its own rules for contributors and agents

- Conventional Commits, and the changelog from git-cliff (`CONTRIBUTING.md:7-8`, `AGENTS.md:287`).
- Never commit to `main` and never push to the remote (`AGENTS.md:287-288`); `task verify` before review (`CONTRIBUTING.md:10`).
- Never re-implement OCX internals in Starlark (`AGENTS.md:63`).
- An extension implementation may not use `module_ctx.os` or read the environment (`AGENTS.md:70`).
- `.agents/memory/hex.md` exists and sets a model policy. It records that stardoc cannot build in a fresh agent worktree (`:45-49`).
- No `hex-*` skill directory exists there.

## 6. Containers and remote execution

- No Dockerfile and no compose file.
- A remote cache, with upload off for local runs (`bazel/baseline.bazelrc:3-6`): `https://bazel-cache.ocx.sh/v1`.
- No remote-execution configuration. `docs/concepts/how-it-works.md:26`: "Remote execution is a non-goal at 0.5.0".
- `docs/guides/remote-cache.md:8-54` describes the lazy launchers for builds with a remote cache.

## 7. Public surface

- Tag classes (`ocx/extensions.bzl`): `download` (`:19`), `project` (`:38`), `policy` (`:112`), `package` (`:122`).
- Exports (`ocx/defs.bzl:17-23`): `ocx_download`, `ocx_project_repo`, `ocx_package_repo`, `ocx_package_hub`, `ocx_platform_constraints`.
- Pages: `docs/defs.md` and `docs/extensions.md` are stardoc output; `docs/index.md`, `docs/tutorial.md`, `docs/concepts/`, `docs/guides/`, `docs/reference/environment.md`.

## 8. Gates

- `task lint` (`taskfile.yml:218-231`): buildifier, licence headers, a link check, `dist:check`.
- `//docs` has a `diff_test` that fails on stale stardoc output (`docs/BUILD.bazel:38-47`).
- CI adds `lock:check`, `baseline:check`, a secret scan over the full history, and workflow lint with actionlint and zizmor.

## 9. Its tooling

- A developer needs `ocx` on `PATH`; the rest comes from `ocx.toml` (`CONTRIBUTING.md:26-29`). Tasks call `ocx exec -g <group> -- bazelisk` (`taskfile.yml:11-12`).
- `.bazelversion` is `9.2.0`; each example and `e2e/bzlmod` has its own.

## 10. ocx builds that are not releases

READ on ocx main, `.claude/rules/workflow-release.md:18-27`. `release.yml` runs by dispatch only and has three modes.

- `release`: the package, the tag, the GitHub release.
- `candidate`: `X.Y.Z-rcN`, a GitHub pre-release.
- `dev`: no tag and no GitHub release. From a branch with an open pull request it publishes `X.Y.Z-pr<N>` to `ghcr.io/ocx-sh/dev/cli`, unannounced, and comments the address on the pull request.

rules_ocx reads a dist manifest with download addresses. A `dev` build is an ocx package in a registry and has no such address.

## 11. This host and the access it has

RUN on 2026-11-25.

- docker 29.10.0, compose 5.8.0. Containers of other sessions are running: two Sigstore stacks.
- The `gh` token has the scopes `repo` and `workflow`, and push rights on `ocx-sh/ocx`, `ocx-sh/rules_ocx` and `ocx-sh/ocx-mirror`.
- The rules_ocx checkout is clean, on `main` at `9648f93`, with one worktree.
- Open in rules_ocx: issue 13 (findings of a Bazel audit of 2026-09-06), issue 20 (follow ocx's exit-code change), pull request 7 (`platform_fallbacks` on `ocx.package`).

## What this shows

- rules_ocx can only run an `ocx` that a dist manifest names. A build of an unreleased ocx reaches it through that manifest and the two source variables, or through a new attribute.
- rules_ocx has no remote-execution test, no local registry and no container setup today.
- A change to rules_ocx's pin needs a released ocx, because the pin is a snapshot of the release manifest.
- Its required check runs on Linux; macOS and Windows run in the deep tier, nightly or on a labelled pull request.
- Its release publishes to the Bazel Central Registry by itself once a tag is pushed.

## What this does not show

- Whether a dist manifest may point at a local file.
- How long the deep tier takes, and what it costs to add a compose-based lane to it.
- Whether pull request 7 will land before a rewrite of `ocx.package`.
