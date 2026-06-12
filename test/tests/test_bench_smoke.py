"""Smoke-validation tests for the bench harness (no Docker/toxiproxy/hyperfine required).

These tests validate the harness contracts purely in Python — no network, no
subprocess, no registry. They run as part of `uv run pytest` alongside the
acceptance tests and must stay green without the bench Docker profile.

Coverage:
- Scenario matrix: exactly 21 rows, correct shape distribution, specific rows present,
  scaling_group + process_count + floor_for fields integrity, per-suite est budgets,
  layer-scaling row integrity (layers field, scaling_group, process_count).
- Suite partition: small ⊂ medium ⊂ large; per-suite est time budgets.
- compare_against_baseline: pure-function behavior, pass/fail threshold semantics,
  threshold edge case (ratio == 1/threshold boundary), missing scenarios,
  baseline-absent skip semantics (ungated_scenarios / not_run_scenarios).
- ScenarioResult / CompareReport dataclass fields; times array present and not replicated.
- BaselineCommand string shape from baseline.build_baseline_command (mocked registry).
- generate_report: multi-suite signature, returns HTML with embedded suite markers;
  works with baselines_by_suite=None.
- z-score math: known-vector unit test for Welch z formula.
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
    def test_exactly_21_rows(self):
        from bench.scenarios import SCENARIOS

        assert len(SCENARIOS) == 21, f"Expected 21 scenarios, got {len(SCENARIOS)}"  # noqa: PLR2004

    def test_unique_names(self):
        from bench.scenarios import SCENARIOS

        names = [s.name for s in SCENARIOS]
        assert len(names) == len(set(names)), "Scenario names must be unique"

    def test_floor_shape_count(self):
        from bench.scenarios import SCENARIOS

        floor = [s for s in SCENARIOS if s.shape == "floor"]
        assert len(floor) == 4, f"Expected 4 floor scenarios, got {len(floor)}"  # noqa: PLR2004

    def test_shape1_count(self):
        from bench.scenarios import SCENARIOS

        shape1 = [s for s in SCENARIOS if s.shape == "shape1"]
        assert len(shape1) == 15, f"Expected 15 shape1 scenarios, got {len(shape1)}"  # noqa: PLR2004

    def test_shape2_count(self):
        from bench.scenarios import SCENARIOS

        shape2 = [s for s in SCENARIOS if s.shape == "shape2"]
        assert len(shape2) == 2, f"Expected 2 shape2 scenarios, got {len(shape2)}"  # noqa: PLR2004

    def test_required_scenario_names_present(self):
        from bench.scenarios import SCENARIOS

        required = {
            # Floor rows
            "baseline_curl_10mbps",
            "baseline_curl_100mbps",
            "baseline_curl_parallel_2",
            "baseline_curl_parallel_4",
            # Shape 1 — single-package
            "ocx_install_10mbps",
            "ocx_install_100mbps",
            "ocx_install_high_bandwidth",
            "ocx_install_unthrottled",
            "ocx_install_latency_50ms",
            "ocx_install_warm",
            # Shape 1 — parallel multi-package
            "ocx_parallel_2_100mbps",
            "ocx_parallel_4_100mbps",
            # Shape 2 — parallel processes
            "ocx_parallel_2_processes",
            "ocx_parallel_4_processes",
            # Layer-scaling rows (medium suite)
            "ocx_layers_1",
            "ocx_layers_2",
            "ocx_layers_4",
            # Large-suite rows
            "ocx_install_100mbps_large",
            "ocx_layers_large_1",
            "ocx_layers_large_2",
            "ocx_layers_large_4",
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
        assert by_name["baseline_curl_parallel_2"].concurrency == 2  # noqa: PLR2004
        assert by_name["baseline_curl_parallel_4"].concurrency == 4  # noqa: PLR2004

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

    def test_parallel_curl_floors_are_floor_shape(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["baseline_curl_parallel_2"].shape == "floor"
        assert by_name["baseline_curl_parallel_4"].shape == "floor"

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
        assert 10 in sizes, "10 MB scenario missing"  # noqa: PLR2004
        assert 12 in sizes, "12 MB (layer) scenario missing"  # noqa: PLR2004

    def test_latency_scenario_has_50ms(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_install_latency_50ms"].latency_ms == 50  # noqa: PLR2004

    def test_unthrottled_has_zero_bandwidth(self):
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_install_unthrottled"].bandwidth_kbps == 0

    # --- Scaling metadata integrity ---

    def test_scaling_group_members_have_process_count_ge2(self):
        """Non-anchor scaling group members must have process_count >= 2.

        Anchors (scenarios listed in SCALING_GROUP_ANCHORS) may have
        process_count=1 — they are the denominator of the efficiency curve.
        """
        from bench.scenarios import SCALING_GROUP_ANCHORS, SCENARIOS

        anchor_names = set(SCALING_GROUP_ANCHORS.values())
        for s in SCENARIOS:
            if s.scaling_group is not None and s.name not in anchor_names:
                assert s.process_count >= 2, (  # noqa: PLR2004
                    f"{s.name}: non-anchor scaling group member must have "
                    f"process_count >= 2, got {s.process_count}"
                )

    def test_scaling_group_anchors_reference_valid_scenarios(self):
        """SCALING_GROUP_ANCHORS must point to scenario names that exist."""
        from bench.scenarios import SCALING_GROUP_ANCHORS, SCENARIOS

        names = {s.name for s in SCENARIOS}
        for group, anchor_name in SCALING_GROUP_ANCHORS.items():
            assert anchor_name in names, (
                f"SCALING_GROUP_ANCHORS[{group!r}] = {anchor_name!r} not in SCENARIOS"
            )

    def test_floor_for_references_valid_scenarios(self):
        """floor_for fields must reference existing scenario names."""
        from bench.scenarios import SCENARIOS

        names = {s.name for s in SCENARIOS}
        for s in SCENARIOS:
            if s.floor_for is not None:
                assert s.floor_for in names, (
                    f"{s.name}: floor_for={s.floor_for!r} not in SCENARIOS"
                )

    def test_parallel_curl_floors_have_floor_for(self):
        """baseline_curl_parallel_* rows must carry floor_for pointing to ocx shape2 rows."""
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert (
            by_name["baseline_curl_parallel_2"].floor_for == "ocx_parallel_2_processes"
        )
        assert (
            by_name["baseline_curl_parallel_4"].floor_for == "ocx_parallel_4_processes"
        )

    def test_scaling_groups_covered(self):
        """Expected scaling groups exist in at least one scenario."""
        from bench.scenarios import SCENARIOS

        groups = {s.scaling_group for s in SCENARIOS if s.scaling_group is not None}
        expected = {
            "ocx_shape1_100mbps",
            "ocx_shape2_100mbps",
            "curl_100mbps",
            "ocx_layers_100mbps",
            "ocx_layers_large_100mbps",
        }
        missing = expected - groups
        assert not missing, f"Missing scaling groups: {missing}"

    # --- Per-scenario run budget (Phase 5.5) ---

    def test_all_scenarios_have_positive_runs(self):
        """Every scenario must have runs >= 1."""
        from bench.scenarios import SCENARIOS

        violations = [s.name for s in SCENARIOS if s.runs < 1]
        assert not violations, f"Scenarios with runs < 1: {violations}"

    def test_all_scenarios_have_nonnegative_warmup(self):
        """warmup must be >= 0 for every scenario."""
        from bench.scenarios import SCENARIOS

        violations = [s.name for s in SCENARIOS if s.warmup < 0]
        assert not violations, f"Scenarios with warmup < 0: {violations}"

    def test_all_scenarios_have_positive_est_seconds(self):
        """Every scenario must have est_seconds > 0 (overhead alone ≥ 1.0)."""
        from bench.scenarios import SCENARIOS

        violations = [s.name for s in SCENARIOS if s.est_seconds <= 0]
        assert not violations, f"Scenarios with est_seconds <= 0: {violations}"

    def test_per_suite_est_seconds_within_budget(self):
        """Per-suite cumulative est_seconds must satisfy SUITE_BUDGET_SECONDS.

        Suite selection is cumulative (large ⊇ medium ⊇ small).
        """
        from bench.scenarios import SCENARIOS, SUITE_BUDGET_SECONDS

        cumulative: dict[str, set[str]] = {
            "small": {"small"},
            "medium": {"small", "medium"},
            "large": {"small", "medium", "large"},
        }
        for suite_name, suite_set in cumulative.items():
            total = sum(s.est_seconds for s in SCENARIOS if s.suite in suite_set)
            budget = SUITE_BUDGET_SECONDS[suite_name]
            assert total <= budget, (
                f"{suite_name}-suite est {total:.1f}s exceeds {budget:.0f}s budget."
            )

    def test_total_est_seconds_within_budget(self):
        """Backward-compat: medium suite sum ≤ SCENARIO_BUDGET_SECONDS (200 s)."""
        from bench.scenarios import SCENARIO_BUDGET_SECONDS, SCENARIOS

        # medium suite = small + medium rows
        total = sum(s.est_seconds for s in SCENARIOS if s.suite in {"small", "medium"})
        assert total <= SCENARIO_BUDGET_SECONDS, (
            f"Medium-suite estimated scenario time {total:.1f}s exceeds "
            f"{SCENARIO_BUDGET_SECONDS:.0f}s budget."
        )

    # --- Layer-scaling rows (Phase 5.6) ---

    def test_layer_rows_present(self):
        """All three layer-scaling rows must exist in the matrix."""
        from bench.scenarios import SCENARIOS

        names = {s.name for s in SCENARIOS}
        for expected in ("ocx_layers_1", "ocx_layers_2", "ocx_layers_4"):
            assert expected in names, f"Layer scaling row {expected!r} missing"

    def test_layer_rows_have_correct_layers_field(self):
        """layers field must match the name suffix for layer-scaling rows."""
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        assert by_name["ocx_layers_1"].layers == 1
        assert by_name["ocx_layers_2"].layers == 2  # noqa: PLR2004
        assert by_name["ocx_layers_4"].layers == 4  # noqa: PLR2004

    def test_layer_rows_have_same_total_payload(self):
        """All layer-scaling rows must have the same size_mb (same total payload)."""
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        sizes = {
            by_name["ocx_layers_1"].size_mb,
            by_name["ocx_layers_2"].size_mb,
            by_name["ocx_layers_4"].size_mb,
        }
        assert len(sizes) == 1, (
            f"Layer rows have different size_mb values: {sizes}. "
            "All must share the same total payload."
        )

    def test_layer_rows_share_scaling_group(self):
        """All three layer rows must share the same scaling_group."""
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        groups = {
            by_name["ocx_layers_1"].scaling_group,
            by_name["ocx_layers_2"].scaling_group,
            by_name["ocx_layers_4"].scaling_group,
        }
        assert None not in groups, "Layer rows must have a non-None scaling_group"
        assert len(groups) == 1, (
            f"Layer rows must share one scaling_group, got {groups}"
        )

    def test_layer_rows_process_count_matches_layer_count(self):
        """process_count must equal layers for layer-scaling rows."""
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        for name in ("ocx_layers_1", "ocx_layers_2", "ocx_layers_4"):
            s = by_name[name]
            assert s.process_count == s.layers, (
                f"{name}: process_count={s.process_count} != layers={s.layers}"
            )

    def test_layer_rows_use_shape1(self):
        """Layer-scaling rows are shape1 (single ocx install invocation)."""
        from bench.scenarios import SCENARIOS

        by_name = {s.name: s for s in SCENARIOS}
        for name in ("ocx_layers_1", "ocx_layers_2", "ocx_layers_4"):
            assert by_name[name].shape == "shape1", (
                f"{name}: expected shape1, got {by_name[name].shape!r}"
            )

    def test_single_layer_rows_have_layers_equals_one(self):
        """Non-layer-scaling scenarios must have layers=1 (default)."""
        from bench.scenarios import SCENARIOS

        layer_names = {
            "ocx_layers_1",
            "ocx_layers_2",
            "ocx_layers_4",
            "ocx_layers_large_1",
            "ocx_layers_large_2",
            "ocx_layers_large_4",
        }
        violations = [
            s.name for s in SCENARIOS if s.name not in layer_names and s.layers != 1
        ]
        assert not violations, f"Non-layer scenarios have layers != 1: {violations}"

    # --- All warmup=0 (v3 budget rule) ---

    def test_all_scenarios_warmup_zero(self):
        """v3 budget rule: all scenarios use warmup=0 to keep full suite < 240 s."""
        from bench.scenarios import SCENARIOS

        violations = [s.name for s in SCENARIOS if s.warmup != 0]
        assert not violations, (
            f"Scenarios with warmup != 0 (v3 rule requires warmup=0): {violations}"
        )

    # --- Quick-mode overhead budget ---

    def test_quick_scenario_subset_est_under_30s(self):
        """Quick mode scenario time must be < 30 s (overhead dominates, target <60s total)."""
        from bench.scenarios import SCENARIOS

        quick_names = {
            "ocx_install_unthrottled",
            "ocx_install_100mbps",
            "ocx_parallel_2_processes",
        }
        subset = [s for s in SCENARIOS if s.name in quick_names]
        assert len(subset) == len(quick_names), (  # noqa: PLR2004
            f"Quick scenario set incomplete: found {[s.name for s in subset]}"
        )
        total = sum(s.est_seconds for s in subset)
        assert total < 30.0, (  # noqa: PLR2004
            f"Quick scenario subset est {total:.1f}s exceeds 30s. "
            "Overhead budget for <60s total run is at risk."
        )

    # --- Suite partition integrity ---

    def test_small_suite_scenarios_have_suite_small(self):
        """The 3 quick-mode scenarios must be tagged suite='small'."""
        from bench.scenarios import SCENARIOS

        small_names = {
            "ocx_install_unthrottled",
            "ocx_install_100mbps",
            "ocx_parallel_2_processes",
        }
        by_name = {s.name: s for s in SCENARIOS}
        for name in small_names:
            assert name in by_name, f"Small-suite scenario {name!r} missing"
            assert by_name[name].suite == "small", (
                f"{name}: expected suite='small', got {by_name[name].suite!r}"
            )

    def test_suite_field_values_are_valid(self):
        """All suite values must be one of 'small', 'medium', 'large'."""
        from bench.scenarios import SCENARIOS

        valid = {"small", "medium", "large"}
        violations = [s.name for s in SCENARIOS if s.suite not in valid]
        assert not violations, f"Scenarios with invalid suite field: {violations}"

    def test_large_suite_rows_have_suite_large(self):
        """Large-only rows must be tagged suite='large'."""
        from bench.scenarios import SCENARIOS

        large_names = {
            "ocx_install_100mbps_large",
            "ocx_layers_large_1",
            "ocx_layers_large_2",
            "ocx_layers_large_4",
        }
        by_name = {s.name: s for s in SCENARIOS}
        for name in large_names:
            assert by_name[name].suite == "large", (
                f"{name}: expected suite='large', got {by_name[name].suite!r}"
            )

    def test_suite_partition_cumulative_small_subset_medium(self):
        """small names must be a subset of medium names (cumulative)."""
        from bench.scenarios import SCENARIOS

        small_names = {s.name for s in SCENARIOS if s.suite == "small"}
        medium_names = {s.name for s in SCENARIOS if s.suite in {"small", "medium"}}
        assert small_names.issubset(medium_names), (
            f"small names not subset of medium: {small_names - medium_names}"
        )

    def test_suite_partition_cumulative_medium_subset_large(self):
        """medium names must be a subset of large names (cumulative)."""
        from bench.scenarios import SCENARIOS

        medium_names = {s.name for s in SCENARIOS if s.suite in {"small", "medium"}}
        large_names = {
            s.name for s in SCENARIOS if s.suite in {"small", "medium", "large"}
        }
        assert medium_names.issubset(large_names), (
            f"medium names not subset of large: {medium_names - large_names}"
        )

    def test_small_suite_est_under_60s(self):
        """small suite scenario time must be < 60 s."""
        from bench.scenarios import SCENARIOS

        total = sum(s.est_seconds for s in SCENARIOS if s.suite == "small")
        assert total < 60.0, f"small suite est {total:.1f}s >= 60s"  # noqa: PLR2004

    def test_large_suite_est_under_550s(self):
        """large suite (cumulative) scenario time must be ≤ 550 s."""
        from bench.scenarios import SCENARIOS

        total = sum(
            s.est_seconds for s in SCENARIOS if s.suite in {"small", "medium", "large"}
        )
        assert total <= 550.0, f"large suite (cumulative) est {total:.1f}s > 550s"  # noqa: PLR2004

    # --- Deterministic fixture names ---

    def test_fixture_repo_name_is_deterministic(self):
        """_fixture_repo_name must not include random components."""
        from bench.harness import _fixture_repo_name

        name1 = _fixture_repo_name(10, 1, 0)
        name2 = _fixture_repo_name(10, 1, 0)
        assert name1 == name2, "Fixture repo name must be deterministic"
        assert "uuid" not in name1.lower(), "Fixture name must not contain 'uuid'"

    def test_fixture_repo_name_encodes_size_and_layers(self):
        """Fixture names must encode size and layer count for cache key uniqueness."""
        from bench.harness import _fixture_repo_name

        n_10mb_1l = _fixture_repo_name(10, 1, 0)
        n_10mb_2l = _fixture_repo_name(10, 2, 0)
        n_5mb_1l = _fixture_repo_name(5, 1, 0)
        assert n_10mb_1l != n_10mb_2l, "Different layers must produce different names"
        assert n_10mb_1l != n_5mb_1l, "Different sizes must produce different names"
        assert "10mb" in n_10mb_1l, "Size must be in fixture name"
        assert "1l" in n_10mb_1l, "Layer count must be in fixture name"

    def test_fixture_repo_name_has_bench_prefix(self):
        """Fixture names must start with 'bench-' to isolate from test repos."""
        from bench.harness import _fixture_repo_name

        name = _fixture_repo_name(10, 1, 0)
        assert name.startswith("bench-"), (
            f"Fixture repo name {name!r} must start with 'bench-' "
            "to isolate from acceptance test repos"
        )


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

    def test_baseline_absent_scenario_is_ungated_not_fail(self):
        """Scenarios in current but absent from baseline are ungated — do not fail gate."""
        from bench.compare import compare_against_baseline

        baseline = {"results": [_make_result("scenario_a", 10.0)]}
        current = {
            "results": [
                _make_result("scenario_a", 8.0),
                _make_result("new_scenario_no_baseline", 5.0),  # no baseline entry
            ]
        }
        report = compare_against_baseline(baseline, current, threshold=0.85)
        # Overall should pass (scenario_a improved; new_scenario is ungated).
        assert report.passed
        assert "new_scenario_no_baseline" in report.ungated_scenarios
        # new_scenario must NOT appear in gated comparisons.
        gated_names = {c.scenario_name for c in report.scenarios}
        assert "new_scenario_no_baseline" not in gated_names

    def test_not_run_scenario_does_not_fail_gate(self):
        """Scenarios in baseline but not run this session are noted, not a failure."""
        from bench.compare import compare_against_baseline

        baseline = {
            "results": [
                _make_result("scenario_a", 10.0),
                _make_result("scenario_b", 20.0),
            ]
        }
        # Only scenario_a was run (subset / quick mode).
        current = {"results": [_make_result("scenario_a", 8.0)]}
        report = compare_against_baseline(baseline, current, threshold=0.85)
        # scenario_a passes; scenario_b is not-run, should NOT fail the gate.
        assert report.passed
        assert "scenario_b" in report.not_run_scenarios
        assert "scenario_b" in report.missing_scenarios  # backward compat alias

    def test_regression_in_gated_scenario_fails_despite_ungated(self):
        """Regression in gated scenario still fails even if ungated scenarios exist."""
        from bench.compare import compare_against_baseline

        baseline = {"results": [_make_result("scenario_a", 10.0)]}
        current = {
            "results": [
                _make_result("scenario_a", 20.0),  # 2.0x — regression
                _make_result("new_ungated", 1.0),
            ]
        }
        report = compare_against_baseline(baseline, current, threshold=0.85)
        assert not report.passed
        assert "new_ungated" in report.ungated_scenarios

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

    def test_report_has_ungated_and_not_run_fields(self):
        """CompareReport exposes ungated_scenarios and not_run_scenarios."""
        from bench.compare import CompareReport, compare_against_baseline

        baseline = {
            "results": [
                _make_result("gated", 10.0),
                _make_result("not_run_this_time", 5.0),
            ]
        }
        current = {
            "results": [
                _make_result("gated", 9.0),
                _make_result("brand_new", 3.0),
            ]
        }
        report = compare_against_baseline(baseline, current)
        assert isinstance(report, CompareReport)
        assert hasattr(report, "ungated_scenarios")
        assert hasattr(report, "not_run_scenarios")
        assert "brand_new" in report.ungated_scenarios
        assert "not_run_this_time" in report.not_run_scenarios
        # gated comparison present.
        assert any(c.scenario_name == "gated" for c in report.scenarios)


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
            "times",
            "suite",
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

    def test_times_defaults_to_empty_list(self):
        from bench.harness import ScenarioResult

        r = ScenarioResult(
            scenario_name="test",
            mean_seconds=1.0,
            stddev_seconds=0.0,
            min_seconds=1.0,
            max_seconds=1.0,
            runs=3,
            command="cmd",
        )
        assert r.times == []

    def test_times_stores_real_values(self):
        """times must store real per-run values, not replicated mean."""
        from bench.harness import ScenarioResult

        real_times = [1.1, 1.2, 0.9]
        r = ScenarioResult(
            scenario_name="test",
            mean_seconds=sum(real_times) / len(real_times),
            stddev_seconds=0.1,
            min_seconds=min(real_times),
            max_seconds=max(real_times),
            runs=len(real_times),
            command="cmd",
            times=real_times,
        )
        assert r.times == real_times
        # Must NOT be mean-replicated (all same value).
        assert len(set(r.times)) > 1, "times array should not be mean-replicated"


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


# ---------------------------------------------------------------------------
# generate_report — pure function contract
# ---------------------------------------------------------------------------

# Minimal template with all required markers (multi-suite + backward-compat single-suite).
_MINIMAL_TEMPLATE = """<!DOCTYPE html>
<html><head></head><body>
<script>/* __VUE_INLINE__ */</script>
<script>
var RS = /* __BENCH_RESULTS_BY_SUITE_JSON__ */;
var BS = /* __BENCH_BASELINES_BY_SUITE_JSON__ */;
var R  = /* __BENCH_RESULTS_JSON__ */;
var B  = /* __BENCH_BASELINE_JSON__ */;
var S  = /* __BENCH_SCENARIOS_META__ */;
</script>
</body></html>"""


class TestGenerateReport:
    def test_returns_string(self):
        """generate_report returns a str."""
        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("s", 1.0)]}}
        html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        assert isinstance(html, str)

    def test_markers_replaced(self):
        """All markers must be replaced in the output."""
        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("s", 1.5)]}}
        baselines_by_suite = {"medium": {"results": [_make_result("s", 2.0)]}}
        html = generate_report(results_by_suite, baselines_by_suite, _MINIMAL_TEMPLATE)

        assert "/* __BENCH_RESULTS_BY_SUITE_JSON__ */" not in html
        assert "/* __BENCH_BASELINES_BY_SUITE_JSON__ */" not in html
        assert "/* __BENCH_RESULTS_JSON__ */" not in html
        assert "/* __BENCH_BASELINE_JSON__ */" not in html
        assert "/* __BENCH_SCENARIOS_META__ */" not in html
        assert "/* __VUE_INLINE__ */" not in html

    def test_results_json_embedded(self):
        """Results JSON is embedded and parseable from the output."""
        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("my_scenario", 3.14)]}}
        html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        assert "my_scenario" in html
        assert "3.14" in html

    def test_baseline_none_embeds_null(self):
        """When baselines_by_suite is None, the baseline slots contain null."""
        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("s", 1.0)]}}
        html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        assert "null" in html

    def test_baseline_dict_embedded(self):
        """When baselines provided, baseline data is embedded."""
        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("s", 1.0)]}}
        baselines_by_suite = {"medium": {"results": [_make_result("s", 2.0)]}}
        html = generate_report(results_by_suite, baselines_by_suite, _MINIMAL_TEMPLATE)
        assert "2.0" in html

    def test_scenarios_meta_embedded(self):
        """Scenarios metadata (floor_for, scaling_group, suite) is embedded."""
        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("s", 1.0)]}}
        html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        assert "scaling_group_anchors" in html

    def test_multi_suite_results_embedded(self):
        """Multiple suite results all appear in output."""
        from bench.report import generate_report

        results_by_suite = {
            "small": {"results": [_make_result("small_scenario", 0.1)]},
            "medium": {"results": [_make_result("medium_scenario", 1.0)]},
            "large": {"results": [_make_result("large_scenario", 4.0)]},
        }
        html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        assert "small_scenario" in html
        assert "medium_scenario" in html
        assert "large_scenario" in html

    def test_suite_names_in_output(self):
        """Suite names must appear in the embedded JSON."""
        from bench.report import generate_report

        results_by_suite = {
            "small": {"results": [_make_result("s", 1.0)]},
            "large": {"results": [_make_result("l", 4.0)]},
        }
        html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        assert '"small"' in html
        assert '"large"' in html

    def test_pure_no_io(self, tmp_path):
        """generate_report must not perform any file I/O on the dicts it receives."""
        import builtins

        from bench.report import generate_report

        results_by_suite = {"medium": {"results": [_make_result("s", 1.0)]}}

        original_open = builtins.open

        def _no_open_for_dicts(path, *args, **kwargs):
            path_str = str(path)
            if "latest.json" in path_str or "baseline.json" in path_str:
                raise AssertionError(
                    f"generate_report must not open files for dict args; tried: {path!r}"
                )
            return original_open(path, *args, **kwargs)

        builtins.open = _no_open_for_dicts
        try:
            html = generate_report(results_by_suite, None, _MINIMAL_TEMPLATE)
        finally:
            builtins.open = original_open

        assert isinstance(html, str)


# ---------------------------------------------------------------------------
# z-score math unit tests
# ---------------------------------------------------------------------------


class TestZScoreMath:
    """Unit tests for the Welch z-score formula used in the dashboard.

    z = (m_cur - m_base) / sqrt(σ_cur²/n_cur + σ_base²/n_base)
    """

    @staticmethod
    def _welch_z(
        m_cur: float,
        s_cur: float,
        n_cur: int,
        m_base: float,
        s_base: float,
        n_base: int,
    ) -> float:
        import math

        se = math.sqrt(s_cur**2 / n_cur + s_base**2 / n_base)
        if se == 0.0:
            return 0.0
        return (m_cur - m_base) / se

    def test_no_change_z_is_zero(self):
        """Identical means → z=0."""
        z = self._welch_z(1.0, 0.1, 5, 1.0, 0.1, 5)
        assert abs(z) < 1e-10

    def test_regression_z_positive(self):
        """Current slower than baseline → z > 0 (positive = regression)."""
        z = self._welch_z(2.0, 0.1, 5, 1.0, 0.1, 5)
        assert z > 0

    def test_improvement_z_negative(self):
        """Current faster than baseline → z < 0 (negative = improvement)."""
        z = self._welch_z(0.8, 0.1, 5, 1.0, 0.1, 5)
        assert z < 0

    def test_known_vector_z_value(self):
        """Known vector: m_cur=1.2, σ=0.1, n=5; m_base=1.0, σ=0.1, n=5 → z≈3.16."""
        import math

        z = self._welch_z(1.2, 0.1, 5, 1.0, 0.1, 5)
        # se = sqrt(0.01/5 + 0.01/5) = sqrt(0.004) ≈ 0.06325
        # z = 0.2 / 0.06325 ≈ 3.162
        expected = 0.2 / math.sqrt(0.01 / 5 + 0.01 / 5)
        assert abs(z - expected) < 1e-10

    def test_noise_threshold_abs_z_under_2(self):
        """|z| < 2 should be classified as noise."""
        z = self._welch_z(1.05, 0.1, 5, 1.0, 0.1, 5)
        assert abs(z) < 2.0  # noqa: PLR2004

    def test_real_regression_abs_z_over_2(self):
        """|z| >= 2 should flag as real signal."""
        z = self._welch_z(1.3, 0.05, 10, 1.0, 0.05, 10)
        assert abs(z) >= 2.0  # noqa: PLR2004

    def test_zero_stddev_returns_zero(self):
        """If both stddevs are zero, z=0 (avoid div by zero)."""
        z = self._welch_z(1.0, 0.0, 5, 1.0, 0.0, 5)
        assert z == 0.0
