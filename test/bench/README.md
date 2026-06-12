# Benchmark Harness

Permanent local benchmark harness for `ocx install` performance regression tracking.
Runs against a locally-hosted [registry:2][registry2] with [toxiproxy][toxiproxy] for
network throttling. Comparison gate exits 1 when any scenario regresses beyond the 0.85x
threshold versus the committed baseline.

Not part of the pytest acceptance suite — the harness is standalone script-driven.
See [Smoke validation](#smoke-validation) for the lightweight pytest integration.

## Prerequisites {#prerequisites}

- **Docker** — registry:2 and toxiproxy run as compose services
- **[hyperfine][hyperfine]** — acquired automatically via `.bench:acquire-hyperfine`
  (requires `gh` CLI if not already on `PATH` or in `test/bin/`)
- **[uv][uv]** — Python environment manager (present via project toolchain)
- **[gh][gh]** — GitHub CLI, needed only if hyperfine is not already installed

All bench tasks call through `task` (Taskfile v3). The `test:` prefix is the namespace
from the root taskfile include.

## Running the Benchmark {#running}

### Setup {#running-setup}

```bash
task test:bench:setup
```

Starts the `bench` Docker Compose profile: toxiproxy on `:5002` (proxy) / `:8474` (API)
and the shared registry on `:5000`. Waits for both services to be reachable before
returning.

The `bench` profile is isolated from `task test` — the standard acceptance test run
never starts toxiproxy.

### Quick mode — fast iteration {#running-quick}

```bash
task test:bench:quick
```

Runs the **small suite** (3 scenarios: unthrottled + 100 Mbps + parallel\_2\_processes).
Target: under 1 minute wall-clock including fixture setup. Fixtures use deterministic
repo names so a second quick run skips pushing (manifests probed via HEAD request).
Compares against baseline where entries exist; absent-from-baseline scenarios reported
as ungated (not a failure). Useful for rapid iteration during development.

### Single-scenario trigger {#running-scenario}

```bash
task test:bench:scenario -- ocx_install_100mbps
task test:bench:scenario -- ocx_install_unthrottled ocx_parallel_2_processes
```

Pass scenario names after `--`. Runs with 3 runs and no warmup. Scenario names must
match entries in `scenarios.py`; use `task test:bench:scenario -- --help` to see the
full list in the error message if needed.

### Full benchmark run — medium suite {#running-bench}

```bash
task test:bench
```

Runs the **medium suite** (17 scenarios) against the current binary, saves results to
`test/bench/results/latest-medium.json` (and `latest.json` for backward compat),
then compares against `test/bench/baseline.json`. Exits 0 if all gated scenarios pass
the threshold; exits 1 if any scenario regresses. Target: under 4 minutes wall-clock.

### Extended run — large suite {#running-large}

```bash
task test:bench:large
```

Runs the **large suite** (all 21 scenarios). The 4 extra large-suite rows use `runs=10`
and larger payloads (50 MB at 100 Mbps; 25-28 MB layer-scaling rows) to produce tighter
per-scenario σ. Run before landing an optimization to get statistically confident
before/after comparisons. Target: under 10 minutes wall-clock. See
[Suite model](#suites) for when to use each tier.

### Baseline capture {#running-baseline}

```bash
task test:bench:baseline
```

Same matrix run, but saves results as `test/bench/baseline.json` (committed to the
repository). Run this before landing any optimization — never after. See
[Baseline update protocol](#baseline-protocol) for when to re-capture.

### Dashboard report {#running-report}

```bash
task test:bench:report
```

Generates `test/bench/results/index.html` — a self-contained single-file HTML
dashboard with no external dependencies (Vue 3 is inlined). Does **not** require
Docker or hyperfine; reads from `results/latest.json`. Open the printed path in a
browser to view the dashboard.

### Teardown {#running-teardown}

```bash
task test:bench:teardown
```

Stops the toxiproxy container only. The shared registry keeps running so
`task test` is unaffected. Use `docker compose down` manually to stop everything.

### Registry data hygiene {#running-registry-gc}

Bench fixtures use deterministic repo names (`bench-<size>mb-<layers>l-<idx>`) so they
persist in the registry across sessions. This is intentional — it speeds up repeat
`bench:quick` runs by skipping fixture push when manifests are already present.

**There is no automatic GC.** If the registry accumulates stale data (e.g. after
changing payload sizes or layer counts), clean it manually:

```bash
# Nuclear option: wipe all registry data and restart.
docker compose -f test/docker-compose.yml down -v
task test:bench:setup
```

The `bench-` prefix isolates bench fixtures from acceptance-test repos; removing the
registry volume also removes acceptance-test data. Run `task test` after restart to
re-push acceptance fixtures.

## Suite Model {#suites}

Three cumulative suites. Suite selection is **cumulative**: large ⊇ medium ⊇ small.

| Suite | Task | Target | Rows | Est. scenario time | When to use |
|-------|------|--------|------|--------------------|-------------|
| **small** | `bench:quick` | < 1 min wall | 3 | ~8.5 s | Every dev iteration; fast fixture reuse |
| **medium** | `bench` | < 4 min wall | 17 | ~75.6 s | Default regression gate before merge |
| **large** | `bench:large` | < 10 min wall | 21 | ~182.6 s | Before landing optimizations (tighter σ) |

The 4 large-suite rows use `runs=10` + larger payloads (50 MB and 25–28 MB layer rows)
to reduce per-scenario σ and give statistically confident before/after comparisons.
They are omitted from the medium gate to keep daily CI under 4 minutes.

**Trade-off: deterministic fixture names.** Bench fixtures are named
`bench-<size>mb-<layers>l-<idx>` (no UUID). A second run reuses existing registry
fixtures (manifest HEAD probe). This means fixtures from previous sessions persist in
the registry; they are isolated from acceptance-test repos by the `bench-` prefix.

## Scenario Matrix {#scenarios}

Twenty-one scenarios (matrix v3) spanning bandwidth profiles, payload sizes, suites,
shapes, and warm/cold OCX state. "Cold" means a fresh `OCX_HOME` before each
[hyperfine][hyperfine] run. "Warm" means the index is pre-populated but no layer blobs
are cached. All scenarios use `warmup=0` (v3 budget rule).

**Duration budgets:** small <60 s, medium ≤200 s, large ≤550 s (scenario time only;
~40-50 s headroom for fixture push + proxy setup + hyperfine startup).

### Medium suite (17 rows) {#scenarios-medium}

| Scenario | Suite | Bandwidth | Payload | Layers | Runs | Est. (s) | Shape | Cold | Purpose |
|----------|-------|-----------|---------|--------|------|----------|-------|------|---------|
| `baseline_curl_10mbps` | medium | 10 Mbps | 5 MB | 1 | 3 | 13.3 | floor | yes | curl+tar floor at 10 Mbps |
| `baseline_curl_100mbps` | medium | 100 Mbps | 10 MB | 1 | 3 | 3.5 | floor | yes | curl+tar floor at 100 Mbps |
| `baseline_curl_parallel_2` | medium | 100 Mbps | 10 MB × 2 | 1 | 3 | 3.6 | floor | yes | 2 concurrent curl+tar floors |
| `baseline_curl_parallel_4` | medium | 100 Mbps | 10 MB × 4 | 1 | 3 | 3.6 | floor | yes | 4 concurrent curl+tar floors |
| `ocx_install_10mbps` | medium | 10 Mbps | 5 MB | 1 | 3 | 13.3 | Shape 1 | yes | Primary regression scenario |
| `ocx_install_100mbps` | **small** | 100 Mbps | 10 MB | 1 | 3 | 3.6 | Shape 1 | yes | Typical corporate link speed |
| `ocx_install_high_bandwidth` | medium | 1 Gbps¹ | 5 MB | 1 | 5 | 1.5 | Shape 1 | yes² | Environment upper bandwidth limit |
| `ocx_install_unthrottled` | **small** | unthrottled | 5 MB | 1 | 5 | 1.3 | Shape 1 | yes² | Loopback; extraction + overhead only |
| `ocx_install_latency_50ms` | medium | 100 Mbps + 50 ms | 10 MB | 1 | 3 | 3.9 | Shape 1 | yes | Latency hiding in pipeline |
| `ocx_install_warm` | medium | unthrottled | 5 MB | 1 | 5 | 1.3 | Shape 1 | no | Warm index, cold layers |
| `ocx_parallel_2_100mbps` | medium | 100 Mbps | 10 MB × 2 | 1 | 3 | 3.7 | Shape 1 | yes | Single `ocx install a b` invocation |
| `ocx_parallel_4_100mbps` | medium | 100 Mbps | 10 MB × 4 | 1 | 3 | 3.8 | Shape 1 | yes | Single `ocx install a b c d` |
| `ocx_parallel_2_processes` | **small** | 100 Mbps | 10 MB × 2 | 1 | 3 | 3.7 | Shape 2 | yes | 2 concurrent `ocx install` processes |
| `ocx_parallel_4_processes` | medium | 100 Mbps | 10 MB × 4 | 1 | 3 | 3.8 | Shape 2 | yes | 4 concurrent `ocx install` processes |
| `ocx_layers_1` | medium | 100 Mbps | 12 MB | 1 | 3 | 4.0 | Shape 1 | yes | Layer scaling anchor: 1 × 12 MB |
| `ocx_layers_2` | medium | 100 Mbps | 12 MB total | 2 | 3 | 4.0 | Shape 1 | yes | 2 × 6 MB — 2-layer pull efficiency |
| `ocx_layers_4` | medium | 100 Mbps | 12 MB total | 4 | 3 | 4.0 | Shape 1 | yes | 4 × 3 MB — 4-layer pull efficiency |

### Large-suite additions (4 rows) {#scenarios-large}

| Scenario | Bandwidth | Payload | Layers | Runs | Est. (s) | Justification |
|----------|-----------|---------|--------|------|----------|---------------|
| `ocx_install_100mbps_large` | 100 Mbps | 50 MB | 1 | 10 | 41.0 | Real tool sizes (cmake ~30 MB). runs=10 → σ/mean <3% |
| `ocx_layers_large_1` | 100 Mbps | 25 MB | 1 | 10 | 21.0 | Layer-scaling anchor at real-tool payload |
| `ocx_layers_large_2` | 100 Mbps | 26 MB total | 2 | 10 | 22.0 | 2-layer efficiency at larger payload |
| `ocx_layers_large_4` | 100 Mbps | 28 MB total | 4 | 10 | 23.0 | 4-layer efficiency at larger payload |

*Rows bold-marked **small** in medium table run in the quick subset.*
*Est. (s): per-row estimated wall-clock = (runs) × expected\_mean\_s + 1.0 s overhead.*

**Shape 1** — single `ocx install a b ...` invocation; [hyperfine][hyperfine] times it directly.

**Shape 2** — N concurrent `ocx install <pkg>` processes fired via `asyncio.TaskGroup`;
wall-clock measured from first launch to last completion, emitted as
[hyperfine][hyperfine]-compatible JSON.

**floor rows** — `curl -sS <blob-url> | tar -xJ -C <tmpdir>` against the same
[toxiproxy][toxiproxy] endpoint. No OCI manifest fetch, no metadata parsing, no symlink
creation — represents the theoretical minimum for download plus extraction. The parallel
curl floor rows (`baseline_curl_parallel_*`) fire N concurrent curl+tar processes via
`asyncio.TaskGroup` to match the Shape 2 parallel scenarios.

¹ "1 Gbps" is the nominal toxic rate. Docker veth / WSL2 virtual NIC may cap below it;
the scenario measures the environment's actual upper bandwidth limit.

² Cold refers to `OCX_HOME` state only. The Linux page cache is NOT dropped between runs
(requires `CAP_SYS_ADMIN` / sudo — not portable). For unthrottled scenarios the numbers
are warm-FS by definition. See [Variance sources](#variance).

## Parallelization Scaling {#scaling}

The scenario matrix includes two scaling families: **process-parallelization** (N
concurrent installs) and **layer-parallelization** (N OCI layers per package). All
groups use the same efficiency formula and dashboard visualization.

### Process scaling {#scaling-process}

| Scaling group | 1-process anchor | 2-process | 4-process |
|---|---|---|---|
| `ocx_shape1_100mbps` | `ocx_install_100mbps` | `ocx_parallel_2_100mbps` | `ocx_parallel_4_100mbps` |
| `ocx_shape2_100mbps` | `ocx_install_100mbps` | `ocx_parallel_2_processes` | `ocx_parallel_4_processes` |
| `curl_100mbps` | `baseline_curl_100mbps` | `baseline_curl_parallel_2` | `baseline_curl_parallel_4` |

The 1-process anchor rows (`ocx_install_100mbps`, `baseline_curl_100mbps`) carry the
same payload and bandwidth as their parallel counterparts but are not tagged as
`scaling_group` members — they are looked up via `SCALING_GROUP_ANCHORS` in
`scenarios.py` so the dashboard can build the efficiency curve without duplicating rows.

### Layer scaling {#scaling-layers}

Two layer-scaling families — medium and large suite:

| Scaling group | 1-layer anchor | 2-layer | 4-layer | Suite |
|---|---|---|---|---|
| `ocx_layers_100mbps` | `ocx_layers_1` | `ocx_layers_2` | `ocx_layers_4` | medium |
| `ocx_layers_large_100mbps` | `ocx_layers_large_1` | `ocx_layers_large_2` | `ocx_layers_large_4` | large |

Medium rows use 12 MB total payload (1 × 12 / 2 × 6 / 4 × 3 MB); large rows use ~25-28 MB
total (closer to real tool binaries). Each layer blob is distinct (separate content paths)
so the registry delivers them independently. `ocx_lib` downloads layers concurrently via
`JoinSet` — the layer-scaling rows measure how much that concurrency reduces total download
time.

`ocx_layers_1` (and `ocx_layers_large_1`) are anchors and are themselves `scaling_group`
members at `process_count=1` (unlike the process-scaling anchors which live outside the
group). The dashboard skips them in the member list to avoid a duplicate N=1 row.

**Insight**: Multi-layer pull gain is bounded by the registry's per-connection throughput
ceiling. At 100 Mbps the single-layer row already saturates the toxiproxy limit, so
efficiency gain from additional layers may be small in the bench environment. Real-world
pull from a public CDN-backed registry (where each layer may come from a different edge
node) can show larger gains.

### Efficiency formula {#scaling-efficiency}

**Efficiency** = T₁ / (N × Tₙ). Perfect linear scaling = 1.0; real values drop due to
shared registry bandwidth saturation and process startup overhead (~20 ms per process for
Shape 2). The dashboard scaling panels visualize these curves — one panel for process
parallelization, one for layer parallelization.

The parallel curl floor rows (`baseline_curl_parallel_*`) are captured
post-optimization because they are ocx-code-independent: they measure infrastructure
overhead only. They are absent from `baseline.json` and therefore treated as **ungated**
by `compare.py` — see [Baseline-absent semantics](#baseline-absent).

## Threshold Semantics {#threshold}

`compare.py` loads `baseline.json` and the current `results/latest.json`, computes
`ratio = current_mean / baseline_mean` per scenario, and applies the gate:

```
FAIL if ratio > 1.0 / threshold   (default threshold = 0.85)
```

At the default threshold this means: fail if the current binary is more than ~17.6%
slower than the baseline (`1/0.85 ≈ 1.176`). Any improvement (ratio < 1.0) always
passes. The 0.85x label in the plan refers to the expected improvement target after
optimization; the gate is the inverse for regression detection.

`compare_against_baseline` in `compare.py` is a pure function — no I/O, no `sys.exit`
inside it. The `__main__` block handles output and translates `report.failed` into a
non-zero process exit. This keeps the function unit-testable without subprocess overhead.

## Baseline-absent Semantics {#baseline-absent}

Scenarios that appear in current results but have **no entry in `baseline.json`** are
reported as **ungated** — they are listed separately in the report and do **not**
contribute to the overall pass/fail decision. This allows new scenarios (new parallel
curl floors, scaling anchor rows) to be captured without requiring a full baseline
re-capture.

Scenarios that are in `baseline.json` but **not run** in the current session (e.g.
during a `bench:quick` or `bench:scenario` partial run) are reported as **not run** and
also do **not** fail the gate. This allows subset runs for fast iteration without
falsely failing 13 other scenarios.

The report distinguishes:

| Category | In baseline | In current | Gate applies |
|---|---|---|---|
| Gated | yes | yes | yes |
| Ungated | no | yes | no |
| Not run | yes | no | no (noted only) |

## Baseline Update Protocol {#baseline-protocol}

`test/bench/baseline.json` is committed to the repository as the reference-hardware
baseline. The times it contains are from the machine where `task test:bench:baseline`
was last run.

**Re-capture the baseline when:**

- Hardware changes (new developer machine, new CI runner)
- An intentional performance change lands (confirmed trade-off, not a regression)
- The scenario matrix changes (new rows, removed rows, changed parameters)

**Never re-capture the baseline after landing an optimization under measurement** — that
defeats the gate.

To update: run `task test:bench:baseline` on the reference machine, then commit the
updated `test/bench/baseline.json` in the same commit as the change that motivated it.

### baseline.json times array note {#baseline-json-note}

[hyperfine][hyperfine]'s JSON export includes a `times` array of per-run wall-clock
values and separate `min`/`max` fields. Shape 2 scenarios (parallel processes, measured
by `parallel_install_wall_clock`) and parallel floor scenarios (measured by
`parallel_curl_wall_clock`) do not use [hyperfine][hyperfine] directly — the harness
synthesizes a [hyperfine][hyperfine]-compatible record where `times` contains the mean
replicated `runs` times. The `min` and `max` fields are real observed values.
`compare_against_baseline` uses `mean` only — the `times` array is for display and
external tooling, not for the gate calculation.

## Dashboard {#dashboard}

The dashboard is a static self-contained HTML file generated by `bench/report.py`.

```bash
task test:bench:report
```

No Docker, no hyperfine, no network access required — reads `results/latest.json` and
(optionally) `baseline.json`. The generated `results/index.html` embeds:

- **Vue 3.5.x** (inlined from `dashboard/vendor/vue.global.prod.js` — no CDN at view time)
- Results and baseline JSON as inline JavaScript literals
- Scenarios metadata (floor\_for, scaling\_group, process\_count) for panel logic

**Panels:**

- **(a) Per-scenario bars** — current (blue), baseline (grey), curl floor (green) as
  horizontal CSS bars scaled to the slowest value shown. Ratio badge with PASS/FAIL for
  gated scenarios; "ungated" label for scenarios absent from baseline; floor ratio shown
  where a floor scenario maps to the current ocx scenario (via `floor_for` field).
  Per-scenario stats: mean ± σ whiskers; Welch z-score badge vs baseline
  (|z|<2 gray "noise", z≤−2 green "real improvement", z≥2 red "real regression").

- **(b) Suite toggle** — toggle buttons for small/medium/large suites; disabled with
  tooltip when no results embedded for that suite. Multi-select overlays several suites
  in grouped bar comparisons so you can see what extra effort buys (tighter σ, stabler
  means).

- **(c) Process parallelization scaling panel** — wall-clock time and efficiency
  (T₁ / (N × Tₙ)) at 1, 2, and 4 processes for ocx Shape 1, ocx Shape 2, and the
  curl floor group.

- **(d) Layer parallelization scaling panel** — wall-clock time and efficiency for
  layer-scaling rows split across 1, 2, and 4 OCI layers at 100 Mbps.

- **(e) Header** — run timestamp, scenario count, runs/warmup, and optional
  `machine_note` from results metadata.

- **Dark/light mode** — toggle in upper-right corner. Default follows
  `prefers-color-scheme`; override persists via `localStorage`.

The `generate_report(results_by_suite, baselines_by_suite, template)` function in
`bench/report.py` is pure (no I/O, returns an HTML string). The `__main__` block scans
`results/` for `latest-<suite>.json` files and embeds all available suites.

**z-score interpretation:**

| z-score range | Meaning |
|---|---|
| \|z\| < 2 | Noise — difference not statistically significant |
| z ≤ −2 | Real improvement (current faster than baseline) |
| z ≥ +2 | Real regression (current slower than baseline) |

z = (m_cur − m_base) / √(σ_cur²/n_cur + σ_base²/n_base) (Welch). Requires real per-run
`times` arrays — available from hyperfine for Shape 1/floor rows and from asyncio wall-clock
measurement for Shape 2/parallel-curl rows.

## Variance Sources {#variance}

Known sources of measurement noise. The 0.85x threshold is chosen to be conservative
enough to absorb this variance on a typical developer machine.

**Page cache not dropped (warm-FS).** For unthrottled and high-bandwidth scenarios the
Linux page cache is not flushed between runs. Network I/O dominates throttled scenarios,
making page cache state irrelevant there. For unthrottled scenarios, reported times are
warm-FS by definition and represent extraction + OCX overhead, not raw I/O.

**CPU frequency scaling and Docker scheduler jitter.** The kernel may scale CPU
frequency mid-run; Docker's veth adds scheduling overhead on top of the TCP stack. The
medium suite uses `runs=3–5` per scenario (v3 budget rule; all warmup=0); the large suite
uses `runs=10` on key rows. The conservative 0.85x threshold absorbs the remaining variance.

**~20 ms process-startup skew in Shape 2.** Shape 2 measures wall-clock time across N
concurrent `ocx install` subprocess launches via `asyncio.TaskGroup`. Python subprocess
startup adds roughly 10–20 ms per process. At 50 MB payload sizes this is noise (< 1%
of total time at 10 Mbps). At very small payloads (5 MB) or unthrottled scenarios the
startup skew is proportionally larger. Shape 2 numbers are therefore most meaningful
for comparison within the same machine, not for absolute overhead claims.

The same startup skew applies to the parallel curl floor rows (`baseline_curl_parallel_*`),
which use `asyncio.create_subprocess_shell` — the noise floor is symmetric between ocx
Shape 2 and curl parallel, making the floor comparison valid.

## Smoke Validation {#smoke-validation}

`test/tests/test_bench_smoke.py` contains pytest-collected unit tests for the harness
itself. They run without Docker, toxiproxy, or [hyperfine][hyperfine]:

- Verifies the scenario matrix has exactly 21 rows with unique names
- Verifies per-scenario `runs`, `warmup`, `est_seconds` fields are positive / non-negative
- Verifies per-suite budgets: small <60 s, medium ≤200 s, large ≤550 s
- Verifies suite partition integrity (small ⊂ medium ⊂ large)
- Verifies scaling\_group, process\_count, floor\_for field integrity
- Verifies layer-scaling rows: `layers` field, `process_count`, shared payload, same `scaling_group`
- Verifies `compare_against_baseline` is pure (no I/O, no exit calls)
- Verifies baseline-absent skip semantics (ungated / not\_run)
- Verifies `generate_report` (multi-suite signature) returns HTML with embedded markers
- Verifies `ScenarioResult` and `CompareReport` dataclasses have expected fields
- Verifies `times` array is stored honestly (not mean-replicated)
- Verifies z-score (Welch) math with known-vector unit tests

Live harness tests (requiring the full Docker environment) are gated behind
`BENCH_SMOKE_LIVE=1` and skipped by default.

`conftest.py` in `test/bench/` provides pytest fixtures for these smoke tests only. It
does not run the full benchmark matrix from pytest.

## File Layout {#file-layout}

```
test/bench/
  __init__.py              # package marker
  conftest.py              # smoke-validation fixtures (pytest only; no Docker required)
  harness.py               # standalone entry point; owns session lifecycle
  scenarios.py             # 21-row scenario matrix v3 + SCALING_GROUP_ANCHORS + SUITE_BUDGET_SECONDS
  baseline.py              # curl+tar floor command builder
  compare.py               # pure comparison function + __main__ exit-code handler
  report.py                # pure generate_report() + __main__ file-IO wrapper
  baseline.json            # committed reference-hardware baseline
  results/                 # .gitignore'd; per-run JSON outputs + index.html dashboard
  dashboard/
    template.html          # Vue 3 single-file app template
    vendor/
      vue.global.prod.js   # Vue 3 global prod build (inlined into generated HTML)
```

<!-- external -->
[registry2]: https://hub.docker.com/_/registry
[toxiproxy]: https://github.com/Shopify/toxiproxy
[hyperfine]: https://github.com/sharkdp/hyperfine
[uv]: https://docs.astral.sh/uv/
[gh]: https://cli.github.com/
