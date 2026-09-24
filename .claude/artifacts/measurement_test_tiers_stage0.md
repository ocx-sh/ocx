# Measurement: Stage 0 verification latency baseline (WP-01 / C-001)

- **Work package:** WP-01 of [`plan_test_speed_tiers.md`](./plan_test_speed_tiers.md) — C-001, P-8, P-11
- **Judged against:** [`adr_test_speed_tiers.md`](./adr_test_speed_tiers.md) Stage 0, C-TIER T1 budget (≤ 120 s), AM-1, AM-4, AM-5
- **Date:** 2026-09-22
- **Bench tree:** throwaway detached worktree `.agents/worktrees/stage0-bench` at `563d3732`
  (`hex/test-speed-tiers`), submodules initialised, `task schema` run once. Removed after the runs.
  Nothing from it lands; this file is the only output of WP-01.
- **Raw data (not committed):** `/home/mherwig/dev/ocx/.tmp/hex-tiers/stage0/` — one directory per run
  under `runs/<label>/` (`host.txt`, `log.txt` time-stamped, `result.txt`), plus profiles and timings.

---

## VERDICT

> **G-0 (pre-D3 arm): GREEN — T1 = 88.5 s with the kept lever, against a 120 s budget.**
> Without any lever T1 is 114.2 s: inside the budget, with 5.8 s to spare.
>
> **Caveat, binding (ADR AM-4):** the pre-D3 scoped arm runs `bazel test //crates/ocx_setup:all`
> only — **no reverse-dependent `rust_test`**. So this GREEN does **not** predict D3. WP-06 re-judges
> G-0 on the new arm. One data point for that re-judgement (not a T1 measurement): `task bazel:test:unit`
> after the same edit took **22.2 s**, against 6–7 s for today's per-crate step (§ A0).

| | |
|---|---|
| **Dominant step** | **test binary build**: 78.2 / 79.3 s of the 115.3 / 113.0 s T1 (L0), which is 69 %. It splits into `.build-binaries` at 53.7 / 54.1 s — a strictly serial `release` rebuild of the `ocx` lib (20.8 s) followed by the `ocx` bin (34.3 s) (§ A2) — and the schema generation that `test:build` depends on at 24.5 / 25.2 s, which rebuilds the `ocx` lib a second time in `release` without `__testing` and links `ocx_schema`. |
| **Kept** | **L1 `[profile.test-bin]` (`incremental = true`)**: −25.7 s on the mean T1 (−22.5 %). `.build-binaries` drops from 54 s to 30 s. |
| **Dropped** | **L2 local sccache**: +3.9 s (+3.4 %). **L3 profile (`incremental = false`) + sccache**: +1.8 s (+1.6 %). After the edit sccache hits nothing: 0 hits, 5 misses and 30 non-cacheable calls per T1 run. Every unit it could serve is either unchanged, so cargo already skips it, or downstream of the edit. |
| **Unchanged re-run** | 25.0–26.4 s under every lever. The pytest legs (`test:scoped` ≈ 10 s, `test:smoke` ≈ 7 s) run uncached by design and are 2/3 of it. |
| **Acceptance** | cold (`NOCACHE=1`) **1618.9 s**, of which Bazel is 1522.9 s, serial, 181/181 executed · warm unchanged re-run **2.3 s**, 0/181 · `test:parallel --force` **135.1 s** (green; the first run was 145.6 s with one timing flake) |
| **Executed count** | docs-only commit **181 / 181** · one-crate commit **181 / 181** (both as expected before D1: the test binary changes on every commit) |

For WP-09 (C-021): land `[profile.test-bin]` with `incremental = true`. Local sccache is dropped, so
AM-5's `incremental = false` clause does not apply. Measured for the REL-01 comments:

- `incremental = true` → `.build-binaries` 53.9 s → 30.1 s after a one-crate edit. T1 88.5 s against 114.2 s.
- `codegen-units = 256` with incremental off (L3, under local sccache) → `.build-binaries` 54.6 s: no effect. The win is incremental's.
- The cost is a cold build that is 61 s slower: `test binary build` 241.8 s against 180.6 s in the warm-up column.

---

## Results — lever × step (the first `task verify:scoped --force` after the reference edit)

Seconds of wall clock per step (method: § Specify). Columns:

- **W**: the warm-up, on a cold `target/`. Not judged.
- **a**, **b**: the two T1 samples. These are the judged cells.
- **R**: the unchanged re-run.

