# Measurement: tiered-verification exit (WP-12 / C-026)

- **Work package:** WP-12 of [`plan_test_speed_tiers.md`](./plan_test_speed_tiers.md) — C-026, S-001, S-011, S-012, S-016, P-8, OQ2
- **Judged against:** [`adr_test_speed_tiers.md`](./adr_test_speed_tiers.md) § Quantified Impact (this file is its "after" column), OQ2, AM-4
- **Baseline it is compared with:** [`measurement_test_tiers_stage0.md`](./measurement_test_tiers_stage0.md) (WP-01), same method
- **Date:** 2026-09-23
- **Bench tree:** throwaway detached worktree `.agents/worktrees/exit-bench` at `d631b95d` (the merged
  `hex/test-speed-tiers`, every WP through WP-10), submodules initialised, `task schema` and
  `task bazel:bootstrap` run once, and its Bazel server started outside the suite lock. Bench commits
  used `--no-verify`, and nothing from them lands. A second throwaway worktree, `oq2-bench`, was used
  for the OQ2 rebuilds. Both were removed afterwards.
- **Raw data (not committed):** `/home/mherwig/dev/ocx/.tmp/hex-tiers/exit/`:
  - `runs/<label>/`: `host.txt`, the time-stamped `log.txt`, `result.txt`, `bin-sha256.txt`, and on the
    last T1 run also `foreign.txt`;
  - `profiles/`, `timings/`, `oq2.tsv`, `share.log`;
  - the scripts `run_accept.sh`, `run_t1.sh`, `oq2.sh`, `share_exit.py`, `foreign.sh`.

---

## VERDICT

