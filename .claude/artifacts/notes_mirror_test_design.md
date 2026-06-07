# Sparring Notes — Pre-Publish Per-OS Testing for ocx-mirror

**Status:** WIP — user sparring with assistant, headed to `/architect` once converged.

Sibling artifact: `research_mirror_per_os_smoke.md` (worker-researcher output).

---

## Corrections from user (record what I got wrong)

1. **Flow diagram wrong.** My earlier sketch
   `bundle → ocx package test <bundle> --platform <X> → push to ocx.sh + cascade`
   was wrong. User has not yet stated the correct flow; needs clarification before locking in. Likely correction direction (to verify): testing target is the **package** (installed/materialized form), not the **bundle tar**. Existing `ocx package test -i ID LAYERS... -- CMD` (per explorer #2) already operates on layers + materializes locally without registry. That subcommand may already be the seam, not a new one. Confirm with user.

2. **Sparring partner role, not driver.** User wants me to delegate research/code-reads to Sonnet/Haiku subagents; my role is taking notes + asking sharp questions, not reasoning everything myself.

---

## Pipeline ground truth (from worker-explorer #1)

- Phase 1 prepare: download → verify → bundle → output at `{work_dir}/{version}/{platform_slug}/bundle.tar.xz`
- Phase 2 push: per-platform, sequential by version
- **Seam exists** between prepare and push in `orchestrator.rs:189-197`
- Bundle persists on disk after prepare; failed push leaves bundle for resume
- Cascade is **per-platform inside push.rs:24-75**, not waiting on all platforms — relevant for failure-mode design

## CLI ground truth (from worker-explorer #2)

- `ocx package test` **already exists.** Inputs: `-i <ID>`, `LAYERS...`, `-p <PLATFORM>`, `-m <metadata>`, trailing `-- CMD`. Materializes package locally (no registry), composes env, execs child command. Auto-deletes temp unless `--keep` or `--output`.
- `ocx package push -i ID LAYERS...` consumes the same `LayerRef` shape.
- `ocx package create PATH` produces the bundle tar.
- Bundle = tar.xz, package = installed dir under `~/.ocx/packages/{...}`. Bridge via `pull_local` (extracts layers → packages/, hardlinked from layers/).

**Implication:** ocx-mirror does NOT need a new subcommand. It can shell out to `ocx package test` against the layer it just bundled. New work is the **executor abstraction** + **CI rendering** + **mirror-side wiring**, not new CLI surface in `ocx package`.

## External research highlights (from worker-researcher #3)

- **Closest prior art:** conda-forge feedstocks (one repo per package, CI rendered by conda-smithy from a small spec) + Nixpkgs `testers.testVersion` (L1 version-output matching as min bar, also catches dynamic-linker failures).
- **No ecosystem does exactly this**: validating **pre-built upstream binaries** across foreign archs pre-publish. OCX is greenfield here.
- **Wine is a non-starter** for MSVC-linked binaries (cmake, gh, node, bun). Use `windows-latest` instead. Wine only viable for GNU-linked Rust/MinGW.
- **GHA `linux/arm64` runners are GA** (Sept 2024) — native is now cheaper/faster than QEMU for aarch64.
- **macOS Intel runners sunsetting**; `macos-15-intel` is bridge until Aug 2027. Use Rosetta (`arch -x86_64` on arm runner) for darwin/amd64.
- **QEMU gotcha:** raw `qemu-<arch>-static ./binary` fails — needs Docker `--platform` or `run-on-arch-action` for sysroot.
- **Breakage reporting:** no ecosystem surveyed uses Discord for per-package CI failure. All use GitHub PR check annotations + summary dashboard. OCX can still pick Discord for *aggregate* daily/weekly digests, but per-package check should be GH-native.

---

## Decisions converging

| # | Decision | Status |
|---|---|---|
| 1 | Test floor = L1 (version output match), per-tool override | Tentative agree |
| 2 | Test target = local artifact, never push to registry first | Confirmed |
| 3 | Executor abstraction (native / qemu / wine / rosetta) per platform | Confirmed (user explicit) |
| 4 | Repo-per-mirror under `ocx-sh/mirror-<tool>` | Confirmed (user explicit) |
| 5 | Repo carries config that **renders CI workflow + test scripts** (conda-smithy pattern) | Confirmed (user explicit) |
| 6 | Default test config + per-platform override (mirrors `metadata.default`/`overrides` pattern) | Confirmed (user explicit) |
| 7 | New CLI command `ocx package test` for running smoke against local layer | Already exists — reuse, don't add |
| 8 | Self-hosted runners | Out of scope for now |
| 9 | Multi-version pipeline status: green / partial / red / skipped | Tentative |
| 10 | Reporting: Discord aggregate + GH PR checks per-job | Tentative (user open to Discord) |
| 11 | Cost tiering: cheap runners gate before expensive ones | Tentative |
| 12 | Wine: only for GNU-linked Windows binaries; default `windows-latest` | Tentative |
| 13 | linux/arm64: native runner over QEMU | Tentative |
| 14 | darwin/amd64: Rosetta on arm runner | Tentative |
| 15 | `ocx-mirror` binary distribution model | OPEN (user just raised) |

## Open questions for user

1. **What about the flow was wrong?** Best guess: test target should be the materialized package via existing `ocx package test`, not the bundle tar. Confirm/correct.
2. **CI renderer location:** lives in `ocx-mirror` binary (new subcommand `ocx-mirror render`), in `ocx-mirror-ci` separate binary, or as Taskfile task? Lean toward subcommand of `ocx-mirror` so spec + renderer ship together.
3. **Generated files committed to mirror repo, or rendered-on-demand in CI?** Conda-forge commits them (visible drift = visible PR). Alternative: reusable workflow + tiny dispatcher.
4. **Cascade behavior on partial:** Allow partial cascade among passing platforms, or atomic-only (any red = block whole version)?
5. **Smoke test scope:** only for new versions, or also for cascade-only updates? Re-runs of already-mirrored versions?
6. **Discord webhook:** aggregate digest only, or per-mirror failure too? Latter risks noise.
7. **Repo namespace:** `ocx-sh/mirror-<tool>` confirmed but: GH org creation/cost? Repo permissions model? Bot identity for cross-repo updates of generated CI?
8. **`ocx-mirror` distribution model** (next section).

---

## ocx-mirror Binary Distribution — Open Reasoning

User point: `ocx-mirror` is currently unreleased. Per-mirror CI must install it. Still pre-release, changing aggressively. Heavy release cycle = overhead.

### Constraints
- Aggressive change pace — tolerate breakage, don't pretend stable
- Mirror byproduct, not flagship — overhead must stay low
- Per-mirror CI needs **a way to fetch known-working version**
- Should not block changes to `ocx-mirror` on per-mirror compat

### Candidate models

| Model | Pro | Con |
|---|---|---|
| **A. Ship inside ocx tarball** | One release artifact, single curl install, distributed everywhere `ocx` is | Couples ocx-mirror cadence to ocx releases. Bloats end-user download for tool 99% of users don't need. Confuses target audience (ocx = end-user, ocx-mirror = publisher). |
| **B. Separate cargo-dist target in ocx workspace, same release** | Single git tag, separate binary download URL, end-user `ocx` stays lean | Still couples versioning. Mirror CI pins `ocx-mirror@vX.Y.Z` from same release. |
| **C. Built from source in each CI run** (`cargo install --git ...`) | Always head, no release needed | Slow (~2-3 min cold cargo build per mirror per run × hundreds of runs). Not reproducible across runs. |
| **D. Pre-built binary published to GHCR as OCI artifact, fetched via `ocx install`** | Dogfood OCX. Single install command. Versioned + cached. | Chicken-and-egg: need ocx + ocx-mirror manifest published before first use. Manageable. |
| **E. Pre-built nightly artifacts in `ocx-sh/ocx-mirror-bin` repo, fetched via curl** | Cheap, fast, no infra | Yet another release pipeline. |
| **F. Per-mirror repo pins commit SHA of ocx-mirror; CI builds binary on first run, caches it** | Reproducible, lazy build | Caching across mirror repos = nontrivial (GHA cache is per-repo). |

### My take (subject to user push-back)

**Phase 1 (now, aggressive iteration):** Model C with twist — `cargo install --git https://github.com/ocx-sh/ocx --rev <SHA> ocx-mirror` cached via `Swatinem/rust-cache` per mirror repo. SHA pinned in `mirror.yml` (`ocx_mirror_rev: abc123`). Renderer (which is itself ocx-mirror) renders that SHA into the generated workflow. Bumping the SHA is a normal PR to the mirror repo. Cargo cache keeps cold builds rare.

**Phase 2 (when surface stabilizes):** Switch to Model D — publish `ocx-mirror` as an OCX package at `ocx.sh/ocx-mirror` (dogfood). Per-mirror CI runs `ocx install ocx-mirror:X.Y.Z` instead of cargo build. Versioned, fast, native to OCX worldview.

**Phase 3 (mature):** Continue Model D; consider GH Action `ocx-sh/setup-ocx-mirror@vX` as ergonomic wrapper.

**Skip A** — end-user `ocx` bloat for a publisher tool wrong audience match.
**Skip B** — same coupling pain, no real gain over D.
**Skip E, F** — extra infra without payoff.

### Bumping the renderer

If ocx-mirror is the renderer, bumping it across N mirror repos = N PRs. Options:
- **Bot** (`ocx-mirror-update-bot`): like dependabot/renovate, opens PRs per mirror repo bumping `ocx_mirror_rev`. Cheap GHA cron in mono repo.
- **Manual fan-out script** (`task mirror:bump-all`): runs locally, opens PRs via `gh`.
- **Reusable workflow** in `ocx-sh/.github` (org-default workflow). Each mirror repo's generated workflow `uses:` it. Bumping the reusable workflow = single change, all mirrors pick up. **Downside:** loses per-mirror pin; everything moves at once.

Hybrid: per-mirror pin for reproducibility + a bot that bumps + a kill-switch reusable workflow for emergency floor.

---

## User-confirmed decisions (round 3 — corrections)

- **Static matrix from mirror.yml, dynamic versions inside each leg.** Matrix shape = platforms (known at render time). Versions = list output from `discover` job. Each leg loops over versions internally. No dynamic-matrix gymnastics, stable PR check names.
- **Drop state.json.** `has_new` gate already self-limits notify. Stateless reporting.
- **Smoke spec shape** in mirror.yml: `command` (shell string) XOR `script` (path in mirror repo) + optional `working_dir`. Tokens `${install_dir}`, `${version}` exposed at runtime via env vars.
- **`ocx package test` stays executor-free.** Executor logic lives in the test command (qemu/wine/rosetta as wrapper around `ocx package test -p <P> ... -- <wrapped-test-cmd>`). Renderer emits per-platform wrapper scripts (`scripts/_smoke-<platform>.sh`) that call user smoke script/command.
- **Executor abstraction location:** in renderer + default table baked into ocx-mirror binary. Spec overrides per platform via `test.overrides.<platform>.executor: native|qemu|wine|rosetta` + `runner: <gha-label>` + optional `qemu_arch:` etc.
- **Mirror repo layout:**
  - `mirror.yml` — hand-edited spec
  - `scripts/smoke.sh` — user-authored test script (optional)
  - `scripts/_smoke-<platform>.sh` — generated wrappers (prefix `_`)
  - `.github/workflows/mirror.yml` — generated
  - `.github/workflows/release.yml` — generated (if applicable)

## Round 5 corrections (re-scope)

- **macOS + Windows back in scope as native runners.** Phase 1 = linux (docker images) + macos-latest (native, rosetta prefix for amd64) + windows-latest (native pwsh). No wine, no qemu yet.
- **Two execution styles in one config:** `containers:` array on platform → docker mode (one matrix entry per image). Absent → native mode (single entry).
- **Mac/Windows native:** runner runs ocx + test directly. Optional `prefix:` for rosetta-style command wrapping.
- **(V, P) tile green = AND across all containers/runners for that platform.** Per-container result kept for diagnostics.

## Round 6 correction — execution model

Round 5 said "Pattern 1: ocx materializes on host, container mounts install dir." **Wrong.** Correct:

- **Phase 1 linux:** docker image IS the execution host. ocx runs inside the container. Host runner is just a launcher.
  - Native arch alignment: linux/amd64 GHA runner → amd64 image. linux/arm64 GHA runner (`ubuntu-24.04-arm`) → arm64 image. No qemu in phase 1.
  - Inside container: install ocx → `ocx package test --platform <P> bundle.tar.xz -- <test-cmd>`
- **Phase 1 mac/windows:** native runner, ocx installed natively, test runs natively. Same shape as before.
- **Phase 2 (future):** linux runner → qemu → docker (foreign arch) → install ocx → `ocx package test`. Same pattern as phase 1, qemu layer added.

Consistent ocx-in-container pattern phase-1 linux → phase-2 foreign-arch. Mac/Windows stays native both phases.

### How ocx enters the container (phase 1 linux only — phase 2 forces (b))

| Option | Mechanism | Tradeoff |
|---|---|---|
| (a) Mount host's ocx binary | `-v $(which ocx):/usr/local/bin/ocx:ro` | Fast, offline. Requires musl-static ocx — glibc-linked breaks across alpine ↔ ubuntu. |
| (b) Curl install in container | `curl -fsSL ocx.sh/install.sh \| sh` first step | Works any libc. Adds 5–30s per leg. Needs internet. |
| (c) Pre-baked test images | `ocx-sh/test-runner-<image>:<ocx-ver>` | Fast, no per-run install. Maintenance burden. |

Architect call (A11 added below).

## Round 4 corrections

- **Scope cut:** linux-only for phase 1. Docker as execution primitive. No wine/rosetta/mac/windows yet. Future: docker+qemu for foreign arch with ocx-in-container.
- **Multi-runner per platform:** mirror.yml `platforms.<P>.runners[]` is array of `{image, shell, gha_runner}`. Each entry = matrix row. (V, P) tile green only if all its runners pass. Catches glibc vs musl etc.
- **No generated wrapper scripts.** Docker invocation goes inline into rendered workflow step. Mirror.yml describes test; renderer bakes docker call into step.
- **mirror.yml `test:` shape:**
  - Single `test: <value>` field with auto-detect (filepath → script, otherwise command)
  - OR explicit `script:` / `command:` forms
  - Optional `working_dir`
  - Per-platform override via `platforms.<P>.test`
- **Env exposed to test:** `OCX_INSTALL_DIR`, `OCX_VERSION`, `OCX_PLATFORM`, `OCX_IMAGE`
- **`tests/` directory convention** for user test scripts in mirror repo

## Revised canonical pipeline (Shape 2 corrected)

```
job: discover (ubuntu)
  ocx-mirror plan
  outputs:
    has_new: bool
    versions: ["3.29.0", "3.29.1"]   # JSON array

job: prepare (matrix: per version, if: has_new == true)
  ocx-mirror prepare --version <V>
  upload bundle-{V}-{P}.tar.xz per platform

job: smoke-and-push (matrix: per platform STATIC from mirror.yml, runs-on: matrix.runner, if: has_new)
  for V in needs.discover.outputs.versions:
    download bundle-{V}-<this-platform>
    bash scripts/_smoke-<this-platform>.sh   # rendered wrapper invokes user smoke script
      under the hood: ocx package test --platform <P> bundle -- <executor-wrapped command>
    if green: ocx package push --platform <this-platform>
    record result-{V}-{P}.json

job: cascade (ubuntu, if: has_new)
  aggregate results
  per-version status: published | partial | failed
  cascade tags per pushed-platforms-set
  promote latest only on fully-green newest
  emit run-summary.json

job: notify (if: has_new AND (any-new-green OR any-red))
  read run-summary
  post Discord webhook
```

## Architect handoff brief (draft — assemble after open questions resolved)

- **Renderer:** `ocx-mirror generate ci` (subcommand of ocx-mirror binary). Reads `mirror.yml`, emits `.github/workflows/*` + scripts. `--check` mode for drift detection. Reuse existing `ocx package test` as smoke executor; no new `ocx package` subcommand.
- **Distribution:** `cargo install --git ocx-sh/ocx --rev <SHA> ocx_mirror` pinned per mirror repo via `mirror.yml: ocx_mirror.rev: <SHA>`. Env override `OCX_MIRROR_SOURCE=path:<dir>|git+<url>?rev=<SHA>|git+<url>?branch=<name>` for hacking. No submodule.
- **Pipeline shape:** dynamic matrix (Shape 2) — plan → prepare-per-version → smoke+push per (V, P) → cascade → notify. Cascade-job aggregates tile results; partial versions cascade among present platforms but never get `latest`.
- **Single-job-iteration variant** documented as alternative for low-platform mirrors; architect to call.
- **Reporting:** per-repo Discord webhook, silent default. Notify only on (a) new green versions, (b) new breakage. State file `.ocx-mirror/state.json` committed on default branch holds last-run (V, P, status) for renotify dedupe. No aggregator-in-central-repo yet (deferred follow-up).
- **Notify message taxonomy:**
  - all-new-green → `📦 <tool>: published <V> (<platforms>)`
  - partial → `⚠️ <tool> <V> partial — failed: <P> (<reason>). Published: <good-Ps>. Run: <URL>`
  - all-failed → `❌ <tool> <V> failed all platforms — see <URL>`
  - unchanged-breakage → silent

## Pipeline graph (canonical — Shape 2)

```
plan (ubuntu)
  ocx-mirror plan --output matrix.json
  outputs.matrix = fromJSON
↓
prepare (matrix: per version, ubuntu-latest)
  ocx-mirror prepare --version <V>
    → download upstream + bundle all platforms
  upload artifacts: bundle-{V}-{P}.tar.xz
↓
smoke-and-push (matrix: per (V, P), runs-on: matrix.runner)
  download bundle-{V}-{P} artifact
  executor-wrap: ocx package test -i <id> <layer> -p <P> -- <smoke_cmd>
  if green: ocx package push --platform <P>
  upload result-{V}-{P}.json
↓
cascade (ubuntu)
  download all result-{V}-{P}.json
  compute per-version status (published | partial | failed)
  cascade tags per pushed-platforms-set
  promote latest only on fully-green newest
  emit run-summary.json
  read+write .ocx-mirror/state.json (diff for notify)
↓
notify (if: needs.cascade outputs `changed` truthy)
  read run-summary + state diff
  post Discord webhook
```

Open architect calls:
- Matrix entry naming convention (PR check names are dynamic — `smoke (3.29.0, linux/amd64)` vs `smoke-3.29.0-linux-amd64`)
- State file location: default-branch commit vs artifact-with-90-day retention. Committing more durable, adds noise to mirror repo history.
- Executor wrapper mechanics: shell wrapper script per executor (qemu wraps in docker --platform, rosetta wraps in `arch -x86_64`, wine wraps in xvfb-run + wine), or build into ocx-mirror as a `--executor` flag on `ocx package test` invocation?

## Architect handoff brief (draft — assemble after open questions resolved)

**Problem:** OCX needs pre-publish multi-OS smoke testing for mirrored packages. Constraints: GHA-hosted runners only, no staging registry, ocx-mirror still pre-release.

**Scope:**
1. Executor abstraction (native / qemu / wine / rosetta) — trait + default table per platform
2. Repo-per-mirror topology under `ocx-sh/mirror-<tool>` — repo template + bot for fan-out updates
3. CI renderer (subcommand of `ocx-mirror`): consumes mirror spec, emits workflow + test scripts
4. Test config schema: `smoke:` section with default + per-platform overrides; integrates with existing `ocx package test`
5. Pipeline status model: green / partial / red / skipped; interaction with cascade (latest gate)
6. `ocx-mirror` distribution: phase 1 cargo-install pinned SHA, phase 2 self-hosted OCX package
7. Reporting: GH PR checks per-job (default), optional Discord aggregate digest, auto-issue on persistent failure

**Out of scope:** self-hosted runners, functional smoke beyond L1, cross-registry promotion, new `ocx package` subcommands (reuse existing `test`).
