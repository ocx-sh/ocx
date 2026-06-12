"""Smoke-validation tests for the bench harness (no Docker/toxiproxy/hyperfine required).

These tests validate the harness contracts purely in Python — no network, no
subprocess, no registry. They run as part of `uv run pytest` alongside the
acceptance tests and must stay green without the bench Docker profile.

Coverage:
- Scenario matrix: exactly 14 rows, correct shape distribution, specific rows present.
- compare_against_baseline: pure-function behavior, pass/fail threshold semantics,
  threshold edge case (ratio == 1/threshold boundary), missing scenarios.
- ScenarioResult / CompareReport dataclass fields.
- BaselineCommand string shape from baseline.build_baseline_command (mocked registry).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

# ---------------------------------------------------------------------------
# sys.path bootstrap (bench/ is not under testpaths but is importable from test/)
# ---------------------------------------------------------------------------
_TEST_DIR = Path(__file__).resolve().parent.parent
if str(_TEST_DIR) not in sys.path:
    sys.path.insert(0, str(_TEST_DIR))


# ---------------------------------------------------------------------------
# Scenario matrix
# ---------------------------------------------------------------------------


class TestScenarioMatrix:
    def test_exactly_14_rows(self):
        from bench.scenarios import SCENARIOS

        assert len(SCENARIOS) == 14, f"Expected 14 scenarios, got {len(SCENARIOS)}"  # noqa: PLR2004

    def test_unique_names(self):
        from bench.scenarios import SCENARIOS

        names = [s.name for s in SCENARIOS]
        assert len(names) == len(set(names)), "Scenario names must be unique"

    def test_floor_shape_count(self):
        from bench.scenarios import SCENARIOS

        floor = [s for s in SCENARIOS if s.shape == "floor"]
        assert len(floor) == 2, f"Expected 2 floor scenarios, got {len(floor)}"  # noqa: PLR2004

    def test_shape1_count(self):
        from bench.scenarios import SCENARIOS

        shape1 = [s for s in SCENARIOS if s.shape == "shape1"]
        assert len(shape1) == 10, f"Expected 10 shape1 scenarios, got {len(shape1)}"  # noqa: PLR2004

    def test_shape2_count(self):
        from bench.scenarios import SCENARIOS

        shape2 = [s for s in SCENARIOS if s.shape == "shape2"]
        assert len(shape2) == 2, f"Expected 2 shape2 scenarios, got {len(shape2)}"  # noqa: PLR2004

    def test_required_scenario_names_present(self):
        from bench.scenarios import SCENARIOS

        required = {
            "baseline_curl_10mbps",
            "baseline_curl_100mbps",
            "ocx_install_10mbps",
            "ocx_install_100mbps",
            "ocx_install_high_bandwidth",
            "ocx_install_unthrottled",
            "ocx_install_latency_50ms",
            "ocx_install_small_10mbps",
            "ocx_install_large_10mbps",
            "ocx_parallel_2_100mbps",
            "ocx_parallel_4_100mbps",
            "ocx_parallel_2_processes",
            "ocx_parallel_4_processes",
            "ocx_install_warm",
        }
        names = {s.name for s in SCENARIOS}
        missing = required - names
        assert not missing, f"Missing required scenarios: {missing}"

    def test_parallel_rows_have_correct_concurrency(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_parallel_2_100mbps"].concurrency == 2  # noqa: PLR2004
        assert by_name["ocx_parallel_4_100mbps"].concurrency == 4  # noqa: PLR2004
        assert by_name["ocx_parallel_2_processes"].concurrency == 2  # noqa: PLR2004
        assert by_name["ocx_parallel_4_processes"].concurrency == 4  # noqa: PLR2004

    def test_parallel_2_is_shape1(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_parallel_2_100mbps"].shape == "shape1"
        assert by_name["ocx_parallel_4_100mbps"].shape == "shape1"

    def test_parallel_processes_is_shape2(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_parallel_2_processes"].shape == "shape2"
        assert by_name["ocx_parallel_4_processes"].shape == "shape2"

    def test_warm_scenario_cold_false(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_install_warm"].cold is False

    def test_all_other_scenarios_cold_true(self):
        from bench.scenarios import SCENARIOS

        warm_exceptions = {"ocx_install_warm"}
        cold_violations = [
            s.name for s in SCENARIOS if s.name not in warm_exceptions and not s.cold
        ]
        assert not cold_violations, (
            f"Unexpected cold=False scenarios: {cold_violations}"
        )

    def test_floor_scenarios_shape_floor(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["baseline_curl_10mbps"].shape == "floor"
        assert by_name["baseline_curl_100mbps"].shape == "floor"

    def test_size_mb_values_present(self):
        from bench.scenarios import SCENARIOS

        sizes = {s.size_mb for s in SCENARIOS}
        assert 5 in sizes, "5 MB scenario missing"  # noqa: PLR2004
        assert 50 in sizes, "50 MB scenario missing"  # noqa: PLR2004
        assert 200 in sizes, "200 MB scenario missing"  # noqa: PLR2004

    def test_latency_scenario_has_50ms(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_install_latency_50ms"].latency_ms == 50  # noqa: PLR2004

    def test_unthrottled_has_zero_bandwidth(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_install_unthrottled"].bandwidth_kbps == 0


# ---------------------------------------------------------------------------
# compare_against_baseline — pure function
# ---------------------------------------------------------------------------


def _make_result(name: str, mean: float) -> dict:
    return {
        "command": name,
        "mean": mean,
        "stddev": 0.5,
        "median": mean,
        "min": mean - 0.5,
        "max": mean + 0.5,
        "times": [mean],
        "exit_codes": [0],
    }


class TestCompareAgainstBaseline:
    def test_improvement_passes(self):
        from bench.compare import compare_against_baseline

        baseline = {"results": [_make_result("scenario_a", 45.0)]}
        current = {"results": [_make_result("scenario_a", 29.0)]}  # 0.64x — improvement
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert report.passed
        assert report.scenarios[0].passed
        assert report.scenarios[0].ratio < 1.0

    def test_regression_fails(self):
        from bench.compare import compare_against_baseline

        baseline = {"results": [_make_result("scenario_a", 45.0)]}
        current = {"results": [_make_result("scenario_a", 60.0)]}  # 1.33x — regression
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert not report.passed
        assert not report.scenarios[0].passed
        assert report.scenarios[0].ratio > 1.0 / 0.85

    def test_slight_regression_within_threshold_passes(self):
        """current slightly slower than baseline but within 1/threshold gate."""
        from bench.compare import compare_against_baseline

        # threshold=0.85 → gate = 1/0.85 ≈ 1.176. current at 1.10x → should pass.
        baseline = {"results": [_make_result("scenario_a", 10.0)]}
        current = {"results": [_make_result("scenario_a", 11.0)]}  # 1.10x < 1.176
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert report.passed
        assert report.scenarios[0].ratio == pytest.approx(1.10, rel=1e-3)

    def test_boundary_exactly_at_threshold_passes(self):
        """ratio == 1/threshold is the boundary; at exactly the limit should pass."""
        from bench.compare import compare_against_baseline

        # 1/0.85 ≈ 1.17647... Ratio exactly 1.17647 → should pass (≤ gate).
        gate = 1.0 / 0.85
        baseline = {"results": [_make_result("scenario_a", 1.0)]}
        current = {"results": [_make_result("scenario_a", gate)]}
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert report.passed, (
            f"ratio==1/threshold should pass (at boundary); ratio={gate:.5f}"
        )

    def test_just_above_threshold_fails(self):
        """ratio just above 1/threshold should fail."""
        from bench.compare import compare_against_baseline

        gate = 1.0 / 0.85 + 0.001
        baseline = {"results": [_make_result("scenario_a", 1.0)]}
        current = {"results": [_make_result("scenario_a", gate)]}
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert not report.passed

    def test_missing_scenario_in_current_fails(self):
        from bench.compare import compare_against_baseline

        baseline = {
            "results": [
                _make_result("scenario_a", 10.0),
                _make_result("scenario_b", 20.0),
            ]
        }
        current = {"results": [_make_result("scenario_a", 8.0)]}  # scenario_b missing
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert not report.passed
        assert "scenario_b" in report.missing_scenarios

    def test_pure_function_no_io(self, tmp_path):
        """compare_against_baseline must not perform any I/O."""
        from bench.compare import compare_against_baseline

        # If the function tries to open any file, it will fail because the paths
        # we pass are dicts (not Path objects) — the function should only touch them
        # via dict lookups.
        baseline = {"results": [_make_result("x", 10.0)]}
        current = {"results": [_make_result("x", 8.0)]}
        # Should not raise even if we mock open to blow up.
        import builtins

        original_open = builtins.open

        def _no_open(*args, **kwargs):
            raise AssertionError("compare_against_baseline must not open files")

        builtins.open = _no_open
        try:
            report = compare_against_baseline(baseline, current)
        finally:
            builtins.open = original_open

        assert report.passed

    def test_zero_baseline_mean_treated_as_pass(self):
        """Zero baseline mean cannot produce a ratio; treated as pass with note."""
        from bench.compare import compare_against_baseline

        baseline = {"results": [_make_result("scenario_a", 0.0)]}
        current = {"results": [_make_result("scenario_a", 5.0)]}
        report = compare_against_baseline(baseline, current)
        assert report.scenarios[0].passed
        assert "skipped" in report.scenarios[0].note

    def test_overall_pass_requires_all_pass(self):
        from bench.compare import compare_against_baseline

        baseline = {
            "results": [
                _make_result("a", 10.0),
                _make_result("b", 10.0),
            ]
        }
        # a passes, b fails.
        current = {
            "results": [
                _make_result("a", 8.0),  # 0.8x — pass
                _make_result("b", 15.0),  # 1.5x — fail
            ]
        }
        report = compare_against_baseline(baseline, current)
        assert not report.passed

    def test_report_fields(self):
        from bench.compare import (
            CompareReport,
            ScenarioComparison,
            compare_against_baseline,
        )

        baseline = {"results": [_make_result("s", 10.0)]}
        current = {"results": [_make_result("s", 8.0)]}
        report = compare_against_baseline(baseline, current)
        assert isinstance(report, CompareReport)
        assert len(report.scenarios) == 1
        sc = report.scenarios[0]
        assert isinstance(sc, ScenarioComparison)
        assert sc.scenario_name == "s"
        assert sc.baseline_mean == pytest.approx(10.0)
        assert sc.current_mean == pytest.approx(8.0)
        assert sc.ratio == pytest.approx(0.8)


# ---------------------------------------------------------------------------
# ScenarioResult dataclass contract
# ---------------------------------------------------------------------------


class TestScenarioResult:
    def test_has_expected_fields(self):
        from bench.harness import ScenarioResult

        fields = set(ScenarioResult.__dataclass_fields__.keys())
        required = {
            "scenario_name",
            "mean_seconds",
            "stddev_seconds",
            "min_seconds",
            "max_seconds",
            "runs",
            "command",
        }
        assert required.issubset(fields), f"Missing fields: {required - fields}"

    def test_constructible(self):
        from bench.harness import ScenarioResult

        r = ScenarioResult(
            scenario_name="test",
            mean_seconds=1.5,
            stddev_seconds=0.1,
            min_seconds=1.4,
            max_seconds=1.6,
            runs=10,
            command="ocx package install foo:1.0.0",
        )
        assert r.scenario_name == "test"
        assert r.runs == 10  # noqa: PLR2004


# ---------------------------------------------------------------------------
# baseline.py — command string shape (no network required)
# ---------------------------------------------------------------------------


class TestBaselineCommandShape:
    def test_bench_cmd_contains_curl_and_tar(self):
        """build_baseline_command produces curl | tar -xJ command string."""
        from bench.baseline import build_baseline_command

        # Fake manifest response.
        fake_manifest = {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "layers": [
                {
                    "digest": "sha256:abc123",
                    "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                    "size": 1024,
                }
            ],
        }

        def _fake_urlopen(req, timeout=10):
            import io
            import urllib.response

            data = json.dumps(fake_manifest).encode()
            return urllib.response.addinfourl(
                io.BytesIO(data),
                headers={},  # type: ignore[arg-type]
                url=str(req.full_url),
                code=200,
            )

        with patch("urllib.request.urlopen", side_effect=_fake_urlopen):
            cmd = build_baseline_command(
                registry="localhost:5000",
                proxy_host="localhost:5002",
                repo="myrepo",
                tag="1.0.0",
            )

        assert "curl" in cmd.bench_cmd
        assert "tar" in cmd.bench_cmd
        assert "-xJ" in cmd.bench_cmd
        assert "localhost:5002" in cmd.download_url
        assert "sha256:abc123" in cmd.download_url
        assert "rm -rf" in cmd.prepare_cmd

    def test_bench_cmd_uses_proxy_host(self):
        """Blob URL routes through the proxy endpoint, not the direct registry."""
        from bench.baseline import build_baseline_command

        fake_manifest = {"layers": [{"digest": "sha256:deadbeef", "size": 100}]}

        def _fake_urlopen(req, timeout=10):
            import io
            import urllib.response

            data = json.dumps(fake_manifest).encode()
            return urllib.response.addinfourl(
                io.BytesIO(data),
                headers={},  # type: ignore[arg-type]
                url=str(req.full_url),
                code=200,
            )

        with patch("urllib.request.urlopen", side_effect=_fake_urlopen):
            cmd = build_baseline_command(
                "localhost:5000", "localhost:5002", "repo", "tag"
            )

        # Bench command must use proxy, not direct registry.
        assert "localhost:5002" in cmd.bench_cmd
        assert "localhost:5000" not in cmd.bench_cmd
