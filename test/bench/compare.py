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

Baseline-absent semantics:
  Scenarios in current results that have NO entry in baseline are reported
  as "ungated" — they are listed in ungated_scenarios and do NOT contribute
  to the overall pass/fail decision. This allows new scenarios (e.g. new
  parallel curl floors) to be added without requiring a full baseline re-capture.

  Scenarios in baseline that are NOT in current results are reported as
  "not run" in not_run_scenarios. They do NOT fail the overall gate — this
  allows partial subset runs (e.g. bench:quick with 3 scenarios) without
  falsely failing the other 15 baseline scenarios.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass, field
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
    passed: bool  # True if ALL gated comparisons passed
    missing_scenarios: list[str]  # kept for backward compat; same as not_run_scenarios
    ungated_scenarios: list[str] = field(default_factory=list)
    # In baseline but not in current run (not a failure — subset runs allowed).
    not_run_scenarios: list[str] = field(default_factory=list)


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
        - scenarios: gated comparisons (scenario in both baseline and current)
        - ungated_scenarios: in current but NOT in baseline (skipped with note)
        - not_run_scenarios: in baseline but NOT in current (noted but not a failure)
        - passed: True iff all gated comparisons pass (ungated/not_run do not affect)
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
    not_run: list[str] = []
    ungated: list[str] = []

    # Scenarios in current but not in baseline → ungated (no gate applied).
    for name in current_index:
        if name not in baseline_index:
            ungated.append(name)

    # Scenarios in baseline → gated comparison (if present in current) or not-run note.
    for name, b_entry in baseline_index.items():
        b_mean = float(b_entry.get("mean", 0.0))
        if name not in current_index:
            # In baseline but not run this session → noted, not a failure.
            not_run.append(name)
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

    # Overall passes iff all gated comparisons pass.
    # not_run and ungated scenarios do NOT affect the gate.
    overall_passed = all(c.passed for c in comparisons)

    return CompareReport(
        scenarios=comparisons,
        threshold=threshold,
        passed=overall_passed,
        missing_scenarios=not_run,  # backward compat alias
        ungated_scenarios=ungated,
        not_run_scenarios=not_run,
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
    if report.ungated_scenarios:
        lines.append("")
        lines.append("Ungated (in current but absent from baseline — no gate applied):")
        for name in sorted(report.ungated_scenarios):
            lines.append(f"  - {name}  [SKIP — no baseline entry]")
    if report.not_run_scenarios:
        lines.append("")
        lines.append("Not run this session (in baseline but not in current results):")
        for name in report.not_run_scenarios:
            lines.append(f"  - {name}  [NOT RUN — partial run OK]")
    lines.append("")
    overall = "PASSED" if report.passed else "FAILED"
    gated_count = len(report.scenarios)
    lines.append(
        f"Overall: {overall}  "
        f"({gated_count} gated, {len(report.ungated_scenarios)} ungated, "
        f"{len(report.not_run_scenarios)} not run; "
        f"threshold: current <= baseline / {report.threshold:.2f} = {1.0 / report.threshold:.3f}x)"
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
