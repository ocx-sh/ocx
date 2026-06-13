"""Benchmark harness entry point for ocx install performance measurement.

Run as standalone script:
    uv run python bench/harness.py [--capture-baseline] [--runs N] [--warmup N]

Lifecycle (this module owns the session):
  1. Assert toxiproxy API (:8474) and registry (:5000) are reachable.
  2. Create toxiproxy proxy (0.0.0.0:5002 → registry:5000) idempotently.
  3. Push benchmark packages via make_package (size_mb per scenario).
  4. For each scenario: BenchRunner.run_scenario / parallel_install_wall_clock.
  5. Save results to BENCH_RESULTS_DIR/latest.json (hyperfine-compatible).
  6. If --capture-baseline: copy latest.json to bench/baseline.json.
  7. Tear down toxiproxy proxy.

BenchRunner owns per-scenario toxic setup/removal (bandwidth + latency toxics).
"""

from __future__ import annotations

import argparse
import asyncio
import dataclasses
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Path bootstrap — allow `python bench/harness.py` from test/ directory
# ---------------------------------------------------------------------------
_BENCH_DIR = Path(__file__).resolve().parent
_TEST_DIR = _BENCH_DIR.parent
if str(_TEST_DIR) not in sys.path:
    sys.path.insert(0, str(_TEST_DIR))

from bench.baseline import build_baseline_command  # noqa: E402
from bench.compare import compare_against_baseline  # noqa: E402
from bench.scenarios import (  # noqa: E402
    DEFAULT_RUNS,
    DEFAULT_WARMUP,
    SCENARIOS,
    SUITE_ORDER,
    Scenario,
)
from src.helpers import make_package  # noqa: E402
from src.runner import OcxRunner  # noqa: E402

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

TOXIPROXY_API_URL = os.environ.get("TOXIPROXY_API_URL", "http://localhost:8474")
REGISTRY_URL = os.environ.get("REGISTRY_URL", "localhost:5000")
PROXY_HOST = os.environ.get("PROXY_HOST", "localhost:5002")
# Inside the Docker network, toxiproxy reaches the registry service by name.
REGISTRY_UPSTREAM = os.environ.get("REGISTRY_UPSTREAM", "registry:5000")
PROXY_NAME = "bench-registry"

BENCH_RESULTS_DIR = Path(
    os.environ.get("BENCH_RESULTS_DIR", str(_BENCH_DIR / "results"))
)
BASELINE_JSON = _BENCH_DIR / "baseline.json"

# ---------------------------------------------------------------------------
# Scratch root — ALL harness temp state goes here, NEVER under /tmp.
#
# /tmp is tmpfs (RAM-backed) on WSL2 and many Linux systems.  Putting large
# package tarballs, OCX_HOME dirs, and hyperfine extract dirs there OOM-kills
# the VM under concurrent matrix runs.
#
# Default: <repo-root>/target/bench-tmp  (disk-backed, gitignored by target/).
# Override: set BENCH_TMPDIR env var to any writable disk-backed path.
# ---------------------------------------------------------------------------
_REPO_ROOT = _TEST_DIR.parent
BENCH_SCRATCH_DIR = Path(
    os.environ.get("BENCH_TMPDIR", str(_REPO_ROOT / "target" / "bench-tmp"))
)

# DEFAULT_RUNS and DEFAULT_WARMUP are imported from bench.scenarios.
# They are the global fallback values; per-scenario Scenario.runs/warmup fields take
# precedence unless the CLI --runs / --warmup flags override everything.

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------


@dataclasses.dataclass(slots=True)
class ScenarioResult:
    """Result from a single benchmark scenario run.

    Attributes
    ----------
    times:
        Per-run wall-clock values in seconds (real measured values).
        For hyperfine scenarios this is the actual ``times`` array from the
        hyperfine JSON export. For Shape-2 / parallel-curl scenarios this is
        the real per-iteration wall-clock list from asyncio.TaskGroup.
        Never mean-replicated — honest data for z-score statistics.
    suite:
        Suite tier this result was collected under ("small"|"medium"|"large").
    """

    scenario_name: str
    mean_seconds: float
    stddev_seconds: float
    min_seconds: float
    max_seconds: float
    runs: int
    command: str
    times: list[float] = dataclasses.field(default_factory=list)
    suite: str = "medium"


# ---------------------------------------------------------------------------
# Toxiproxy REST API client (stdlib urllib only — no new Python deps)
# ---------------------------------------------------------------------------


