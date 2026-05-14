# ADR: ocx-mirror Pre-Publish Multi-Runner Test Pipeline

## Metadata

**Status:** Accepted (all open calls resolved 2026-05-13)
**Date:** 2026-05-13
**Deciders:** mherwig (user), architect (Claude)
**Beads Issue:** N/A
**Related ADRs:** [adr_ocx_mirror.md](./adr_ocx_mirror.md), [adr_cascade_platform_aware_push.md](./adr_cascade_platform_aware_push.md), [adr_index_routing_semantics.md](./adr_index_routing_semantics.md)
**Source artifacts:** [handover_architect_mirror_test_pipeline.md](./handover_architect_mirror_test_pipeline.md), [notes_mirror_test_design.md](./notes_mirror_test_design.md), [research_mirror_per_os_smoke.md](./research_mirror_per_os_smoke.md)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, GitHub Actions, OIDC where applicable)
- [x] No deviation
**Domain Tags:** infrastructure, devops, integration
**Supersedes:** none
**Superseded By:** none

## Context

OCX mirrors upstream binaries into OCI registries via `ocx-mirror`. Today there is no pre-publish validation that a mirrored package actually starts on its declared platforms before its tag becomes visible at `ocx.sh`. We want per-mirror CI in `ocx-sh/mirror-<tool>` repos that smoke-tests each new `(version, platform)` tile before push and only publishes the tiles that pass.

The handover document captured five rounds of user sparring and converged on twelve user-confirmed decisions (D1–D12) and eleven open architect calls (A1–A11). This ADR reaffirms what holds, challenges what does not, resolves the open calls with options/recommendations + explicit `Awaiting user feedback` flags, and documents the risks that survived analysis.

Downstream consumer of this ADR is `/swarm-plan`; stage breakdown is at the bottom and is intentionally testable-boundary shaped.

## Decision Drivers

1. **Reproducibility** — published mirrors must be byte-identical to what passed smoke
2. **Operability** — per-mirror failure should not block other mirrors; failures must surface fast
3. **Cost** — minimize wall-clock × runner-minutes; macOS/Windows are ~10×/1.7× Linux
4. **Cascade correctness** — tag promotion must respect partial vs full platform sets per [adr_cascade_platform_aware_push.md](./adr_cascade_platform_aware_push.md)
5. **Pre-release tolerance** — `ocx-mirror` itself is unreleased and changing aggressively; distribution mechanism must tolerate breakage without flag-day fan-outs

## Industry Context & Research

**Research artifact:** [`research_mirror_per_os_smoke.md`](./research_mirror_per_os_smoke.md)

**Trending approaches:**
- conda-forge's feedstock model (one repo per package, CI rendered by `conda-smithy`) is the closest prior art for repo-per-mirror + CI generation.
- Nixpkgs `testers.testVersion` is the canonical L1 smoke pattern (version-output match + implicit dynamic-linker validation).
- GHA `linux/arm64` native runners are GA (Sept 2024) — QEMU is no longer the cheapest path for aarch64.
- Wine is unreliable for MSVC-linked Windows binaries (cmake, gh, node) — `windows-latest` is the only viable path.

**Key insight:** No surveyed ecosystem validates **pre-built upstream binaries** across multiple base images / foreign archs *before* publish to a public registry. OCX is greenfield here, but the conda-forge + Nixpkgs hybrid pattern (one repo per package, generated CI, version-output smoke as the minimum bar) gives the strongest template.

---

## Reaffirmed Decisions (D1–D12)

The architect reviewed each of the twelve user-confirmed decisions in the handover. The block below states each verbatim and the architect's verdict. Decisions challenged or with non-trivial caveats are called out under "Challenged Decisions" further down.

