#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The unit-test floor and skip ceiling, re-derived from Bazel's build event stream.

    scripts/bazel_test_floor.py --bep <build_event_json_file>
    scripts/bazel_test_floor.py --self-test

`cargo nextest list --workspace --release --locked` is what `rust:test:floor`
reads, and on `verify-basic.yml`'s `smoke` job that listing cost a measured
**536.5 s** — it must build every test binary in release before it can name a
case (`measurement_bazel_r2.md` § 6). Swapping only the *execution* step to
Bazel therefore removed a 173.5 s step and left the 536.5 s compile in place,
which is the arithmetic WP-30's NO-GO rested on. This reader is the other half:
the floor's subject moves to the run Bazel already did, and `cargo nextest list`
leaves the gating lane entirely.

**What it reads, and why it is not `test.xml`.** The ADR's design was a
`rust_test` wired to `$XML_OUTPUT_FILE`, giving Bazel a real per-case JUnit
report. Measured on this tree at bazel 9.2.0, rules_rust 0.68 — no `rust_test`
in `crates/*/BUILD.bazel` carries that wiring, so Bazel writes its **synthesised**
`test.xml`, which is the ADR's own falsifier:

    <testsuite name="crates/<crate>/<crate>_test" tests="1" …>
      <testcase name="crates/<crate>/<crate>_test" …>

(that `name` is the target LABEL with the colon rendered as a slash - it is
not a path, which is why it is spelled with placeholders here: a literal one
reads as a file claim to `scripts/dead_path_sweep.py`)

One `testcase` per *target*. A reader summing those elements reports **34**
against a floor of 8174 and passes nothing while looking like a count. So this
takes the ADR's declared fallback: the `test.log` named in the same
`TestResult` event, and libtest's own summary line,

    test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

which `crates/TEST_TARGET_MAP.toml` was itself generated from. Both files are
`testActionOutput` entries of the same event, and a cache hit replays both —
measured: `//crates/ocx_exit:ocx_exit_test` `(cached) PASSED`, `test.log` present
and parseable, `cachedLocally: true`.

**Per-target, never only a sum.** `quality-core.md` § "Unchecked Green": a
sum-only floor is green while three `#[test]` fns are deleted from inside an
existing target, because the sum has 10 tests of headroom and the target count
does not move at all. The map records the count *per label*, this compares
*per label*, and the sum falls out of that rather than being asserted beside it.

**Three floors, three subjects** (ADR § "Its authority stops at that number"):

    this reader     crate test targets      `TEST_TARGET_MAP.toml` rows — 34
    bazel:build:drift   every //crates/ + external/ target      56
    bep_to_otlp     every target the BEP announces              54

`CRATES_TEST_TARGETS` is imported from `scripts/bazel_gate_proofs.py` rather
than spelled here: a second copy of a floor is the copy that goes stale.

**`--junit <path>` — the per-case report, from the same logs.** Until the WP-30
lane swap, `verify-basic.yml`'s `smoke` job published
`target/nextest/ci/junit.xml` to the pull request (EnricoMi's
publish-unit-test-result-action) and to otel.ocx.sh (`.github/actions/
test-telemetry`). Both steps left with `cargo nextest`, and Bazel's synthesised
`test.xml` cannot stand in for the reason above: one `testcase` per *target*,
named after the label. Measured on this tree for a deliberately failed test, the
failing `#[test]` fn's name appears **only** inside the testsuite-level
`<system-out>` CDATA — never as a `testcase name`, and never in `<error
message>`, which reads `exited with error code 101`. EnricoMi (v2.23.0) takes a
case's text from its own `failure`/`error` element and its stdout from the
*testcase-level* `<system-out>` Bazel does not emit, so publishing
`bazel-testlogs/**/test.xml` names 34 targets and cannot name one Rust test. So
this writes the report instead, off libtest's own per-case lines in the very
`test.log` the floor already opens:

    test exit_code::tests::exit_code_success_is_zero ... ok
    test exit_code::tests::exit_code_usage_error_is_64 ... ignored
    test exit_code::tests::exit_code_failure_is_one ... FAILED

**Floored on its own reader** (`quality-core.md` § "Unchecked Green"). The
`testcase` elements emitted for a target must number exactly
`passed + failed + ignored + measured` from that target's summary line — two
independent readings of one log, so a libtest format change that moves one moves
them apart and reds. Measured red: a fixture whose separator is ` .. ` instead of
` ... ` — the shape such a change has — leaves the per-case parser matching
nothing while `RESULT_LINE` still parses, and the writer reports `junit-count`
per target rather than handing a publisher a thin, well-formed, empty report.

**And a red target can never vanish from it.** A log the per-case parser cannot
read at all — a timeout, a harness that died before its first line, a test rule
that is not libtest — still gets one `testcase` named after the target, carrying
the log's tail as its failure. Same for a target whose log parses clean while the
build event stream reports it FAILED: the publisher's report must not be quieter
than the run.