> **Graph targets: met, except for one named residual.** The acceptance binary is byte-identical across
> a docs-only commit and a clean → dirty tree. The only target that re-executes on those changes and
> on an unchanged re-run is `//test:test_windows_shim`, the one module left on `UNCACHED_MODULES`
> (WP-08b divergence). Editing `test/tests/test_install.py` executes 1 target plus that residual.
>
> **Lead item for the speed-up workstream — OQ2's letter and its purpose disagree.** By the ADR's
> letter OQ2 is decided: 8 of 12 WP merges left `test/bin/ocx` unchanged (67 %; 8 of 10 = 80 % after
> D1), which is ≥ ½, so the WP-merge T2 stays `bazel:test:accept`. By its purpose (avoid the cold
> serial path) it goes the other way: only **5 of 11** merges were actually cache-served (45 %). Shared
> suite inputs — `test/taskfile.yml`, `test/pyproject.toml`, the test BUILD files — re-key all targets,
> so WP-06, WP-07 and WP-08b ran the whole suite on an unchanged binary. Narrowing `:suite_inputs` is
> the first thing that workstream should take on.
>
> **T1: missed as measured, on a contaminated host.** From 18:40 local onwards a foreign session
> (`ocx-sion/.agents/worktrees/wp1`, `wp3`) ran cargo release builds, a Bazel build and pytest on this
> host. A 10-minute quiet wait never found a quiet minute. Every judged T1 sample is therefore marked
> contaminated:
> - reference edit: 218.9 s and 147.4 s;
> - verb edit: 241.5, 230.2 and 252.9 s.
>
> **G-0: the last clean evidence is WP-06's 109.9 s with the landed lever (GREEN). The exit re-measure
> is contaminated, and a host-exclusive re-measure is owed to the speed-up workstream.** The exit
> samples neither confirm nor refute 109.9 s.
>
> The verb edit is **projected, not measured clean**, to miss 120 s. Its scoped pytest took 71–83 s
> (pytest's own figure; the whole step took 80–92 s). A clean anchor bounds the load inflation at
> ≤ 1.3×: the reference edit's scoped pytest takes 6.1–7.5 s clean (WP-06) against 8.1 s here. So the
> verb edit's pytest is ≥ 55 s clean. Add WP-06's ≈ 95 s of clean non-test steps, and the verb edit
> projects to **≥ 150 s**.
>
> **Lint tier: met** — 11.4 s and 14.8 s against 30 s, even under load. **Escalation share:** 39/50
> (78 %), and the same without AM-3. **OQ2:** decided by the ADR's letter — keep `bazel:test:accept`
> at WP merges (see the lead item above for the conflict).

---

## ADR Quantified Impact — the "after" column

| Metric | Before (ADR / WP-01) | After (target) | **After (measured)** | Verdict |
|---|---|---|---|---|
| Acceptance targets executed locally, warm cache, docs-only commit | 181 | 0 | **1 of 172** — the residual `test_windows_shim`; 0 others (A6) | **missed by the named residual** |
| … clean → dirty tree, no code change | 181 | 0 | **1 of 172** — the residual; 0 others (A6d) | **missed by the named residual** |
| … unchanged tree re-run | 0 | 0 | **1 of 172** — the residual; 0 others (A4) | **missed by the named residual** (a regression from 0; the cost is 0.85 s) |
| … in CI | 181 | 181 (D6) | Not measurable without a CI run. Static evidence: `verify-deep.yml:440` runs `task bazel:test:accept` on a fresh runner, with the cache credential read-only (`:406-415`) and no acceptance writer (D6). Nothing can serve a verdict there, so every target executes. | not measured (static evidence only) |
| … `test/tests/test_x.py` edit | 5 | 1 + every uncached module | **2 of 172** = `test_install` + the residual (A7) | **met per the ADR wording** (1 + the uncached list); **missed per C-026/S-016** (target 1 with an empty list, and the `test_windows_shim` residual remains) |
| `test/bin/ocx*` sha256 across docs-only / clean → dirty | differs | equal | **equal**: `ocx` 479ff16a…, `ocx-shim` d57e1f7f…, the same across A3 → A7. The red side is reachable: every T1 edit changed `ocx` (3c2b6bc7…, 523de245…, …). | **met** |
| T1, reference edit (`ocx_setup/src/lib.rs:802`), first `verify:scoped --force` | WP-01 88.5 s (pre-D3 arm); WP-06 109.9 s (new arm) | ≤ 120 s | **218.9 / 147.4 s — contaminated** | **missed as measured; not judgeable** |
| T1, verb edit (`ocx_cli/src/command/install.rs:44`) | — | ≤ 120 s | **241.5 / 230.2 / 252.9 s — contaminated**; projected clean ≥ 150 s (VERDICT) | **missed as measured; projected, not measured clean, to miss** |
| Lint tier wall clock (`test:lint:structure`) | — | ≤ 30 s | **14.8 / 11.4 s** standalone; 11–17 s inside the T1 runs | **met** |
| CI minutes per PR push | ≈ 18 | ≈ 18 | Not measurable without a CI run. The basic tier was not touched by this plan. | not measured |

---

## Before / after against WP-01's exact numbers

| Row | WP-01 (before) | Exit (after) | Host at exit run (load1 / swap used / foreign processes at start) |
|---|---:|---:|---|
| T1 reference edit, kept lever: sample a / b / mean | 89.4 / 87.5 / **88.5 s** (pre-D3 arm) | 218.9 / 147.4 / **183.2 s** (D3 arm) | contaminated (23.0 / 27 GB / 12 and 33.2 / 27 GB / 3) |
| T1 unchanged re-run (R) | 25.4 s | 47.9 s | contaminated (load1 25) |
| T1 warm-up on a cold dev `target/` (W) | 419.8 s | 382.3 s | 16.9 / 27 GB / 0 — not a quiet host |
| Bazel hit rate, verify:scoped bazel step (T1) | 0–25 % (`5 processes: 2 internal, 4 linux-sandbox`) | **72.9 %** (`17 processes: 43 action cache hit, 2 internal, 16 linux-sandbox`, `Executed 1 out of 34`) | — |
| Bazel hit rate, the same step, verb edit | — | 60 % (`32 processes: 25 action cache hit, 8 disk cache hit, 11 internal, 22 linux-sandbox`, `Executed 9 out of 34`) | — |
| Bazel hit rate, `bazel:test:accept` cold | 0 % (`1089 processes: 908 internal, 181 local`) | build actions **99.7 %** = 1189/1192; 87.2 % over all non-internal actions (`2661 processes: 1189 disk cache hit, 1297 internal, 3 linux-sandbox, 172 local`); test actions 0 % by construction | — |
| Bazel hit rate, `bazel:test:accept` warm | n/a (no `processes` line, 0/181) | **99.9 %** (`2 processes: 1032 action cache hit, 1 internal, 1 local`, `Executed 1 out of 172` — the residual) | — |
| `bazel:test:accept` **cold** (`NOCACHE=1`), wall clock | **1618.9 s**: 181 targets, 3884 cases, Bazel 1522.9 s, compile warm | **1734.3 s**: 172 targets, 3445 cases, Bazel 1584.6 s. The ~147 s before Bazel includes a cold `release` schema compile, because C1 had wiped `target/`. | 11.3 / 20 GB / 0 at start; 13 foreign processes at its end. **Not like-for-like** (172 targets and 3445 cases against 181 and 3884, plus ≈ 147 s of cold schema compile): no speed verdict |
| `bazel:test:accept` **warm**, unchanged | **2.3 s**, 0/181 | **8.1 s**, 1/172 (the residual) | contaminated (13) |
| `task test:parallel --force`, full suite | **135.1 s** (3723 passed) | **200.0 s** (3284 passed, 156 skipped, 5 xfailed) | **contaminated** (10) — not a speed comparison |
| `cargo build --timings`, test binary, cold `target/` | 133.4 s (`--release`) | 167.1 s (`--profile test-bin`) | 15.4 / 19 GB / 0 — not a quiet host |
| The same, after the reference edit | 56.4 s | **29.6 s** | 17.0 / 20 GB / 0 — not a quiet host |

The cold and after-edit timings trade exactly as WP-01 predicted for `incremental = true`: the cold
build is slower (+34 s here, +61 s in WP-01) and the after-edit build is about halved.

### T1 per step (seconds; C-001 timing method)

| Step | WP-01 L1 mean (pre-D3) | WP-06 L1 mean (D3 arm) | exit ref-a | exit ref-b | exit R | exit verb-va | verb-vb | verb-vc |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| plan + git:hooks | 0.5 | 0.9 | 1.9 | 1.1 | 1.1 | 1.1 | 1.1 | 2.9 |
| lint tier | — | 10.5 | 17.3 | 12.2 | 12.1 | 13.5 | 12.2 | 12.5 |
| rows check | — | 1.3 | 2.7 | 1.8 | 1.3 | 1.6 | 1.4 | 1.5 |
| fmt + cargo check + clippy -p | 5.3 | 5.0 | 10.1 | 6.6 | 2.9 | 22.9 | 10.5 | 10.5 |
| bazel test (`bazel:test:unit`) | 6.0 | 19.8 | 33.8 | 25.5 | 1.9 | 38.6 | 24.0 | 40.2 |
| doc test + doc ratchet | 4.9 | 5.0 | 14.1 | 9.7 | 2.9 | 14.9 | 17.7 | 15.3 |
| test binary build | 54.9 | 52.5 | 90.1 | 69.1 | 5.5 | 59.6 | 65.0 | 67.7 |
| ↳ schema generation | 24.8 | — | 37.0 | 30.2 | — | 24.7 | 26.3 | 29.6 |
| ↳ `.build-binaries` and the rest | 30.1 | — | 53.1 | 38.8 | — | 34.8 | 38.7 | 38.2 |
| test:smoke + test:scoped | 16.9 | 15.1 | 47.6 | 20.0 | 19.2 | 88.0 | 97.0 | 101.0 |
| mark | 0.0 | — | 1.1 | 1.5 | 0.9 | 1.2 | 1.3 | 1.1 |
| **total wall** | **88.5** | **109.9** | **218.9** | **147.4** | **47.9** | **241.5** | **230.2** | **252.9** |
| host (load1 / foreign processes at start) | clean | clean | 23.0 / 12 | 33.2 / 3 | 25.1 / 2 | 25.4 / 0 ¹ | 21.4 / 4 | 16.6 / 3 ² |

Every run exited 0. The plan printed `scoped ['ocx_setup']` with 3 globs for the reference edit, and
`scoped ['ocx']` with 18 globs for the verb edit.

¹ Zero foreign processes at the instant of the start sample, with load1 still 25. The same foreign
session was active one sample before and one after.
² `foreign.txt` for this run: 83 samples at a 3 s interval, a foreign process present in **83/83**,
mean 7.2 foreign processes, mean load1 19.7.

**What the contaminated samples still indicate:**
- The verb edit's `test:scoped` step takes 80–92 s over `install`'s 18 marked modules; pytest itself
  reports 71.2 / 82.8 / 78.6 s for 584 tests. Scaled by the ≤ 1.3× inflation bound (VERDICT), that is
  ≥ 55 s clean, and ≥ 150 s for the whole run. This is **projected, not measured clean**.
- `bazel:test:unit` costs 20–40 s under D3 against 6 s pre-D3, as WP-06 found. Its hit rate is now
  60–73 %: the reverse dependents recompile, and only the changed crate's tests execute.

---

## C-026 rows in detail

### Executed acceptance count (S-011, S-012, S-016)

Every run is `env -u RUSTC_WRAPPER ocx exec -- task -d <bench> bazel:test:accept`, under the suite lock.
Each run exited 0 and reported `3445 cases (floor 3445)`.

| Run | Change | `Executed` line | Targets executed | `test/bin/ocx` / `ocx-shim` sha256 (first 16 chars) | Wall |
|---|---|---|---|---:|---:|
| A3 | none (cold, `NOCACHE=1`) | `Executed 172 out of 172` | all | 479ff16a9f0f984f / d57e1f7f41a668e1 | 1734.3 s |
| A4 | unchanged re-run | `Executed 1 out of 172` | `//test:test_windows_shim` | 479ff16a9f0f984f / d57e1f7f41a668e1 | 8.1 s |
| A6 | docs-only commit (`README.md` + one blank line) | `Executed 1 out of 172` | `//test:test_windows_shim` | 479ff16a9f0f984f / d57e1f7f41a668e1 | 32.4 s ³ |
| A6d | README.md edited, uncommitted (dirty) | `Executed 1 out of 172` | `//test:test_windows_shim` | 479ff16a9f0f984f / d57e1f7f41a668e1 | 4.1 s |
| A7 | `test/tests/test_install.py` + one comment line, uncommitted | `Executed 2 out of 172` | `//test:test_install`, `//test:test_windows_shim` | 479ff16a9f0f984f / d57e1f7f41a668e1 | 5.4 s |

Report form requested: docs-only **1 executed, of which residual 1**; clean → dirty **1, residual 1**;
unchanged **1, residual 1**; `test_install.py` edit **2 = 1 + residual 1**. The four sibling-reading
sweepers of CLAUDE.md § Bazel no longer re-run: they moved to the lint tier (WP-05).

³ The docs-only commit still costs 25 s in `test:build`. `cargo build --release -p ocx_schema`
recompiles the `ocx` lib without `__testing`, and the real-provenance `build.rs` path re-runs on a new
HEAD. The acceptance targets do not depend on it, so it adds no executions — but it does add wall
clock to every T1/T2 after a commit (see Findings).

### Escalation share, last 50 `crates/ocx_cli/` commits on `main`

Method: `share_exit.py` calls the real `scoped_gate.py --plan` entry point (`main(["--plan"])`) once
per commit. The only thing replaced is `changed_paths`, which is swapped for that commit's synthetic
change list (first parent, `--no-renames`). Table and markers are the bench's (`d631b95d`); `origin/main`
is `15946973`, and the window runs `2eb66016..63c1b71f`. The script checks that exactly 50 commits were
read and that the variant really drops all seven AM-3 rows.

| Change list | Live table | Without AM-3 rows |
|---|---:|---:|
| the commit's `crates/ocx_cli/**` paths (WP-06's scope) | **39/50 (78 %)** | **39/50 (78 %)** |
| the commit's whole path list | 48/50 (96 %) | 48/50 (96 %) |

