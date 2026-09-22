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

Stdlib only (plan DEC-8).
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
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from bazel_gate_proofs import (
    CRATES_TEST_TARGETS,
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


@dataclasses.dataclass(frozen=True)
class Counts:
    """One target's libtest summary, summed over its attempts' log."""

    executed: int
    ignored: int
    filtered: int


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


def read_results(bep: Path) -> tuple[dict[str, Counts], list[Finding]]:
    """`{label: Counts}` over every `TestResult` in `bep`, plus per-target read failures.

    The highest `attempt` wins: a flaky target under `--flaky_test_attempts`
    emits one event per attempt, and summing them would inflate the very number
    this gate floors on.
    """
    best: dict[str, tuple[int, str]] = {}
    try:
        text = bep.read_text(encoding="utf-8")
    except OSError:
        return {}, []
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
        logs = [
            output
            for output in event.get("testResult", {}).get("testActionOutput", [])
            if output.get("name") == "test.log" and output.get("uri", "").startswith("file:")
        ]
        if not label or not logs:
            continue
        if attempt >= best.get(label, (0, ""))[0]:
            best[label] = (attempt, logs[-1]["uri"])

    observed: dict[str, Counts] = {}
    findings: list[Finding] = []
    for label, (_, uri) in sorted(best.items()):
        path = _log_path(uri)
        try:
            log = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            log = ""
        matches = list(RESULT_LINE.finditer(log))
        if not matches:
            findings.append(
                Finding("floor-unreadable", UNREADABLE_MSG.format(label=label, path=path))
            )
            continue
        observed[label] = Counts(
            executed=sum(int(m["passed"]) + int(m["failed"]) for m in matches),
            ignored=sum(int(m["ignored"]) for m in matches),
            filtered=sum(int(m["filtered"]) for m in matches),
        )
    return observed, findings


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


def run_check(bep: Path) -> int:
    if not bep.is_file() or bep.stat().st_size == 0:
        print(EMPTY_MSG.format(path=bep), file=sys.stderr)
        return 1
    observed, findings = read_results(bep)
    if not observed and not findings:
        print(EMPTY_MSG.format(path=bep), file=sys.stderr)
        return 1
    rows = read_rows(TEST_TARGET_MAP)
    findings += floor_findings(observed, rows)
    findings += ceiling_findings(observed, read_ceiling(CEILING_FILE))
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


def _log(path: Path, *, passed: int, failed: int = 0, ignored: int = 0, filtered: int = 0) -> str:
    """A libtest log with the real preamble, written to `path`, returning its `file://` URI."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f"running {passed + ignored} tests\ntest some::case ... ok\n\n"
        f"test result: ok. {passed} passed; {failed} failed; {ignored} ignored; "
        f"0 measured; {filtered} filtered out; finished in 0.01s\n",
        encoding="utf-8",
    )
    return path.as_uri()


def _bep(path: Path, entries: dict[str, str]) -> Path:
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
                        "status": "PASSED",
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


def _fixture(work: Path, rows: list[Row], *, name: str = "bep.json") -> Path:
    """The live map, replayed as a stream that satisfies it exactly. Built from the real
    rows so the green leg is a statement about *this* tree rather than about an invented
    universe that can drift away from it."""
    entries = {
        row.label: _log(
            work / "logs" / row.label.replace("//", "").replace(":", "/") / "test.log",
            passed=row.cases,
            ignored=row.ignored,
            filtered=row.skipped,
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
        len(observed) == len(rows) == CRATES_TEST_TARGETS,
        f"the fixture must carry all {CRATES_TEST_TARGETS} targets, it carries {len(observed)}",
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
        len(rows) == CRATES_TEST_TARGETS,
        f"crates/TEST_TARGET_MAP.toml has {len(rows)} rows, "
        f"bazel_gate_proofs.CRATES_TEST_TARGETS is {CRATES_TEST_TARGETS} — the two floors "
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
    print(
        f"bazel test floor self-test: {checks} checks passed — the floor shown red on a "
        f"deleted target, on three tests deleted inside a surviving target, on a stopped "
        f"reader, on a mute log and on an empty stream, and green on the live universe"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="show every finding red and green")
    mode.add_argument("--bep", type=Path, help="the --build_event_json_file to judge")
    args = parser.parse_args()
    return self_test() if args.self_test else run_check(args.bep)


if __name__ == "__main__":
    raise SystemExit(main())
