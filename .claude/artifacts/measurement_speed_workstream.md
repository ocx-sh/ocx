# Measurement: speed-up workstream (after `plan_test_speed_tiers.md`)

Branch `hex/test-speed-tiers--speed`, merged into `hex/test-speed-tiers`. ADR: `adr_test_speed_tiers.md` AM-9…AM-12. Commit SHAs below are the rebased commits on `hex/test-speed-tiers`; the runs were taken on their pre-rebase originals.

## Host and stack

- **Host:** 32 cores, 31 GB, WSL2. Every sample records load1 and any foreign cargo/bazel/pytest processes. Numbers from a contaminated run are marked or left out.
- **Stack:** unless a row says otherwise, numbers come from the **shared** `test` compose stack, the same stack as the WP-01 baseline.
- **Private-stack rows:** these ran on a fresh private project (`speedacc`, ports 52xx) while the shared registry was full. Compare them to the baseline with that difference in mind. The project was torn down on 2026-09-24 at 02:26Z.

## Results against WP-01

| Metric | WP-01 baseline | After | Where |
|---|---:|---:|---|
| Cold `bazel:test:accept` (every target executes) | 1618.9 s (WP-12 A3: 1734 s) | **316 s** green, including the `ocx` rebuild. 172/172 targets, 3400 cases. Load1 at start 7.2; the only foreign process was an idle wait script | shared stack, tip 3028d903, 8 jobs |
| … same, earlier runs | — | 283 s (b31996a9, shared; 3445 cases, the floor before the pilot port-down); 372 s / 305 s (1c7b16a5, **private stack**) | |
| Warm `bazel:test:accept`, unchanged tree | 2.3 s | **2 s**; `Executed 1 out of 172`, the `test_windows_shim` residual | shared |
| Action-cache hit rate, warm | — | **1031 / 1032** actions (99.9 %) | shared |
| Targets executed after a `test/taskfile.yml` edit | 172 | **2**: its one reader plus the residual | graph query + run |
| Targets re-keyed per moved input | 172 each | `taskfile.yml` 1; `bench/`, `scenarios/`, `scripts/`, `specs/` 1 each; `recordings/` 6; floors, ceilings and `docker/` 0 | graph query |
| 33-module subset, serial vs 8 jobs | — | **416 s → 119 s** (load1 4.8 / 6.4 at start) | shared |
| `task test:parallel` (reference) | 135.1 s | not re-measured | |
| T1, reference edit (`ocx_setup`) | 114.2 s | 129.7 / 127.1 s on today's tree → **96.1 s** (91.8 / 99.1 s in the schema-gen trial) | shared, quiet window |
| T1, verb edit (`command/install.rs`) | — (projected ≥ 150 s) | 190.6 / 184.8 s → **148.4 s**; 145.9 s in the trial | shared, quiet window |
| The six `patch_global_slot` modules (`test_patches`, `test_frozen`, `test_managed_config`, `test_doc_scripts`, `test_execution_records`, `test_execution_record_standards`), `task test:parallel --force`, back to back | — | pytest 51.2 / 61.4 / 54.2 s with the group → **23.7 / 22.1 s** isolated (task wall 68.2 / 60.5 → 50.3 / 33.1 s); 240 passed, 0 skipped both sides. **Contaminated:** load1 2.6-29 at start, a foreign ocx-sion suite running; judged T1 / cold-accept samples owed to the final measurement pass | shared, A/B alternating |

**Kept cuts:**
- 7ee5207a: per-module inputs.
- b31996a9: concurrent acceptance targets.
- 1c7b16a5: every xdist group gets a slot lock, and the runner's locks are guarded.
- c4961809: the scoped gate's build and unit legs run as concurrent lanes.

**Measured and dropped:**
- **rules_rust `pipelined_compilation`:** `bazel:test:unit` took 26.0 / 24.1 s without it and 22.1 / 23.4 s with it. The Bazel lane is off T1's critical path after c4961809, so the gain doesn't reach T1.
- **A dev-profile or own-target-dir `ocx_schema` build:** no measurable T1 win. The schema step shrinks, but `.build-binaries` grows by the same amount under the concurrent lanes.

**Where T1 still misses:** the verb edit misses 120 s because of the serial `patch_global_slot` xdist group. It accounts for 53 of the 56 s of scoped pytest. See `research_patch_global_slot_isolation.md`. *Refine pass:* each patch test now has its own patch registry path (`<registry>/p<uuid4>` in `test_patches.py`), so the modules no longer serialise; `module_slots` names the group on `test_patches.py` alone. The group keeps two tests there: `test_global_descriptor_publishes_at_the_bare_registry_root`, the one deliberate `--global` writer of the bare host's `global` slot, and `test_patch_publish_without_config_errors`, which has no tier to scope. The verb-edit T1 is to be re-measured.

## Final pass (2026-09-24, contaminated host)

Taken on the branch tip before the finalize rebase (`1b06361c`, same tree content as the rebased series). Host load1 4–16 throughout, with foreign cargo, rustc and Bazel builds from other agents running beside it, so these numbers are upper bounds, not a clean read.

| Metric | WP-01 baseline | Final pass |
|---|---:|---:|
| T1, reference edit (`ocx_setup`), steady-state sample | 114.2 s | **104.9 s** |
| T1, verb edit (`command/install.rs`), steady-state sample | — | **188.7 s** |
| Cold `bazel:test:accept` (`NOCACHE=1`, every target executes) | 1618.9 s | **388.2 s**, 172/172 targets, 3401 cases |
| Warm `bazel:test:accept`, unchanged tree | 2.3 s | **75.4 s** at **99.96 %** action-cache hits (2659 hits, 1 local action) |

The first sample of each T1 kind (409.0 s reference, 258.2 s verb) ran on the fresh worktree's cold per-worktree caches and is not counted. The warm wall clock is the outlier: the cache hit rate matches the earlier 99.9 %, and the time went to host contention, not to re-executed targets. The owner re-takes a clean T1 pair (reference and verb edit) on a quiet host after finalize.

## Deferred

- **D6 remote cache writer for acceptance results:** not built (AM-12). CI tests the `--release` artifact it downloads (`SKIP_BUILD` defaults to `CI`, so `bazel:test:accept` rebuilds nothing there) and local runs build `--profile test-bin`, so their digests never match. The D6 preconditions are also unmet. Which binary CI should test is open: [ocx-sh/ocx#518](https://github.com/ocx-sh/ocx/issues/518).
- **`test_windows_shim` residual:** it costs 0.85 s per run. The fix is a two-line hunk that `scripts/test_diff_guard.py` refuses by design.
- **Unlocked registry re-probe:** review finding W2 — closed in the refine pass: `_recycle_registry` re-checks `registry_accepts_writes` under `_COMPOSE_LOCK` before its `rm` and its `up` (four allow-listed one-line re-points in `test/src/helpers.py`).