Every run is `ocx exec -- task -d <bench> verify:scoped --force` with the lever environment of § Specify.
The raw log of each is `runs/<label>/log.txt`, and every run exited 0.

| step | L0-w | **L0-a** | **L0-b** | L0-R | L1-w | **L1-a** | **L1-b** | L1-R |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| plan | 0.4 | 0.5 | 0.4 | 0.4 | 0.5 | 0.5 | 0.4 | 0.4 |
| git:hooks | 0.1 | 0.1 | 0.0 | 0.1 | 0.0 | 0.1 | 0.0 | 0.0 |
| fmt | 1.7 | 1.8 | 1.7 | 1.8 | 1.7 | 1.7 | 1.7 | 1.7 |
| cargo check | 42.8 | 2.8 | 2.7 | 0.3 | 44.4 | 2.7 | 2.6 | 0.3 |
| clippy -p | 23.9 | 0.9 | 0.9 | 0.3 | 24.1 | 0.9 | 0.9 | 0.3 |
| bazel test | 13.8 | 7.3 | 6.1 | 0.4 | 6.2 | 6.1 | 5.8 | 0.3 |
| doc test | 39.7 | 1.1 | 0.9 | 0.4 | 41.2 | 0.9 | 0.9 | 0.5 |
| doc ratchet | 41.1 | 4.3 | 4.0 | 0.4 | 42.3 | 4.1 | 3.9 | 0.4 |
| test binary build | 180.6 | 78.2 | 79.3 | 4.5 | 241.8 | 55.5 | 54.3 | 4.5 |
| ↳ schema generation | 101.9 | 24.5 | 25.2 | 3.8 | 103.3 | 25.1 | 24.4 | 3.8 |
| ↳ `.build-binaries` | 78.7 | 53.7 | 54.1 | 0.7 | 138.5 | 30.3 | 29.9 | 0.7 |
| test:smoke | 14.3 | 7.3 | 6.9 | 7.6 | 7.4 | 6.7 | 7.7 | 6.8 |
| test:scoped | 11.8 | 11.0 | 10.0 | 10.3 | 10.1 | 10.2 | 9.2 | 10.2 |
| mark | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| **total wall (s)** | **370.3** | **115.3** | **113.0** | **26.4** | **419.8** | **89.4** | **87.5** | **25.4** |
| exit code | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| host load1 at start / foreign build procs | 3.00 / 0 | 10.68 / 0 | 10.93 / 0 | 9.06 / 0 | 8.12 / 0 | 10.66 / 0 | 6.78 / 0 | 5.62 / 0 |

| step | L2-w | **L2-a** | **L2-b** | L2-R | L3-w | **L3-a** | **L3-b** | L3-R |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| plan | 0.7 | 0.5 | 0.4 | 0.4 | 0.5 | 0.5 | 0.4 | 0.4 |
| git:hooks | 0.1 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| fmt | 1.8 | 1.7 | 1.7 | 1.7 | 1.8 | 1.7 | 1.7 | 1.7 |
| cargo check | 56.4 | 3.0 | 2.7 | 0.3 | 37.8 | 2.9 | 2.6 | 0.3 |
| clippy -p | 26.9 | 1.1 | 1.0 | 0.3 | 22.9 | 1.0 | 1.0 | 0.3 |
| bazel test | 6.3 | 6.2 | 6.6 | 0.2 | 6.3 | 6.2 | 5.8 | 0.2 |
| doc test | 46.3 | 1.1 | 1.0 | 0.4 | 33.4 | 1.2 | 1.0 | 0.4 |
| doc ratchet | 48.4 | 4.2 | 4.0 | 0.4 | 34.9 | 4.2 | 3.9 | 0.4 |
| test binary build | 212.4 | 83.1 | 83.4 | 4.5 | 186.2 | 82.7 | 81.8 | 4.6 |
| ↳ schema generation | 123.7 | 27.2 | 27.5 | 3.8 | 45.6 | 27.7 | 27.7 | 3.8 |
| ↳ `.build-binaries` | 88.7 | 55.9 | 55.9 | 0.7 | 140.6 | 55.0 | 54.1 | 0.8 |
| test:smoke | 7.1 | 6.5 | 6.8 | 6.8 | 7.0 | 6.8 | 6.5 | 7.1 |
| test:scoped | 9.9 | 10.0 | 11.1 | 9.8 | 10.0 | 9.8 | 10.3 | 10.1 |
| mark | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| **total wall (s)** | **416.2** | **117.6** | **118.6** | **25.0** | **340.8** | **117.1** | **114.9** | **25.7** |
| exit code | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| host load1 at start / foreign build procs | 3.01 / 0 | 8.18 / 0 | 13.29 / 0 | 10.78 / 0 | 6.27 / 0 | 14.83 / 0 | 11.60 / 0 | 13.09 / 0 |