| # | Decision (summary) | Verdict | Note |
|---|---|---|---|
| D1 | Repo-per-mirror under `ocx-sh/mirror-<tool>` | **Reaffirm** | Matches conda-forge prior art; mature scaling path |
| D2 | Renderer subcommand `ocx-mirror generate ci` with `--check` drift mode | **Reaffirm** | Templates baked in via `include_str!` (see A2) |
| D3 | Reuse existing `ocx package test` for smoke execution | **Reaffirm** | Verified: `ocx package test` already materializes locally, composes env, execs CMD after `--`. No new `ocx package` subcommand. |
| D4 | Static matrix (platforms × containers) + dynamic version list inside leg | **Reaffirm** | Required for stable check names + `required_status_checks` |
| D5 | Pipeline shape: discover → prepare → smoke-and-push → cascade → notify | **Reaffirm with restructure** | Final shape (per user feedback 2026-05-13): `discover → prepare → test → push → notify`. Test job runs per-`(platform, container)` matrix, emits JUNIT XML. Push job is **single serial** ubuntu-latest job that aggregates JUNIT, ANDs container results per `(V, P)`, then for each green tile calls `ocx package push --cascade -p <P> --format json`. No separate cascade job — `ocx package push --cascade` already handles cascade-tag writes with correct universe. Serial across all `(V, P)` eliminates cross-platform image-index race on shared tags (`latest`, `3.29`, etc.). |
| D6 | Test config shape in `mirror.yml` (single-field `test:` with auto-detect, per-platform override) | **Replaced** | Final shape (per user feedback): `tests:` is an **array** of `{name, command}` entries, **command-only** (no script field, no auto-detect). Each test entry produces one JUNIT testcase. Multi-line scripts must be invoked as artifact files via a shell command (`bash ./tests/smoke.sh`). Per-platform override allowed (replaces entire `tests:` list for that platform). |
| D7 | Multi-runner per platform via `containers: []` array | **Reaffirm** | `(V, P)` green ⇔ AND across all containers for that `(V, P)`, where each container produces its own JUNIT report and overall result is AND across all JUNIT testsuites for that `(V, P)` |
| D8 | ocx-in-container execution model for linux | **Reaffirm** | Native-arch alignment (linux/amd64→amd64 image, linux/arm64→arm64 image). Phase-2 qemu underlay preserves shape. |
| D9 | Distribution via pinned-SHA `cargo install --git ...` | **Reaffirm** | Already shipping musl + glibc targets per `dist-workspace.toml`; A11 can short-circuit cargo build with a direct release download |
| D10 | Per-repo Discord webhook, silent default | **Reaffirm** | Webhook URL = secret; renderer references `${{ secrets.DISCORD_WEBHOOK_URL }}`, never bakes URL into mirror.yml |
| D11 | Stateless renotify (`has_new` gate is enough) | **Reaffirm** | Confirmed by re-reading `adr_ocx_mirror.md` — already-mirrored detection uses registry tag-list set-diff, no local state file needed |
| D12 | Pipeline status model: `published` / `partial` / `failed` / `skipped-existing` / `skipped-executor` | **Reaffirm** | Cascade-on-partial = cascade among present platforms, never `latest`. Promote `latest` only if newest version is fully-green. |

---

## Replaced Decisions (post-user-feedback 2026-05-13)

### D5 — Final pipeline shape

User confirmed: keep push and cascade fused via `--cascade`, eliminate cross-platform image-index race via **single serial push job**.

```
discover                       → ubuntu                  (1 job)
prepare    (matrix: V)         → ubuntu-latest           (M = |new versions|)
test       (matrix: P × C)     → matrix.runner           (P × C jobs, inner loop over V × tests)
                                  emits: junit-{V}-{P}-{C}.xml
push       (single, serial)    → ubuntu-latest           (1 job)
                                  reads: all junit-*.xml
                                  for (V, P) where AND(C junit results) == green:
                                    ocx package push --cascade -p <P> -i <target>:<V> bundle.tar.xz --format json
                                  emits: run-summary.json
notify     (conditional)       → ubuntu                  (1 job)
```

Why serial push: cross-platform cascade writes (`latest`, `3.29`, etc.) target the same image-index. Concurrent matrix-leg pushes would race on `GET → modify → PUT` of image-index manifests with last-writer-wins behavior. Serial push (ordered oldest-V-first, then platform-order from spec) lets `ocx package push --cascade`'s internal cascade logic (existing, per `adr_ocx_mirror.md` "Cascade Correctness") see correct universe at each iteration. Push is network-bound (~5–10s per platform) — serializing P platforms ≈ 30–60s wall-clock, negligible vs test job (~minutes).

Result capture: `ocx package push --cascade --format json` emits Printable JSON to stdout (existing CLI subsystem pattern, per `subsystem-cli.md`). Push job collects per-`(V, P)` JSON, accumulates into `run-summary.json`. **No new `ocx-mirror cascade-finalize` subcommand needed.** **No new flag on `ocx package push` needed.**

### D6 — `tests:` array, command-only

User clarified: `ocx package test` semantics around multi-line scalars are not great; treat them as inline scripts instead. Phase-1 schema:

```yaml
tests:
  - name: version
    command: cmake --version
  - name: smoke
    command: bash ./tests/smoke.sh

# Per-platform override (replaces tests list entirely for that platform):
platforms:
  windows/amd64:
    runner: windows-latest
    tests:
      - name: version
        command: cmake.exe --version
      - name: smoke
        command: pwsh -File ./tests/smoke.ps1
```

Rules:

- `tests` is required, must contain ≥1 entry
- Each entry: `name` (unique within mirror.yml, used as JUNIT testcase name), `command` (single-line string)
- Multi-line scripts: author script as artifact file in repo, invoke via command (`bash ./tests/smoke.sh args`)
- Per-platform `tests:` shadows top-level entirely (no partial override)
- Sideloading runtime deps (e.g., publishing `prefix-dev/shell` as ocx package and composing into test env) deferred to phase 2

Each test produces one JUNIT `<testcase>`. Container leg's testsuite has `name="ocx-mirror.<tool>.<platform>.<container_id>"`. Acceptance test action (`EnricoMi/publish-unit-test-result-action`) consumes JUNIT for PR annotations. Discord notify also reads JUNIT summary.

`version_regex` field considered earlier (research §5 for tools with non-standard `--version` output) deferred — users encode regex assertions inside their `command:` via grep/sed if needed.

---

## Open Architect Calls (A1–A11)

Each below presents options, the architect's recommendation, and an explicit `Awaiting user feedback` flag.

### A1: `ocx package test` flag reconciliation

The handover assumed phase-1 linux flow needs *materialize-then-exec-docker* from the host, hence the `--keep + --output` `conflicts_with` is a blocker.

**Architect challenge:** This assumption is wrong under D8 (ocx-in-container). The actual phase-1 linux step is:

```bash
docker run --rm -v "$RUNNER_TEMP/bundle.tar.xz:/bundle.tar.xz:ro" \
  -e OCX_VERSION="$VERSION" -e OCX_PLATFORM="<P>" -e OCX_IMAGE="<image>" \
  <image> <shell> -c '
    <install-ocx-in-container>   # see A11
    ocx package test --platform <P> /bundle.tar.xz -- <user-test-cmd>
  '
```

The host never calls `ocx package test`; the container does, as a single invocation. Same for mac/windows native legs (one invocation, no docker wrapper). The existing `conflicts_with` between `--keep` and `--output` never binds.

**Options:**
- **(a) Do nothing — current clap rules are correct.** Verified: pipeline never needs `--keep` and `--output` together.
- **(b) Relax `conflicts_with` anyway** — costs nothing, gives flexibility for future flows (e.g., a "smoke + push from same materialized dir" optimization)
- **(c) Add explicit `--exec-and-keep DIR` flag** — over-design for unclear need

**Recommendation:** **(a) Do nothing.** If a future flow surfaces the real need for `--output --keep`, relax at that point. YAGNI.

**Resolved 2026-05-13: (a) — do nothing.**

### A2: Renderer template strategy

**Options:**
- **(a) Templates baked via `include_str!`** (cargo-dist style)
- **(b) Templates as on-disk files alongside binary** — needs install-side packaging
- **(c) Per-mirror template override** — escape hatch

**Recommendation:** **(a) `include_str!` baked templates.** Standard Rust idiom for this exact need (cargo-dist, cargo-generate, dist itself). Single binary, no install-time file handling. (c) is YAGNI for phase 1; if a real escape hatch is ever needed, add an `extends:` mechanism on top of (a) without breaking existing renders.

**Resolved 2026-05-13: (a).**

### A3: Renderer drift detection

**Options:**
- **(a) `ocx-mirror generate ci --check`** exits non-zero on drift
- **(b) Embedded config-hash header** checked at workflow runtime
- **(c) Both**

**Recommendation:** **(a) only.** Drift is a render-time concern; runtime hash check duplicates effort and can't change the outcome (the workflow has already started). Run `--check` as the first step inside the generated workflow itself ("self-audit"), so editing a generated file out-of-band immediately fails CI on next push.

**Resolved 2026-05-13: (a).**

### A4: Bumping ocx-mirror SHA across N mirror repos

**Options:**
- **(a) Renovate-style bot** opens per-repo PRs bumping `ocx_mirror.rev`
- **(b) Reusable workflow** in `ocx-sh/.github` — single update propagates
- **(c) Hybrid** — per-repo pin + reusable workflow as emergency floor