Stdlib only (plan DEC-8) — `xml.etree.ElementTree` writes the report, because a
hand-rolled emitter is exactly the escaping bug `quality-core.md` § "Don't Own
Non-Domain Code" keeps as its worked example.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import re
import sys
import tempfile
import tomllib
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from bazel_gate_proofs import (
    CRATES_TEST_TARGETS,
    CRATES_TWIN_TEST_TARGETS,
    Finding,
    codes,
    expect,
    report,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
TEST_TARGET_MAP = REPO_ROOT / "crates" / "TEST_TARGET_MAP.toml"
CEILING_FILE = REPO_ROOT / "crates" / "NEXTEST_SKIP_CEILING"

#: libtest's summary line. Every count is read from it and none is inferred:
#: `executed` is `passed + failed`, because a red target must not lower the
#: floor it is being judged against — a failing test still ran.
RESULT_LINE = re.compile(
    r"^test result: \w+\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored; (?P<measured>\d+) measured; (?P<filtered>\d+) filtered out",
    re.MULTILINE,
)

#: libtest's per-case line — the only place in the whole run where a Rust test's
#: own name is written down. `test result: ok. 21 passed; …` carries no ` ... `,
#: so the summary line cannot match this and the two readings stay independent.
CASE_LINE = re.compile(r"^test (?P<name>\S[^\n]*?) \.\.\. (?P<outcome>[^\n]+?)[ \t]*$", re.MULTILINE)

#: A failing case's captured output, `---- <name> stdout ----` through to the next
#: block or the trailing `failures:` roll-up. This is the panic message, and it is
#: what makes a published failure actionable rather than `error code 101`.
#:
#: **This text reaches a PUBLIC pull-request check**, verbatim, as does `_tail`'s
#: 40-line log excerpt below. Nothing sensitive is in scope today because nothing
#: sensitive reaches the test: no `.bazelrc` line and no taskfile passes
#: `--test_env`, so a `rust_test` action runs with Bazel's own static environment
#: — `PATH` plus the `TEST_*` variables Bazel synthesises — and the one product
#: variable any unit test reads, `__OCX_TESTING_REQUIRE_LIVE_SHELLS`, is set by no
#: lane. `--test_env` (or `--test_env=NAME` without a value, which *inherits* from
#: the client) is the knob that changes that: adding one puts whatever it names
#: one panic away from the pull request, so a secret never belongs behind it.
FAILURE_BLOCK = re.compile(
    r"^---- (?P<name>[^\n]+?) (?:stdout|stderr) ----\n(?P<body>.*?)(?=^---- |^failures:$|\Z)",
    re.MULTILINE | re.DOTALL,
)

#: XML 1.0 admits #x9, #xA, #xD and the printable planes — and nothing else, not
#: even as an escape. libtest copies a failing test's raw stdout into its block,
#: ANSI colour and all, and `ElementTree` serialises a `\x1b` verbatim: the file
#: is then ill-formed and the publisher rejects the whole report over one case.
XML_FORBIDDEN = re.compile(
    "[^\\x09\\x0a\\x0d\\x20-\\ud7ff\\ue000-\\ufffd\\U00010000-\\U0010ffff]"
)

#: Bazel statuses that are not a red. `FLAKY` lands on the summary event, never on
#: the attempt this reader keeps, but naming it costs nothing and a future retry
#: policy that does emit it must not manufacture a phantom failure.
PASSING_STATUS = frozenset({"PASSED", "FLAKY"})

READER_MSG = (
    "bazel test floor read {read} test target(s) from the build event stream, "
    "crates/TEST_TARGET_MAP.toml has {rows} rows and scripts/bazel_gate_proofs.py "
    "floors at {floor} — the reader stopped early, or the run selected a partial graph"
)
UNREADABLE_MSG = (
    "bazel test floor: {label} has no libtest `test result:` line in {path} — its count is "
    "unknown, which is not the same as its count being high enough"
)
MISSING_MSG = (
    "bazel test floor: {label} is in crates/TEST_TARGET_MAP.toml and absent from the run — "
    "a target that did not execute contributes no cases, and the sum alone cannot see it"
)
SHRANK_MSG = (
    "bazel test floor: {label} executed {observed} case(s), crates/TEST_TARGET_MAP.toml "
    "records {recorded} — tests were removed from an existing target. Counts rise only; "
    "lower the row in the same commit and say why in its body"
)
CEILING_MSG = (
    "bazel test ceiling: {ignored} `#[ignore]`d case(s) across {targets} target(s) exceeds "
    "crates/NEXTEST_SKIP_CEILING ({ceiling})"
)
EMPTY_MSG = (
    "bazel test floor: {path} holds no `testResult` event. An export of nothing is the one "
    "thing this gate must not report as a success — check that `bazel test //crates/...` ran "
    "with `--build_event_json_file`"
)
JUNIT_COUNT_MSG = (
    "bazel junit: {label} yields {written} `test <name> ... <outcome>` line(s) and its libtest "
    "summary records {expected} (passed + failed + ignored + measured) — two readings of one "
    "log that disagree. The report would be thinner than the run, which is what a libtest "
    "output-format change looks like from here; fix CASE_LINE, do not lower this"
)
JUNIT_EMPTY_MSG = (
    "bazel junit: not one `testcase` could be built from {targets} target(s), so {path} was "
    "not written. A well-formed report naming nothing is the one thing this writer must not "
    "hand a publisher that reports exactly what it is given"
)
UNREADABLE_CASE_MSG = (
    "the per-case reader found no `test <name> ... <outcome>` line and no libtest summary in "
    "this target's test.log, so not one of its cases can be named. Bazel reported {status}. "
    "The tail of the log follows"
)
STATUS_CASE_MSG = (
    "Bazel reported {status} for this target while every case in its test.log reads as a pass "
    "— the failure is the test binary's, not any one case's (a crash after the summary, a "
    "timeout in a fixture teardown). The tail of the log follows"
)


@dataclasses.dataclass(frozen=True)
class Counts:
    """One target's libtest summary, summed over its attempts' log."""

    executed: int
    ignored: int
    filtered: int
    measured: int = 0

    @property
    def lines(self) -> int:
        """How many per-case lines that summary implies — the floor `--junit`'s writer is
        held to. `filtered` is the one bucket libtest prints no line for."""
        return self.executed + self.ignored + self.measured


@dataclasses.dataclass(frozen=True)
class Case:
    """One libtest case, or one whole target standing in for cases nothing could read."""

    name: str
    outcome: str
    detail: str

    @property
    def failed(self) -> bool:
        return self.outcome.startswith("FAILED")

    @property
    def skipped(self) -> bool:
        return self.outcome.startswith("ignored")


@dataclasses.dataclass(frozen=True)
class Run:
    """One target's highest-numbered attempt: where its log is, what Bazel called it,
    and how long it took. The duration is the only timing in the whole run — libtest
    prints none per case without `--report-time`, which rules_rust does not pass."""

    log: Path
    status: str
    seconds: float


@dataclasses.dataclass(frozen=True)
class Row:
    """One `crates/TEST_TARGET_MAP.toml` `[[target]]`."""

    label: str
    cases: int
    ignored: int
    skipped: int


def read_rows(path: Path) -> list[Row]:
    """The committed per-target record. Absent or unparseable yields `[]`, which the
    reader floor below turns into a finding rather than into a vacuous pass."""
    try:
        payload = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return []
    return [
        Row(
            label=str(entry["label"]),
            cases=int(entry.get("cases", 0)),
            ignored=int(entry.get("ignored", 0)),
            skipped=int(entry.get("skipped", 0)),
        )
        for entry in payload.get("target", [])
        if "label" in entry
    ]


def _log_path(uri: str) -> Path:
    """A BEP `file://` URI as a path. Bazel percent-encodes; `urllib` is the decoder."""
    parsed = urllib.parse.urlparse(uri)
    return Path(urllib.request.url2pathname(parsed.path))


def _seconds(payload: dict) -> float:
    """proto3 renders a `Duration` as `"0.051s"`; the same event also carries
    `testAttemptDurationMillis` as a *string*. Either, or 0 — a report with no timing
    is still a report, and neither field is load-bearing for any floor here."""
    duration = payload.get("testAttemptDuration")
    if isinstance(duration, str) and duration.endswith("s"):
        try:
            return float(duration[:-1])
        except ValueError:
            return 0.0
    try:
        return float(payload.get("testAttemptDurationMillis", 0)) / 1000.0
    except (TypeError, ValueError):
        return 0.0


def read_runs(bep: Path) -> dict[str, Run]:
    """`{label: Run}` over every `TestResult` in `bep`, in label order.

    The highest `attempt` wins: a flaky target under `--flaky_test_attempts`
    emits one event per attempt, and summing them would inflate the very number
    this gate floors on.

    Split out of `read_results` so the floor and `--junit` share *one* walk of the
    stream (`run_check` calls this once and hands the result to both). Two walks
    would be two chances for the report to name a target set the floor never judged.
    """
    best: dict[str, tuple[int, Run]] = {}
    try:
        text = bep.read_text(encoding="utf-8")
    except OSError:
        return {}
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        identifier = event.get("id", {}).get("testResult")
        if not isinstance(identifier, dict):
            continue
        label = str(identifier.get("label", ""))
        attempt = int(identifier.get("attempt", 1) or 1)
        payload = event.get("testResult", {})
        logs = [
            output
            for output in payload.get("testActionOutput", [])
            if output.get("name") == "test.log" and output.get("uri", "").startswith("file:")
        ]
        if not label or not logs:
            continue
        if attempt >= best.get(label, (0, None))[0]:
            best[label] = (
                attempt,
                Run(
                    log=_log_path(logs[-1]["uri"]),
                    status=str(payload.get("status", "")),
                    seconds=_seconds(payload),
                ),
            )
    return {label: run for label, (_, run) in sorted(best.items())}


def _read_log(path: Path) -> str:
    """Absent or unreadable yields `""`, which every reader below turns into a finding
    rather than into a target that quietly stops existing."""
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def read_results(bep: Path) -> tuple[dict[str, Counts], list[Finding]]:
    """`{label: Counts}` over every `TestResult` in `bep`, plus per-target read failures."""
    return count_results(read_runs(bep))


def count_results(runs: dict[str, Run]) -> tuple[dict[str, Counts], list[Finding]]:
    """Each run's libtest summary, or the finding that it had none."""
    observed: dict[str, Counts] = {}
    findings: list[Finding] = []
    for label, run in runs.items():
        matches = list(RESULT_LINE.finditer(_read_log(run.log)))
        if not matches:
            findings.append(
                Finding("floor-unreadable", UNREADABLE_MSG.format(label=label, path=run.log))
            )
            continue
        observed[label] = Counts(
            executed=sum(int(m["passed"]) + int(m["failed"]) for m in matches),
            ignored=sum(int(m["ignored"]) for m in matches),
            filtered=sum(int(m["filtered"]) for m in matches),
            measured=sum(int(m["measured"]) for m in matches),
        )
    return observed, findings


def read_cases(log: str) -> list[Case]:
    """libtest's per-case lines, each failure carrying its `---- <name> stdout ----` block.

    Case lines *inside* a failure block are skipped. libtest copies a failing test's
    captured stdout there verbatim, so a test that prints `test x ... ok` would
    otherwise be counted as a case of its own and red the count floor over nothing.
    """
    panics: dict[str, list[str]] = {}
    blocks: list[tuple[int, int]] = []
    for match in FAILURE_BLOCK.finditer(log):
        panics.setdefault(match["name"], []).append(match["body"].strip())
        blocks.append((match.start(), match.end()))
    return [
        Case(
            name=match["name"],
            outcome=match["outcome"],
            detail="\n\n".join(panics.get(match["name"], ())),
        )
        for match in CASE_LINE.finditer(log)
        if not any(start <= match.start() < end for start, end in blocks)
    ]


def floor_findings(
    observed: dict[str, Counts], rows: list[Row], *, minimum: int = CRATES_TEST_TARGETS
) -> list[Finding]:
    """The reader floor first, then the per-target comparison it makes meaningful."""
    findings: list[Finding] = []
    if len(observed) < max(minimum, len(rows)):
        findings.append(
            Finding(
                "floor-reader",
                READER_MSG.format(read=len(observed), rows=len(rows), floor=minimum),
            )
        )
    for row in rows:
        seen = observed.get(row.label)
        if seen is None:
            findings.append(Finding("floor-missing", MISSING_MSG.format(label=row.label)))
        elif seen.executed < row.cases:
            findings.append(
                Finding(
                    "floor-shrank",
                    SHRANK_MSG.format(
                        label=row.label, observed=seen.executed, recorded=row.cases
                    ),
                )
            )
    return findings


def ceiling_findings(observed: dict[str, Counts], ceiling: int) -> list[Finding]:
    """C-013 in Bazel's currency. A cache hit replays the target's `test.log`, so the
    ignored count is the test binary's own and cannot move with cache warmth — which is
    the property the nextest arm buys by never reading a run artifact at all."""
    ignored = sum(counts.ignored for counts in observed.values())
    if ignored > ceiling:
        return [
            Finding(
                "ceiling",
                CEILING_MSG.format(ignored=ignored, targets=len(observed), ceiling=ceiling),
            )
        ]
    return []


def read_ceiling(path: Path) -> int:
    try:
        return int(path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return 0


# ---------------------------------------------------------------------------
# --junit — one `testcase` per libtest case, floored on the summary parser
# ---------------------------------------------------------------------------


def _xml_text(text: str) -> str:
    return XML_FORBIDDEN.sub("", text)


def _tail(log: str, lines: int = 40) -> str:
    return "\n".join(log.splitlines()[-lines:])


def junit_tree(
    runs: dict[str, Run], observed: dict[str, Counts]
) -> tuple[ET.Element, list[Finding]]:
    """The `testsuites` document, and every way the two readings of a log disagreed.

    `runs` and `observed` come from one stream walk, so a target present in the first
    and absent from the second means exactly one thing: its log had no libtest summary.
    That target gets a `testcase` of its own rather than no entry at all.
    """
    root = ET.Element("testsuites")
    findings: list[Finding] = []
    for label, run in runs.items():
        log = _read_log(run.log)
        cases = read_cases(log)
        counts = observed.get(label)
        if counts is None:
            cases = [
                *cases,
                Case(
                    name=label,
                    outcome="FAILED",
                    detail=f"{UNREADABLE_CASE_MSG.format(status=run.status or 'no status')}\n\n"
                    f"{_tail(log)}",
                ),
            ]
        elif len(cases) != counts.lines:
            findings.append(
                Finding(
                    "junit-count",
                    JUNIT_COUNT_MSG.format(
                        label=label, written=len(cases), expected=counts.lines
                    ),
                )
            )
        if run.status not in PASSING_STATUS and not any(case.failed for case in cases):
            cases = [
                *cases,
                Case(
                    name=label,
                    outcome="FAILED",
                    detail=f"{STATUS_CASE_MSG.format(status=run.status or 'no status')}\n\n"
                    f"{_tail(log)}",
                ),
            ]
        suite = ET.SubElement(
            root,
            "testsuite",
            {
                "name": label,
                "tests": str(len(cases)),
                "failures": str(sum(1 for case in cases if case.failed)),
                "errors": "0",
                "skipped": str(sum(1 for case in cases if case.skipped)),
                "time": f"{run.seconds:.3f}",
            },
        )
        for case in cases:
            # `classname` is the target and `name` is libtest's own test path, which is
            # the whole point: EnricoMi renders `<classname> ‣ <name>`, so a failure
            # arrives in the pull request as the `#[test]` fn someone can go and open.
            element = ET.SubElement(
                suite,
                "testcase",
                {"classname": _xml_text(label), "name": _xml_text(case.name), "time": "0"},
            )
            if case.failed:
                message = _xml_text(case.detail.splitlines()[0] if case.detail else "") or "FAILED"
                ET.SubElement(element, "failure", {"message": message}).text = _xml_text(
                    case.detail
                )
            elif case.skipped:
                ET.SubElement(element, "skipped", {"message": _xml_text(case.outcome)})
    return root, findings


def write_junit(
    path: Path, runs: dict[str, Run], observed: dict[str, Counts]
) -> tuple[int, list[Finding]]:
    """Write the report, and return how many `testcase` elements it carries."""
    root, findings = junit_tree(runs, observed)
    written = sum(len(suite) for suite in root)
    if not written:
        return 0, [
            *findings,
            Finding("junit-empty", JUNIT_EMPTY_MSG.format(targets=len(runs), path=path)),
        ]
    root.set("tests", str(written))
    root.set("failures", str(len(root.findall("./testsuite/testcase/failure"))))
    root.set("errors", "0")
    root.set("skipped", str(len(root.findall("./testsuite/testcase/skipped"))))
    root.set("time", f"{sum(run.seconds for run in runs.values()):.3f}")
    path.parent.mkdir(parents=True, exist_ok=True)
    tree = ET.ElementTree(root)
    ET.indent(tree, space="  ")
    tree.write(path, encoding="utf-8", xml_declaration=True)
    return written, findings


def run_check(bep: Path, junit: Path | None = None) -> int:
    if not bep.is_file() or bep.stat().st_size == 0:
        print(EMPTY_MSG.format(path=bep), file=sys.stderr)
        return 1
    runs = read_runs(bep)
    observed, findings = count_results(runs)
    if not observed and not findings:
        print(EMPTY_MSG.format(path=bep), file=sys.stderr)
        return 1
    rows = read_rows(TEST_TARGET_MAP)
    findings += floor_findings(observed, rows)
    findings += ceiling_findings(observed, read_ceiling(CEILING_FILE))
    if junit is not None:
        written, junit_findings = write_junit(junit, runs, observed)
        findings += junit_findings
        if written:
            print(
                # `runs`, not `observed`: a target whose log no summary could be read
                # from is in the report as its own failing case, and saying `observed`
                # here would report a narrower scope than the one that was written.
                f"bazel junit: {written} <testcase> element(s) across {len(runs)} target(s) "
                f"written to {junit}"
            )
    if not findings:
        print(
            f"bazel test floor: {len(observed)} test target(s) under //crates/... reported "
            f"{sum(c.executed for c in observed.values())} executed case(s), "
            f"{sum(c.ignored for c in observed.values())} ignored, "
            f"{sum(c.filtered for c in observed.values())} filtered out — every one of "
            f"{len(rows)} recorded targets at or above its count"
        )
    return report(findings)


# ---------------------------------------------------------------------------
# Self-test — every finding shown red, and the same fixture shown green
# ---------------------------------------------------------------------------


def _log(
    path: Path,
    *,
    passed: int,
    failed: int = 0,
    ignored: int = 0,
    filtered: int = 0,
    mangle: bool = False,
) -> str:
    """A libtest log with the real preamble and one per-case line per non-filtered case,
    written to `path`, returning its `file://` URI.

    `mangle=True` writes a summary `RESULT_LINE` still parses beside per-case lines
    `CASE_LINE` cannot — ` .. ` for ` ... `, the shape a libtest output-format change
    has from this side. That is the fixture the count floor exists for.
    """
    separator = " .. " if mangle else " ... "
    lines = [f"Executing tests from {path.parent.name}", "", f"running {passed + failed + ignored} tests"]
    lines += [f"test fixture::passes_{index}{separator}ok" for index in range(passed)]
    lines += [f"test fixture::skips_{index}{separator}ignored" for index in range(ignored)]
    lines += [f"test fixture::fails_{index}{separator}FAILED" for index in range(failed)]
    if failed:
        lines.append("")
        lines.append("failures:")
        for index in range(failed):
            lines += [
                "",
                f"---- fixture::fails_{index} stdout ----",
                f"thread 'fixture::fails_{index}' panicked at crates/demo/src/lib.rs:7:5:",
                "assertion `left == right` failed",
                "  left: 1",
                " right: 2",
            ]
        lines += ["", "failures:"] + [f"    fixture::fails_{i}" for i in range(failed)]
    lines += [
        "",
        (
            f"test result: {'FAILED' if failed else 'ok'}. {passed} passed; {failed} failed; "
            f"{ignored} ignored; 0 measured; {filtered} filtered out; finished in 0.01s"
        ),
        "",
    ]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines), encoding="utf-8")
    return path.as_uri()


