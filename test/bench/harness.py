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
import uuid
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
from bench.scenarios import SCENARIOS, Scenario  # noqa: E402
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

DEFAULT_RUNS = 10
DEFAULT_WARMUP = 1

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------


@dataclasses.dataclass(slots=True)
class ScenarioResult:
    """Result from a single benchmark scenario run."""

    scenario_name: str
    mean_seconds: float
    stddev_seconds: float
    min_seconds: float
    max_seconds: float
    runs: int
    command: str


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
        """Apply bandwidth and/or latency toxics for a scenario."""
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

            with tempfile.TemporaryDirectory(prefix="bench-ocx-home-") as tmp_ocx_home:
                if scenario.shape == "floor":
                    # curl+tar baseline: use the first package's blob.
                    pkg_short = packages[0]
                    repo, tag = pkg_short.split(":", 1)
                    baseline_cmd = build_baseline_command(
                        self.registry_url, self.proxy_host, repo, tag
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

        return self._parse_hyperfine_result(scenario.name, export_path)

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
        stddev_s = (
            (sum((t - mean_s) ** 2 for t in timings) / len(timings)) ** 0.5
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
        )

        # Write hyperfine-compatible JSON.
        export_path = results_dir / f"{scenario.name}.json"
        self._write_hyperfine_json([result], export_path)
        return result

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
            # Cold: each run gets a fresh TemporaryDirectory.
            for _ in range(runs):
                with tempfile.TemporaryDirectory(
                    prefix="bench-ocx-home-parallel-"
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
        self, scenario_name: str, export_path: Path
    ) -> ScenarioResult:
        """Parse a hyperfine --export-json file and return a ScenarioResult."""
        data = json.loads(export_path.read_text())
        results = data.get("results", [])
        if not results:
            msg = f"hyperfine JSON for {scenario_name} has no results"
            raise RuntimeError(msg)
        entry = results[0]
        return ScenarioResult(
            scenario_name=scenario_name,
            mean_seconds=float(entry.get("mean", 0.0)),
            stddev_seconds=float(entry.get("stddev", 0.0)),
            min_seconds=float(entry.get("min", 0.0)),
            max_seconds=float(entry.get("max", 0.0)),
            runs=int(entry.get("times", [0]).__len__()),
            command=str(entry.get("command", "")),
        )

    def save_results(self, results: list[ScenarioResult], path: Path) -> None:
        """Write all scenario results as hyperfine-compatible JSON."""
        path.parent.mkdir(parents=True, exist_ok=True)
        self._write_hyperfine_json(results, path)

    @staticmethod
    def _write_hyperfine_json(results: list[ScenarioResult], path: Path) -> None:
        """Write results in hyperfine export-json format."""
        entries = []
        for r in results:
            entries.append(
                {
                    "command": r.scenario_name,
                    "mean": r.mean_seconds,
                    "stddev": r.stddev_seconds,
                    "median": r.mean_seconds,  # approximation when no raw times
                    "user": 0.0,
                    "system": 0.0,
                    "min": r.min_seconds,
                    "max": r.max_seconds,
                    "times": [r.mean_seconds] * r.runs,
                    "exit_codes": [0] * r.runs,
                    "parameters": {},
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


def _setup_bench_packages(
    ocx_binary: Path,
    registry_url: str,
    scenarios: list[Scenario],
) -> dict[str, list[str]]:
    """Push benchmark packages to the registry and return scenario → package list mapping.

    Packages are deduplicated by (size_mb, concurrency) key so each unique
    combination is only pushed once.
    """
    pushed: dict[tuple[int, int], list[str]] = {}
    scenario_packages: dict[str, list[str]] = {}

    with tempfile.TemporaryDirectory(prefix="bench-setup-ocx-home-") as tmp_home:
        runner = OcxRunner(ocx_binary, Path(tmp_home), registry_url)

        for scenario in scenarios:
            key = (scenario.size_mb, scenario.concurrency)
            if key not in pushed:
                pkg_list: list[str] = []
                n = max(1, scenario.concurrency)
                with tempfile.TemporaryDirectory(
                    prefix="bench-pkg-build-"
                ) as build_tmp:
                    for i in range(n):
                        repo = f"bench-{scenario.size_mb}mb-{uuid.uuid4().hex[:8]}-{i}"
                        tag = "1.0.0"
                        info = make_package(
                            runner,
                            repo,
                            tag,
                            Path(build_tmp),
                            size_mb=scenario.size_mb,
                            cascade=False,
                        )
                        pkg_list.append(info.short)
                pushed[key] = pkg_list

            scenario_packages[scenario.name] = pushed[key]

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
        default=DEFAULT_RUNS,
        help=f"Number of hyperfine runs per scenario (default: {DEFAULT_RUNS})",
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=DEFAULT_WARMUP,
        help=f"Number of hyperfine warmup runs (default: {DEFAULT_WARMUP})",
    )
    parser.add_argument(
        "--scenarios",
        nargs="*",
        help="Run only named scenarios (default: all 14)",
    )
    parser.add_argument(
        "--compare",
        action="store_true",
        help="Compare against baseline.json after running",
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
    selected = SCENARIOS
    if args.scenarios:
        name_set = set(args.scenarios)
        selected = [s for s in SCENARIOS if s.name in name_set]
        if not selected:
            print(
                f"ERROR: No scenarios matched {args.scenarios}. "
                "Available: " + ", ".join(s.name for s in SCENARIOS),
                file=sys.stderr,
            )
            return 2  # noqa: PLR2004

    # 5. Push packages.
    print(f"Pushing benchmark packages to {REGISTRY_URL}...")
    scenario_packages = _setup_bench_packages(ocx_binary, REGISTRY_URL, selected)

    # 6. Run scenarios.
    BENCH_RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    all_results: list[ScenarioResult] = []

    # Warm scenarios need a persistent OCX_HOME with pre-populated index.
    # We create one per unique (size_mb, concurrency) combination of warm scenarios
    # and keep it alive for the full bench session.
    warm_homes: list[tempfile.TemporaryDirectory[str]] = []
    warm_home_map: dict[str, Path] = {}  # scenario_name → warm OCX_HOME path

    try:
        for scenario in selected:
            if not scenario.cold:
                packages = scenario_packages.get(scenario.name, [])
                td = tempfile.TemporaryDirectory(prefix="bench-warm-home-")
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
            print(f"  Running scenario: {scenario.name} ...")
            if scenario.shape == "shape2":
                result = runner.parallel_install_wall_clock(
                    scenario=scenario,
                    packages=packages,
                    runs=args.runs,
                    results_dir=BENCH_RESULTS_DIR,
                    warm_ocx_home=warm_home_map.get(scenario.name),
                )
            else:
                result = runner.run_scenario(
                    scenario=scenario,
                    packages=packages,
                    warmup=args.warmup,
                    runs=args.runs,
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
        # 7. Teardown proxy.
        try:
            toxi.delete_proxy(PROXY_NAME)
        except RuntimeError:
            pass  # Best-effort teardown.

    # 8. Save combined results.
    latest_path = BENCH_RESULTS_DIR / "latest.json"
    runner.save_results(all_results, latest_path)
    print(f"\nResults saved to: {latest_path}")

    # 9. Optionally save as baseline.
    if capture_baseline:
        shutil.copy2(latest_path, BASELINE_JSON)
        print(f"Baseline saved to: {BASELINE_JSON}")

    # 10. Optionally compare against baseline.
    if args.compare and not capture_baseline and BASELINE_JSON.exists():
        baseline_data = json.loads(BASELINE_JSON.read_text())
        current_data = json.loads(latest_path.read_text())
        report = compare_against_baseline(baseline_data, current_data)
        from bench.compare import _format_report  # noqa: PLC0415

        print("\n" + _format_report(report))
        return 0 if report.passed else 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