`load1` at start is the tail of the bench's own previous run: the protocol runs back to back, and every
start had zero foreign `cargo|rustc|pytest` processes. More evidence of isolation: the ambient sccache
server (port 14226, the one every other shell on this host uses) counted **0** compile requests across
every judged run. It counted 10 during L0-w only, which is the warm-up, not judged: foreign work, or
the tail of an earlier one. The bench's own sccache (port 14399) reported `Cache location: Local disk:
"/home/mherwig/.cache/stage0-sccache"` — no S3 tier.

### T1 per lever and the keep/drop decision

| Lever | T1a | T1b | mean | Δ vs L0 | R | W (fresh `target/`) | Decision |
|---|---:|---:|---:|---:|---:|---:|---|
| L0 none | 115.3 | 113.0 | **114.2** | — | 26.4 | 370.3 | baseline (spread 2.3 s) |
| L1 profile, incremental | 89.4 | 87.5 | **88.5** | **−25.7 s (−22.5 %)** | 25.4 | 419.8 | **kept** |
| L2 local sccache | 117.6 | 118.6 | **118.1** | +3.9 s (+3.4 %) | 25.0 | 416.2 | **dropped** |
| L3 profile (incr. off) + sccache | 117.1 | 114.9 | **116.0** | +1.8 s (+1.6 %) | 25.7 | 340.8 | **dropped** |

sccache per T1 run (L2 and L3 alike): 35 compile requests, **0 hits**, 5 misses, 30 non-cacheable
calls, 18 of them `incremental`. Where sccache does pay is a fresh `target/`. L3's warm-up ran on the
cache L2 had filled: 5162 hits, 438 misses, and **340.8 s against L0's 370.3 s (−8 %)**. That is the
new-worktree case, which T1 does not measure. It is recorded here for a later workstream and does not
count toward G-0.

### Bazel action-cache hit rate

Rate = cache hits / (cache hits + executed spawns). Bazel's `N processes` total is not the sum of its
parts on 9.2.0, so N is not the denominator (e.g. `523 processes: 348 disk cache hit, 194 internal, 15
linux-sandbox`).

| Run | Bazel's line | hit rate |
|---|---|---:|
| verify:scoped bazel step, first run on a fresh output base (L0-w) | `1325 processes: 938 disk cache hit, 381 internal, 7 linux-sandbox` | 99.3 % |
| verify:scoped bazel step, T1 (L0-a, L0-b, L1-a, L3-a) | `5 processes: 2 internal, 4 linux-sandbox` | 0 % |
| verify:scoped bazel step, T1 (L1-b, L2-a, L2-b, L3-b) | `5 processes: 1 disk cache hit, 2 internal, 3 linux-sandbox` | 25 % |
| verify:scoped bazel step, unchanged re-run (every R) | no `processes` line; `Executed 0 out of 1 test` | n/a (nothing to run) |
| `task bazel:test:unit` after the edit (A0) | `523 processes: 348 disk cache hit, 194 internal, 15 linux-sandbox`; `Executed 1 out of 34 tests` | 95.9 % |
| `bazel:test:accept` cold, `NOCACHE=1` (A3) | `1089 processes: 908 internal, 181 local`; `Executed 181 out of 181` | 0 % (by construction) |
| `bazel:test:accept` warm re-run (A4) | no `processes` line; `Executed 0 out of 181` | n/a (all 181 served by the test-result cache) |
| `bazel:test:accept` after a docs-only commit (A6) | `363 processes: 182 internal, 181 local`; `Executed 181 out of 181` | 0 % |
| `bazel:test:accept` after a one-crate commit (A7) | `363 processes: 182 internal, 181 local`; `Executed 181 out of 181` | 0 % |

In a T1 run the bazel step recompiles the `ocx_setup` rlib and `ocx_setup_test` and re-runs the test.
None of that can hit a cache, because the source is new. The step costs 6–7 s.

---

## Acceptance and profile runs (lever L0)

Every run in this section exited 0, except where the table says otherwise.

| # | Run | Wall | Exit | Result | Host (load1 / foreign) |
|---|---|---:|---:|---|---|
| A1 | `cargo build --release -p ocx -p ocx_shim --features ocx/__testing --locked --timings`, cold `target/` | 133.4 s | 0 | report `timings/cold.html` | 8.35 / 0 |
| A2 | same, after the reference edit | 56.4 s | 0 | report `timings/after-edit.html` | 8.24 / 0 ¹ |
| A0 | `task bazel:test:unit -- --profile=…` after the edit | 22.2 s | 0 | `Executed 1 out of 34 tests`; profile `profiles/bazel-unit-after-edit.profile.gz` | 6.77 / 0 ¹ |
| A3 | `task bazel:test:accept NOCACHE=1 -- --profile=…` (**cold**) | **1618.9 s** | 0 | `Executed 181 out of 181`; 3884 cases (floor 3884), 156 skipped. Before Bazel starts, 95.7 s go to `test:build` (schema generation 41.0 s, `.build-binaries` 52.7 s): compile is **warm** — a one-crate rebuild on a warm `target/`, because A0's edit had just been reverted. Bazel itself: **1522.9 s**. | 6.49 / 0 |
| A4 | `task bazel:test:accept -- --profile=…` (**warm**, unchanged) | **2.3 s** | 0 | `Executed 0 out of 181`; the reports were read back and the floor held at 3884 | 1.38 / 0 |
| A5 | `task test:parallel --force` (full suite) | **135.1 s** | 0 | `3723 passed, 156 skipped, 5 xfailed in 128.48s` | 1.4 / 0 |
| A5′ | the same, first attempt | 145.6 s | 201 | `1 failed, 3722 passed`. The failure is `test_project_concurrency.py::test_mutation_lock_holder_blocks_other_writers` (`ocx add must exit 75 while another process holds the mutation lock; got 0`), a timing flake under xdist load. It passed serially in A3 and in the A5 re-run. | 1.38 / 0 |
| A6 | docs-only commit (`README.md` + one blank line, `--no-verify`), then `task bazel:test:accept` | 1615.2 s | 0 | **`Executed 181 out of 181`**; `test:build` 59.2 s (`.build-binaries` 58.8 s — `ocx`'s `build.rs` re-ran on the new HEAD; schema generation skipped, `README.md` is not in its `sources:`); Bazel 1555.8 s | 31.23 / 0 ² |
| A7 | one-crate commit (the reference edit), then `task bazel:test:accept` | 1627.4 s | 0 | **`Executed 181 out of 181`**; `test:build` 75.0 s (schema generation 20.8 s, `.build-binaries` 52.3 s); Bazel 1552.2 s | 1.25 / 0 |

¹ The recorded foreign count was 1. That process was the operator's own `until … cargo-timings …`
polling shell: its command text contains the word `cargo`, and it runs no build. This is a
self-matching detector, and the count is corrected to 0.
² `load1` 31 is the tail of A5's 32 xdist workers, which ended the second before. No foreign process was running.

**Reproduce.** Every command above runs from the bench root, with
`TMPDIR=/home/mherwig/.cache/wp01tmp`, under the suite lock, with `RUSTC_WRAPPER` unset. The scripts
are `.tmp/hex-tiers/stage0/run_lever.sh <L0|L1|L2|L3> [phases]` and `run_accept.sh <A…>`.

- **Start the bench's Bazel server outside the suite lock first** (`ocx exec bazel -- bazel info`
  from the bench root). A server first started inside `( flock 9; … ) 9>lock` inherits fd 9 and
  holds the host-wide suite lock for as long as it lives.
- **A re-run needs an unused phase letter.** A reused `rsync-<lever>-<phase>` tag hits the shared
  Bazel disk cache and understates the bazel step by ≈ 6 s. `run_lever.sh` accepts only the phase
  letters `w a b c` and `R`, and silently skips any other letter.

**Review re-run.** The reviewer re-ran one baseline row and one lever row: L0 112.7 s (−1.3 % vs
114.2 s), L1 89.6 s (+1.2 % vs 88.5 s), both within C-001's ±15 %. Raw data:
`/home/mherwig/dev/ocx/.tmp/hex-tiers/stage0-review/`.

### A1 — `cargo build --timings`, cold (the `.build-binaries` shape)

- **Size and parallelism:** 744 units, 133.3 s. Mean 5.18 active units against `jobs = 12`, max 14.
  For **49.3 s**, ≤ 1 unit was active.
- **Critical chain:** `aws-lc-sys` build script (run) 3.6 → 43.2 s, then `aws-lc-rs` → `rustls` →
  `reqwest` → `oci-client` → `ocx_oci` → `ocx_config` → `ocx_store` → `ocx_index` → `ocx_package` →
  `ocx_shell` → `ocx_project` → `ocx_package_manager` → `ocx_setup` → `ocx` lib (78.6–99.5 s) →
  `ocx` bin (99.5–133.3 s).
- **Serial phases:** the start is gated on the 39.6 s `aws-lc-sys` C build. The tail is `ocx` lib +
  `ocx` bin, 54.7 s with nothing else to run beside them.

Top 10 units:

| Unit | Time |
|---|---:|
| `aws-lc-sys` build-script run | 39.6 s |
| `ocx` bin | 33.7 s |
| `zstd-sys` build-script run | 30.9 s |
| `starlark` (frontend 10.0 + codegen 20.4) | 30.4 s |
| `ocx` lib (frontend 9.6 + codegen 11.4) | 21.0 s |
| `prost-reflect` | 13.4 s |
| `ocx_oci` | 13.3 s |
| `ocx_package_manager` | 13.2 s |
| `ocx_sign` | 12.9 s |
| `sigstore` | 11.7 s |

### A2 — `cargo build --timings` after the reference edit (the T1 rebuild)

- **Size:** 56.3 s, 3 units rebuilt: `ocx_setup` 2.9 s → `ocx` lib 20.8 s → `ocx` bin 34.3 s.
- **Parallelism:** mean 1.03 active. For 54.3 of the 56.3 s, ≤ 1 unit was active.
- **Critical path:** the whole run. It is a strictly serial chain, and `jobs = 12` cannot help it.

This is the `.build-binaries` cell of T1, and the reason the profile lever works: incremental codegen
reuses most of the `ocx` lib and bin CGUs.

### A0 — Bazel profile, `bazel:test:unit` after the edit (the post-D3 step)

21.7 s span; critical path 21.07 s = `Compiling Rust rlib ocx_setup` 1.54 s → `Compiling Rust bin
ocx_cli_test (192 files)` **19.53 s** → test (cache check). 523 actions, 78.2 s of action time, mean
concurrency 3.68, 0.1 s idle. Top actions: `ocx_cli_test` 19.5 s, `ocx_cli` rlib 8.4 s, then eight
test binaries at 4–5.6 s each (`help_surface`, `ocx_setup_test`, `borrowed_vocabulary_matches_spec`,
`golden_schemas`, `json_keys_are_snake_case`, `schema_outputs`, `ocx_schema_test`, `ocx_cli_bin_test`).
**Executed 1 of 34.** Every reverse dependent was recompiled, and its test result was still served
from cache: the relinked test binaries came out byte-identical (the changed literal is unreachable
from their code), so the test action's key did not change. The D3 cost is therefore the recompile
of the rdeps (≈ 20 s on the critical path), not re-running their tests.

### A3 — Bazel profile, `bazel:test:accept` cold

- **Size:** 1522.9 s span; 181 test actions, 1528.5 s of action time.
- **Parallelism:** mean concurrency **1.00**, 0.8 s idle. The run is fully serial by design
  (`--local_test_jobs=1` plus the `exclusive` tag), so the critical path is simply the longest single
  target, `//test:test_shell_reconcile` at 101.8 s.