def _bep(path: Path, entries: dict[str, str], *, status: str = "PASSED") -> Path:
    """A build event stream carrying one `TestResult` per entry, and the noise a real one
    carries around them — a reader keyed on line position would pass over this and fail on
    the real thing."""
    lines = [json.dumps({"id": {"started": {}}, "started": {"uuid": "fixture"}})]
    for label, uri in entries.items():
        lines.append(
            json.dumps(
                {
                    "id": {"testResult": {"label": label, "run": 1, "shard": 1, "attempt": 1}},
                    "testResult": {
                        "status": status,
                        "testAttemptDuration": "0.051s",
                        "cachedLocally": True,
                        "testActionOutput": [
                            {"name": "test.xml", "uri": uri.replace("test.log", "test.xml")},
                            {"name": "test.log", "uri": uri},
                        ],
                    },
                }
            )
        )
        lines.append(json.dumps({"id": {"targetCompleted": {"label": label}}}))
    lines.append(json.dumps({"id": {"buildFinished": {}}}))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def _fixture(
    work: Path,
    rows: list[Row],
    *,
    name: str = "bep.json",
    folder: str = "logs",
    mangled: frozenset[str] = frozenset(),
) -> Path:
    """The live map, replayed as a stream that satisfies it exactly. Built from the real
    rows so the green leg is a statement about *this* tree rather than about an invented
    universe that can drift away from it. `mangled` names the labels whose per-case lines
    the reader must fail on while their summary still parses."""
    entries = {
        row.label: _log(
            work / folder / row.label.replace("//", "").replace(":", "/") / "test.log",
            passed=row.cases,
            ignored=row.ignored,
            filtered=row.skipped,
            mangle=row.label in mangled,
        )
        for row in rows
    }
    return _bep(work / name, entries)


