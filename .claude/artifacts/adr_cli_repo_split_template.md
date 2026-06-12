# ADR: CLI Repo-Split Template (ocx-mirror pattern for satellite CLIs)

## Metadata

**Status:** Accepted
**Date:** 2026-06-12
**Deciders:** Michael Herwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** infrastructure | devops
**Supersedes:** N/A (refines the distribution leg of `adr_cli_plugin_pattern.md`)
**Superseded By:** N/A

## Context

`crates/ocx_mirror` lived in the ocx mono-repo but is an independent product:
own release cadence, own docs audience (mirror-repo operators), no reverse
dependency from ocx. More satellite CLIs are planned (`ocx-mcp`, `ocx-dist`).
The mono-repo coupled their release trains to ocx's, bloated ocx CI, and mixed
their design records into ocx's artifact store.

This ADR records the repeatable contract used to split `ocx-mirror` into
`ocx-sh/ocx-mirror`, to be reused for every future satellite CLI.

## Decision Drivers

- Independent versioning + release cadence per tool
- Mirror development against unreleased `ocx_lib` without publishing crates
- Keep ocx CI lean; satellite breakage must not gate ocx merges
- Repeatability for `ocx-mcp` / `ocx-dist` (template value)
- `ocx_lib` is not on crates.io and should not need to be

## Considered Options

### Option 1: Satellite vendors ocx as submodule, `ocx_lib` path dep (chosen)

**Description:** Satellite repo carries `external/ocx` git submodule;
`ocx_lib = { path = "external/ocx/crates/ocx_lib" }`. Same shape ocx itself
uses for its vendored forks.

| Pros | Cons |
|------|------|
| No published crate needed; bump = submodule pointer | Recursive clone required |
| Standalone repo builds offline-deterministic via Cargo.lock | Toolchain/feature lists must track ocx manually |
| ocx repo completely clean — zero satellite coupling | ocx_lib API breaks surface only at submodule bump |
| Feature-branch co-dev: pin submodule to ocx branch | |

### Option 2: ocx workspace member from satellite submodule (`external/<name>` in ocx)

**Description:** ocx mounts the satellite as workspace member + patches its
git dep to the local path.

| Pros | Cons |
|------|------|
| ocx CI co-verifies satellite against current lib | Cross-repo dance on every breaking lib change |
| | Satellite needs git dep + replicated patch (silent-unpatch footgun) |
| | Per-invocation cargo patch warnings in ocx |

### Option 3: Publish `ocx_lib` to crates.io

**Description:** Satellite depends on a released crate.

| Pros | Cons |
|------|------|
| Standard Cargo flow | Pre-1.0 lib API churn = constant release ceremony |
| | Publishes an API surface ocx does not want to stabilize yet |

## Decision Outcome

**Chosen Option:** Option 1 — "easy-to-maintain dependency rather than a
published crate" (fork-style vendoring, reversed: satellite vendors ocx).

### The repeatable contract (`ocx-<name>`)

1. **Crate shape** — package manifest at repo root (binary `ocx-<name>`),
   explicit `[workspace]` table with `exclude = ["external/ocx"]` (without the
   exclude, the satellite workspace claims the vendored tree and ocx's
   workspace-inheritance fails to resolve). No workspace inheritance; dep
   feature lists copied EXACTLY from ocx `[workspace.dependencies]`.
2. **Dependency wiring** —
   `ocx_lib = { path = "external/ocx/crates/ocx_lib" }` plus re-declared
   `[patch.crates-io]` pointing into the **nested** fork submodules
   (`external/ocx/external/rust-oci-client`, `.../docker_credential`).
   Patch tables do not travel with path deps; dropping them does NOT error —
   cargo silently resolves unpatched crates.io releases (#1 footgun). CI
   asserts the fork source via `cargo tree -i oci-client`. Clones and CI
   checkouts are always `--recurse-submodules`.
3. **Bump procedure** — advance `external/ocx`, re-init nested submodules,
   `cargo update -p ocx_lib` if version changed, sync `rust-toolchain.toml`
   channel + dep feature lists, `task verify`. Renovate `git-submodules`
   manager opens bump PRs.
4. **CI shape** — five workflows: `verify` (PR/main + `workflow_call`;
   fmt/clippy/build/nextest + acceptance vs `registry:2` service; fork-bind
   assert), `build-matrix` (reusable 6-target: zigbuild musl, native darwin,
   cargo-xwin windows — shared by dev + release, zero drift), `deploy-dev`
   (manual; `dev.ocx.sh/ocx/<name>:<ver>-dev_<TS>` + cascade), `release`
   (tag-driven; `ocx.sh/ocx/<name>:<ver>_<TS>` + cascade + GitHub Release via
   `gh` CLI — lean, no cargo-dist: the ocx registry IS the channel), `docs`
   (mkdocs-material → `actions/deploy-pages`). Publishing `ocx` CLI comes from
   `ocx-sh/setup-ocx`, never from same-run artifacts. Repo needs committed
   `ocx.lock` (setup-ocx auto-pulls the project toolchain from `ocx.toml`).
5. **Registry layout** — first-party namespace `ocx/<name>`; rolling dev tags
   on dev.ocx.sh, plain `X.Y.Z_<TS>` + cascade (X.Y.Z → X.Y → X → latest) on
   ocx.sh. Binary named `ocx-<name>` so `ocx <name>` plugin dispatch
   (`adr_cli_plugin_pattern.md`) works.
6. **Versioning** — independent semver continuing above any tags already
   published from the mono-repo era (cascade tags must never move backwards);
   git-cliff `initial_tag`, conventional commits, `task release:prepare`
   ceremony (human tags + pushes).
7. **Repo scaffold** — copies of rustfmt/deny/.licenserc/cliff/taskfile-verify
   contract; `ocx.toml` + `.envrc` direnv toolchain; `.claude/` subset
   (quality rules copied; subsystem rule moves with the code, paths rescoped
   to `src/**`); mirror-specific design records filter-repo'd with history.
8. **History** — `git filter-repo` from a fresh clone with `--path` set +
   `--path-rename crates/ocx_<name>/:` hoist; design records and tests move
   in the same pass.

### Consequences

**Positive:**
- ocx workspace, CI, docs, AI config all shrink; satellite owns its destiny
- Satellite can pin any ocx commit, including feature branches
- Pattern documented + proven once (ocx-mirror) — `ocx-mcp`/`ocx-dist` copy it

**Negative:**
- ocx_lib API breaks discovered at submodule bump, not in ocx CI (accepted:
  satellites no longer gate ocx)
- Manual sync duties: toolchain channel, dep feature lists, setup-ocx version

**Risks:**
- Silent unpatched `oci-client` if patch table dropped → CI `cargo tree`
  assert + `--locked` everywhere
- Non-recursive clone → loud build failure (patch paths empty) → README note

## Implementation Plan

1. [x] Split executed for ocx-mirror (`ocx-sh/ocx-mirror`, PR #1)
2. [x] ocx-side removal (workspace, CI, tests, website, AI config)
3. [ ] Re-evaluate at n=2 (`ocx-mcp`): if diff vs ocx-mirror is pure name
       substitution, extract a GitHub template repo or `cargo generate`
       template; consider `ocx-sh/.github` shared workflows at n=3
4. [ ] Grimoire-distributed shared AI rules (replaces copied quality rules)

## Links

- [adr_cli_plugin_pattern.md](./adr_cli_plugin_pattern.md) — plugin dispatch;
  this ADR closes its "no install path" gap for satellites
- [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror) — first instance

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-12 | Michael Herwig / Claude | Initial draft from executed split |
