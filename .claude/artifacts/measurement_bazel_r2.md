# Measurement: the R2 threshold for the Bazel lane swap (WP-30 / ADR WP-1c)

- **Work package:** WP-30 of [`plan_bazel_build_adoption.md`](./plan_bazel_build_adoption.md) — C-028, S-016
- **Gates:** WP-23 (`verify-basic.yml` swap) and WP-24 (`verify-deep.yml` Linux leg)
- **Threshold judged against:** [`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md) § Orchestrator rulings **R2**
- **Binding constraint:** **C-029** — the remote cache realm is not deployed,
  `BAZEL_CACHE_READ_AUTH` / `BAZEL_CACHE_WRITE_AUTH` are unset, so every cache number
  below comes from `--disk_cache` alone.
- **Date:** 2026-09-21
- **Tree:** worktree `.agents/worktrees/wp30-measurement`, branch
  `hex/bazel-adoption--wp30-measurement`, base `sion` @ `f1381427`
- **Measurement host:** WSL2, 32 logical cores, 31 GiB RAM, 32 GiB swap.
  `.cargo/config.toml` `jobs = 12`; `.bazelrc` `build --jobs=12`. `RUSTC_WRAPPER=sccache`
  is set in this shell and **was in force for every cargo number** — Bazel does not
  consult it.

---

## VERDICT

> **NO-GO for the lane swap.**
>
> **WP-23 and WP-24 do not land.** `.verify:build-test` keeps its fourteen steps, the
> `smoke` job of `verify-basic.yml` keeps `task rust:test:unit -- --profile ci`, and the
> Linux leg of `verify-deep.yml` keeps `cargo nextest run`. The terminal state is the
> ADR's **B-without-the-swap**.

**The number it rests on: 173.5 s.** That is the median wall-clock, over the ten most
recent successful `verify-basic` runs, of the **one step the swap replaces** — `Test` in
`Smoke (Linux)`. Every other step in that job survives the swap unchanged, for reasons
given under § 6.

**The threshold it is judged against:** R2, ratified —

> a warm-cache `bazel test //crates/...` on a GitHub-hosted runner must beat
> `cargo nextest run --workspace --profile ci` on the same commit by **≥ 4 minutes
> median over 5 runs**, with per-lookup RTT **< 150 ms p50**. Otherwise […] **A2's lane
> swap does not land**.

**173.5 s < 240 s.** A Bazel step costing *literally zero* misses the ratified margin by
**66.5 s**. The threshold is not merely unmet — on this surface it is **unreachable**,
because the quantity being optimised is smaller than the margin demanded. The RTT half of
the threshold is unmeasurable (§ 3) and does not need to be measured: the wall-clock half
already fails.

The second, independent reading agrees. The literal C-028 comparison, run locally, median
over 5 each: `cargo nextest run --workspace --release --locked --profile ci` **71.01 s**
against `bazel test //crates/...` warm **4.44 s** — a delta of **66.57 s** against a
required 240 s.

`verify-deep` contributes nothing either way, for the structural reason M-01 gave and this
measurement re-confirms on fresh runs (§ 7).

**This verdict is not "insufficient data".** Both readings are complete, both are on the
surfaces R2 names, and both fail by more than 2×. The one thing that could change it is a
**re-spec, not a measurement** — § 10 states it and names the measurement that re-spec
would then require.

---

## 0. Baseline re-measured first — the obligation R2 attaches

> *"WP-1c re-measures the nextest baseline before comparing anything to it — against the
> measured 1731 s / 3407 s, never the stale 3347 s."*

```
gh run list --workflow <wf> --status success --limit 50 \
  --json databaseId,createdAt,updatedAt,startedAt
# duration = updatedAt - startedAt
```

| Workflow | Plan's figure | Re-measured | n | Spread |
|---|---:|---:|---:|---|
| `verify-basic.yml` | 1731 s | **1732 s** | 50 | min 515, p25 1650, p75 1810, max 3217 |
| `verify-deep.yml` | 3407 s | **2753 s** | 39 real of 46 | min 1323, max 3895 |

**`verify-basic`'s 1731 s is confirmed exactly.**

**`verify-deep`'s 3407 s is itself now stale, and so is the "stale" 3347 s.** Two
corrections:

1. Seven of the 46 successful `verify-deep` runs are **degenerate** — 18–24 s, no real
   work. Including them (the naive 50-run median) yields 2654 s, which is an artefact of
   the degenerate runs, not a measurement. Excluding them: **2753 s over 39 runs.**
2. The workflow has got materially faster. The five most recent real runs are 2226,
   2236, 2480, 2572 and 3422 s → **median 2480 s**. The 3347 s the plan calls stale is
   run `33335409372` of 2026-08-30; 3407 s is `34168250480` of 2026-09-07. Both are
   *older* than four of the five runs above.

Nothing downstream of this document depends on `verify-deep`'s absolute number (§ 7
shows the swap's contribution to it is zero), but the plan's Measured-facts table should
not keep quoting 3407 s as current.

---

## 1. `bazel test //crates/...` vs `cargo nextest run`, median over 5 — C-028 item 1

Same commit, same worktree, run back to back, nothing else building (per-run 1-minute
load average recorded beside each). Both engines were brought to a warm state first.

### 1a. The cargo arm

```
# build the test binaries first (this is CI's `Unit-test floor` step verbatim)
cargo nextest list --workspace --release --locked --message-format json
# then, five times:
cargo nextest run --workspace --release --locked --profile ci
```

| Run | Wall | nextest `Summary` | load at start |
|---|---:|---:|---:|
| 1 | 71.10 s | 70.162 s | 11.72 |
| 2 | 71.01 s | 70.373 s | 6.19 |
| 3 | 71.43 s | 70.733 s | 2.82 |
| 4 | 70.34 s | 69.717 s | 2.40 |
| 5 | 70.35 s | 69.717 s | 2.28 |

**Median 71.01 s**, spread 1.09 s (70.34–71.43). `8215 tests run: 8215 passed, 8 skipped`
on every run; peak RSS 659 MB. The 4.3× swing in host load across the five runs moved the
result by 1.5 % — this baseline is not load-sensitive.

### 1b. The Bazel arm — and why "warm" needs three states, not two

`--remote_cache=` (explicitly empty, C-029) and a **dedicated, initially empty**
`--disk_cache=~/.cache/ocx/wp30-disk-a`, so no number here is contaminated by the shared
`~/.cache/ocx/bazel-disk` that sibling worktrees populate.

Per WP-36, two runs cannot separate the in-memory action cache from `--disk_cache`. Three
can, and all three were taken:

| State | How it was produced | Median | Spread | n |
|---|---|---:|---:|---:|
| **cold** | fresh `--disk_cache`, `bazel clean` | **174.18 s** | — | 1 |
| **warm, same server** | repeat, server up | **0.26 s** | 0.25–0.85 | 5 |
| **warm, fresh server** | `bazel clean` + `bazel shutdown` each time, `--disk_cache` warm | **4.44 s** | 4.43–6.10 | 5 |

```
ocx exec bazel -- bazel test //crates/... \
  --remote_cache= --disk_cache=/home/mherwig/.cache/ocx/wp30-disk-a \
  --build_event_json_file=<bep>
```

**Which hypotheses these three separate.** Two runs would have given 174.18 s → 0.26 s and
said only "something cached". The third state is the discriminator: at 4.44 s Bazel
reports `1843 processes: 1304 disk cache hit, 573 internal` and `Executed 0 out of 34
tests`, so the 0.26 s state is *analysis reuse inside a live server* and the 4.44 s state
is *1304 action results read back off disk*. **4.44 s is the CI-shaped warm number** — a
CI runner never has a live server.

Cold and warm both report `34 tests pass`; the cold run executed all 34
(`1843 processes: 573 internal, 1304 linux-sandbox`).

### 1c. The comparison R2 asks for

| Quantity | Value |
|---|---:|
| `cargo nextest run --workspace --release --locked --profile ci`, warm, median of 5 | **71.01 s** |
| `bazel test //crates/...`, warm fresh server, median of 5 | **4.44 s** |
| **Delta** | **66.57 s** |
| **R2 requires** | **≥ 240 s** |
| **Shortfall** | **173.43 s** |

**Scope, stated beside the number.** This is the *local* host, not a GitHub-hosted runner
— R2 names the runner. The local host is the **friendlier** of the two to Bazel: 32 cores
against a runner's 4, and a `--disk_cache` sitting on local NVMe rather than one that has
to be restored over the network. A runner can only be slower. The CI-scope reading in § 6
is the one that decides, and it fails harder.

**Two scope defects in the comparison itself, both of which flatter Bazel and neither of
which rescues it:**

- **The two engines do not build the same thing.** `cargo nextest run --workspace
  --release` tests a **release** build. `bazel test //crates/...`, exactly as
  `taskfiles/rust.taskfile.yml`'s `test:unit:bazel` arm spells it, carries no `-c` flag
  and therefore builds **fastbuild**. `.config/nextest.toml` records the consequence: the
  slowest release test is 21.8 s and the same test is "roughly 100 s" in a debug build.
  So on any run where Bazel actually *executes* tests rather than replaying them, it is
  executing a materially slower binary.
- **The cargo arm's 71.01 s excludes its build; the Bazel arm's 4.44 s excludes nothing.**
  That is the correct comparison for a warm lane, and it is the one R2 asks for — but it
  means the 66.57 s delta is Bazel's *best* case, not its typical one.

---

## 2. Cold, and the marginal cost of one crate — C-028 item 3

`du -sb` on the dedicated `--disk_cache`, which is empty at the start of the sequence.
Each row is the state after the command in it.

| Step | Wall | tests re-executed | disk-cache bytes | Δ |
|---|---:|---:|---:|---:|
| cold `bazel test //crates/...` | 174.18 s | 34 / 34 | 2 244 591 359 | **+2.09 GiB** |
| touch **`ocx_util`** (hub) and re-test | 87.05 s | **27 / 34** | 3 369 063 082 | **+1 124 471 723 B (1.05 GiB)** |
| restore, touch **`ocx_announce`** (leaf) and re-test | 28.28 s | **8 / 34** | 3 633 595 779 | **+264 532 697 B (252 MiB)** |

The probe in both cases was an `#[inline(never)] pub fn` appended to the crate's
`lib.rs` — emitted code, not a dead `pub const` the linker strips (the failure mode
`bazel_gate_proofs.A1_ALIKE_MSG` warns about). Both probes were reverted;
`git status --porcelain` is empty and `grep -rn wp30_probe crates/` finds nothing.

**Which hypotheses this separates.** A universally-invalidating graph and a per-crate one
both re-run *something*; these two probes part company, 8 against 27, and both stay inside
their queried reverse-dependency closure:

```
bazel query 'kind(rust_test, rdeps(//crates/..., //crates/ocx_announce:ocx_announce))' → 10
bazel query 'kind(rust_test, rdeps(//crates/..., //crates/ocx_util:ocx_util))'         → 29
```

8 ≤ 10 and 27 ≤ 29. **The graph is per-crate.** That property is real, and it is the one
thing this measurement confirms in Bazel's favour. It is not worth 173.5 s.

**Sizing consequence for ADR ruling 10.** The ruling sizes the 50 GB remote cache for
*"release-profile Rust artefacts"*. Two corrections: the Bazel graph is **fastbuild**, not
release (§ 1c), and the working set is **2.09 GiB cold with 0.25–1.05 GiB added per
touched crate**. On every `main` commit, a hub-crate change alone is ~1 GiB of new CAS;
50 GB is roughly 47 such commits before LRU eviction begins. This is a genuine number for
ruling 10 and it survives the NO-GO, because it also bounds what `actions/cache` would
have to carry (§ 5).

---

## 3. Per-lookup RTT p50 — C-028 item 2 — **NOT MEASURED, and not estimated**

**Unmeasurable on this run, by C-029.** The reader realm is implemented on
`server-hetzner1`'s `bazel-cache-reader-realm` branch but **not applied to the live host**,
and neither `BAZEL_CACHE_READ_AUTH` nor `BAZEL_CACHE_WRITE_AUTH` exists. Every
`--remote_cache` lookup from here is a 401.

**A local probe would have been the wrong measurement anyway, not merely an
approximation.** R2's RTT is *"Hetzner ↔ GitHub-hosted runners"*. This host is neither
endpoint. A figure taken from a WSL2 workstation on a domestic link, reported under
R2's name, would be the exact "green as wide as what ran" defect — a true number filed
under a scope it never touched. No number is offered.

**Owner action → § 11.**

The wall-clock half of R2 fails without it, so the verdict does not wait on this.

---

## 4. The `bunx vitepress build` chain — C-028 item 4 — **A3 aborts**

ADR § A3's abort condition, verbatim:

> **WP-1c measures the `bunx vitepress build` chain at under 3 minutes cold, or at fewer
> than 5 invocations per week** → the site rule is not written.

### 4a. Cold, on a real cold machine — CI

The `Build Website` job of `deploy-website.yml` is a fresh runner every time: it downloads
the generated inputs as artifacts and runs the chain once. Per-step medians, five most
recent successful runs:

| Step | Median | Spread |
|---|---:|---|
| **Build with VitePress** | **9 s** | 8–11 |
| whole `Build Website` job | 26 s | 26–29 |

### 4b. Cold and warm, locally

```
bun install --frozen-lockfile              # 0.39 s, 147 packages
task website:scripts:publish --force       # 0.58 s — generates website/src/_scripts/
bunx vitepress build                       # timed
```

| State | Wall |
|---|---:|
| cold — no `.vitepress/dist`, no Vite cache | **5.64 s** |
| warm ×3 | **5.46 s** median (5.46, 5.46, 5.48) |

**There is no warm state to speak of: 5.46 s against 5.64 s.** `vitepress build` keeps no
incremental cache worth the name, which also means a Bazel rule around it has nothing to
reuse *between* invocations that vitepress is not already failing to reuse.

A standalone `bunx vitepress build` **fails** without `website/src/_scripts/`
(`ENOENT … src/_scripts/getting-started/exec.sh`); the timing above is after generating it.
That is the same input-completeness property the coarse rule would have to declare.

### 4c. Invocation frequency

```
gh run list --workflow deploy-website.yml --limit 50 --json createdAt,startedAt,updatedAt
```

50 runs spanning 30.2 days → **1.66 runs/day = 11.6 per week**; median whole-workflow
duration 469 s.

### 4d. Ruling

| A3 clause | Measured | Fires? |
|---|---|---|
| chain under 3 minutes cold | **9 s** on CI, 5.64 s locally | **YES** |
| fewer than 5 invocations/week | 11.6/week | no |

The clauses are disjunctive (*"…at under 3 minutes cold, **or** at…"*). The first clause
fires by a factor of 20.

> **A3 aborts. WP-34's coarse site rule is not written.** A hand-written Starlark rule and
> a `bazel-quality.md` MUST-exemption, bought for a 9-second step that runs 11.6 times a
> week, is 104 seconds of machine time per week against a permanent maintenance surface.
> WP-37's contributor page and WP-33/WP-33b's cast targets are separate decisions and this
> ruling does not touch them.

---

## 5. `--disk_cache` layering — C-028 item 5 — **yes, it layers; no, it is not worth buying for stage 2**

### 5a. It layers, and it shields a dead remote — measured

```
bazel clean && bazel shutdown
ocx exec bazel -- bazel test //crates/... \
  --disk_cache=/home/mherwig/.cache/ocx/wp30-disk-a
#   ^ note: no --remote_cache= override, so .bazelrc's
#     build --remote_cache=https://bazel-cache.ocx.sh/v1 is live and anonymous = 401
```

→ `Elapsed time: 4.710s`, `1843 processes: 1304 disk cache hit, 573 internal`,
`Executed 0 out of 34 tests`, **no remote error surfaced**, wall **4.79 s**.

Against the same run with the remote disabled outright (4.44 s median), a
**denied remote costs 0.35 s**, inside the 4.43–6.10 s spread. The disk cache is consulted
first and a dead or refusing remote is invisible behind a warm one.

### 5b. `--remote_upload_local_results=false` does not suppress disk writes — re-confirmed independently

`.bazelrc` carries `build --remote_upload_local_results=false` and it was in force on
**every** run in this document. The dedicated disk cache still grew from **0 to
2 244 591 359 bytes** on the cold run. This re-confirms WP-36's finding on 9.2.0 from a
different starting state.

### 5c. The ruling

**Yes** — layering works, is free when the disk tier hits, and is the only cache tier that
is measurable at all today (C-029).

**But it buys stage 2 nothing, for three measured reasons.**

1. An ephemeral runner keeps a `--disk_cache` only via `actions/cache`. The entry is
   **2.09 GiB** and grows **0.25–1.05 GiB per touched crate** (§ 2), against GitHub's
   10 GiB per-repository LRU — so the entry churns, and the restore is a network transfer
   on the critical path.
2. The step it would accelerate is worth **173.5 s** (§ 6). A 2 GiB restore is a
   meaningful fraction of that before a single action is replayed.
3. The lane swap does not land, so on `Smoke (Linux)` and `verify-deep`'s Linux leg there
   is no Bazel step for a disk cache to accelerate.

Layering stays correct for the **local developer** loop, where `.bazelrc`'s
`--disk_cache=~/.cache/ocx/bazel-disk` is shared across all four fixed worktrees and every
agent worktree, and where § 1b's 4.44 s is the real experience. Keep the flag; do not
build CI machinery around it.

---

## 6. The step-level split of `Smoke (Linux)` — C-028 item 6, P4's reopen evidence

**This is the half of the ADR's signal that M-01 never measured**, and it is the number
the verdict rests on.

```
gh api /repos/ocx-sh/ocx/actions/runs/<id>/jobs?per_page=100
# per-step started_at / completed_at, job "Smoke (Linux)",
# the 10 most recent successful verify-basic runs
```

Job total median **1266 s** (n=10, min 1013, max 2195). Sum of step medians 1301 s. Steps
above 20 s:

| Step | Command | Median | Spread | Swapped by WP-23? |
|---|---|---:|---:|:--:|
| **Unit-test floor** | `task rust:test:floor` → `cargo nextest list --workspace --release --locked` | **536.5 s** | 423–667 | **NO** |
| **Build** | `task rust:build` → `cargo build --release -p ocx --features ocx/__testing` | 197.5 s | 138–495 | **NO** |
| **Test** | `task rust:test:unit -- --profile ci` → `cargo nextest run --workspace --release --locked` | **173.5 s** | 122–193 | **YES** |
| Generate Schema | `task schema:generate` | 113.5 s | 53–157 | NO |
| Clippy | `task rust:clippy:check` | 47.5 s | 31–177 | NO |
| Lint ratchet | `task rust:lint:ratchet` | 45.0 s | 30–156 | NO |
| Doc tests | `task rust:test:doc` → `cargo test --doc` | 41.0 s | 33–114 | NO |
| Setup Rust | action | 36.0 s | 14–51 | NO |
| Doc ratchet | `task rust:doc:ratchet` | 26.0 s | 19–143 | NO |
| Rust Cache | action | 22.5 s | 1–33 | NO |

### 6a. How much of `Smoke (Linux)` is compile?

**734 s of 1266 s — 58 %.** `Unit-test floor` (536.5 s) is `cargo nextest list`, which
must *build every test binary in release* before it can list a case; `Build` (197.5 s) is
the release `ocx` with `--features ocx/__testing`.

Test **execution** is 173.5 s — 14 % of the job. Corroborated from inside the step:
nextest's own `Summary` line, read out of six CI job logs,

```
gh api /repos/ocx-sh/ocx/actions/jobs/<job-id>/logs | grep 'tests run:'
```

| Run | `Summary [ … ]` |
|---|---:|
| 35564969286 | 121.219 s |
| 35529003604 | 149.805 s |
| 35539665948 | 156.297 s |
| 35530907494 | 165.012 s |
| 35534184764 | 179.450 s |
| 35538021028 | 182.011 s |

**Median 160.65 s**, n=6. So of the `Test` step's 173.5 s, ~161 s is test execution and
~13 s is cargo confirming the binaries the floor step already built are up to date. The
step is execution, and the execution is 161 s.

### 6b. Why 173.5 s is the whole prize, and the floor step is not

**Because the specification says so, in two places, and the code implements it.**

- **C-016**: *"`verify-basic.yml` `smoke`: nextest **execution** swapped"* — execution, not
  listing.
- **C-012**, re-proved rather than asserted in `scripts/bazel_floor_proofs.py:15-22`:
  *"the floor is untouched"*. `taskfiles/rust.taskfile.yml`'s `test:floor` has **no engine
  dispatch at all** — no `OCX_TEST_ENGINE`, one recipe, `cargo nextest list --workspace
  --release --locked`.
- The **ceiling** under the Bazel engine runs `cargo nextest list` too:
  `bazel_floor_proofs.py:985-994` defines `NEXTEST_LIST = ("cargo", "nextest", "list",
  "--workspace", "--release", "--locked", …)`, and the taskfile's own summary says
  *"there is no nextest Summary line to read […] The count comes from `cargo nextest list`
  instead"*.
- WP-19's withdrawal note in the plan says the same thing from the other side: *"the floor
  is a `cargo nextest list` declaration check and the execution swap does not touch it"*.

**So in the swapped lane, `Smoke (Linux)` still pays the full release compile of every test
binary — 536.5 s — and then additionally pays a Bazel fastbuild compile-or-replay of the
same 34 targets.** The swap removes one 173.5 s step and adds a step. It does not remove a
single second of compilation.

### 6c. What the swap would do to `verify-basic`

`Smoke (Linux)` (1266 s) is the workflow's critical path; `Smoke Acceptance` (498 s median)
follows it and the two sum to ≈ the 1732 s workflow median. Removing the `Test` step
entirely, at zero cost:

| | Median |
|---|---:|
| `verify-basic` today | 1732 s |
| best case after the swap, Bazel step free | 1558.5 s |
| **improvement** | **173.5 s — 10.0 %** |
| **R2 requires** | **240 s** |

### 6d. P4's reopen condition — does clause 4 fire?

P4's condition:

> WP-30's step-level cold/warm measurement on the affected Linux lanes. **If compile
> dominates `Smoke (Linux)`**, clause 4 fires on a surface Bazel actually touches and
> WP-00's verdict can be amended in that commit.

**Compile dominates `Smoke (Linux)`: 734 s of 1266 s, 58 %. The antecedent is true.**

**The consequent is false, and the conditional's own wording is what refutes it** — *"on a
surface Bazel actually touches"*. The 734 s of compile is **cargo's**, inside
`Unit-test floor` and `Build`, and C-012 and ADR ruling 3 place both outside the swap.
Bazel touches exactly one step in this job and that step contains **~13 s** of compile-ish
work and ~161 s of execution.

> **Clause 4 does not fire. WP-00's fourth-line verdict stands unamended.** P4 was right to
> decline the judgement, and this measurement is the reason it was right — it just is not
> the reason P4 anticipated. The largest item in the job *is* compile; it is simply not
> reachable from the lane the ADR scoped.

---

## 7. `verify-deep` — M-01's matrix arithmetic, re-confirmed on fresh runs

Build-stage job totals, five most recent successful `verify-deep` runs:

| Leg | Median | Spread |
|---|---:|---|
| **Build & Unit Test (Windows)** | **1456 s** | 1191–1973 |
| Build & Unit Test (Linux) | 759 s | 641–1386 |
| Build & Unit Test (macOS) | 655 s | 567–1427 |

The stage's duration is `max(windows, macos, linux)` = **Windows, in all five runs**.
Bazel takes only the Linux leg (ADR § Stage 2 ruling 1).

**Taking the Linux leg to zero shortens the stage by 0 s.** Even the leg's own best case is
bounded: its steps are `Build` 253 s, **`Test` 365 s** (349–522 — here `Test` includes the
test-binary compile, there being no separate floor step), `Generate Schema` 116 s,
`Checkout` 9 s. Removing all 365 s leaves 403.5 s, still 1052 s short of Windows.

> **Stage 2's contribution to `verify-deep` is structurally zero. WP-24 lands no
> measurable benefit under any Bazel result whatsoever**, and C-017 would additionally add
> an *ungated report-only nextest parity probe* to that leg — i.e. run both engines. The
> swap makes the leg longer.

---

## 8. Cache-state reading from the BEP — three corrections, and one false green in the plan

The brief for this WP said: read cache state from the BEP, not the terminal summary;
`cachedLocally` is proto3 and therefore absent when false. That is true and insufficient.
Five BEP files, 34 `TestResult` events each, across four distinct cache states:

```
--build_event_json_file=<bep>, then per TestResult event:
  executionInfo.strategy | top-level cachedLocally | executionInfo.cachedRemotely
```

| State | `executionInfo.strategy` | top-level `cachedLocally=true` | `executionInfo.cachedRemotely=true` | terminal line |
|---|---|---:|---:|---|
| cold | `linux-sandbox` ×34 | 0 | 0 | `Executed 34 out of 34` |
| warm, **same server** | *no `executionInfo`* ×34 | **34** | 0 | `Executed 0 out of 34` |
| warm, **fresh server**, disk cache | **`disk cache hit`** ×34 | **0** | **34** | `Executed 0 out of 34` |
| hub crate touched | 27 `linux-sandbox`, 7 none | 7 | 0 | `Executed 27 out of 34` |
| leaf crate touched | 8 `linux-sandbox`, 19 `disk cache hit`, 7 none | 7 | **19** | `Executed 8 out of 34` |

**C-1 — `cachedLocally` is false for every `--disk_cache` hit, which is the CI-shaped warm
state.** It is true *only* for the in-memory action cache, which a CI runner never has. A
reader keying "was this cached" on `cachedLocally` reports **0 of 34 cached** on the run
where **34 of 34 were cache hits**. That is not a field that is absent-when-false; it is a
field that answers the opposite of the question on the one state that matters.

**C-2 — `cachedRemotely: true` is emitted for a `--disk_cache` hit, with `--remote_cache=`
explicitly empty.** Bazel 9.2.0 classifies the disk cache as a remote cache in
`executionInfo`. **This makes S-016 a false green.** S-016 asserts *"Nonzero remote cache
hits"* with a red half of *"remove the credential → 0 hits"*. With `.bazelrc` putting
`--disk_cache=~/.cache/ocx/bazel-disk` on **every** invocation, removing the read
credential leaves `cachedRemotely=true` on every replayed target. The scenario the plan
calls *"without this, the entire value proposition is unasserted"* is, as written,
assertable with the remote cache switched off.

**C-3 — the only reliable discriminator is `executionInfo.strategy`**: `linux-sandbox` =
executed, `disk cache hit` = disk tier, `executionInfo` absent = in-memory action cache.
All three reconcile exactly with the terminal `Executed M out of N` on all five files.

These three belong to WP-21 (S-016's assertion) and WP-26 (`bep_to_otlp.py`). The patch is
in § 12; this WP's file set is one file and does not include them.

---

## 9. Confounders, named

| Confounder | Handling |
|---|---|
| Sibling builders on a 32 GB host | 1-minute load average recorded per cargo run (2.28–11.72) and per Bazel run (8.08–13.37). The cargo baseline moved 1.5 % across a 4.3× load swing. |
| `RUSTC_WRAPPER=sccache` | In force for every cargo number, none of the Bazel ones. It affects **build** time only; the § 1c comparison is of warm runs where cargo compiles nothing. The local cold `cargo nextest list` at 166 s is therefore *not* comparable to Bazel's 174 s cold and is not compared to it anywhere. |
| Shared `--disk_cache` from sibling WPs | Every Bazel number uses a **dedicated, initially empty** `~/.cache/ocx/wp30-disk-a`. |
| Bazel server state | Explicitly a measured variable, not a nuisance: three states, § 1b. |
| Remote cache reachable | Disabled with `--remote_cache=` on every timed run except § 5a, where it is the subject. |
| `aquery --output=jsonproto` `actionKey` | **Not used.** WP-31 measured it byte-identical across three tool states. Every cache claim here is re-execution counts, BEP `strategy`, or `du -sb` byte deltas. |
| Bazel profile mismatch | Bazel is `fastbuild`, cargo is `--release`. Named in § 1c; it flatters Bazel. |
| Local host vs GitHub-hosted runner | Named in § 1c; the local host is Bazel's friendlier surface. The deciding reading (§ 6) is taken on CI. |

---

## 10. What would change this verdict — and it is not a measurement

The NO-GO is against **the swap as C-012 and C-016 specify it**. One re-spec could in
principle move the arithmetic, and it is a decision, not data:

> **Re-spec the floor and the ceiling to read Bazel's own graph**, dropping
> `cargo nextest list --workspace --release --locked` from the lane entirely.

Then the swappable total becomes `Unit-test floor` + `Test` = **536.5 + 173.5 = 710 s**,
and 240 s of margin exists **if and only if** the Bazel step costs under 470 s on a
GitHub-hosted runner.

**That number does not exist and cannot be produced from this host.** The 4.44 s measured
in § 1b is a 32-core box with a warm local-NVMe disk cache. A runner has 4 cores, no warm
disk cache except an `actions/cache` restore of the 2.09 GiB working set (§ 2), plus the
`@crates` crate_universe splice on a fresh checkout. The settling measurement is:

> **`bazel test //crates/...` on a `ubuntu-latest` GitHub-hosted runner, warm remote cache,
> median over 5, on a throwaway lane** — which needs both the deployed reader realm and a
> temporary workflow, i.e. it is downstream of the owner actions in § 11.

Three things that re-spec would cost, so the decision is taken with them visible:

1. `NEXTEST_FLOOR` = 8213 and `NEXTEST_SKIP_CEILING` = 9 are **per-testcase** floors over
   8223 listed cases. Bazel's graph floors at **34 targets**. Swapping a
   per-testcase floor for a per-target one is a 242× loss of resolution: deleting 200
   `#[test]` functions inside one crate is invisible to a target-count floor.
2. `bazel_floor_proofs.py`'s C-013 proof, its `_cached_as_skipped` control (0 on a cold
   BEP, 33 on the cached one) and C-012's two-recipe separation are all built on the
   listing being present. They would be rewritten, not adjusted.
3. § 8's C-1 says a Bazel-graph-based reader must key on `executionInfo.strategy`.
   Keyed on `cachedLocally`, a fully-cached CI run reads as fully executed.

**Recommendation attached to the re-spec: do not take it.** It trades a per-testcase
declaration gate for a per-target one in order to chase a 710 s ceiling on a job whose
execution content is 161 s, on the one workflow where a win is even possible.

---

## 11. Owner actions — the things this WP could not measure

Neither is a prerequisite for the verdict. Both are prerequisites for anyone ever taking
the RTT measurement R2 names.

1. **Apply `server-hetzner1`'s `bazel-cache-reader-realm` branch on the live host** —
   until then every anonymous `--remote_cache` read is a 401 and `bazel-cache/README.md:7-9`
   ("reads are anonymous") stays wrong.
2. **Create the two GitHub secrets `BAZEL_CACHE_READ_AUTH` and `BAZEL_CACHE_WRITE_AUTH`.**

With both done, the outstanding measurement is § 10's throwaway-lane run, which also yields
R2's RTT p50 (`--remote_timeout=30s`, `--remote_retries=2` bound the worst case per
operation). **It is only worth taking if the owner first decides to reopen § 10's
re-spec.** Under the current specification the arithmetic in § 6 settles it without any
remote number at all.