- **Top 10 targets:** 626 s together, 41 % of the run.

| Target | Time |
|---|---:|
| `test_shell_reconcile` | 101.8 s |
| `test_transport_git` | 100.8 s |
| `test_shell_reconcile_edge_cases` | 96.4 s |
| `test_announce` | 75.5 s |
| `test_doc_scripts` | 52.9 s |
| `test_sbom` | 44.5 s |
| `test_sign` | 43.5 s |
| `test_package_claim` | 39.2 s |
| `test_self_setup` | 36.1 s |
| `test_state_providers` | 35.7 s |

- **Against `test:parallel`:** 1522.9 s serial against 135.1 s, a factor of 11.3.

A4's profile (warm) spans 1.4 s: 0.35 s command init, 0.85 s analysis, no action executed.

### A8 — Bazel profile, the scoped arm's bazel step in T1 (L0-a)

`runs/L0-a/bazel-last-command.profile.gz` (its `build_id` equals the T1a invocation's ID
`eb6d9b86-…`, so it is that command's profile and not the later `bazel info`'s). 7.3 s span: 0.68 s
command init, 6.55 s build. Critical path 6.25 s = `Compiling Rust bin ocx_setup_test (12 files)`
4.88 s → `Testing //crates/ocx_setup:ocx_setup_test` 1.35 s. 5 actions (also the `ocx_setup` rlib,
1.28 s, in parallel with the test binary); mean concurrency 1.16, 0.2 s idle.