def prove_floor(work: Path, rows: list[Row]) -> int:
    """Green on the live universe, then red on each way it can shrink."""
    checks = 0
    ceiling = read_ceiling(CEILING_FILE)

    bep = _fixture(work, rows)
    observed, read_findings = read_results(bep)
    expect(not read_findings, f"the green fixture must read cleanly, got {codes(read_findings)}")
    expect(
        len(observed) == len(rows) == CRATES_TEST_TARGETS + CRATES_TWIN_TEST_TARGETS,
        f"the fixture must carry all {CRATES_TEST_TARGETS + CRATES_TWIN_TEST_TARGETS} targets, "
        f"it carries {len(observed)}",
    )
    total = sum(counts.executed for counts in observed.values())
    findings = floor_findings(observed, rows) + ceiling_findings(observed, ceiling)
    expect(not findings, f"the green fixture must produce no finding, got {codes(findings)}")
    print(
        f"FLOOR GREEN : {len(observed)} targets, {total} executed cases, "
        f"{sum(c.ignored for c in observed.values())} ignored (ceiling {ceiling}) — no finding"
    )
    checks += 1

    # A target deleted from the graph. The sum drops with it, but that is not what
    # is asserted: a sum floor set below the total would not have noticed.
    dropped = dict(observed)
    victim = rows[0].label
    del dropped[victim]
    red = floor_findings(dropped, rows)
    expect("floor-missing" in codes(red), f"a dropped target must red, got {codes(red)}")
    print(f"FLOOR RED   : {next(f.message for f in red if f.code == 'floor-missing')[:120]}")
    checks += 1

    # Three `#[test]` fns deleted from inside a target that still exists — the case
    # the previous draft's target-count fallback could not see at all, and the one
    # `crates/NEXTEST_FLOOR`'s 10 cases of headroom absorb.
    biggest = max(rows, key=lambda row: row.cases)
    shrunk = dict(observed)
    shrunk[biggest.label] = dataclasses.replace(
        observed[biggest.label], executed=biggest.cases - 3
    )
    red = floor_findings(shrunk, rows)
    expect(codes(red) == ["floor-shrank"], f"three deleted tests must red alone, got {codes(red)}")
    expect(
        sum(c.executed for c in shrunk.values()) > total - 10,
        "the mutation must stay inside NEXTEST_FLOOR's headroom, or it proves the sum floor",
    )
    print(f"FLOOR RED   : {red[0].message[:150]}")
    checks += 1

    # The reader itself stopping. Three targets is a graph that loaded almost
    # nothing, and a per-target comparison over three rows is green on all three.
    partial = {row.label: observed[row.label] for row in rows[:3]}
    red = floor_findings(partial, rows)
    expect("floor-reader" in codes(red), f"a partial read must red, got {codes(red)}")
    print(f"FLOOR RED   : {next(f.message for f in red if f.code == 'floor-reader')[:150]}")
    checks += 1

    # A log with no summary line: the target ran, the count is unknown. Silence here
    # would leave it out of `observed` and red only on the reader floor, which reads
    # as "the graph shrank" and sends the reader to the wrong file.
    mute = work / "logs" / "mute" / "test.log"
    mute.parent.mkdir(parents=True, exist_ok=True)
    mute.write_text("running 5 tests\n<the harness died here>\n", encoding="utf-8")
    _, red = read_results(_bep(work / "mute-bep.json", {"//crates/x:y": mute.as_uri()}))
    expect(codes(red) == ["floor-unreadable"], f"a mute log must red, got {codes(red)}")
    print(f"FLOOR RED   : {red[0].message[:150]}")
    checks += 1

    # An empty stream. `run_check` is entered rather than the predicate, because the
    # emptiness is refused before any predicate sees it.
    empty = work / "empty-bep.json"
    empty.write_text("", encoding="utf-8")
    expect(run_check(empty) == 1, "an empty BEP must exit 1")
    absent = work / "no-such-bep.json"
    expect(run_check(absent) == 1, "an absent BEP must exit 1")
    print("FLOOR RED   : an empty and an absent build event stream both exit 1")
    checks += 1

    return checks