**Recommendation:** Phase-1: **manual fan-out script** (`task mirror:bump-all`) — runs locally, opens PRs via `gh`. Bot (a) is the right answer once breadth justifies its existence (~10+ mirrors). (c) is the phase-3 endgame. Defer.

**Resolved 2026-05-13: Deferred entirely.** Handle when mirror breadth makes manual bumps painful; no phase-1 deliverable.

### A5: Pipeline state file location

**Options:**
- **(a) Stateless** — registry tag-list is the truth (D11)
- **(b) `.ocx-mirror/state.json` committed on default branch**
- **(c) Workflow artifact (90-day retention)**

**Recommendation:** **(a) Stateless.** Confirmed by re-reading `adr_ocx_mirror.md` — the canonical already-mirrored check is set-diff against registry tag-list, and partial-platform detection is "always-on" via image-index inspection. Adding a state.json would duplicate truth and introduce a re-sync risk.

The push job emits `run-summary.json` as a workflow artifact (1-day retention) purely for the notify job — that is not state, that is an inter-job message.

**Resolved 2026-05-13: (a) stateless.**

### A6: Required-check policy in mirror-repo template

**Options:**
- **(a) Yes** — block merge to default branch on red smoke
- **(b) No** — advisory
- **(c) Yes for production mirrors, no for experimental**

**Recommendation:** **(a) Yes**, with a caveat: branch protection is a per-repo admin-token operation, not something the renderer can set inline. The renderer should emit a `scripts/install-branch-protection.sh` snippet that the repo bootstrap process runs once (via `gh api`). Document required-check names (`test (linux/amd64, ubuntu_2404)`, `push`, `notify`) in the generated README so manual setup is also viable.

**Resolved 2026-05-13: (a).**

### A7: Cascade behavior on partial-version

**Options:**
- **(a)** Partial cascades among present platforms, never `latest` (D12)
- **(b)** Stricter — partial blocks the entire version cascade until all green

**Recommendation:** **(a).** Reaffirms D12. This is what `adr_cascade_platform_aware_push.md` enables — per-platform cascade is a designed-in capability. Blocking the whole version on one platform's failure (b) defeats the value of multi-platform mirroring and creates pressure to drop platforms from `mirror.yml` to "fix" the gate.

**Resolved 2026-05-13: (a).** User-flagged race concern (older version pushed later overwrites newer) mitigated by single-serial push job (revised D5) processing versions oldest-first.

### A8: `prefix:` field for rosetta-style command wrapping

**Options:**
- **(a)** Per-mirror override in `mirror.yml`
- **(b)** Hardcoded default in renderer executor table
- **(c)** Default in table, overridable per-mirror

**Recommendation:** **(c).** Defaults: `darwin/amd64` on `macos-latest` → `["arch", "-x86_64"]`. Mirror-yml can override (e.g., a mirror that ships universal binaries needs no prefix). Avoids per-mirror boilerplate while preserving escape hatch.

**Resolved 2026-05-13: (c).**

### A9: Container shell defaults

Simplified given D6 replacement: tests are individual command strings, each test invocation runs as `<shell> -c '<command>'` inside the container. Shell needed only as wrapper.

**Options:**
- **(a) Per-container `shell:` with defaults** (`alpine→sh`, `ubuntu/fedora/debian→bash`)
- **(b) Renderer infers from image-name pattern**
- **(c) Always require explicit `shell:`**

**Recommendation:** **(a).** Defaults table baked into renderer. Pattern-inference (b) breaks on tagged image names. Explicit-always (c) is boilerplate. Sideloading shell as ocx-package dep deferred to phase 2 (user-flagged: "what if we publish prefix-dev/shell as an ocx package").

**Resolved per user feedback.**

### A10: GHA service-containers vs manual `docker run`

**Options:**
- **(a) Manual `docker run`** in step (current sketch)
- **(b) Two-job pattern** — prepare on host, smoke in container
- **(c) Single container job** with ocx installed at start

**Recommendation:** **(a).** GHA's `container:` job-level config locks the entire job; we need pre/post steps on the host (artifact download, result upload, push logic). Manual `docker run` is the cleanest, and is the natural entrypoint for inserting a qemu layer underneath in phase 2.

**Resolved 2026-05-13: (a).** **Refined per plan-handoff:** evolved into `docker build` (ephemeral Dockerfile that `ADD`s the host-downloaded `ocx` binary) followed by `docker run` against the built tag — structurally still manual `docker run`, just against a per-leg image that bakes the binary instead of mounting it at runtime. See [system_design_mirror_test_pipeline.md](./system_design_mirror_test_pipeline.md) §5.3.

