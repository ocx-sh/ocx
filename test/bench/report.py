"""Benchmark dashboard report generator.

Generates a self-contained single-file HTML dashboard from benchmark results.

    generate_report(results_by_suite, baselines_by_suite, template) -> str

PURE function: takes dicts + template string, returns HTML string with
results and baseline JSON inlined. No file I/O, no side effects.

Multi-suite support: ``results_by_suite`` is a dict keyed by suite name
("small", "medium", "large") containing only the suites that have result
files on disk. The dashboard renders suite toggle buttons and disables suites
with no embedded data.

__main__ block:
  - Scans test/bench/results/ for latest-<suite>.json files
  - Falls back to latest.json if no suite files found (backward compat)
  - Reads test/bench/baseline.json (optional)
  - Reads test/bench/dashboard/template.html
  - Calls generate_report()
  - Writes test/bench/results/index.html
  - Prints the output path

Usage:
    uv run python bench/report.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

_BENCH_DIR = Path(__file__).resolve().parent
_TEST_DIR = _BENCH_DIR.parent
if str(_TEST_DIR) not in sys.path:
    sys.path.insert(0, str(_TEST_DIR))


# ---------------------------------------------------------------------------
# Markers embedded in the template that this function replaces.
# ---------------------------------------------------------------------------
_RESULTS_BY_SUITE_MARKER = "/* __BENCH_RESULTS_BY_SUITE_JSON__ */"
_BASELINES_BY_SUITE_MARKER = "/* __BENCH_BASELINES_BY_SUITE_JSON__ */"
_SCENARIOS_META_MARKER = "/* __BENCH_SCENARIOS_META__ */"
_VUE_INLINE_MARKER = "/* __VUE_INLINE__ */"

# Backward-compat single-suite markers (template may use either form).
_RESULTS_MARKER = "/* __BENCH_RESULTS_JSON__ */"
_BASELINE_MARKER = "/* __BENCH_BASELINE_JSON__ */"

_VENDOR_DIR = _BENCH_DIR / "dashboard" / "vendor"


def generate_report(
    results_by_suite: dict[str, dict[str, Any]],
    baselines_by_suite: dict[str, dict[str, Any]] | None,
    template: str,
) -> str:
    """Generate a self-contained HTML dashboard from bench results.

    PURE function — no I/O, no side effects. Takes parsed JSON dicts and the
    template HTML string; returns the rendered HTML string with data inlined.

    The template must contain the following marker comments:

      /* __BENCH_RESULTS_BY_SUITE_JSON__ */   → dict[suite_name, results_dict]
      /* __BENCH_BASELINES_BY_SUITE_JSON__ */  → dict[suite_name, baseline_dict] | null
      /* __BENCH_SCENARIOS_META__ */           → scenarios metadata dict
      /* __VUE_INLINE__ */                     → Vue 3 global prod build

    Backward-compat single-suite markers also replaced if present:
      /* __BENCH_RESULTS_JSON__ */   → most recent suite's results
      /* __BENCH_BASELINE_JSON__ */  → "medium" baseline or null

    Parameters
    ----------
    results_by_suite:
        Dict mapping suite names to their parsed results JSON dicts.
        Only include suites for which result files exist.
    baselines_by_suite:
        Dict mapping suite names to baseline JSON dicts, or None if no
        baselines are available. Typically only "medium" is present.
    template:
        The HTML template string read from dashboard/template.html.

    Returns
    -------
    str
        Self-contained HTML with results, baselines, and scenarios metadata inlined.
    """
    from bench.scenarios import SCALING_GROUP_ANCHORS, SCENARIOS  # noqa: PLC0415

    # Build scenarios metadata for the dashboard.
    scenarios_meta: list[dict[str, Any]] = [
        {
            "name": s.name,
            "shape": s.shape,
            "bandwidth_kbps": s.bandwidth_kbps,
            "size_mb": s.size_mb,
            "concurrency": s.concurrency,
            "cold": s.cold,
            "scaling_group": s.scaling_group,
            "process_count": s.process_count,
            "floor_for": s.floor_for,
            "suite": s.suite,
        }
        for s in SCENARIOS
    ]

    results_by_suite_json = json.dumps(results_by_suite)
    baselines_by_suite_json = (
        json.dumps(baselines_by_suite) if baselines_by_suite is not None else "null"
    )
    scenarios_meta_json = json.dumps(
        {
            "scenarios": scenarios_meta,
            "scaling_group_anchors": SCALING_GROUP_ANCHORS,
        }
    )

    # Backward-compat single-suite values.
    # Use the "medium" suite if available, else pick the most recent (last in dict).
    _most_recent_results = results_by_suite.get("medium") or (
        next(iter(reversed(list(results_by_suite.values()))), {})
        if results_by_suite
        else {}
    )
    _medium_baseline = (
        (baselines_by_suite or {}).get("medium") if baselines_by_suite else None
    )
    results_single_json = json.dumps(_most_recent_results)
    baseline_single_json = (
        json.dumps(_medium_baseline) if _medium_baseline is not None else "null"
    )

    # Inline Vue 3 source so output is fully self-contained.
    vue_vendor_path = _VENDOR_DIR / "vue.global.prod.js"
    if vue_vendor_path.exists():
        vue_inline = vue_vendor_path.read_text()
    else:
        vue_inline = (
            "console.error('vue.global.prod.js not found in dashboard/vendor/; "
            "run: cp website/node_modules/vue/dist/vue.global.prod.js "
            "test/bench/dashboard/vendor/');"
        )

    html = template
    html = html.replace(_VUE_INLINE_MARKER, vue_inline, 1)
    html = html.replace(_RESULTS_BY_SUITE_MARKER, results_by_suite_json, 1)
    html = html.replace(_BASELINES_BY_SUITE_MARKER, baselines_by_suite_json, 1)
    html = html.replace(_SCENARIOS_META_MARKER, scenarios_meta_json, 1)
    # Backward-compat replacements (no-op if markers absent from template).
    html = html.replace(_RESULTS_MARKER, results_single_json, 1)
    html = html.replace(_BASELINE_MARKER, baseline_single_json, 1)

    return html


# ---------------------------------------------------------------------------
# __main__ — file I/O wrapper
# ---------------------------------------------------------------------------

_SUITE_NAMES = ["small", "medium", "large"]


def main() -> int:
    results_dir = _BENCH_DIR / "results"
    baseline_path = _BENCH_DIR / "baseline.json"
    template_path = _BENCH_DIR / "dashboard" / "template.html"
    output_path = results_dir / "index.html"

    if not template_path.exists():
        print(
            f"ERROR: template not found: {template_path}",
            file=sys.stderr,
        )
        return 1

    # Scan for per-suite result files; fall back to latest.json for compat.
    results_by_suite: dict[str, Any] = {}
    for suite in _SUITE_NAMES:
        suite_path = results_dir / f"latest-{suite}.json"
        if suite_path.exists():
            results_by_suite[suite] = json.loads(suite_path.read_text())

    if not results_by_suite:
        # Fallback: single latest.json (older harness run).
        latest_path = results_dir / "latest.json"
        if latest_path.exists():
            results_by_suite["medium"] = json.loads(latest_path.read_text())
        else:
            print(
                f"ERROR: no results files found in {results_dir}\n"
                "Run `task test:bench` or `task test:bench:quick` first.",
                file=sys.stderr,
            )
            return 1

    baselines_by_suite: dict[str, Any] | None = None
    if baseline_path.exists():
        baselines_by_suite = {"medium": json.loads(baseline_path.read_text())}
    else:
        print(
            f"NOTE: baseline.json not found at {baseline_path} — "
            "dashboard will show current results only (no baseline comparison)."
        )

    template = template_path.read_text()
    html = generate_report(results_by_suite, baselines_by_suite, template)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(html)
    print(f"Dashboard written to: {output_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