def prove_ceiling(work: Path, rows: list[Row]) -> int:
    """The skip ceiling, red and green, on the same stream the floor reads."""
    ceiling = read_ceiling(CEILING_FILE)
    expect(ceiling > 0, f"crates/NEXTEST_SKIP_CEILING is {ceiling} — a ceiling of 0 proves nothing")
    observed, _ = read_results(_fixture(work, rows, name="ceiling-bep.json"))
    live = sum(counts.ignored for counts in observed.values())
    expect(not ceiling_findings(observed, ceiling), f"{live} ignored must be under {ceiling}")

    over = dict(observed)
    victim = rows[0].label
    over[victim] = dataclasses.replace(observed[victim], ignored=ceiling + 1)
    red = ceiling_findings(over, ceiling)
    expect(codes(red) == ["ceiling"], f"an over-ceiling stream must red, got {codes(red)}")
    print(f"CEILING GREEN: {live} ignored against a ceiling of {ceiling}")
    print(f"CEILING RED  : {red[0].message[:150]}")
    return 2


def prove_junit(work: Path, rows: list[Row]) -> int:
    """`--junit` red and green: the exact count, a named failure, a mangled per-case
    parser, an unreadable log, and a target Bazel reds while its cases all pass."""
    checks = 0
    reports = work / "reports"

    # Green, on the live universe. `reports/` does not exist yet, which is also the
    # proof that the writer creates its parent.
    bep = _fixture(work, rows, name="junit-bep.json")
    observed, read_findings = read_results(bep)
    expect(not read_findings, f"the green fixture must read cleanly, got {codes(read_findings)}")
    written, findings = write_junit(reports / "unit-junit.xml", read_runs(bep), observed)
    expect(not findings, f"the green fixture must emit no junit finding, got {codes(findings)}")
    document = ET.parse(reports / "unit-junit.xml")
    cases = document.findall(".//testcase")
    recorded = sum(row.cases + row.ignored for row in rows)
    expect(
        len(cases) == written == recorded,
        f"{len(cases)} testcase elements for {recorded} recorded cases — the report and "
        f"crates/TEST_TARGET_MAP.toml must name the same universe",
    )
    # Every target owns a testsuite, `classname` included — even
    # `//crates/ocx_cli:macos_self_contained`, whose 0 cases are all filtered out on
    # this host. An empty suite is the honest report of a target that ran nothing;
    # dropping it would make "absent" mean both "ran nothing" and "never ran".
    expect(
        len(document.findall("testsuite")) == len(rows),
        f"{len(document.findall('testsuite'))} testsuites for {len(rows)} targets",
    )
    expect(
        {case.get("classname") for case in cases} <= {row.label for row in rows},
        "every testcase must name a target the run actually carried",
    )
    expect(
        len(document.findall(".//skipped")) == sum(row.ignored for row in rows),
        "an `#[ignore]`d case must reach the report as <skipped>, not vanish from it",
    )
    print(
        f"JUNIT GREEN : {len(cases)} <testcase> across {len(rows)} targets, "
        f"{len(document.findall('.//skipped'))} skipped — exactly the recorded counts"
    )
    checks += 1

    # A failing case, by its own name and with its panic text. This is the whole point:
    # Bazel's synthesised test.xml can name the target and nothing else.
    label = "//crates/demo:demo_test"
    failing = _log(work / "failing" / "test.log", passed=2, failed=1)
    bep = _bep(work / "junit-fail-bep.json", {label: failing}, status="FAILED")
    observed, _ = read_results(bep)
    written, findings = write_junit(reports / "fail.xml", read_runs(bep), observed)
    expect(not findings, f"a readable failing log must emit no junit finding, got {codes(findings)}")
    document = ET.parse(reports / "fail.xml")
    named = document.find(".//testcase[@name='fixture::fails_0']/failure")
    expect(named is not None, "the failing case must appear under its own libtest name")
    expect(
        "assertion `left == right` failed" in (named.text or ""),
        f"the failure must carry its panic text, it carries {(named.text or '')[:80]!r}",
    )
    expect(
        document.find(f".//testcase[@name='{label}']") is None,
        "a log whose cases are readable must never fall back to a target-level case",
    )
    expect(written == 3, f"3 cases were run, {written} were reported")
    print(f"JUNIT GREEN : failing case named `fixture::fails_0` — {named.get('message')}")
    checks += 1

    # The count floor, on one target only: the report is written, well-formed and
    # thick — and wrong. A writer floored on nothing would ship it.
    victim = max(rows, key=lambda row: row.cases)
    bep = _fixture(
        work,
        rows,
        name="junit-one-mangled.json",
        folder="one-mangled",
        mangled=frozenset({victim.label}),
    )
    observed, _ = read_results(bep)
    written, red = write_junit(reports / "one-mangled.xml", read_runs(bep), observed)
    expect(codes(red) == ["junit-count"], f"one mangled target must red alone, got {codes(red)}")
    expect(len(red) == 1, f"one mangled target must yield one finding, got {len(red)}")
    expect(
        written == recorded - victim.cases - victim.ignored,
        "the mutation must land: the report must be exactly the mangled target thinner",
    )
    print(f"JUNIT RED   : {red[0].message[:150]}")
    checks += 1

    # Every target mangled — the state a libtest format change actually produces. The
    # per-case parser matches nothing, `RESULT_LINE` still parses, and the writer
    # refuses to hand a publisher a well-formed report naming nobody.
    bep = _fixture(work, rows, name="junit-mangled.json", folder="mangled", mangled=frozenset(
        row.label for row in rows
    ))
    observed, _ = read_results(bep)
    written, red = write_junit(reports / "mangled.xml", read_runs(bep), observed)
    expect(
        codes(red) == ["junit-count", "junit-empty"],
        f"a mangled per-case parser must red on both counts, got {codes(red)}",
    )
    # Every target that ran a case reds on its own. `macos_self_contained` runs none on
    # this host, and 0 lines against a summary of 0 is a match however the separator is
    # spelled — a target with nothing to mangle cannot be evidence that mangling reds.
    carrying = [row for row in rows if row.cases + row.ignored]
    expect(
        sum(1 for finding in red if finding.code == "junit-count") == len(carrying),
        f"every one of {len(carrying)} targets carrying a case must red, not a sum over them",
    )
    expect(written == 0 and not (reports / "mangled.xml").exists(), "no report must be written")
    print(f"JUNIT RED   : {next(f.message for f in red if f.code == 'junit-empty')[:150]}")
    checks += 1

    # A log nothing can read — a timeout, a harness that died before its first line.
    # Silence here is a red target that vanished from the report.
    label = "//crates/x:y_test"
    dead = work / "dead" / "test.log"
    dead.parent.mkdir(parents=True, exist_ok=True)
    dead.write_text("running 5 tests\n<the harness died here>\n", encoding="utf-8")
    bep = _bep(work / "junit-dead.json", {label: dead.as_uri()}, status="TIMEOUT")
    observed, read_findings = read_results(bep)
    expect(codes(read_findings) == ["floor-unreadable"], "the floor must red on a mute log")
    written, findings = write_junit(reports / "dead.xml", read_runs(bep), observed)
    fallback = ET.parse(reports / "dead.xml").find(f".//testcase[@name='{label}']/failure")
    expect(written == 1, f"an unreadable log must still report its target, reported {written}")
    expect(fallback is not None, "the target itself must carry the failure")
    expect("harness died" in (fallback.text or ""), "the fallback must carry the log's tail")
    expect(not findings, f"the fallback is the handling, not a second finding: {codes(findings)}")
    print(f"JUNIT RED   : {label} unreadable — reported as {fallback.get('message')[:90]}")
    checks += 1

    # The other way a red target goes quiet: every case passes and the binary still
    # failed. A report built from the log alone would say 3 passed and nothing else.
    label = "//crates/x:crash_test"
    crashed = _log(work / "crashed" / "test.log", passed=3)
    bep = _bep(work / "junit-crash.json", {label: crashed}, status="FAILED")
    observed, _ = read_results(bep)
    written, findings = write_junit(reports / "crash.xml", read_runs(bep), observed)
    document = ET.parse(reports / "crash.xml")
    crash = document.find(f".//testcase[@name='{label}']/failure")
    expect(not findings, f"the status fallback must not red the count floor: {codes(findings)}")
    expect(written == 4, f"3 passing cases plus the target's own failure, got {written}")
    expect(crash is not None, "a FAILED target whose cases all pass must still be a failure")
    print(f"JUNIT RED   : {label} passed every case and Bazel reported FAILED — named anyway")
    checks += 1

    return checks