---

## 12. Patches owed to other work packages

This WP owns exactly one file, `.claude/artifacts/measurement_bazel_r2.md`. Everything
below was found while measuring and is handed back rather than applied.

**P-1 → WP-21, `.claude/tests/test_workflows.py` (S-016's assertion).** S-016 is a false
green as specified (§ 8, C-2). Replace *"Nonzero remote cache hits"* with a reading that a
warm `--disk_cache` cannot satisfy:

> S-016, restated: over the lane's BEP, **at least one `TestResult` carries
> `executionInfo.strategy == "remote cache hit"`**. `cachedRemotely` is true for a
> `--disk_cache` hit on Bazel 9.2.0 and therefore cannot carry this assertion; `.bazelrc`
> puts `--disk_cache` on every invocation, so the red half ("remove the credential → 0
> hits") is unreachable while the field is `cachedRemotely`.

**P-2 → WP-26, `scripts/bep_to_otlp.py`.** The cache-state reader must key on
`executionInfo.strategy`, with the mapping measured in § 8: `linux-sandbox` → executed,
`disk cache hit` → disk tier, `remote cache hit` → remote tier, `executionInfo` absent →
in-memory action cache (and `cachedLocally` is true in exactly that last case and no
other). A reader keying on `cachedLocally` reports 0 % cached on a 100 % disk-cached run.

**P-3 → the plan's § Measured facts, M-01.** `verify-deep`'s baseline is no longer 3407 s.
Current: **2753 s** over the 39 non-degenerate successful runs, **2480 s** over the five
most recent. Seven of 46 successful runs are degenerate (18–24 s) and must be excluded
before any median is taken.

**P-4 → the plan's § Measured facts, M-02.** Three of its counts are superseded by WP-12's
committed output, which M-02 itself anticipated (*"these numbers are WP-12's output, not
its input"*). Measured on this tree: `bazel query 'kind(rust_test, //crates/...)'` = **34**
(M-02 re-derived 33); `bazel_gate_proofs.CRATES_RULE_TARGETS` = 53,
`DRIFT_TARGET_FLOOR` = 56. And M-02's stated reason for 33 is refuted: it held that
`ocx_shim` contributes zero targets and that the `ocx` binary's `#[cfg(test)]` suite has no
Bazel target. Both `//crates/ocx_shim:ocx_shim_bin_test` and
`//crates/ocx_cli:ocx_cli_bin_test` exist — rules_rust compiles a `[[bin]]` crate's test
module as a `rust_test` with no `rust_binary` anywhere, which
`bazel_floor_proofs.py:56-64` already records.

**P-5 → ADR ruling 10.** The remote cache is sized for *"release-profile Rust artefacts"*.
`bazel test //crates/...` as the taskfile spells it builds **fastbuild**. Working set:
2.09 GiB cold, +1.05 GiB per hub-crate change, +252 MiB per leaf-crate change (§ 2).

**P-6 → WP-34 (`website/BUILD.bazel`, `website/site.bzl`).** A3's abort condition fires
(§ 4d). The site rule is not written.

**P-7 → observed, not owned.** `Cargo.bazel.lock.json`, copied into this worktree, records
crate_universe `path =` dependencies as **absolute paths into the generating worktree**:
`bazel query` here emits `WARN: Build is not hermetic - path dependency pulling in crate at
/home/mherwig/dev/ocx-sion/external/{docker_credential,rust-oci-client,sigstore-rs}/Cargo.toml`
— the *main* checkout, not this one. Consistent with DX-23's reason for gitignoring the
file, and it means a query in worktree A can read worktree B's submodules.

---

## Appendix — every command, in order

```sh
# scope: worktree .agents/worktrees/wp30-measurement @ f1381427

# § 0 baselines
gh run list --workflow verify-basic.yml --status success --limit 50 \
  --json databaseId,createdAt,updatedAt,startedAt
gh run list --workflow verify-deep.yml  --status success --limit 50 \
  --json databaseId,createdAt,updatedAt,startedAt

# § 6 / § 7 step-level splits
gh api /repos/ocx-sh/ocx/actions/runs/<run-id>/jobs?per_page=100   # 10 verify-basic, 5 verify-deep
gh api /repos/ocx-sh/ocx/actions/jobs/<job-id>/logs | grep 'tests run:'

# § 1a cargo arm
cargo nextest list --workspace --release --locked --message-format json    # 166 s, 36 suites, 8223 cases
for i in 1..5: cargo nextest run --workspace --release --locked --profile ci

# § 1b/§ 2 bazel arm — dedicated empty disk cache, remote disabled
mkdir -p ~/.cache/ocx/wp30-disk-a
ocx exec bazel -- bazel clean
ocx exec bazel -- bazel test //crates/... --remote_cache= \
  --disk_cache=~/.cache/ocx/wp30-disk-a --build_event_json_file=bep_cold.json
for i in 1..5: <same>                                        # warm, same server
for i in 1..5: bazel clean && bazel shutdown && <same>        # warm, fresh server
du -sb ~/.cache/ocx/wp30-disk-a                               # after each phase
bazel query 'kind(rust_test, rdeps(//crates/..., //crates/ocx_util:ocx_util))'      # 29
bazel query 'kind(rust_test, rdeps(//crates/..., //crates/ocx_announce:ocx_announce))'  # 10

# § 5a layering, .bazelrc's real --remote_cache live (anonymous = 401)
bazel clean && bazel shutdown
ocx exec bazel -- bazel test //crates/... --disk_cache=~/.cache/ocx/wp30-disk-a

# § 4 website
cd website && bun install --frozen-lockfile
ocx exec -- task website:scripts:publish --force
bunx vitepress build                                          # cold, then x3 warm
gh run list --workflow deploy-website.yml --limit 50 --json createdAt,startedAt,updatedAt

# § 8 BEP reading
python3 -c "<per-TestResult: executionInfo.strategy, cachedLocally, executionInfo.cachedRemotely>"
```