### Findings outside the lever question (for the speed-up workstream)

1. **Schema generation is 21–22 % of T1** (24.5–27.7 s) under every lever, and no lever touches it.
   `test:build` depends on `website:schema:default`, whose `sources:` glob is every
   `crates/*/src/**/*.rs`. So any Rust edit runs `cargo run -p ocx_schema --release` seven times, and
   the first of them rebuilds the `ocx` lib a second time in `release` without `__testing`. After L1,
   it is the largest single cost left in T1.
2. **A fresh worktree cannot run `verify:scoped`'s bazel step.** `Cargo.bazel.lock.json` is gitignored.
   The scoped arm calls `bazel test //crates/<c>:all` directly rather than through `bazel:bootstrap`,
   so on a new worktree it fails with `Unable to read lockfile` (`runs/finding-fresh-worktree-no-bazel-lock/`).
   `bazel:bootstrap`'s repin then fails too, whenever `TMPDIR` is under `$HOME` (the worker brief's
   rule): cargo-bazel refuses a splice beside `~/.cargo/config.toml`. The bench repinned by hand, as
   the bootstrap task's own error message suggests:
   `bazel shutdown; TMPDIR=/var/tmp/ocx-splice CARGO_BAZEL_REPIN=1 bazel fetch --repo=@crates --repo_env=TMPDIR=/var/tmp/ocx-splice`.
   The server must be restarted for the change to take.