### A11: How ocx enters the linux container

**Options reframed given the verified fact that ocx already ships musl-static and glibc artifacts via `dist-workspace.toml`:**

- **(a-musl) Download ocx musl release artifact at runner, mount into container.** Works across alpine/ubuntu/fedora because musl-static. Single host-side download per leg, mount as read-only volume. Cost ~3–5s download (cached via `Swatinem/rust-cache` or actions cache).
- **(b) `curl ocx.sh/install.sh | sh` inside each container.** Resolves musl/glibc internally. Adds 5–30s per container. Robust to libc surprises.
- **(c) Pre-baked `ocx-sh/test-runner-<image>:<ocx-ver>` images.** Fastest steady-state, image-maintenance burden.

**Note:** the original sub-decision ("is ocx musl-static today?") is already answered — both targets ship in releases. The host's `which ocx` would resolve to glibc on Ubuntu, so naive `(a)` from the original handover (mount host's binary) would break for alpine.

**Recommendation:** **(a-musl)** — download the musl-static asset for the matching arch directly from the GitHub release pinned by `mirror.yml: ocx_mirror.rev`, mount into the container.

```bash
# Inside the runner host
ARCH=$(uname -m)  # x86_64 or aarch64
gh release download "$OCX_MIRROR_REV" \
  --repo ocx-sh/ocx \
  --pattern "ocx-${ARCH}-unknown-linux-musl.tar.xz" \
  --output "$RUNNER_TEMP/ocx-musl.tar.xz"
tar -xJf "$RUNNER_TEMP/ocx-musl.tar.xz" -C "$RUNNER_TEMP/ocx-musl"

# Per-leg ephemeral Dockerfile (see system_design §5.3 for full template)
cat >"$RUNNER_TEMP/ocx-musl/Dockerfile" <<'EOF'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ADD ocx /usr/bin/ocx
RUN chmod +x /usr/bin/ocx
EOF
docker build --build-arg "BASE_IMAGE=${IMAGE}" -t "ocx-test:${OCX_MIRROR_REV}" "$RUNNER_TEMP/ocx-musl"
docker run --rm "ocx-test:${OCX_MIRROR_REV}" ...
```

(Original sketch used `-v` runtime mount; refined to `ADD` for layer cache, production-topology fidelity, single-knob override.)

Caveat: this requires the pinned `ocx_mirror.rev` to correspond to a *tagged release* (not an arbitrary SHA) since `gh release download` resolves by tag. Two sub-options:

- **(a-musl-tagged)** Treat `ocx_mirror.rev` as a release tag, not a SHA — needs release cadence for ocx-mirror
- **(a-musl-built)** Keep `ocx_mirror.rev` as a SHA, but add a separate CI job in `ocx-sh/ocx` that publishes musl artifacts to a stable URL keyed by SHA (e.g., `https://github.com/ocx-sh/ocx/releases/download/musl-snapshots/{SHA}/ocx-musl-x86_64.tar.xz`)

**Resolved 2026-05-13: (a-musl-tagged) for phase 1.** `mirror.yml: ocx_mirror.release_tag` is required when any linux platform declares `containers:`. Mirror bumps move forward by re-tagging ocx-mirror releases; mirror.yml updates pin to that tag. Revisit later if release cadence becomes painful; (b) curl-install or (a-musl-built) snapshot-by-SHA remain fallbacks.

---

## Risk Register