The leading escalation reason is "CLI code outside command/" (34 commits). WP-06's 39/50 is reproduced.

The AM-3 rows change no decision, for a structural reason. With their `[security]` rows removed, each
of the seven AM-3 files still escalates, now as "no acceptance module carries a `command` marker for
'<verb>'", because security verbs carry no marker (checked per file). The "without AM-3" share would
only drop if those verbs were given markers.

### OQ2 — WP merges that left `test/bin/ocx` unchanged

Method (**rebuild**, not log inference): `oq2.sh` walks the plan base (`563d3732`) and every WP merge
on `hex/test-speed-tiers`'s first-parent chain, in a throwaway worktree with submodules. At each one it
runs that tree's own `.build-binaries` cargo line (`--release` before WP-09, `--profile test-bin`
from WP-09 on) and hashes `ocx` and `ocx-shim`. A merge counts as unchanged when its hash equals its
first parent's.

| Merge | SHA | profile | `ocx` | changed? | why |
|---|---|---|---|---|---|
| base | 563d3732 | release | da6fbcd8… | — | |
| WP-01 | 992804e8 | release | fc10b6f4… | **changed** | pre-D1: the git SHA is baked in, and no crate changed |
| WP-03 | cb150ea4 | release | ab1a29db… | **changed** | `crates/ocx_cli/build.rs` (D1 lands) |
| WP-04 | 63345dfc | release | ab1a29db… | unchanged | |
| WP-02 | a0584af7 | release | ab1a29db… | unchanged | |
| WP-05 | 41e5d2c3 | release | 2553110b… | **changed** | `command/deprecated.rs` |
| WP-06 | 69fdd4eb | release | 2553110b… | unchanged | |
| WP-07 | 07ad0602 | release | 2553110b… | unchanged | |
| WP-08a | efe832a7 | release | 2553110b… | unchanged | |
| WP-08b | ed23bd1a | release | 2553110b… | unchanged | |
| WP-09 | fad8312e | test-bin | 586a3716… | **changed** | profile switch |
| WP-11 | 83d1664b | test-bin | 586a3716… | unchanged | |
| WP-10 | d631b95d | test-bin | 586a3716… | unchanged | |

