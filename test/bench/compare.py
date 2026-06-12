"""Benchmark comparison: current results vs saved baseline.

PURE function `compare_against_baseline` returns a report dict — no I/O,
no sys.exit inside it. The __main__ block handles printing + exit code.

Threshold semantics (spec §D4):
  - Pass: current_mean <= baseline_mean (no regression)
  - Fail: current_mean > baseline_mean / threshold
    i.e. current is more than (1/threshold) times slower than baseline.
    At threshold=0.85: fail if current > baseline * (1/0.85) ≈ 1.176×.
    Equivalently: the optimized code must be at most ~17.6% SLOWER than
    the baseline. Any improvement (ratio < 1.0) always passes.
    The threshold is intended for regression gating: if current is slower
    than baseline by more than 1/0.85 - 1 ≈ 17.6%, it fails.
    The 0.85× label in the spec table refers to the improvement target
    (expected ratio after optimization); the gate is the inverse for
    regression detection.

Ratio:
  ratio = current_mean / baseline_mean
  ratio < 1.0 = improvement (current is faster)
  ratio > 1.0 = regression (current is slower)
  FAIL if ratio > 1.0 / threshold (i.e. > ~1.176 at threshold=0.85)
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(slots=True)
class ScenarioComparison:
    """Comparison result for a single scenario."""

    scenario_name: str
    baseline_mean: float
    current_mean: float
    ratio: float  # current / baseline; < 1.0 = improvement
    passed: bool
    threshold: float
    note: str = ""


@dataclass(slots=True)
class CompareReport:
    """Full comparison report across all scenarios."""

    scenarios: list[ScenarioComparison]
    threshold: float
    passed: bool  # True if ALL scenarios passed
    missing_scenarios: list[str]  # In baseline but not in current results


def compare_against_baseline(
    baseline: dict[str, Any],
    current: dict[str, Any],
    threshold: float = 0.85,
) -> CompareReport:
    """Compare current benchmark results against a saved baseline.

    PURE function — no I/O, no sys.exit, no print. Callers handle output
    and process exit. This keeps the function unit-testable.

    Parameters
    ----------
    baseline:
        Parsed baseline JSON dict (hyperfine-compatible format with "results" list).
    current:
        Parsed current results JSON dict (same format).
    threshold:
        Regression gate threshold. Default 0.85. A scenario fails when:
            current_mean > baseline_mean / threshold
        (i.e. current is more than 1/threshold - 1 ≈ 17.6% slower than baseline).

    Returns
    -------
    CompareReport
        Per-scenario comparisons plus overall pass/fail.
    """

    # Index baseline and current by scenario name (hyperfine "command" field).
    def _index(results_dict: dict[str, Any]) -> dict[str, dict[str, Any]]:
        index: dict[str, dict[str, Any]] = {}
        for entry in results_dict.get("results", []):
            name = entry.get("command", "")
            if name:
                index[name] = entry
        return index

    baseline_index = _index(baseline)
    current_index = _index(current)

    comparisons: list[ScenarioComparison] = []
    missing: list[str] = []

    for name, b_entry in baseline_index.items():
        b_mean = float(b_entry.get("mean", 0.0))
        if name not in current_index:
            missing.append(name)
            continue
        c_entry = current_index[name]
        c_mean = float(c_entry.get("mean", 0.0))

        if b_mean <= 0.0:
            # Cannot compare; treat as pass with a note.
            comparisons.append(
                ScenarioComparison(
                    scenario_name=name,
                    baseline_mean=b_mean,
                    current_mean=c_mean,
                    ratio=0.0,
                    passed=True,
                    threshold=threshold,
                    note="baseline mean is zero or missing — skipped",
                )
            )
            continue

        ratio = c_mean / b_mean
        # Fail if current is more than 1/threshold times the baseline.
        passed = ratio <= (1.0 / threshold)
        note = ""
        if not passed:
            note = f"regression: {ratio:.3f}x > {1.0 / threshold:.3f}x gate"
        comparisons.append(
            ScenarioComparison(
                scenario_name=name,
                baseline_mean=b_mean,
                current_mean=c_mean,
                ratio=ratio,
                passed=passed,
                threshold=threshold,
                note=note,
            )
        )

    overall_passed = all(c.passed for c in comparisons) and not missing

    return CompareReport(
        scenarios=comparisons,
        threshold=threshold,
        passed=overall_passed,
        missing_scenarios=missing,
    )


def _format_report(report: CompareReport) -> str:
    """Format the comparison report as a human-readable table string."""
    lines: list[str] = []
    lines.append(
        f"{'Scenario':<40} {'Baseline':>10} {'Current':>10} {'Ratio':>8} {'Status':>8}"
    )
    lines.append("-" * 80)
    for c in report.scenarios:
        status = "PASS" if c.passed else "FAIL"
        note = f"  {c.note}" if c.note else ""
        lines.append(
            f"{c.scenario_name:<40} "
            f"{c.baseline_mean:>9.3f}s "
            f"{c.current_mean:>9.3f}s "
            f"{c.ratio:>7.3f}x "
            f"{status:>8}"
            f"{note}"
        )
    if report.missing_scenarios:
        lines.append("")
        lines.append("Missing from current results (in baseline but not run):")
        for name in report.missing_scenarios:
            lines.append(f"  - {name}")
    lines.append("")
    overall = "PASSED" if report.passed else "FAILED"
    lines.append(
        f"Overall: {overall}  "
        f"(threshold: current <= baseline / {report.threshold:.2f} = {1.0 / report.threshold:.3f}x)"
    )
    return "\n".join(lines)


if __name__ == "__main__":
    if len(sys.argv) < 3:  # noqa: PLR2004
        print(
            "Usage: python -m bench.compare <baseline.json> <current.json> [--threshold 0.85]",
            file=sys.stderr,
        )
        sys.exit(2)

    baseline_path = Path(sys.argv[1])
    current_path = Path(sys.argv[2])

    threshold_val = 0.85
    if "--threshold" in sys.argv:
        idx = sys.argv.index("--threshold")
        threshold_val = float(sys.argv[idx + 1])

    baseline_data = json.loads(baseline_path.read_text())
    current_data = json.loads(current_path.read_text())

    report = compare_against_baseline(
        baseline_data, current_data, threshold=threshold_val
    )
    print(_format_report(report))

    sys.exit(0 if report.passed else 1)