Top risks identified during review, in declining order of severity:

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | **Cascade tag write race** between concurrent mirror-runs targeting same registry (e.g., two PRs landing simultaneously) | Medium | High (`latest` points at wrong version, partial cascade visible) | Within-run race eliminated by D5 single-serial push job. Across-run race mitigated by workflow-level `concurrency:` block with `group: mirror-${{ github.workflow }}-publish` + `cancel-in-progress: false`. Single mirror repo controls its own subtree (e.g., `ocx.sh/cmake`), so cross-mirror races are out of scope. Document constraint in `subsystem-mirror.md`. |
| R2 | **`ocx_mirror.rev` drift** across mirror repos — once breadth ≥10, manual bumps stall and the renderer + workflow versions diverge across the fleet | High | Medium (latent breakage, hard to debug) | A4-(a) bot becomes mandatory at ~10 mirrors. Renderer `--check` (A3) flags drift inside each repo's own CI. Stage S9 in swarm-plan input documents the manual fan-out as the phase-1 minimum. |
| R3 | **Discord webhook URL leakage** if user pastes URL into `mirror.yml` instead of using GitHub secrets | Medium | High (anyone with repo read can spam channel) | Renderer validates `mirror.yml`: reject `notify.discord_webhook_url:` containing literal `https://discord.com/...` at parse time; require `${{ secrets.DISCORD_WEBHOOK_URL }}` placeholder or a `secret_name:` field. Document at the spec-schema level. |
| R4 | **Phase-2 qemu underlay** may interact badly with ocx-in-container model — ocx running under emulation may have different syscall / dynamic-linker behavior than tested on bare metal | Medium | Medium (false-negative or false-positive smoke results on foreign archs) | Deferred to phase 2; flagged here so phase-1 design does not over-commit to ocx-in-container being the universal model. The sketch in D8 holds, but phase-2 will need a smoke-research spike validating qemu+docker+musl-ocx on at least one foreign arch (ppc64le suggested as widest-supported by `run-on-arch-action`). |
| R5 | **`ocx package test` exit-code semantics not yet aligned with mirror needs** — current implementation may conflate "test command exited non-zero" with "ocx-internal failure to materialize." Mirror CI needs unambiguous: exit 0 = passed, exit ≠ 0 = failed | Low | Medium | Stage S2 in swarm-plan input must verify `ocx package test` returns the child command's exit code on success path (Unix passthrough, Windows saturate per `quality-rust-exit_codes.md` and `utility::child_process::propagate_exit_code`). Add unit test if missing. |

---

## Decision Outcome

**Chosen approach (single sentence):** Per-mirror GHA pipeline rendered from `mirror.yml` by a new `ocx-mirror generate ci` subcommand, structured as five jobs (discover/prepare/test/push/notify), reusing the existing `ocx package test` and `ocx package push --cascade --format json` CLI surfaces unchanged, with `ocx-mirror` distributed via pinned-SHA cargo install (phase 1) and ocx (musl-static) entering linux containers via tagged-release download (A11-pending).

**Rationale (one paragraph):** This composition lets every existing OCX surface stay intact (no new `ocx package` subcommands per D3, no protocol changes to `Client::push_package` or `push_cascade` per `adr_ocx_mirror.md`, no new index semantics per `adr_index_routing_semantics.md`) while extending the mirror-side surface with one renderer subcommand and two new orchestration subcommands (`ocx-mirror plan`, `ocx-mirror notify`) whose contracts are pure I/O over JSON. The single-serial push job (revised D5 per user feedback) keeps `ocx package push --cascade` as the sole writer of cascade tags, eliminating the cross-platform image-index race while keeping cascade logic in its existing home. The `tests:` array + JUNIT output (revised D6) gives mirror authors multi-test ergonomics and reuses existing GHA test-reporting tooling (`EnricoMi/publish-unit-test-result-action`) without new infrastructure. The repo-per-mirror topology (D1) maps to conda-forge prior art that has scaled to thousands of feedstocks, and the stateless reporting model (D11) prevents the most common drift mode in similar systems (state-file vs registry divergence).

### Quantified Impact

| Metric | Today | After phase 1 | Notes |
|---|---|---|---|
| Pre-publish smoke coverage | 0 platforms | All declared platforms × all declared containers | Per `(V, P)` tile validated before push |
| `latest` tag promotion correctness | Manual / undefined | Auto, gated on fully-green newest | Per D12 + A7 |
| Per-mirror CI minutes (typical, 1 version, 3 linux + 2 mac + 1 win) | N/A | ~8 minutes wall-clock | Linux containers parallel; mac/win serial |
| Time-to-publish (TTP) for new version | Unknown (manual) | ≤ 15 min from upstream release detection to `latest` tag | Bounded by discover-job poll frequency + smoke duration |
| Per-mirror runner cost (typical run) | $0 | ~$0.20 | 6 linux-mins + 4 mac-mins + 2 win-mins at GHA list price |

### Consequences

**Positive:**
- Every published mirror has machine-verified smoke evidence
- Partial-platform breakage stops being silent — diagnosed at PR time, not post-publish
- Mirror repos are self-contained: a maintainer reading one `ocx-sh/mirror-<tool>` repo sees everything that decides what gets pushed
- Phase-2 (qemu, foreign archs) reuses the same docker-in-runner shape

