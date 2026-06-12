# Research: Local Install Benchmark Tooling

Date: 2026-06-12. Axis: tech (tooling selection). Companion: [research_install_perf_optimization.md](./research_install_perf_optimization.md).

Goal: fully-local, reproducible `ocx install` benchmark with emulated internet bandwidth; before/after comparisons; parallel installs of 2–4 packages; baseline floor = plain download+extract; permanent regression artifact.

## Recommendations

| Axis | Pick | Version | New dep |
|---|---|---|---|
| Bench harness | **hyperfine** | 1.20.0 (2025-11) | yes — toolchain (prefer `ocx.toml` dogfood if mirrored, else gh release) |
| HTTP throttling | **toxiproxy** (Shopify) | 2.12.0, `ghcr.io/shopify/toxiproxy` | yes — `test/docker-compose.yml` service |
| Local registry | **registry:2** (existing) | — | no change |
| CI regression | JSON artifact + threshold compare script | — | none yet (Bencher `shell_hyperfine` adapter = later option) |

Both picks = boring tech: hyperfine is the de-facto Rust-CLI bench standard (uv `BENCHMARKS.md` hard-requires it; ripgrep/fd/bat use it); toxiproxy = years of Shopify production, REST API, no root needed (vs tc/netem which needs CAP_NET_ADMIN — not viable on GH Actions).

## hyperfine fit

- `--warmup N`, `--prepare` (e.g. wipe `$OCX_HOME` between runs), `--setup`/`--cleanup`, `--runs N`, `--export-json` + `--export-markdown`, `-L param v1,v2` parameter sweep (bandwidth profiles, package sizes).
- Baseline floor: hyperfine compares named commands natively — `ocx install pkg` vs `curl <blob-url> | tar -xJ` floor; JSON includes relative ratios.
- **Parallel installs (2–4)**: hyperfine can't time concurrent subprocesses → thin Python wrapper (asyncio TaskGroup, fires N `ocx install` concurrently, measures wall clock, emits hyperfine-compatible JSON) feeding the same pipeline. Alternative: single `ocx install a b c d` invocation IS the parallel path (install_all JoinSet) — bench both shapes.

## toxiproxy fit

- Toxics: `bandwidth` (KB/s, byte-accurate, deterministic within a few %), `latency` (+jitter), `down`, `slow_close`.
- Wiring: service in `test/docker-compose.yml`, listen :5002 → upstream registry:5000, control API :8474. Scenario setup = one REST call (`POST /proxies/.../toxics`) per bandwidth profile; deterministic, scriptable between hyperfine `-L` values.
- Suggested profiles: 10 Mbps (slow), 100 Mbps (typical), 1 Gbps (fast), unthrottled (localhost ceiling) + 20–50 ms latency variant — stresses different pipeline stages (connection setup vs throughput vs CPU-bound extract).

```yaml
bench-proxy:
  image: ghcr.io/shopify/toxiproxy:2.12.0
  ports: ["5002:5002", "8474:8474"]
```

## Rejected

- **tc/netem**: root/CAP_NET_ADMIN, not portable to CI. 
- **nginx limit_rate**: not dynamically scriptable per scenario, response-body-only granularity.
- **custom throttling proxy**: innovation token for a solved problem; nontrivial flow-control correctness.
- **criterion/divan as outer harness**: in-process model fights multi-second subprocess + external registry state. (Still optional for stage microbenches.)
- **pytest-benchmark**: calibration model wrong for 2–10 s subprocesses; hyperfine strictly better fit.
- **zot registry**: benefit only without proxy in front; throttling proxy dominates timing anyway. Track for mirror scenarios separately.

## Existing infra to build on (discovered)

- `test/docker-compose.yml`: registry:2 on :5000 (+mirror :5001), session-start lifecycle in `conftest.py`, readiness probe `helpers.py:37-54`.
- `test/src/helpers.py make_package(size_mb=...)`: builds + pushes tar.xz packages with size padding — ready-made fixture generator for bench payloads.
- `test/src/runner.py OcxRunner`: isolated `OCX_HOME`, `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES` env — reuse env shape for bench invocations.
- Taskfile conventions: new `bench:*` namespace per `subsystem-taskfiles.md`.
