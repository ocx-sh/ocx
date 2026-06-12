"""Benchmark scenario matrix — v3.

Shape annotations:
  Shape 1 = single `ocx install a b c d` invocation (hyperfine times directly)
  Shape 2 = N concurrent `ocx install <pkg>` processes (wall-clock via asyncio.TaskGroup)
  floor   = curl+tar baseline, no ocx (theoretical minimum)

Scaling metadata:
  scaling_group: str | None
      Name of the scaling family this row belongs to.
      All rows sharing a scaling_group form a curve over process_count 1/2/4.
  process_count: int
      Number of concurrent processes / layers this row represents.
      1 = single anchor (denominator for efficiency); 2/4 = parallel rows.
      For layer-scaling scenarios, process_count = number of layers.
  floor_for: str | None
      Name of the ocx scenario this floor row directly corresponds to.
      Set on "floor" shape rows only; None on all other shapes.

Suite membership (cumulative: large ⊇ medium ⊇ small):
  suite: Suite ("small" | "medium" | "large")
      "small"  — fast iteration subset (<1 min wall incl. setup); also called bench:quick.
      "medium" — full regression gate (<4 min wall); default task test:bench.
      "large"  — optional extended run (<10 min wall); task test:bench:large.
                 Adds higher runs=10 on key rows + larger payloads (50 MB / 25 MB layer)
                 closer to real tool sizes. Amortizes startup noise. Only run for
                 tighter statistical confidence; justification documented per large row.

Per-scenario run budget (v3 hard cap):
  runs: int
      Number of hyperfine --min-runs (overrides global CLI default).
  warmup: int
      Number of hyperfine --warmup runs (overrides global CLI default).
  est_seconds: float
      Estimated wall-clock contribution in seconds.
      Computed as: (runs + warmup) * expected_mean_s + 1.0 overhead.
      Smoke tests assert per-suite sums ≤ SUITE_BUDGET_SECONDS[suite].
      Remaining headroom (~40 s for small/medium, ~50 s for large) covers
      fixture push, proxy creation, hyperfine startup overhead.

Suite budgets (scenario time only — not including setup overhead):
  small:  < 60 s
  medium: ≤ 200 s  (regression gate; ~40 s headroom for ≤240 s total)
  large:  ≤ 550 s  (optional; ~50 s headroom for ≤600 s total)

Duration budget rule (v3):
  fast   (<0.1 s mean) → runs=5, warmup=0
  medium (~1 s mean)   → runs=3, warmup=0
  slow   (>3 s mean)   → runs=3, warmup=0
  large-suite key rows → runs=10, warmup=0 (tighter σ justifies higher cost)

Payload sizes (v3):
  10 Mbps rows : 5 MB  (≈4.1 s/run → medium boundary, runs=3, warmup=0)
  100 Mbps rows: 10 MB (≈0.85 s/run → medium, ≫ ~50 ms noise floor)
  unthrottled/high_bw/warm: 5 MB (sub-100 ms — fast)
  layer rows: 12 MB total (1×12 / 2×6 / 4×3 MB @100 Mbps ≈1.0 s)
  large 100 Mbps: 50 MB (≈4.0 s/run — closer to real tool binaries; runs=10)
  large layer rows: 25 MB total (1×25 / 2×13 / 4×7 MB @100 Mbps ≈2.0 s; runs=10)

Memory hygiene (v3):
  ALL temp dirs created under BENCH_SCRATCH_DIR (see harness.py).
  Default: <repo>/target/bench-tmp (disk-backed, gitignored under target/).
  NEVER creates dirs under /tmp (tmpfs = RAM-backed on WSL).
  Peak disk estimate: large suite pushes ~50 MB + 25 MB×3 = ~125 MB fixtures;
  scratch per-run ~50 MB OCX_HOME × concurrency; peak ≤ 500 MB total.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

Shape = Literal["floor", "shape1", "shape2"]
Suite = Literal["small", "medium", "large"]

# Default run budget — used as fallback when per-scenario fields not set.
# These are the GLOBAL defaults; per-scenario fields take precedence in the harness.
DEFAULT_RUNS = 10
DEFAULT_WARMUP = 1


@dataclass(slots=True)
class Scenario:
    """A single benchmark scenario definition.

    Attributes
    ----------
    name:
        Unique identifier used in result JSON keys and hyperfine command names.
    bandwidth_kbps:
        Toxiproxy bandwidth toxic rate in KB/s. 0 = no toxic (unthrottled).
        Conversion: 10 Mbps = 10 * 1000 / 8 = 1250 KB/s (decimal Mbps).
    latency_ms:
        One-way added latency in milliseconds. 0 = no latency toxic.
    size_mb:
        Package payload size in MB (passed to make_package(size_mb=...)).
    concurrency:
        Number of packages to install concurrently (Shape 1: passed as args;
        Shape 2: N separate processes; floor parallel: N curl procs).
    shape:
        "floor"  = curl+tar baseline (no ocx, uses baseline.py command)
        "shape1" = single `ocx install pkg_a pkg_b ...` invocation
        "shape2" = N concurrent `ocx install <pkg>` processes
    cold:
        True = fresh OCX_HOME each hyperfine run.
        False = index pre-populated, but no cached layers (warm index).
    runs:
        Per-scenario hyperfine --min-runs (overrides global default).
    warmup:
        Per-scenario hyperfine --warmup (overrides global default).
    est_seconds:
        Estimated wall-clock seconds including all runs + warmup.
        Smoke tests assert per-suite sums ≤ SUITE_BUDGET_SECONDS[suite].
    scaling_group:
        Name of the scaling family this row belongs to, or None.
    process_count:
        Concurrent process count or layer count. 1 = single anchor.
        For ocx_layers_* rows: number of layers in the package.
    floor_for:
        Name of the ocx scenario this floor row corresponds to.
    layers:
        Number of layers in the package (for layer-scaling scenarios only).
        1 = default (single-layer package). Set per row for layer rows.
    suite:
        Suite membership: "small" | "medium" | "large". Cumulative:
        large ⊇ medium ⊇ small. Harness --suite flag filters by suite.
    notes:
        Human-readable footnotes.
    """

    name: str
    bandwidth_kbps: int  # 0 = unthrottled
    latency_ms: int  # 0 = no added latency
    size_mb: int
    concurrency: int
    shape: Shape
    cold: bool = True
    runs: int = DEFAULT_RUNS
    warmup: int = DEFAULT_WARMUP
    est_seconds: float = 0.0
    scaling_group: str | None = None
    process_count: int = 1
    floor_for: str | None = None
    layers: int = 1  # number of layers; >1 for layer-scaling scenarios
    suite: Suite = "medium"  # default: part of medium gate
    notes: str = ""
    # Derived: packages list populated by harness at runtime from pushed packages.
    packages: list[str] = field(default_factory=list)

    @property
    def total_fixture_mb(self) -> int:
        """Total MB pushed to registry for this scenario (size_mb × concurrency)."""
        return self.size_mb * max(1, self.concurrency)


# ---------------------------------------------------------------------------
# Bandwidth constants
# ---------------------------------------------------------------------------
_BW_10MBPS = 1250  # KB/s  (10 Mbps decimal)
_BW_100MBPS = 12500  # KB/s (100 Mbps decimal)
_BW_1GBPS = 131072  # KB/s (128 MB/s — exceeds typical veth cap)

# ---------------------------------------------------------------------------
# Duration budget helpers
# ---------------------------------------------------------------------------
# est_seconds = (runs + warmup) * expected_mean_s + 1.0 overhead per scenario
#
# v3 expected_mean_s per config (observed on WSL2 / Docker veth):
#   5 MB @ 10 Mbps  ≈ 4.1 s   (network-bound; 5 MB / 1.25 MB/s)
#   10 MB @ 100 Mbps ≈ 0.85 s (network-bound; 10 MB / 12.5 MB/s)
#   5 MB unthrottled ≈ 0.06 s (extraction + OCX overhead only)
#   5 MB high_bw     ≈ 0.10 s (capped by veth; ~0.10 s observed)
#   10 MB + 50ms lat ≈ 0.95 s (100 Mbps + 50 ms one-way)
#   5 MB warm        ≈ 0.06 s (index populated; no download)
#   12 MB 1-layer    ≈ 1.0 s  (layer scaling anchor @100 Mbps)
#   curl 10 Mbps     ≈ 4.1 s  (no OCX overhead, same BW)
#   curl 100 Mbps    ≈ 0.82 s (pure download + xz extract, 10 MB)
#
# Budget rule (v3):
#   slow  (mean > 3 s) → runs=3, warmup=0
#   medium(mean 0.5–3 s) → runs=3, warmup=0
#   fast  (mean < 0.5 s) → runs=5, warmup=0
#
# Total scenario time target ≤ 200 s (SCENARIO_BUDGET_SECONDS).
# ~40 s headroom for fixture push + proxy setup + hyperfine startup overhead.


def _est(runs: int, warmup: int, mean_s: float) -> float:
    """Compute est_seconds = (runs + warmup) * mean_s + 1.0 overhead."""
    return round((runs + warmup) * mean_s + 1.0, 1)


# Per-suite scenario-time budgets (setup overhead excluded).
# Suite selection is cumulative: large ⊇ medium ⊇ small.
# Smoke tests assert sum(est_seconds for s if s.suite ≤ target) ≤ SUITE_BUDGET_SECONDS[suite].
SUITE_BUDGET_SECONDS: dict[str, float] = {
    "small": 60.0,  # <1 min wall incl. setup
    "medium": 200.0,  # ≤240 s total; ~40 s setup headroom
    "large": 550.0,  # ≤600 s total; ~50 s setup headroom
}

# Backward-compat alias — smoke tests and harness imports that predate suite model.
SCENARIO_BUDGET_SECONDS = SUITE_BUDGET_SECONDS["medium"]

# Suite ordering for cumulative membership checks.
SUITE_ORDER: list[Suite] = ["small", "medium", "large"]

# ---------------------------------------------------------------------------
# Scenario matrix — v3
# Payload changes from v2:
#   100 Mbps rows: 50 MB → 10 MB per package
#   unthrottled/high_bw/warm: 50 MB → 5 MB
#   layer rows: 40 MB total → 12 MB total (1×12 / 2×6 / 4×3 MB)
#   10 Mbps rows: 5 MB (unchanged)
# Run counts: all warmup=0; slow rows runs=3; medium runs=3; fast runs=5
# ---------------------------------------------------------------------------

SCENARIOS: list[Scenario] = [
    # ------------------------------------------------------------------ #
    # Floor rows — curl+tar baseline                                       #
    # ------------------------------------------------------------------ #
    # 5 MB @10 Mbps floor — matches ocx_install_10mbps config.
    Scenario(
        name="baseline_curl_10mbps",
        bandwidth_kbps=_BW_10MBPS,
        latency_ms=0,
        size_mb=5,
        concurrency=1,
        shape="floor",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 4.1),
        process_count=1,
        floor_for="ocx_install_10mbps",
        notes=(
            "curl+tar floor at 10 Mbps, 5 MB payload; "
            "matches ocx_install_10mbps for fair comparison."
        ),
    ),
    # 10 MB @100 Mbps floor — matches ocx_install_100mbps config.
    Scenario(
        name="baseline_curl_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=1,
        shape="floor",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.82),
        process_count=1,
        floor_for="ocx_install_100mbps",
        notes="curl+tar floor at 100 Mbps, 10 MB; theoretical minimum for download+extract.",
    ),
    # ------------------------------------------------------------------ #
    # Single-package install — Shape 1                                     #
    # ------------------------------------------------------------------ #
    Scenario(
        name="ocx_install_10mbps",
        bandwidth_kbps=_BW_10MBPS,
        latency_ms=0,
        size_mb=5,
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 4.1),
        process_count=1,
        notes=(
            "Primary regression scenario; network-bound at 10 Mbps, 5 MB payload. "
            "Signal ≫ noise: ~4 s >> ~50 ms overhead."
        ),
    ),
    Scenario(
        name="ocx_install_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.85),
        process_count=1,
        suite="small",
        notes=(
            "100 Mbps, 10 MB payload; typical corporate link speed. "
            "Signal ~0.85 s >> ~50 ms noise floor."
        ),
    ),
    Scenario(
        name="ocx_install_high_bandwidth",
        bandwidth_kbps=_BW_1GBPS,
        latency_ms=0,
        size_mb=5,
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=5,
        warmup=0,
        est_seconds=_est(5, 0, 0.10),
        process_count=1,
        notes=(
            "1 Gbps nominal, 5 MB; measures environment upper bandwidth limit. "
            "Warm-FS by definition (page cache not dropped)."
        ),
    ),
    Scenario(
        name="ocx_install_unthrottled",
        bandwidth_kbps=0,  # No toxic
        latency_ms=0,
        size_mb=5,
        suite="small",
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=5,
        warmup=0,
        est_seconds=_est(5, 0, 0.06),
        process_count=1,
        notes="No bandwidth toxic, 5 MB; loopback speed; measures extraction + OCX overhead.",
    ),
    Scenario(
        name="ocx_install_latency_50ms",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=50,
        size_mb=10,
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.95),
        process_count=1,
        notes="100 Mbps + 50 ms one-way latency, 10 MB; exercises latency hiding in pipeline.",
    ),
    # ------------------------------------------------------------------ #
    # Warm scenario                                                         #
    # ------------------------------------------------------------------ #
    Scenario(
        name="ocx_install_warm",
        bandwidth_kbps=0,
        latency_ms=0,
        size_mb=5,
        concurrency=1,
        shape="shape1",
        cold=False,
        runs=5,
        warmup=0,
        est_seconds=_est(5, 0, 0.06),
        process_count=1,
        notes=(
            "Warm index, cold layers, unthrottled, 5 MB; measures index-lookup overhead. "
            "Index pre-populated before first run; wipes packages/layers between runs."
        ),
    ),
    # ------------------------------------------------------------------ #
    # Parallel install — Shape 1 (single multi-package invocation)         #
    # Scaling group "ocx_shape1_100mbps": anchor=ocx_install_100mbps       #
    # ------------------------------------------------------------------ #
    Scenario(
        name="ocx_parallel_2_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=2,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.90),
        scaling_group="ocx_shape1_100mbps",
        process_count=2,
        notes="Shape 1: `ocx install a b`; 2 × 10 MB @100 Mbps.",
    ),
    Scenario(
        name="ocx_parallel_4_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=4,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.92),
        scaling_group="ocx_shape1_100mbps",
        process_count=4,
        notes="Shape 1: `ocx install a b c d`; 4 × 10 MB @100 Mbps.",
    ),
    # ------------------------------------------------------------------ #
    # Parallel install — Shape 2 (N concurrent processes)                  #
    # Scaling group "ocx_shape2_100mbps": anchor=ocx_install_100mbps       #
    # ------------------------------------------------------------------ #
    Scenario(
        name="ocx_parallel_2_processes",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=2,
        shape="shape2",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.90),
        scaling_group="ocx_shape2_100mbps",
        process_count=2,
        suite="small",
        notes="Shape 2: 2 concurrent `ocx install` processes; 2 × 10 MB @100 Mbps; wall-clock.",
    ),
    Scenario(
        name="ocx_parallel_4_processes",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=4,
        shape="shape2",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.92),
        scaling_group="ocx_shape2_100mbps",
        process_count=4,
        notes="Shape 2: 4 concurrent `ocx install` processes; 4 × 10 MB @100 Mbps; wall-clock.",
    ),
    # ------------------------------------------------------------------ #
    # Parallel curl floors — match Shape 2 parallel rows                   #
    # Scaling group "curl_100mbps": anchor=baseline_curl_100mbps           #
    # ------------------------------------------------------------------ #
    Scenario(
        name="baseline_curl_parallel_2",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=2,
        shape="floor",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.85),
        scaling_group="curl_100mbps",
        process_count=2,
        floor_for="ocx_parallel_2_processes",
        notes="2 concurrent curl+tar — floor for ocx_parallel_2_processes; 2 × 10 MB @100 Mbps.",
    ),
    Scenario(
        name="baseline_curl_parallel_4",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=10,
        concurrency=4,
        shape="floor",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 0.87),
        scaling_group="curl_100mbps",
        process_count=4,
        floor_for="ocx_parallel_4_processes",
        notes="4 concurrent curl+tar — floor for ocx_parallel_4_processes; 4 × 10 MB @100 Mbps.",
    ),
    # ------------------------------------------------------------------ #
    # Layer-scaling scenarios (Phase 5.6, resized in v3)                   #
    # 12 MB total @100 Mbps, split 1×12 / 2×6 / 4×3 MB layers.            #
    # Scaling group "ocx_layers_100mbps": process_count = layer count.     #
    # ------------------------------------------------------------------ #
    Scenario(
        name="ocx_layers_1",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=12,  # 1 × 12 MB layer
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 1.0),
        scaling_group="ocx_layers_100mbps",
        process_count=1,
        layers=1,
        notes=(
            "Layer scaling anchor: 1 × 12 MB @100 Mbps. "
            "Baseline for multi-layer pull efficiency measurement."
        ),
    ),
    Scenario(
        name="ocx_layers_2",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=12,  # 2 × 6 MB layers = 12 MB total
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 1.0),
        scaling_group="ocx_layers_100mbps",
        process_count=2,
        layers=2,
        notes="Layer scaling: 2 × 6 MB = 12 MB total @100 Mbps; measures 2-layer pull gain.",
    ),
    Scenario(
        name="ocx_layers_4",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=12,  # 4 × 3 MB layers = 12 MB total
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=3,
        warmup=0,
        est_seconds=_est(3, 0, 1.0),
        scaling_group="ocx_layers_100mbps",
        process_count=4,
        layers=4,
        notes="Layer scaling: 4 × 3 MB = 12 MB total @100 Mbps; measures 4-layer pull gain.",
    ),
    # ------------------------------------------------------------------ #
    # Large-suite rows — suite="large" only                               #
    # Justification: higher runs (10) + larger payloads closer to real    #
    # tool binary sizes. Reduces per-run variance (σ/mean < 3%) on the   #
    # key 100 Mbps scenario. Not in medium gate to keep daily CI <4 min.  #
    # ------------------------------------------------------------------ #
    # 50 MB @100 Mbps: real tool binaries (cmake ~30 MB, node ~70 MB).
    # runs=10 → σ/mean typically <3% vs ~8% at runs=3.
    # est: 10 × 4.0 + 1.0 = 41.0 s
    Scenario(
        name="ocx_install_100mbps_large",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=10,
        warmup=0,
        est_seconds=_est(10, 0, 4.0),
        process_count=1,
        suite="large",
        notes=(
            "Large-suite: 50 MB @100 Mbps, runs=10. Payload closer to real tool binaries "
            "(cmake ~30 MB, node ~70 MB). Tighter σ amortizes startup noise."
        ),
    ),
    # 25 MB total layer scaling @100 Mbps — 1-layer anchor.
    # runs=10 + larger payload yields statistically tighter efficiency curve.
    # est: 10 × 2.0 + 1.0 = 21.0 s
    Scenario(
        name="ocx_layers_large_1",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=25,  # 1 × 25 MB layer
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=10,
        warmup=0,
        est_seconds=_est(10, 0, 2.0),
        scaling_group="ocx_layers_large_100mbps",
        process_count=1,
        layers=1,
        suite="large",
        notes=(
            "Large-suite layer-scaling anchor: 1 × 25 MB @100 Mbps, runs=10. "
            "25 MB total matches mid-size real tool. Tighter σ vs 12 MB medium rows."
        ),
    ),
    # 25 MB total, 2 layers (2 × 13 MB; 13 avoids fractional MB in make_package).
    # est: 10 × 2.0 + 1.0 = 21.0 s
    Scenario(
        name="ocx_layers_large_2",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=26,  # 2 × 13 MB = 26 MB total (closest even split ≥ 25 MB)
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=10,
        warmup=0,
        est_seconds=_est(10, 0, 2.1),
        scaling_group="ocx_layers_large_100mbps",
        process_count=2,
        layers=2,
        suite="large",
        notes=(
            "Large-suite layer scaling: 2 × 13 MB = 26 MB total @100 Mbps, runs=10. "
            "Measures 2-layer concurrent pull efficiency at larger payload."
        ),
    ),
    # 28 MB total, 4 layers (4 × 7 MB).
    # est: 10 × 2.2 + 1.0 = 23.0 s
    Scenario(
        name="ocx_layers_large_4",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=28,  # 4 × 7 MB = 28 MB total
        concurrency=1,
        shape="shape1",
        cold=True,
        runs=10,
        warmup=0,
        est_seconds=_est(10, 0, 2.2),
        scaling_group="ocx_layers_large_100mbps",
        process_count=4,
        layers=4,
        suite="large",
        notes=(
            "Large-suite layer scaling: 4 × 7 MB = 28 MB total @100 Mbps, runs=10. "
            "Measures 4-layer concurrent pull efficiency at real-tool payload size."
        ),
    ),
]

# ---------------------------------------------------------------------------
# Scaling group anchor map
# ---------------------------------------------------------------------------
# Maps scaling_group → scenario_name for the process_count=1 anchor.
# The anchor row may carry scaling_group=None to avoid modifying existing rows
# (ocx_install_100mbps and baseline_curl_100mbps) or may carry process_count=1
# (ocx_layers_1 is itself in the group at process_count=1).
SCALING_GROUP_ANCHORS: dict[str, str] = {
    "ocx_shape1_100mbps": "ocx_install_100mbps",
    "ocx_shape2_100mbps": "ocx_install_100mbps",
    "curl_100mbps": "baseline_curl_100mbps",
    "ocx_layers_100mbps": "ocx_layers_1",
    "ocx_layers_large_100mbps": "ocx_layers_large_1",
}

# ---------------------------------------------------------------------------
# Sanity checks
# ---------------------------------------------------------------------------

_EXPECTED_ROWS = 21
assert len(SCENARIOS) == _EXPECTED_ROWS, (  # noqa: PLR2004
    f"Scenario matrix must have exactly {_EXPECTED_ROWS} rows, got {len(SCENARIOS)}"
)

_names = [s.name for s in SCENARIOS]
assert len(_names) == len(set(_names)), "Scenario names must be unique"

# Scaling group members must have process_count >= 2, OR be an anchor at process_count=1
# that is explicitly in the group (like ocx_layers_1).
for _s in SCENARIOS:
    if _s.scaling_group is not None:
        anchor = SCALING_GROUP_ANCHORS.get(_s.scaling_group)
        is_anchor = anchor == _s.name
        assert _s.process_count >= 1, (  # noqa: PLR2004
            f"process_count must be ≥ 1 on scaling group member {_s.name!r}"
        )
        if not is_anchor:
            assert _s.process_count >= 2, (  # noqa: PLR2004
                f"Non-anchor scaling group member {_s.name!r} must have "
                f"process_count >= 2; got {_s.process_count}. "
                "Use SCALING_GROUP_ANCHORS for the 1-process anchor."
            )

# floor_for references must point to valid scenario names.
_name_set = set(_names)
for _s in SCENARIOS:
    if _s.floor_for is not None:
        assert _s.floor_for in _name_set, (
            f"floor_for={_s.floor_for!r} in {_s.name!r} references unknown scenario"
        )

# Duration budget: per-suite cumulative sums must not exceed SUITE_BUDGET_SECONDS.
# Suite selection is cumulative (large ⊇ medium ⊇ small), so we check the
# scenarios that would be RUN at each suite tier (all rows whose suite is in
# the cumulative set for that tier).
_SUITE_CUMULATIVE: dict[str, set[str]] = {
    "small": {"small"},
    "medium": {"small", "medium"},
    "large": {"small", "medium", "large"},
}
for _suite_name, _suite_set in _SUITE_CUMULATIVE.items():
    _suite_est = sum(s.est_seconds for s in SCENARIOS if s.suite in _suite_set)
    _budget = SUITE_BUDGET_SECONDS[_suite_name]
    assert _suite_est <= _budget, (
        f"Estimated {_suite_name}-suite scenario time {_suite_est:.1f}s exceeds "
        f"{_budget:.0f}s budget. Reduce runs or shrink payloads."
    )