**Negative:**
- Renderer is now part of the contract surface: `mirror.yml` schema additions and template changes are user-visible
- Bumping `ocx_mirror.rev` across many mirrors is a real cost (A4) — phase 1 absorbs this manually
- Mirror authors must learn the auto-detect rules for the `test:` field (D6 caveat addresses ambiguity)

**Risks:** see Risk Register above.

---

## Phase-1 Swarm-Plan Input

Suggested stages for `/swarm-plan` consumption. Each stage is testable in isolation; later stages depend on earlier ones.

| Stage | Scope | Acceptance |
|---|---|---|
| **S1** | `mirror.yml` schema extension: `tests:` array (`{name, command}`), `platforms.*.runner`, `platforms.*.containers[]`, `platforms.*.prefix`, `platforms.*.shell`, `platforms.*.tests` (override), `ocx_mirror.rev` (or `release_tag`), `notify.discord.webhook_secret` (env-var name only, URL literals rejected) | Round-trip serde tests + invalid-spec rejection tests (empty `tests:`, hardcoded discord URL, missing required runner, ambiguous shell on non-standard image, etc.) |
| **S2** | `ocx package test` audit (Risk R5): verify child-command exit code passes through via `propagate_exit_code` (Unix passthrough, Windows saturate). Verify `--format json` works on `ocx package push --cascade` and emits stable schema with `manifest_digest` + `cascade_tags_written` fields. Add regression tests if missing. | Acceptance tests: `ocx package test -- false` → exit 1; `-- true` → exit 0; `-- nonexistent` → 127. `ocx package push --cascade --format json` emits parseable JSON with both fields. |
| **S3** | `ocx-mirror generate ci` subcommand + `include_str!` templates. Renders fixture `mirror.yml` → expected workflow files (mirror.yml workflow, install scripts, README snippet). | Golden tests on 3-fixture corpus (minimal, multi-container linux, full 5-platform). Generated headers `# DO NOT EDIT — generated by ocx-mirror generate ci`. |
| **S4** | `ocx-mirror generate ci --check` drift detector. Exit 0 on match, exit 65 (DataError) on drift. Self-audit step as first step of generated workflow. | Golden tests + integration test: render, mutate file, `--check` returns 65 with path-only diff hint in stderr. |
| **S5** | `ocx-mirror plan` subcommand: discover phase, emits `{has_new, versions[], target, ocx_mirror_rev}` JSON. | Unit tests against fake registry fixture: empty, one-new, mixed new+existing, prerelease filter, backfill cap, partial-platform backfill. |
| **S6** | `ocx-mirror prepare --version <V>` subcommand: extract Phase-1 prepare from existing pipeline for a single version. Produces `_mirror/<V>/<platform_slug>/bundle.tar.xz` for every declared platform. | Integration test against fake registry+source: emits expected bundle file tree + idempotent on re-run. |
| **S7** | Push job logic: a small `ocx-mirror push --bundles-dir <dir> --junit-dir <dir> --write-summary <path>` helper that loops `(V, P)` deterministic order (oldest V first, then platform-order from spec), ANDs JUNIT testsuite results across containers for each `(V, P)`, calls `ocx package push --cascade -p <P> --format json` for greens, accumulates output into `run-summary.json`. Single-process; runs on one ubuntu-latest job. | Unit tests cover D12 status table exhaustively; integration test against fake registry verifies cascade-tag universe correctness (newest fully-green → `latest`, partial → no `latest`). |
| **S8** | Reference workflow files integration-tested via `act` or synthetic GHA env. Test job runs ocx-in-container per A11-resolved mechanism, emits JUNIT XML. Push job consumes JUNIT, pushes via `ocx package push --cascade --format json`. Notify job posts Discord. | Integration test on small mirror fixture (suggested: `mirror-shfmt`) produces real published artifact in `registry:2` test registry + dry-run Discord payload. |
| **S9** | `ocx-mirror notify --run-summary <path> --webhook-env-var <NAME>` subcommand + payload generator covering D10 taxonomy (silent / new-green / partial / all-failed) + JUNIT-aware test-failure summarization. | Golden tests on Discord payload JSON for each outcome. Webhook URL only via env-var sourced from `${{ secrets.DISCORD_WEBHOOK_URL }}`. |
| **S10** | Reference mirror repo (`ocx-sh/mirror-shfmt` or similar) bootstrapped end-to-end: render, commit, run, publish. Document manual `ocx_mirror.rev` bump procedure (A4 phase-1 minimum). | E2E run produces published artifact at `ocx.sh/shfmt:<V>` + Discord post (to test channel) + PR-annotation from JUNIT. Bump-procedure doc reviewed. |