def prove_synthesised_xml_is_not_the_count() -> int:
    """The ADR's falsifier, kept as a named control: Bazel's synthesised `test.xml`
    carries one `testcase` per **target**, so a reader summing those elements reports the
    target count and calls it a case count. Measured on this tree at
    `//crates/ocx_exit:ocx_exit_test` — 21 cases in the log, `tests="1"` in the XML."""
    synthesised = (
        '<?xml version="1.0" encoding="UTF-8"?>\n<testsuites>\n'
        '  <testsuite name="crates/demo_crate/demo_crate_test" tests="1" failures="0" errors="0">\n'
        '    <testcase name="crates/demo_crate/demo_crate_test" status="run" duration="0" time="0">'
        "</testcase>\n  </testsuite>\n</testsuites>\n"
    )
    by_xml = synthesised.count("<testcase ")
    by_log = int(
        RESULT_LINE.search(
            "test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )["passed"]
    )
    expect(by_xml == 1, f"the synthesised form must carry one testcase per target, got {by_xml}")
    expect(by_log == 21, f"the log must carry the real count, got {by_log}")
    print(
        f"XML CONTROL : the synthesised test.xml reports {by_xml} case for a target the log "
        f"reports {by_log} for — which is why this reader takes the log"
    )
    return 1


def prove_counts(rows: list[Row]) -> int:
    """The two floors that judge the same universe must agree, or one of them is stale."""
    expect(
        len(rows) == CRATES_TEST_TARGETS + CRATES_TWIN_TEST_TARGETS,
        f"crates/TEST_TARGET_MAP.toml has {len(rows)} rows, "
        f"bazel_gate_proofs.CRATES_TEST_TARGETS + CRATES_TWIN_TEST_TARGETS is "
        f"{CRATES_TEST_TARGETS + CRATES_TWIN_TEST_TARGETS} — the two floors "
        f"read the same universe and disagree",
    )
    expect(
        sum(row.ignored for row in rows) <= read_ceiling(CEILING_FILE),
        "the recorded ignored counts already exceed crates/NEXTEST_SKIP_CEILING",
    )
    print(
        f"COUNTS      : {len(rows)} rows = CRATES_TEST_TARGETS, "
        f"{sum(r.cases for r in rows)} recorded cases, {sum(r.ignored for r in rows)} ignored"
    )
    return 1


def self_test() -> int:
    rows = read_rows(TEST_TARGET_MAP)
    expect(rows, f"{TEST_TARGET_MAP} has no `[[target]]` rows — the fixture would be empty")
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    checks = 0
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        checks += prove_counts(rows)
        checks += prove_synthesised_xml_is_not_the_count()
        checks += prove_floor(work, rows)
        checks += prove_ceiling(work, rows)
        checks += prove_junit(work, rows)
    print(
        f"bazel test floor self-test: {checks} checks passed — the floor shown red on a "
        f"deleted target, on three tests deleted inside a surviving target, on a stopped "
        f"reader, on a mute log and on an empty stream, and green on the live universe; "
        f"--junit shown red on a mangled per-case parser (one target and all of them), on "
        f"an unreadable log and on a target Bazel failed while its cases passed, and green "
        f"at exactly the recorded case count with a failure named and its panic attached"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="show every finding red and green")
    mode.add_argument("--bep", type=Path, help="the --build_event_json_file to judge")
    parser.add_argument(
        "--junit",
        type=Path,
        help="also write a per-case JUnit XML report here (needs --bep); parent dirs created",
    )
    args = parser.parse_args()
    if args.self_test:
        if args.junit is not None:
            parser.error("--junit reports on a run; there is no run under --self-test")
        return self_test()
    return run_check(args.bep, args.junit)


if __name__ == "__main__":
    raise SystemExit(main())