**Unchanged: 8/12 (67 %); after D1 (WP-04 onward): 8/10 (80 %).** Both are ≥ ½, so under OQ2's rule
the WP-merge T2 stays `bazel:test:accept`.

The merge-verify logs (`.tmp/hex-tiers/merge-*.log`) corroborate the binary side. Where the binary was
unchanged, T2 executed 0/181 (WP-04, WP-02), 23/173 (WP-08a, the uncached list then), 2/172 (WP-11) and
1/172 (WP-10). **But WP-06, WP-07 and WP-08b re-executed the full suite with an unchanged binary.**
Each edited a `:suite_inputs` file:
- WP-06: `test/taskfile.yml`, `test/pyproject.toml`;
- WP-07: `test/taskfile.yml`;
- WP-08b: `test/BUILD.bazel`, `test/bazel.bzl`, among others.

So a cache-served T2 happened at only 5 of the 11 merges whose logs carry an acceptance run (45 %).
OQ2 as the ADR words it is decided on the binary share (8/12, so `bazel:test:accept` stays). This
second number is the one OQ2's purpose turns on. It is the VERDICT's lead item for the speed-up
workstream.

---

## Profiles (raw files under `.tmp/hex-tiers/exit/`)

**`bazel --profile` of A3, the cold `bazel:test:accept`** (`profiles/bazel-accept-cold.profile.gz`,
summary `profiles/bazel-accept-cold.summary.txt`):
- **Span:** 1584.6 s. 809 actions and 1593.7 s of action time; the 172 test actions account for 1563.2 s.
  The first test starts at 20.9 s, after 3.8 s of repository fetch on a fresh output base.