**Out of scope for phase 1:** foreign archs (mips, sparc, ppc64le, s390x, riscv64) via qemu, ocx-mirror as OCX-packaged tool (dogfood install), aggregator workflow in `ocx-sh/.github`, branch-protection auto-installation (manual `gh api` script provided instead — A6).

## Implementation Plan

1. [ ] User reviews this ADR, resolves all `Awaiting user feedback` flags, marks status `Accepted`
2. [ ] `/swarm-plan` consumes the Phase-1 Swarm-Plan Input table above and produces stage-by-stage plan with assigned workers
3. [ ] Stages S1–S6 executable in parallel after S1 (schema is the only blocker); S7 depends on S2–S6; S8 depends on S6; S9 depends on S7+S8
4. [ ] Stage S9 deliverable = a real mirror repo + Discord post + published artifact, all observable

## Validation

- [x] All twelve D-decisions either reaffirmed or replaced with a documented alternative in this ADR
- [x] All eleven A-calls resolved (A1/A4/A11 final feedback 2026-05-13)
- [ ] Risk register reviewed by `worker-reviewer` (security perspective on R3, ops perspective on R1+R2)
- [ ] Cost analysis approved by user (estimate above is back-of-envelope)
- [ ] Phase-1 swarm-plan input table maps cleanly to existing OCX agent roles (builder/tester/reviewer)

## Links

- [Handover](./handover_architect_mirror_test_pipeline.md) — input to this ADR
- [Sparring notes](./notes_mirror_test_design.md) — five rounds of design dialogue
- [Research](./research_mirror_per_os_smoke.md) — external trend scout
- [Original ocx-mirror ADR](./adr_ocx_mirror.md) — pipeline + cascade contract this builds on
- [Cascade platform-aware push ADR](./adr_cascade_platform_aware_push.md) — per-platform cascade enabler
- [Index routing semantics ADR](./adr_index_routing_semantics.md) — Query vs Resolve operation discipline
- Companion artifact: [`system_design_mirror_test_pipeline.md`](./system_design_mirror_test_pipeline.md) — component contracts

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-13 | Architect (Claude) | Initial draft from handover_architect_mirror_test_pipeline.md |
| 2026-05-13 | Architect (Claude) | Revised after user feedback: single serial push job (revised D5), tests:[]+JUNIT (revised D6), drop cascade-finalize subcommand |
| 2026-05-13 | Architect (Claude) | Locked final open calls (A1: do nothing; A4: deferred; A11: musl-tagged-release). Status → Accepted. |
| 2026-05-13 | Architect (Claude) | **Plan-level refinements during /swarm-plan handoff:** (1) CLI surface consolidated — five new subcommands moved under `ocx-mirror pipeline` subgroup: `ocx-mirror pipeline {generate ci, plan, prepare, push, notify}`. Refines D5 + D6 + D11 + A11 wording (originally flat). (2) Install reduced to **direct `gh release download` on host** for all targets (`-unknown-linux-gnu` / `-apple-darwin` / `-pc-windows-msvc` / `-unknown-linux-musl`). `gh` CLI inherits `GITHUB_TOKEN` from the runner automatically — no extra auth wiring; private releases work too. **Container leg uses a per-leg ephemeral Dockerfile with `ADD ocx /usr/bin/ocx`** so the binary lives inside the image filesystem (not a `-v` runtime mount), matching production Dockerfile install topology and gaining `docker build` layer cache. No `ocx_install:` mirror.yml block, no `install-ocx.sh` template, no third-party setup action. Two workflow env vars cover the integration-test "use locally-built binary" case: `OCX_BINARY_OVERRIDE` (host/native) and `OCX_BINARY_OVERRIDE_MUSL` (container). When set, a shell guard substitutes the source tar; the Dockerfile and `docker build` step stay byte-identical so override paths exercise exactly the production install flow. Refines A11 (musl-tagged-release retained for container path), A2 (templates two files: workflow YAML + branch-protection script + README snippet; no install-ocx.sh), A10 (manual `docker run` evolves into `docker build` + `docker run` against ephemeral image tag). Rationale: setup-ocx is being split into a separate `setup.ocx.sh` website repo for shell/Dockerfile/devcontainer install with auto-update — CI matrix legs need only a pinned binary, irrelevant. Auto-update is a non-goal in this scope. |