3. **`test_project_concurrency.py::test_mutation_lock_holder_blocks_other_writers` is timing-flaky**
   under `test:parallel` (A5′).

---

## Specify (written before any timed run)

### Host

WSL2 (kernel 6.18.33.2), 32 logical cores, 31 GiB RAM, 32 GiB swap, `/home` on local ext4 (`/dev/sdd`).
cargo/rustc 1.95.0, bazel 9.2.0, go-task 3.53.1, sccache 0.17.0. `.cargo/config.toml` and
`~/.cargo/config.toml` both set `jobs = 12`; `.bazelrc` and `~/.bazelrc` set `build --jobs=12`.
Bazel caches: `--disk_cache=~/.cache/ocx/bazel-disk` plus read-only `--remote_cache=https://bazel-cache.ocx.sh/v1`
(`--remote_upload_local_results=false`), both shared with every other checkout on this host.

**Ambient shell state that the levers override.** This host's login shell exports
`RUSTC_WRAPPER=sccache`, and `~/.config/sccache/config` chains a local disk tier in front of the
shared S3 store (`sccache.ocx.sh`). That is *not* the "none" lever: every bench run sets the wrapper
explicitly (unset for "none"; a disk-only config on its own port for the sccache levers), so no bench
run reads or writes the S3 store (C-001 invariant).

### Reference edit

`crates/ocx_setup/src/lib.rs:802` — the string literal in

```rust
log::info!("managed-config fence is current but the snapshot is absent or mismatched; re-syncing");
```