class ToxiproxyClient:
    """Minimal toxiproxy REST API client using stdlib urllib.

    All toxiproxy REST calls use JSON. Endpoints follow the toxiproxy v2 API:
    https://github.com/Shopify/toxiproxy#toxiproxy-api
    """

    def __init__(self, api_url: str) -> None:
        self._api = api_url.rstrip("/")

    def _request(
        self,
        method: str,
        path: str,
        data: dict[str, Any] | None = None,
    ) -> dict[str, Any] | list[Any]:
        """Send a JSON request to the toxiproxy API."""
        url = f"{self._api}{path}"
        body = json.dumps(data).encode() if data is not None else None
        req = urllib.request.Request(
            url,
            data=body,
            method=method,
            headers={"Content-Type": "application/json"} if body else {},
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                content = resp.read()
                return json.loads(content) if content else {}
        except urllib.error.HTTPError as e:
            body_text = e.read().decode(errors="replace")
            msg = f"toxiproxy {method} {url} → HTTP {e.code}: {body_text}"
            raise RuntimeError(msg) from e
        except urllib.error.URLError as e:
            msg = f"toxiproxy {method} {url} failed: {e}"
            raise RuntimeError(msg) from e

    def is_reachable(self) -> bool:
        """Return True if the toxiproxy API responds."""
        try:
            self._request("GET", "/version")
            return True
        except (RuntimeError, OSError):
            return False

    def list_proxies(self) -> dict[str, Any]:
        """Return all currently configured proxies."""
        result = self._request("GET", "/proxies")
        if isinstance(result, dict):
            return result
        return {}

    def create_proxy(self, name: str, listen: str, upstream: str) -> dict[str, Any]:
        """Create a proxy idempotently (no-op if already exists)."""
        existing = self.list_proxies()
        if name in existing:
            return existing[name]
        result = self._request(
            "POST",
            "/proxies",
            {"name": name, "listen": listen, "upstream": upstream, "enabled": True},
        )
        return result if isinstance(result, dict) else {}

    def delete_proxy(self, name: str) -> None:
        """Delete a proxy by name (no-op if not found)."""
        existing = self.list_proxies()
        if name not in existing:
            return
        self._request("DELETE", f"/proxies/{name}")

    def list_toxics(self, proxy_name: str) -> list[dict[str, Any]]:
        """List all toxics on a proxy."""
        result = self._request("GET", f"/proxies/{proxy_name}/toxics")
        if isinstance(result, list):
            return [item for item in result if isinstance(item, dict)]
        return []

    def add_bandwidth_toxic(
        self, proxy_name: str, rate_kbps: int, toxic_name: str = "bandwidth"
    ) -> None:
        """Add a bandwidth toxic that caps throughput to rate_kbps KB/s.

        toxiproxy bandwidth toxic `rate` is in KB/s.
        """
        self._request(
            "POST",
            f"/proxies/{proxy_name}/toxics",
            {
                "name": toxic_name,
                "type": "bandwidth",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": {"rate": rate_kbps},
            },
        )

    def add_latency_toxic(
        self, proxy_name: str, latency_ms: int, toxic_name: str = "latency"
    ) -> None:
        """Add a latency toxic that adds latency_ms ms one-way delay."""
        self._request(
            "POST",
            f"/proxies/{proxy_name}/toxics",
            {
                "name": toxic_name,
                "type": "latency",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": {"latency": latency_ms, "jitter": 0},
            },
        )

    def remove_toxic(self, proxy_name: str, toxic_name: str) -> None:
        """Remove a named toxic from a proxy (no-op if not found)."""
        existing_names = {t["name"] for t in self.list_toxics(proxy_name)}
        if toxic_name not in existing_names:
            return
        self._request("DELETE", f"/proxies/{proxy_name}/toxics/{toxic_name}")


# ---------------------------------------------------------------------------
# BenchRunner
# ---------------------------------------------------------------------------


class BenchRunner:
    """Runs benchmark scenarios with toxiproxy network conditioning.

    Owns per-scenario toxic setup and teardown. Session-scoped state
    (proxy creation, registry packages) is managed by the harness entry point.

    Invariant: run_scenario always tears down the toxic after the run,
    even on failure.
    """

    def __init__(
        self,
        ocx_binary: Path,
        registry_url: str,
        proxy_host: str,
        toxiproxy_api_url: str,
        hyperfine_binary: str = "hyperfine",
    ) -> None:
        self.ocx_binary = ocx_binary
        self.registry_url = registry_url
        self.proxy_host = proxy_host
        self._toxi = ToxiproxyClient(toxiproxy_api_url)
        self._hyperfine = hyperfine_binary

    def _ocx_env(self, ocx_home: str) -> dict[str, str]:
        """Build minimal env for ocx install via the bench proxy."""
        return {
            "OCX_HOME": ocx_home,
            "OCX_DEFAULT_REGISTRY": self.proxy_host,
            "OCX_INSECURE_REGISTRIES": self.proxy_host,
            "PATH": os.environ.get("PATH", ""),
            "HOME": os.environ.get("HOME", str(Path.home())),
        }

    def _apply_toxics(self, scenario: Scenario) -> None:
        """Apply bandwidth and/or latency toxics for a scenario.

        Always clears any stale toxics first (handles killed-run leftovers).
        """
        self._remove_toxics()
        if scenario.bandwidth_kbps > 0:
            self._toxi.add_bandwidth_toxic(PROXY_NAME, scenario.bandwidth_kbps)
        if scenario.latency_ms > 0:
            self._toxi.add_latency_toxic(PROXY_NAME, scenario.latency_ms)

    def _remove_toxics(self) -> None:
        """Remove all toxics from the bench proxy (best-effort teardown)."""
        try:
            for toxic in self._toxi.list_toxics(PROXY_NAME):
                name = toxic.get("name", "")
                if name:
                    self._toxi.remove_toxic(PROXY_NAME, name)
        except RuntimeError:
            pass  # Teardown is best-effort; do not mask original exception.

    def run_scenario(
        self,
        scenario: Scenario,
        packages: list[str],
        warmup: int = DEFAULT_WARMUP,
        runs: int = DEFAULT_RUNS,
        results_dir: Path = BENCH_RESULTS_DIR,
    ) -> ScenarioResult:
        """Apply toxiproxy config, run hyperfine for a scenario, return result.

        For floor (curl+tar) scenarios, uses baseline.build_baseline_command.
        For shape1 scenarios, uses `ocx install <pkgs>`.
        Shape 2 scenarios are handled separately by parallel_install_wall_clock.

        Parameters
        ----------
        scenario:
            Scenario definition from scenarios.py.
        packages:
            List of package identifiers to install (e.g. ["bench-pkg-50mb:1.0.0"]).
            For floor scenarios: the first package's layer blob is used.
        warmup:
            Number of hyperfine warmup runs (--warmup).
        runs:
            Minimum number of hyperfine runs (--min-runs).
        results_dir:
            Directory for hyperfine --export-json output.
        """
        results_dir.mkdir(parents=True, exist_ok=True)
        export_path = results_dir / f"{scenario.name}.json"

        try:
            self._apply_toxics(scenario)

            with tempfile.TemporaryDirectory(
                prefix="bench-ocx-home-", dir=BENCH_SCRATCH_DIR
            ) as tmp_ocx_home:
                if scenario.shape == "floor":
                    # curl+tar baseline: use the first package's blob.
                    pkg_short = packages[0]
                    repo, tag = pkg_short.split(":", 1)
                    baseline_cmd = build_baseline_command(
                        self.registry_url,
                        self.proxy_host,
                        repo,
                        tag,
                        scratch_dir=str(BENCH_SCRATCH_DIR / "curl-extract"),
                    )
                    bench_cmd = baseline_cmd.bench_cmd
                    prepare_cmd = baseline_cmd.prepare_cmd
                else:
                    # shape1: single `ocx package install <pkg_a> [pkg_b ...]` invocation.
                    pkgs_str = " ".join(packages)
                    env_str = " ".join(
                        f"{k}={v}"
                        for k, v in self._ocx_env(tmp_ocx_home).items()
                        if k not in ("PATH", "HOME")
                    )
                    bench_cmd = (
                        f"env {env_str} {self.ocx_binary} package install {pkgs_str}"
                    )
                    if scenario.cold:
                        # Cold scenario: wipe OCX_HOME before each run.
                        prepare_cmd = (
                            f"rm -rf {tmp_ocx_home} && mkdir -p {tmp_ocx_home}"
                        )
                    else:
                        # Warm scenario: index pre-populated by _warm_ocx_home() before
                        # hyperfine is invoked. Between runs, wipe packages/ and layers/
                        # only — keep the index (tags/) so each run starts with a warm
                        # index but no cached layers, matching spec §D4 "warm (index
                        # populated)" definition.
                        prepare_cmd = (
                            f"rm -rf {tmp_ocx_home}/packages "
                            f"{tmp_ocx_home}/layers "
                            f"{tmp_ocx_home}/symlinks "
                            f"{tmp_ocx_home}/temp"
                        )

                cmd: list[str] = [self._hyperfine]
                cmd += ["--min-runs", str(runs)]
                cmd += ["--warmup", str(warmup)]
                if prepare_cmd:
                    cmd += ["--prepare", prepare_cmd]
                cmd += ["--export-json", str(export_path)]
                cmd += ["--command-name", scenario.name]
                cmd += [bench_cmd]

                subprocess.run(cmd, check=True)  # noqa: S603

        finally:
            self._remove_toxics()

        return self._parse_hyperfine_result(
            scenario.name, export_path, suite=scenario.suite
        )

    def parallel_curl_wall_clock(
        self,
        scenario: Scenario,
        packages: list[str],
        runs: int = DEFAULT_RUNS,
        results_dir: Path = BENCH_RESULTS_DIR,
    ) -> ScenarioResult:
        """Floor shape with concurrency > 1: fire N concurrent curl | tar -xJ processes.

        Each package in `packages` maps to one curl+tar process (cold runs only —
        no warm-state concept for floor scenarios). Wall-clock measured from first
        launch to last completion. Emits hyperfine-compatible JSON.

        Parameters
        ----------
        scenario:
            Must be shape "floor" with concurrency > 1. The blob URL for each
            package is resolved from the registry manifest before timing begins.
        packages:
            List of package short identifiers ("repo:tag"), one per concurrent process.
        runs:
            Number of wall-clock measurement repetitions.
        results_dir:
            Directory for the emitted hyperfine-compatible JSON.
        """
        if scenario.shape != "floor" or scenario.concurrency < 2:  # noqa: PLR2004
            msg = (
                f"parallel_curl_wall_clock is for floor scenarios with concurrency>=2; "
                f"got shape={scenario.shape!r} concurrency={scenario.concurrency}"
            )
            raise ValueError(msg)

        results_dir.mkdir(parents=True, exist_ok=True)

        # Resolve blob URLs for each package before timing begins.
        curl_cmds: list[str] = []
        prepare_cmds: list[str] = []
        for pkg_short in packages:
            repo, tag = pkg_short.split(":", 1)
            baseline_cmd = build_baseline_command(
                self.registry_url,
                self.proxy_host,
                repo,
                tag,
                scratch_dir=str(BENCH_SCRATCH_DIR / f"curl-extract-{repo}"),
            )
            curl_cmds.append(baseline_cmd.bench_cmd)
            prepare_cmds.append(baseline_cmd.prepare_cmd)

        try:
            self._apply_toxics(scenario)
            timings = asyncio.run(
                self._run_parallel_commands(curl_cmds, runs, prepare_cmds=prepare_cmds)
            )
        finally:
            self._remove_toxics()

        if not timings:
            msg = f"No timing measurements collected for {scenario.name}"
            raise RuntimeError(msg)

        mean_s = sum(timings) / len(timings)
        # Sample stddev (÷ n−1) to match hyperfine's convention so the dashboard's
        # Welch z-score consumes both sources with a consistent formula.
        # Guard len < 2 to avoid ZeroDivisionError on single-run scenarios.
        stddev_s = (
            (sum((t - mean_s) ** 2 for t in timings) / (len(timings) - 1)) ** 0.5
            if len(timings) > 1
            else 0.0
        )
        result = ScenarioResult(
            scenario_name=scenario.name,
            mean_seconds=mean_s,
            stddev_seconds=stddev_s,
            min_seconds=min(timings),
            max_seconds=max(timings),
            runs=len(timings),
            command=f"parallel({len(curl_cmds)}x curl|tar via asyncio.TaskGroup)",
            times=timings,
            suite=scenario.suite,
        )

        export_path = results_dir / f"{scenario.name}.json"
        self._write_hyperfine_json([result], export_path)
        return result

    def parallel_install_wall_clock(
        self,
        scenario: Scenario,
        packages: list[str],
        runs: int = DEFAULT_RUNS,
        results_dir: Path = BENCH_RESULTS_DIR,
        warm_ocx_home: Path | None = None,
    ) -> ScenarioResult:
        """Shape 2: fire N concurrent `ocx package install <pkg>` processes, wall-clock.

        Uses asyncio.TaskGroup to launch N ocx install processes concurrently
        and measures wall-clock time from first launch to last completion.
        Emits hyperfine-compatible JSON for unified pipeline.

        Parameters
        ----------
        scenario:
            Must be shape2.
        packages:
            List of package identifiers; one process is spawned per package.
        runs:
            Number of wall-clock measurement repetitions.
        results_dir:
            Directory for the emitted hyperfine-compatible JSON.
        warm_ocx_home:
            For cold=False (warm) scenarios, the pre-populated OCX_HOME directory to
            reuse across runs. Between runs, packages/layers/symlinks/temp are wiped
            but tags/ (index) is preserved — matching spec §D4 warm definition.
            Ignored for cold=True scenarios (fresh TemporaryDirectory per run).
        """
        if scenario.shape != "shape2":
            msg = f"parallel_install_wall_clock is for shape2 scenarios; got {scenario.shape}"
            raise ValueError(msg)

        results_dir.mkdir(parents=True, exist_ok=True)

        try:
            self._apply_toxics(scenario)
            timings = asyncio.run(
                self._run_parallel_wall_clock(scenario, packages, runs, warm_ocx_home)
            )
        finally:
            self._remove_toxics()

        if not timings:
            msg = f"No timing measurements collected for {scenario.name}"
            raise RuntimeError(msg)

        mean_s = sum(timings) / len(timings)
        # Sample stddev (÷ n−1) to match hyperfine's convention so the dashboard's
        # Welch z-score consumes both sources with a consistent formula.
        # Guard len < 2 to avoid ZeroDivisionError on single-run scenarios.
        stddev_s = (
            (sum((t - mean_s) ** 2 for t in timings) / (len(timings) - 1)) ** 0.5
            if len(timings) > 1
            else 0.0
        )
        result = ScenarioResult(
            scenario_name=scenario.name,
            mean_seconds=mean_s,
            stddev_seconds=stddev_s,
            min_seconds=min(timings),
            max_seconds=max(timings),
            runs=len(timings),
            command=f"parallel({len(packages)}x ocx package install via asyncio.TaskGroup)",
            times=timings,
            suite=scenario.suite,
        )

        # Write hyperfine-compatible JSON.
        export_path = results_dir / f"{scenario.name}.json"
        self._write_hyperfine_json([result], export_path)
        return result

    async def _run_parallel_commands(
        self,
        shell_cmds: list[str],
        runs: int,
        prepare_cmds: list[str] | None = None,
    ) -> list[float]:
        """Run N concurrent shell commands repeatedly; return wall-clock timings per run.

        Each element of shell_cmds is a full shell command string (e.g. a curl | tar
        pipeline). Commands are spawned via asyncio.create_subprocess_shell so shell
        operators (pipes) work correctly. All N commands run concurrently via
        asyncio.TaskGroup; wall-clock is from first launch to last completion.

        prepare_cmds run UNTIMED before every timed iteration (hyperfine --prepare
        equivalent) — e.g. wiping and recreating the curl extract directories that
        the timed pipelines write into.
        """
        timings: list[float] = []
        for _ in range(runs):
            if prepare_cmds:
                async with asyncio.TaskGroup() as tg:
                    for cmd in prepare_cmds:
                        tg.create_task(self._run_single_shell_command(cmd))
            t_start = time.perf_counter()
            async with asyncio.TaskGroup() as tg:
                for cmd in shell_cmds:
                    tg.create_task(self._run_single_shell_command(cmd))
            timings.append(time.perf_counter() - t_start)
        return timings

    async def _run_single_shell_command(self, cmd: str) -> None:
        """Run a single shell command string as an asyncio subprocess."""
        proc = await asyncio.create_subprocess_shell(
            cmd,
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await proc.communicate()
        if proc.returncode != 0:
            msg = (
                f"shell command failed (rc={proc.returncode}): "
                f"{cmd!r}: {stderr.decode(errors='replace').strip()}"
            )
            raise RuntimeError(msg)

    async def _run_parallel_wall_clock(
        self,
        scenario: Scenario,
        packages: list[str],
        runs: int,
        warm_ocx_home: Path | None = None,
    ) -> list[float]:
        """Run N concurrent installs repeatedly; return wall-clock timings per run."""
        timings: list[float] = []

        if scenario.cold:
            # Cold: each run gets a fresh disk-backed TemporaryDirectory.
            for _ in range(runs):
                with tempfile.TemporaryDirectory(
                    prefix="bench-ocx-home-parallel-", dir=BENCH_SCRATCH_DIR
                ) as tmp_ocx_home:
                    env = {
                        **os.environ,
                        "OCX_HOME": tmp_ocx_home,
                        "OCX_DEFAULT_REGISTRY": self.proxy_host,
                        "OCX_INSECURE_REGISTRIES": self.proxy_host,
                    }
                    t_start = time.perf_counter()
                    async with asyncio.TaskGroup() as tg:
                        for pkg in packages:
                            tg.create_task(self._run_single_install(pkg, env))
                    timings.append(time.perf_counter() - t_start)
        else:
            # Warm: reuse the pre-populated OCX_HOME; wipe packages/layers between runs
            # but preserve tags/ (index) — spec §D4 "warm (index populated)".
            if warm_ocx_home is None:
                msg = "warm_ocx_home must be provided for cold=False shape2 scenarios"
                raise ValueError(msg)
            ocx_home_str = str(warm_ocx_home)
            env = {
                **os.environ,
                "OCX_HOME": ocx_home_str,
                "OCX_DEFAULT_REGISTRY": self.proxy_host,
                "OCX_INSECURE_REGISTRIES": self.proxy_host,
            }
            _WARM_WIPE_DIRS = ("packages", "layers", "symlinks", "temp")
            for _ in range(runs):
                # Wipe installed state; preserve index (tags/).
                for subdir in _WARM_WIPE_DIRS:
                    p = warm_ocx_home / subdir
                    if p.exists():
                        shutil.rmtree(p)
                t_start = time.perf_counter()
                async with asyncio.TaskGroup() as tg:
                    for pkg in packages:
                        tg.create_task(self._run_single_install(pkg, env))
                timings.append(time.perf_counter() - t_start)

        return timings

    async def _run_single_install(self, package: str, env: dict[str, str]) -> None:
        """Run a single `ocx package install <package>` as an asyncio subprocess."""
        proc = await asyncio.create_subprocess_exec(
            str(self.ocx_binary),
            "package",
            "install",
            package,
            env=env,
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await proc.communicate()
        if proc.returncode != 0:
            msg = (
                f"ocx package install {package} failed (rc={proc.returncode}): "
                f"{stderr.decode(errors='replace').strip()}"
            )
            raise RuntimeError(msg)

    def _parse_hyperfine_result(
        self, scenario_name: str, export_path: Path, suite: str = "medium"
    ) -> ScenarioResult:
        """Parse a hyperfine --export-json file and return a ScenarioResult."""
        data = json.loads(export_path.read_text())
        results = data.get("results", [])
        if not results:
            msg = f"hyperfine JSON for {scenario_name} has no results"
            raise RuntimeError(msg)
        entry = results[0]
        # Capture the real per-run times array from hyperfine — never replicate mean.
        raw_times = [float(t) for t in entry.get("times", [])]
        return ScenarioResult(
            scenario_name=scenario_name,
            mean_seconds=float(entry.get("mean", 0.0)),
            stddev_seconds=float(entry.get("stddev", 0.0)),
            min_seconds=float(entry.get("min", 0.0)),
            max_seconds=float(entry.get("max", 0.0)),
            runs=len(raw_times),
            command=str(entry.get("command", "")),
            times=raw_times,
            suite=suite,
        )

    def save_results(self, results: list[ScenarioResult], path: Path) -> None:
        """Write all scenario results as hyperfine-compatible JSON."""
        path.parent.mkdir(parents=True, exist_ok=True)
        self._write_hyperfine_json(results, path)

    @staticmethod
    def _write_hyperfine_json(results: list[ScenarioResult], path: Path) -> None:
        """Write results in hyperfine export-json format.

        Always writes the real ``times`` array — never mean-replicated.
        For Shape-2 and parallel-curl scenarios the harness stores real per-iteration
        wall-clock values so z-score statistics remain honest.
        """
        entries = []
        for r in results:
            # Use real times if available; empty list is acceptable (dashboard handles it).
            times_arr = r.times if r.times else []
            entries.append(
                {
                    "command": r.scenario_name,
                    "mean": r.mean_seconds,
                    "stddev": r.stddev_seconds,
                    "median": r.mean_seconds,
                    "user": 0.0,
                    "system": 0.0,
                    "min": r.min_seconds,
                    "max": r.max_seconds,
                    "times": times_arr,
                    "exit_codes": [0] * len(times_arr),
                    "parameters": {},
                    "suite": r.suite,
                }
            )
        path.write_text(json.dumps({"results": entries}, indent=2))


# ---------------------------------------------------------------------------
# Session lifecycle helpers
# ---------------------------------------------------------------------------


def _assert_reachable(url: str, label: str, timeout: int = 5) -> None:
    """Assert that a URL responds with HTTP 2xx/4xx, fail fast with clear message."""
    for attempt in range(timeout * 2):
        try:
            with urllib.request.urlopen(url, timeout=2):
                return
        except urllib.error.HTTPError:
            # Any HTTP error means the service is up; 4xx is fine.
            return
        except (urllib.error.URLError, OSError):
            if attempt < timeout * 2 - 1:
                time.sleep(0.5)
    sys.exit(
        f"ERROR: {label} at {url} is not reachable.\n"
        "Run `task bench:setup` to start the bench Docker profile.\n"
        "If already running, check `docker compose --profile bench ps`."
    )


def _find_hyperfine() -> str:
    """Locate the hyperfine binary: PATH first, then test/bin/."""
    if shutil.which("hyperfine"):
        return "hyperfine"
    local_bin = _TEST_DIR / "bin" / "hyperfine"
    if local_bin.exists():
        return str(local_bin)
    sys.exit(
        "ERROR: hyperfine not found on PATH or in test/bin/.\n"
        "Run `task .bench:acquire-hyperfine` to download it."
    )


def _find_ocx_binary() -> Path:
    """Locate the ocx binary from OCX_COMMAND env or test/bin/."""
    if env_path := os.environ.get("OCX_COMMAND"):
        p = Path(env_path)
        if p.exists():
            return p
    default = _TEST_DIR / "bin" / "ocx"
    if default.exists():
        return default
    sys.exit(
        "ERROR: ocx binary not found.\n"
        "Set OCX_COMMAND env var or run `task build` to build it."
    )


def _create_proxy_idempotent(toxi: ToxiproxyClient) -> None:
    """Create the bench proxy if it doesn't exist.

    Proxy: listen on 0.0.0.0:5002 → upstream registry:5000 (Docker network).
    """
    toxi.create_proxy(
        name=PROXY_NAME,
        listen="0.0.0.0:5002",
        upstream=REGISTRY_UPSTREAM,
    )


def _warm_ocx_home(
    ocx_binary: Path,
    ocx_home: Path,
    proxy_host: str,
    packages: list[str],
) -> None:
    """Pre-populate the OCX index for a warm scenario.

    Runs `ocx index update <repo>` for each package in the given OCX_HOME via
    the proxy endpoint. This populates tags/ (the local index) without downloading
    layers. The warm --prepare command then wipes packages/layers/symlinks/temp
    but leaves tags/ intact, so each hyperfine run starts with a populated index
    but no cached layers — spec §D4 "warm (index populated)" definition.

    Parameters
    ----------
    ocx_binary:
        Path to the ocx binary.
    ocx_home:
        The OCX_HOME directory to pre-populate.
    proxy_host:
        Proxy endpoint (e.g. "localhost:5002") used as OCX_DEFAULT_REGISTRY.
    packages:
        List of package short identifiers (e.g. ["bench-50mb-abc:1.0.0"]).
    """
    env = {
        "OCX_HOME": str(ocx_home),
        "OCX_DEFAULT_REGISTRY": proxy_host,
        "OCX_INSECURE_REGISTRIES": proxy_host,
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", str(Path.home())),
    }
    for pkg_short in packages:
        repo = pkg_short.split(":")[0]
        subprocess.run(  # noqa: S603
            [str(ocx_binary), "index", "update", repo],
            env=env,
            check=True,
            capture_output=True,
        )


def _probe_manifest(registry_url: str, repo: str, tag: str) -> bool:
    """Return True if the manifest for repo:tag exists in the registry.

    Uses a HEAD request against the OCI manifests endpoint.  A 200 response
    means the fixture is already present and can be reused without re-pushing.
    A 404 (or any error) means we need to push.
    """
    url = f"http://{registry_url}/v2/{repo}/manifests/{tag}"
    req = urllib.request.Request(url, method="HEAD")
    req.add_header(
        "Accept",
        "application/vnd.oci.image.manifest.v1+json,"
        "application/vnd.oci.image.index.v1+json",
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return resp.status == 200  # noqa: PLR2004
    except (urllib.error.HTTPError, urllib.error.URLError, OSError):
        return False


def _fixture_repo_name(size_mb: int, n_layers: int, idx: int) -> str:
    """Deterministic fixture repo name — stable across harness invocations.

    bench-<size>mb-<layers>l-<idx>

    Using deterministic names (no UUID) lets the harness skip re-pushing
    fixtures that are already present in the registry from a previous run.
    Bench repos are isolated from test repos by the ``bench-`` prefix.
    Trade-off: shared registry state between sessions; acceptable here because
    bench fixtures are content-addressed by size+layers (immutable semantics).
    """
    return f"bench-{size_mb}mb-{n_layers}l-{idx}"


def _setup_bench_packages(
    ocx_binary: Path,
    registry_url: str,
    scenarios: list[Scenario],
    scratch_dir: Path,
) -> dict[str, list[str]]:
    """Push benchmark packages and return scenario → package list mapping.

    Packages are deduplicated by (size_mb, n_packages, n_layers) key so each
    unique combination is only pushed once per session.

    Deterministic repo names (no UUID) allow reusing fixtures across harness
    invocations: a manifest HEAD probe skips push when already present.  This
    makes repeated `bench:quick` runs substantially faster (~5–10 s saved per
    already-present fixture key).

    All temp dirs are created under ``scratch_dir`` (disk-backed) to avoid
    RAM exhaustion on WSL2 where /tmp is tmpfs.
    """
    # Map (size_mb, n_packages, n_layers) → list of pushed package shorts.
    resolved: dict[tuple[int, int, int], list[str]] = {}
    scenario_packages: dict[str, list[str]] = {}

    with tempfile.TemporaryDirectory(
        prefix="bench-setup-home-", dir=scratch_dir
    ) as tmp_home:
        runner = OcxRunner(ocx_binary, Path(tmp_home), registry_url)

        for scenario in scenarios:
            n_packages = max(1, scenario.concurrency)
            n_layers = max(1, scenario.layers)
            key = (scenario.size_mb, n_packages, n_layers)

            if key not in resolved:
                pkg_list: list[str] = []
                with tempfile.TemporaryDirectory(
                    prefix="bench-pkg-build-", dir=scratch_dir
                ) as build_tmp:
                    for i in range(n_packages):
                        repo = _fixture_repo_name(scenario.size_mb, n_layers, i)
                        tag = "1.0.0"
                        short = f"{repo}:{tag}"

                        if _probe_manifest(registry_url, repo, tag):
                            # Fixture already present — register with index only.
                            runner.plain("index", "update", repo)
                            print(f"    fixture reused: {short}")
                        else:
                            make_package(
                                runner,
                                repo,
                                tag,
                                Path(build_tmp),
                                size_mb=scenario.size_mb,
                                cascade=False,
                                layers=n_layers,
                            )
                            print(f"    fixture pushed: {short}")
                        pkg_list.append(short)
                resolved[key] = pkg_list

            scenario_packages[scenario.name] = resolved[key]

    return scenario_packages


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="OCX install performance benchmark harness"
    )
    parser.add_argument(
        "--capture-baseline",
        action="store_true",
        help="Save results as the reference baseline (test/bench/baseline.json)",
    )
    # Alias used by spec/taskfile: --save-baseline
    parser.add_argument(
        "--save-baseline",
        action="store_true",
        help="Alias for --capture-baseline",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=None,
        help=(
            "Override hyperfine --min-runs for ALL scenarios "
            "(default: per-scenario value from scenarios.py)"
        ),
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=None,
        help=(
            "Override hyperfine --warmup for ALL scenarios "
            "(default: per-scenario value from scenarios.py)"
        ),
    )
    parser.add_argument(
        "--scenarios",
        nargs="*",
        help=f"Run only named scenarios (default: all {len(SCENARIOS)})",
    )
    parser.add_argument(
        "--compare",
        action="store_true",
        help="Compare against baseline.json after running",
    )
    parser.add_argument(
        "--suite",
        choices=["small", "medium", "large"],
        default=None,
        help=(
            "Run scenarios for the given suite tier (cumulative: large ⊇ medium ⊇ small). "
            "small=<1 min, medium=<4 min (default gate), large=<10 min. "
            "Takes precedence over --scenarios when both supplied."
        ),
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:  # noqa: PLR0912,PLR0915
    """Harness entry point. Returns process exit code."""
    args = _parse_args(argv)
    capture_baseline = args.capture_baseline or args.save_baseline

    # 1. Verify services are up.
    _assert_reachable(f"http://{REGISTRY_URL}/v2/", "Registry (localhost:5000)")
    _assert_reachable(f"{TOXIPROXY_API_URL}/version", "toxiproxy API (localhost:8474)")

    # 2. Create proxy idempotently.
    toxi = ToxiproxyClient(TOXIPROXY_API_URL)
    _create_proxy_idempotent(toxi)

    # 3. Locate binaries.
    ocx_binary = _find_ocx_binary()
    hyperfine = _find_hyperfine()

    runner = BenchRunner(
        ocx_binary=ocx_binary,
        registry_url=REGISTRY_URL,
        proxy_host=PROXY_HOST,
        toxiproxy_api_url=TOXIPROXY_API_URL,
        hyperfine_binary=hyperfine,
    )

    # 4. Select scenarios.
    # --suite takes precedence; cumulative: large runs all, medium runs small+medium, etc.
    effective_suite = args.suite or "medium"
    suite_idx = SUITE_ORDER.index(effective_suite)
    suites_in_tier = set(SUITE_ORDER[: suite_idx + 1])

    if args.suite:
        selected = [s for s in SCENARIOS if s.suite in suites_in_tier]
    elif args.scenarios:
        name_set = set(args.scenarios)
        selected = [s for s in SCENARIOS if s.name in name_set]
        if not selected:
            print(
                f"ERROR: No scenarios matched {args.scenarios}. "
                f"Available ({len(SCENARIOS)}): "
                + ", ".join(s.name for s in SCENARIOS),
                file=sys.stderr,
            )
            return 2  # noqa: PLR2004
    else:
        # Default: medium suite (backward compat — same as before suite flag existed).
        selected = [s for s in SCENARIOS if s.suite in {"small", "medium"}]

    # 5. Create scratch root — ALL session temp state lives here (disk-backed).
    #    Wiped entirely at session end so peak disk usage stays bounded.
    BENCH_SCRATCH_DIR.mkdir(parents=True, exist_ok=True)

    try:
        # 6. Push (or reuse) packages — only for the selected scenarios.
        print(f"Setting up benchmark fixtures for {REGISTRY_URL}...")
        scenario_packages = _setup_bench_packages(
            ocx_binary, REGISTRY_URL, selected, BENCH_SCRATCH_DIR
        )

        # 7. Run scenarios.
        BENCH_RESULTS_DIR.mkdir(parents=True, exist_ok=True)
        all_results: list[ScenarioResult] = []

        # Warm scenarios need a persistent OCX_HOME with pre-populated index.
        # Keep it alive for the full bench session; cleaned up in the finally block.
        warm_homes: list[tempfile.TemporaryDirectory[str]] = []
        warm_home_map: dict[str, Path] = {}  # scenario_name → warm OCX_HOME path

        try:
            for scenario in selected:
                if not scenario.cold:
                    packages = scenario_packages.get(scenario.name, [])
                    td = tempfile.TemporaryDirectory(
                        prefix="bench-warm-home-", dir=BENCH_SCRATCH_DIR
                    )
                    warm_homes.append(td)
                    warm_path = Path(td.name)
                    print(
                        f"  Pre-populating warm index for: {scenario.name} "
                        f"({len(packages)} packages)..."
                    )
                    _warm_ocx_home(ocx_binary, warm_path, PROXY_HOST, packages)
                    warm_home_map[scenario.name] = warm_path

            for scenario in selected:
                packages = scenario_packages.get(scenario.name, [])
                # CLI --runs / --warmup override per-scenario values; absent = use scenario defaults.
                effective_runs = args.runs if args.runs is not None else scenario.runs
                effective_warmup = (
                    args.warmup if args.warmup is not None else scenario.warmup
                )
                print(
                    f"  Running scenario: {scenario.name} "
                    f"(runs={effective_runs}, warmup={effective_warmup}) ..."
                )
                if scenario.shape == "shape2":
                    result = runner.parallel_install_wall_clock(
                        scenario=scenario,
                        packages=packages,
                        runs=effective_runs,
                        results_dir=BENCH_RESULTS_DIR,
                        warm_ocx_home=warm_home_map.get(scenario.name),
                    )
                elif scenario.shape == "floor" and scenario.concurrency > 1:
                    # Parallel curl floor: N concurrent curl|tar processes, wall-clock.
                    result = runner.parallel_curl_wall_clock(
                        scenario=scenario,
                        packages=packages,
                        runs=effective_runs,
                        results_dir=BENCH_RESULTS_DIR,
                    )
                else:
                    result = runner.run_scenario(
                        scenario=scenario,
                        packages=packages,
                        warmup=effective_warmup,
                        runs=effective_runs,
                        results_dir=BENCH_RESULTS_DIR,
                    )
                all_results.append(result)
                print(
                    f"    {scenario.name}: mean={result.mean_seconds:.3f}s "
                    f"stddev={result.stddev_seconds:.3f}s"
                )
        finally:
            # Clean up warm home directories.
            for td in warm_homes:
                try:
                    td.cleanup()
                except OSError:
                    pass
            # 8. Teardown proxy.
            try:
                toxi.delete_proxy(PROXY_NAME)
            except RuntimeError:
                pass  # Best-effort teardown.

    finally:
        # 9. Wipe scratch root entirely — frees all per-session disk usage.
        #    Best-effort: do not mask exceptions from the bench run itself.
        try:
            if BENCH_SCRATCH_DIR.exists():
                shutil.rmtree(BENCH_SCRATCH_DIR)
        except OSError:
            pass  # Non-fatal — files on disk are gitignored under target/.

    # 10. Save per-suite results + latest.json (backward compat symlink/copy).
    BENCH_RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    suite_results_path = BENCH_RESULTS_DIR / f"latest-{effective_suite}.json"
    runner.save_results(all_results, suite_results_path)
    print(f"\nResults saved to: {suite_results_path}")

    # latest.json = copy of the most recent run (any suite) for backward compat.
    latest_path = BENCH_RESULTS_DIR / "latest.json"
    shutil.copy2(suite_results_path, latest_path)

    # 11. Optionally save as baseline.
    if capture_baseline:
        shutil.copy2(suite_results_path, BASELINE_JSON)
        print(f"Baseline saved to: {BASELINE_JSON}")

    # 12. Optionally compare against baseline.
    if args.compare and not capture_baseline and BASELINE_JSON.exists():
        baseline_data = json.loads(BASELINE_JSON.read_text())
        current_data = json.loads(suite_results_path.read_text())
        report = compare_against_baseline(baseline_data, current_data)
        from bench.compare import _format_report  # noqa: PLC0415

        print("\n" + _format_report(report))
        return 0 if report.passed else 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