- **Parallelism:** mean concurrency **1.01**, 9.5 s idle. The run is serial by design
  (`--local_test_jobs=1`, `exclusive`).
- **Critical path:** 105.73 s = `Testing //test:test_shell_reconcile`.
- **Top 10 (s):**

  | Target | Time |
  |---|---:|
  | `test_shell_reconcile` | 105.6 |
  | `test_transport_git` | 101.9 |
  | `test_shell_reconcile_edge_cases` | 97.8 |
  | `test_announce` | 77.4 |
  | `test_doc_scripts` | 53.6 |
  | `test_sbom` | 44.2 |
  | `test_sign` | 43.5 |
  | `test_self_setup` | 39.4 |
  | `test_package_claim` | 39.3 |
  | `test_state_providers` | 37.7 |

  Same set and order of magnitude as WP-01's A3; the top 10 are 41 % of the run.
- The warm profile (A4) spans 3.6 s. Its critical path is the residual's test action, 0.85 s.

**`cargo build --timings` of the test binary** (`timings/cold.html`, `timings/after-edit.html`,
summaries `*.summary.txt`):
- **Cold (C1):**
  - **Size and parallelism:** 744 units, 167.1 s. Mean 5.30 active units, max 14; ≤ 1 unit was active
    for 52.8 s.
  - **Critical chain:** `aws-lc-sys` build script 5.3 → 53.0 s, then `aws-lc-rs` → `rustls` → `reqwest`
    → `oci-client` → `ocx_oci` → `ocx_config` → `ocx_store` → `ocx_index` → `ocx_package` → `ocx_shell`
    → `ocx_project` → `ocx_package_manager` → `ocx_setup` → `ocx` lib (110.2–134.2 s) → `ocx` bin
    (134.2–167.1 s).
  - **Top units (s):**

    | Unit | Time |
    |---|---:|
    | `aws-lc-sys` build-script run | 47.6 |
    | `starlark` | 45.7 |
    | `zstd-sys` build-script run | 36.2 |
    | `ocx` bin | 32.9 |
    | `ocx_sign` | 26.1 |
    | `ocx` lib | 24.0 |
    | `ocx_oci` | 21.8 |
    | `ocx_config` | 18.4 |
    | `sigstore` | 17.9 |
    | `prost-reflect` | 16.8 |

    Codegen is the larger part of each under `codegen-units = 256`.
