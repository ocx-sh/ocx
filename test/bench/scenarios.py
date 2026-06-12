"""Benchmark scenario matrix — 14 rows exactly as spec §D4 table.

Shape annotations:
  Shape 1 = single `ocx install a b c d` invocation (hyperfine times directly)
  Shape 2 = N concurrent `ocx install <pkg>` processes (wall-clock via asyncio.TaskGroup)
  floor   = curl+tar baseline, no ocx (theoretical minimum)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

Shape = Literal["floor", "shape1", "shape2"]


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
        Note: toxiproxy bandwidth toxic operates in KB/s.
    latency_ms:
        One-way added latency in milliseconds. 0 = no latency toxic.
    size_mb:
        Package payload size in MB (passed to make_package(size_mb=...)).
    concurrency:
        Number of packages to install concurrently.
    shape:
        "floor"  = curl+tar baseline (no ocx, uses baseline.py command)
        "shape1" = single `ocx install pkg_a pkg_b ...` invocation
        "shape2" = N concurrent `ocx install <pkg>` processes (parallel_install_wall_clock)
    cold:
        True = fresh OCX_HOME each hyperfine run (--prepare wipes OCX_HOME).
        False = index pre-populated, but no cached layers (warm index scenario).
    notes:
        Human-readable footnotes from the spec table.
    """

    name: str
    bandwidth_kbps: int  # 0 = unthrottled
    latency_ms: int  # 0 = no added latency
    size_mb: int
    concurrency: int
    shape: Shape
    cold: bool = True
    notes: str = ""
    # Derived: packages list populated by harness at runtime from pushed packages
    packages: list[str] = field(default_factory=list)


# ---------------------------------------------------------------------------
# Scenario matrix — 14 rows per spec §D4
# ---------------------------------------------------------------------------

# Bandwidth conversion helpers (spec uses Mbps, toxiproxy uses KB/s):
# 10 Mbps  = 10 * 1000 / 8  = 1250 KB/s  (decimal Mbps → decimal KB/s)
# 100 Mbps = 100 * 1000 / 8 = 12500 KB/s
# Note: toxiproxy bandwidth toxic uses KiB/s internally but the spec table
# references Mbps nominal — we use decimal KB/s (1 Mbps = 125 KB/s) which
# is the conventional interpretation for network throughput.
_BW_10MBPS = 1250  # KB/s
_BW_100MBPS = 12500  # KB/s
# 1 Gbps nominal: set very high so the toxic effectively does not throttle
# (Docker veth/WSL2 NIC caps the actual rate; scenario measures environment
# upper bandwidth limit — see spec §D4 footnote ¹).
_BW_1GBPS = 131072  # KB/s (128 MB/s — exceeds typical veth cap intentionally)

SCENARIOS: list[Scenario] = [
    # --- Floor rows (curl+tar baseline, Shape "floor") ---
    Scenario(
        name="baseline_curl_10mbps",
        bandwidth_kbps=_BW_10MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="floor",
        cold=True,
        notes="curl+tar floor at 10 Mbps; theoretical minimum for 50 MB layer download+extract",
    ),
    Scenario(
        name="baseline_curl_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="floor",
        cold=True,
        notes="curl+tar floor at 100 Mbps; theoretical minimum for 50 MB layer download+extract",
    ),
    # --- Single-package install scenarios (Shape 1) ---
    Scenario(
        name="ocx_install_10mbps",
        bandwidth_kbps=_BW_10MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes="Primary regression scenario; network-bound at 10 Mbps; 50 MB package",
    ),
    Scenario(
        name="ocx_install_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes="100 Mbps; typical corporate link speed; 50 MB package",
    ),
    Scenario(
        name="ocx_install_high_bandwidth",
        bandwidth_kbps=_BW_1GBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes=(
            "1 Gbps nominal toxic rate (footnote ¹): Docker veth/WSL2 vNIC may cap "
            "below the toxic rate — measures the environment's actual upper bandwidth "
            "limit. Cold = OCX_HOME fresh, page cache NOT dropped (warm-FS). "
            "See spec §D4 footnote ²."
        ),
    ),
    Scenario(
        name="ocx_install_unthrottled",
        bandwidth_kbps=0,  # No toxic — full loopback speed
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes=(
            "No bandwidth toxic; loopback speed. Cold = OCX_HOME fresh, "
            "page cache NOT dropped (warm-FS). See spec §D4 footnote ²."
        ),
    ),
    Scenario(
        name="ocx_install_latency_50ms",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=50,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes="100 Mbps + 50 ms one-way latency; exercises latency hiding in streaming pipeline",
    ),
    Scenario(
        name="ocx_install_small_10mbps",
        bandwidth_kbps=_BW_10MBPS,
        latency_ms=0,
        size_mb=5,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes="Small package (5 MB) at 10 Mbps; verifies per-request overhead dominates",
    ),
    Scenario(
        name="ocx_install_large_10mbps",
        bandwidth_kbps=_BW_10MBPS,
        latency_ms=0,
        size_mb=200,
        concurrency=1,
        shape="shape1",
        cold=True,
        notes="Large package (200 MB) at 10 Mbps; exercises streaming at scale",
    ),
    # --- Parallel install — Shape 1 (single multi-package invocation) ---
    Scenario(
        name="ocx_parallel_2_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=2,
        shape="shape1",
        cold=True,
        notes="Shape 1: single `ocx install a b` invocation; 2 packages × 50 MB at 100 Mbps",
    ),
    Scenario(
        name="ocx_parallel_4_100mbps",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=4,
        shape="shape1",
        cold=True,
        notes="Shape 1: single `ocx install a b c d` invocation; 4 packages × 50 MB at 100 Mbps",
    ),
    # --- Parallel install — Shape 2 (N concurrent ocx install processes) ---
    Scenario(
        name="ocx_parallel_2_processes",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=2,
        shape="shape2",
        cold=True,
        notes=(
            "Shape 2: 2 concurrent `ocx install <pkg>` processes via asyncio.TaskGroup; "
            "wall-clock measured; emits hyperfine-compatible JSON. 100 Mbps, 50 MB each."
        ),
    ),
    Scenario(
        name="ocx_parallel_4_processes",
        bandwidth_kbps=_BW_100MBPS,
        latency_ms=0,
        size_mb=50,
        concurrency=4,
        shape="shape2",
        cold=True,
        notes=(
            "Shape 2: 4 concurrent `ocx install <pkg>` processes via asyncio.TaskGroup; "
            "wall-clock measured; emits hyperfine-compatible JSON. 100 Mbps, 50 MB each."
        ),
    ),
    # --- Warm scenario (warm index, cold layers) ---
    Scenario(
        name="ocx_install_warm",
        bandwidth_kbps=0,  # Unthrottled — measures index-lookup vs network overhead
        latency_ms=0,
        size_mb=50,
        concurrency=1,
        shape="shape1",
        cold=False,  # Warm: index pre-populated, no cached layers
        notes=(
            "Warm index, cold layers, unthrottled; measures index-lookup overhead vs "
            "baseline. OCX_HOME preserved across runs (--prepare does NOT wipe it); "
            "index is pre-populated before first run. Page cache NOT dropped (warm-FS)."
        ),
    ),
]

# Sanity check: exactly 14 rows.
assert len(SCENARIOS) == 14, (
    f"Scenario matrix must have exactly 14 rows, got {len(SCENARIOS)}"
)

# Name uniqueness check.
_names = [s.name for s in SCENARIOS]
assert len(_names) == len(set(_names)), "Scenario names must be unique"