Production code (the file's `#[cfg(test)]` starts at `:1278`), reached by `ocx self setup`; the literal
is emitted into the binary, so the edit changes codegen (DX-37), not only a comment. No test or doc
matches the text (`grep -rl re-syncing crates test website` → this file only). Each edit replaces
the last word (`re-syncing`) with a 10-character tag `rsync-<lever>-<phase>` (e.g. `rsync-L0-a`), a
string never used before (a re-run must use an unused tag: a reused one is served by the shared Bazel
disk cache and understates the bazel step by ≈ 6 s), so every edit is a real source change for cargo, Bazel and go-task's source
checksums alike. Same length on purpose: the line sits at rustfmt's width, and a longer tag made the
first attempt fail `cargo fmt --check` (kept as `runs/harness-red-fmt/`, EXIT 201 — the harness's
red case).

`ocx_setup` is not a hub (not in `scoped_gate.py --plan`'s `hubs`), not ecosystem, not in
`TABLE_ESCALATES`, so the plan decision is `scoped`, `crates: ["ocx_setup"]` — the pre-D3 arm.

### Bench bookkeeping (why the base is HEAD)

`scoped_gate.py` diffs against the last *full* mark's head, else `merge-base origin/main HEAD`. On
this branch the merge-base diff carries eight `.claude/` files, which would add a `claude:tests`
step no WP pays for its code edit. So after each lever's config is in place (and committed, for the
profile levers), the bench writes a full mark at the bench HEAD by hand —
`python3 scripts/scoped_gate.py --mark full`, run in the bench with `CLAUDE_PROJECT_DIR` unset, so
the mark file is the bench's own `.claude/hooks/.state/commit-verified` and never the shared one.
The changed set of every timed run is then exactly `crates/ocx_setup/src/lib.rs`, the state a WP is
in after its last full verify. Bench commits use `git commit --no-verify` (nothing lands).

### Lever configurations (AM-5: profile and sccache are exclusive where they conflict)

| Lever | Cargo profile of the test binary | `RUSTC_WRAPPER` | How applied |
|---|---|---|---|
| **L0 none** | `release` (as `.build-binaries` today: `cargo build --release -p ocx -p ocx_shim --features ocx/__testing --locked`) | unset (`env -u RUSTC_WRAPPER`) | tree as checked out |
| **L1 profile** | `[profile.test-bin]` `inherits = "release"`, `incremental = true`, `codegen-units = 256`, `debug = 0` | unset | bench commit: the profile appended to `.cargo/config.toml`; `.build-binaries` (`test/taskfile.yml:163,168-169`) builds `--profile test-bin` and copies from `target/test-bin/` |
| **L2 sccache** | `release` | `sccache`, `SCCACHE_CONF=.tmp/hex-tiers/stage0/sccache-local.conf` (one `[cache.disk]`, `dir = ~/.cache/stage0-sccache`, 20 GiB), `SCCACHE_SERVER_PORT=14399`, cache dir empty at start | environment only |
| **L3 profile + sccache** | `[profile.test-bin]` as L1 but `incremental = false` (AM-5: sccache does not cache incremental units) | as L2, cache dir as L2 left it | bench commit + environment |

The bench defines the profile in `.cargo/config.toml` rather than the root `Cargo.toml` so the root
manifest — an input of Bazel's `crates` extension and of `scoped_gate.py`'s escalation table — stays
byte-identical; cargo applies a config-defined `[profile.*]` exactly like a manifest one (checked:
`cargo build --profile test-bin -p ocx_exit` finished under "`test-bin` profile" and wrote
`target/test-bin/incremental/`). WP-09 lands it in `Cargo.toml` (C-021).
`debug = 0` restates the release default (the root manifest has no `[profile.release]`); it is listed
because the ADR names it. `codegen-units = 256` is cargo's incremental default; release's is 16.

### Protocol per lever

1. `rm -rf <bench>/target` (every lever changes a build setting, so each starts from the same cold
   `target/`; Bazel's output base, disk cache and go-task's `.task/` are not touched).
2. Apply the lever (commit or environment) and write the bench full mark at HEAD.
3. **W** — warm-up: edit `-w`, then `task verify:scoped --force`. Cold `target/`; recorded as a
   secondary column (a fresh worktree's first gate), never judged.
4. **T1a** — edit `-a`, then the timed `task verify:scoped --force`. **This is the judged cell.**
5. **R** — the same command again, nothing changed (the unchanged re-run column).
6. **T1b** — edit `-b`, the timed command again: a second sample of T1. The table reports both
   samples; the lever decision uses their mean.

Every run is one command, from the bench root, under the host-wide suite lock:

```sh
cd /home/mherwig/dev/ocx/.agents/worktrees/stage0-bench
export TMPDIR=/home/mherwig/.cache/wp01tmp
( flock 9; /home/mherwig/dev/ocx/.tmp/hex-tiers/stage0/bench.sh <label> \
    env <lever env> ocx exec -- task -d /home/mherwig/dev/ocx/.agents/worktrees/stage0-bench verify:scoped --force
) 9>/home/mherwig/dev/ocx/.agents/acceptance-suite.lock
```

`<lever env>`: L0/L1 `-u RUSTC_WRAPPER`; L2/L3 `RUSTC_WRAPPER=sccache SCCACHE_CONF=… SCCACHE_SERVER_PORT=14399`.

### Per-step timing method (stated once)

`bench.sh` runs the command with stdout+stderr merged into a process substitution (not a pipe: `$?`
is the command's own exit code) that prefixes every line with seconds since start
(`time.monotonic()`). go-task writes `task: [<task>] <cmd>` to stderr, unbuffered, immediately before
it runs each command; a child's buffered output is flushed when it exits, which is before go-task
prints the next line. So a step's wall clock is the stamp of its `task: [` line to the stamp of the
next one (the last step ends at EOF). The time before the first `task: [` line is go-task start-up
plus variable evaluation, which runs `scoped_gate.py --plan` — reported as the **plan** step. The run's
total wall clock is `date +%s.%N` around the whole command. Resolution: ~1 ms; accuracy is bounded by
go-task's own start-up of each nested task (a few ms).

**Host load** is recorded before every run into `runs/<label>/host.txt`: `uptime`, `free -g`,
`pgrep -fa 'cargo|rustc|pytest'` (this script's own processes excluded — a detector must not match its
own invocation), the lever environment, and the RSS of every idle Bazel server. A run with a foreign
build process is marked contaminated.

### Steps (C-001 rows)

git:hooks · plan · fmt (`cargo fmt --all -- --check`) · cargo check (`--workspace --all-targets --locked`) ·
clippy -p ocx_setup · bazel test (`//crates/ocx_setup:all`) · doc test (`cargo test --doc -p ocx_setup`) ·
doc ratchet (`rust:doc:ratchet`) · test binary build (`test:build`: the schema generation it depends on,
then `.build-binaries`) · test:smoke (pytest legs) · test:scoped (pytest over `ocx_setup`'s rows) · mark.

### Acceptance and profile runs (lever L0, after the lever rows)

| # | What | Command (from the bench root, under the suite lock) |
|---|---|---|
| A1 | `cargo build --timings`, cold, the `.build-binaries` shape | `rm -rf target && env -u RUSTC_WRAPPER cargo build --release -p ocx -p ocx_shim --features ocx/__testing --locked --timings` |
| A2 | `cargo build --timings` after the reference edit (the T1 rebuild) | edit, then the same command |
| A3 | `bazel:test:accept` **cold** (results uncached; compile warm), profiled | `task bazel:test:accept NOCACHE=1 -- --profile=<stage0>/bazel-accept-cold.profile.gz` |
| A4 | `bazel:test:accept` **warm** (unchanged re-run) | `task bazel:test:accept` |
| A5 | `task test:parallel --force` (full suite) | `task test:parallel --force` |
| A6 | executed count, docs-only commit | commit a one-line `README.md` change (`--no-verify`), then `task bazel:test:accept` |
| A7 | executed count, one-crate commit | commit the reference edit, then `task bazel:test:accept` |
| A8 | Bazel profile of the scoped unit/build step | copied `command.profile.gz` of the T1a run's `bazel test //crates/ocx_setup:all` (Bazel writes a JSON trace profile per command by default) |

Every `task` above is `env -u RUSTC_WRAPPER ocx exec -- task -d <bench> …`. Bazel profiles are summarised
with `bazel analyze-profile`; cargo timings from the `cargo-timing.html` report. *(As run: both were
parsed by two small scripts beside the raw data, `bzprof.py` — phases, `critical path component`
events, top actions, time-weighted concurrency — and `cargotimings.py` — `UNIT_DATA` /
`CONCURRENCY_DATA`, chain walked back through `unblocked_units` — so no Bazel server ran during a timed
run.)* **Action-cache hit
rate** is read off Bazel's `INFO: N processes: …` line (and `Executed N out of M test targets`) in each
run's log: hit rate = (disk cache hit + remote cache hit) / (N − internal). *(Amended after the first
read: Bazel 9.2.0's N is not the sum of its parts, so the denominator became hits + executed spawns — see
§ Bazel action-cache hit rate.)*

### Decision rules

- A lever is **kept** only if its mean T1 is lower than L0's mean T1 by more than the spread of L0's two
  samples (and by ≥ 5 %); otherwise **dropped**.
- **G-0** (pre-D3 arm): mean T1 with the kept levers ≤ 120 s → GREEN, > 120 s → RED.