- **After the reference edit (C2):**
  - 29.6 s, 3 units rebuilt: `ocx_setup` 0.5 s → `ocx` lib 2.4 s → `ocx` bin **25.9 s**.
  - Mean 0.98 active, and 29.4 s serial.
  - Incremental reuse took the lib from 20.8 s down to 2.4 s. The `ocx` bin unit is now 87 % of the
    rebuild.

---

## Findings for the speed-up workstream

1. **T1 needs a host-exclusive re-measure.** Every judged sample here is contaminated by a foreign
   session. The plan's T1 claim therefore rests on WP-06's 109.9 s and on WP-09's single 118.8 s
   sanity sample, taken at load1 11.3. The margin to 120 s is thin either way.
2. **Verb edits miss 120 s on `test:scoped`.** `install` selects 18 modules, including the doc-script
   and state-provider harness modules that carry the doc-script verb set (WP-06 divergence). That
   step takes 80–92 s under load (pytest 71–83 s), and ≥ 55 s projected clean, putting the verb edit at
   ≥ 150 s projected (not measured clean). Candidates: trim the harness modules' verb set, or the `--lf`/`--nf`
   follow-up.
3. **The schema generation is still 25–37 s of every T1** (WP-01 finding 1, unchanged). A docs-only
   commit also pays 25 s of it in `test:build`, because the non-`__testing` `ocx` lib re-runs its
   real-provenance `build.rs` on a new HEAD.
4. **`:suite_inputs` edits defeat the binary-level cache.** `test/taskfile.yml` and
   `test/pyproject.toml` are in it, so 3 of the 8 unchanged-binary merges still re-ran all 172 targets.
5. **The `test_windows_shim` residual** turns the unchanged re-run from 0 to 1 target: 0.85 s, plus a
   Bazel invocation that can no longer be a pure no-op. Empty the list — the owner-deferred guard
   exception — and three ADR rows reach their target of 0.

## Reproduce

Every run was made from the bench root with `TMPDIR=/home/mherwig/.cache/wp12tmp`, `RUSTC_WRAPPER`
unset and `CLAUDE_PROJECT_DIR` unset (so the mark file is the bench's own), under
`flock .agents/acceptance-suite.lock`.
- **Harness:** `bench.sh`, `stamp.py` and `steps.py` are copied unchanged from WP-01's `stage0/` and
  WP-06's `wp06-t1/` (the per-step timing method is WP-01 § Per-step timing method). The drivers are
  `run_accept.sh C1 C2 A3 A4 A5 A6 A6d A7` and `run_t1.sh setup w a R b vw va vb L1 L2`, followed by
  `QUIET=1 run_t1.sh vc …`.
- **T1 protocol (as WP-01):**
  - The base is a hand-written full mark at the bench HEAD `d631b95d`, and the landed
    `[profile.test-bin]` is in the tree. A warm-up edit (`w`, `vw`) precedes each series and is not
    judged.
  - Each edit uses a new tag of the literal's width: `x12-T1-<p>xx` at `ocx_setup/src/lib.rs:802`,
    `v12-<p>xx` at `ocx_cli/src/command/install.rs:44`.
  - The `vc` run is the only one with a 3-second foreign-process sampler (`foreign.sh`). Its quiet gate
    (60 s without any foreign cargo/rustc/pytest/sandbox process, at most 10 min) never opened.
- **Side effect:** the bench's `verify:scoped` runs appended records to the shared
  `ocx-gate/scoped_runs.jsonl`, with worktree `exit-bench`. Their trees carry bench-only tags, so no
  merge tree can ever match them.
