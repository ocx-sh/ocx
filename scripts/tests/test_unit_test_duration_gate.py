"""`unit_test_duration_gate.py`'s proofs, as pytest.

The gate is a parser over a log, and every way it can fail is a way it can also
go quietly vacuous. Each state below holds one of those ways; every assertion
reads the gate's own wording, never merely the exit status. The fixture log
carries exactly crates/NEXTEST_FLOOR result lines, the over-budget one at
counter 1 where nextest's right-aligned padding is — `\\(\\d+/\\d+\\)` once
matched none of the padded counters while reporting a clean parse, gluing
`(   1/8215)` onto the id. Every case pins `--budget-seconds 1.0` and its `CI`
mode, so none changes verdict with the script's default or the environment.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import unit_test_duration_gate as gate

FLOOR = int(gate.FLOOR.read_text(encoding="utf-8").strip())
SLOW_TEST = "ocx_fixture slow::tests::deliberately_over_budget"
# The duration and the id glued by two spaces is a shape only the gate's own
# failure listing produces — the fixture spells both inside `[   7.500s]`.
RED_BUDGET = f"7.500s {SLOW_TEST}"
GREEN_SLOWEST = f"slowest {SLOW_TEST} at 7.500s"
GREEN_MATCHED = "1/1 allowlist entries matched, covering 1 test(s)"
HARD_MODE = "hard mode (CI is set)"
ADVISORY_MODE = "advisory mode (CI is unset)"
ADVISORY_CLOSER = "1 finding(s) above the budget did NOT fail this run"


def fixture_log(spelling: str, lines: int = FLOOR) -> str:
    """A nextest run log in one of the three spellings a real one comes in:
    plain (`tee` with no TTY), real escape codes sitting inside the id (`tee`
    with one), and `gh run view --log`'s literal `^[` plus its line prefix."""
    esc = {"plain": "", "esc": "\x1b", "caret": "^["}[spelling]
    pre = "Smoke (Linux)\tTest\t2026-09-20T12:19:02.2393027Z " if spelling == "caret" else ""

    def c(code: str) -> str:
        return f"{esc}[{code}m" if esc else ""

    def row(i: int, secs: str, module: str, name: str) -> str:
        return (
            f"{pre}{c('32;1')}        PASS{c('0')} [{secs}] ({i:>4}/{FLOOR}) "
            f"{c('35;1')}ocx_fixture{c('0')} {c('36')}{module}{c('0')}{c('36')}::{c('0')}{c('34;1')}{name}{c('0')}"
        )

    rows = [row(1, "   7.500s", "slow::tests", "deliberately_over_budget")]
    rows += [row(i, "   0.003s", "fixture::tests", f"case_{i}") for i in range(2, FLOOR + 1)]
    return "\n".join(rows[:lines]) + "\n"


@pytest.fixture
def judge(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]):
    """Run the gate over a fixture log and allowlist; returns (status, stdout, stderr)."""

    def run(allow: str | None, *, spelling: str = "plain", lines: int = FLOOR, ci: bool = True, budget: float = 1.0):
        if ci:
            monkeypatch.setenv("CI", "1")
        else:
            monkeypatch.delenv("CI", raising=False)
        log = tmp_path / f"{spelling}-{lines}.log"
        log.write_text(fixture_log(spelling, lines), encoding="utf-8")
        allowlist = gate.ALLOWLIST
        if allow is not None:
            allowlist = tmp_path / "allowlist"
            allowlist.write_text(allow, encoding="utf-8")
        status = gate.run(log, budget, allowlist)
        out, err = capsys.readouterr()
        return status, out, err

    return run


def test_the_fixture_holds_the_floor() -> None:
    assert fixture_log("plain").count("\n") == FLOOR, "the cases below would judge the wrong thing"


def test_1_over_budget_unlisted_is_red(judge) -> None:
    status, _, err = judge("")
    assert status == 1, "the gate accepted a 7.500s test with an empty allowlist — it is not gating"
    assert RED_BUDGET in err, f"red without naming the slow test — it failed for another reason: {err}"


def test_2_short_log_is_red_on_the_reader_floor(judge) -> None:
    status, _, err = judge("", lines=3)
    assert status == 1, "the gate reported on a 3-line log — every green it prints could be a subset run"
    assert "3 timed result lines read, floor is" in err, err


@pytest.mark.parametrize("spelling", ["plain", "esc", "caret"])
def test_3_4_allowlisted_is_green_in_every_spelling(judge, spelling: str) -> None:
    status, out, _ = judge(f"{SLOW_TEST}\n", spelling=spelling)
    green = out.splitlines()[-1]
    assert status == 0, out
    assert f"{FLOOR} tests read (floor {FLOOR})" in green, green
    assert GREEN_SLOWEST in green, f"a counter or colour code was left on the test id: {green}"
    assert GREEN_MATCHED in green, green


def test_4b_hard_mode_fails_and_names_itself(judge) -> None:
    status, out, err = judge("", ci=True)
    assert status == 1, "hard mode did not fail an over-budget unlisted test — CI=1 enforces nothing"
    assert HARD_MODE in out, out
    assert RED_BUDGET in err, err


def test_4b_advisory_mode_reports_and_passes(judge) -> None:
    status, out, err = judge("", ci=False)
    assert status == 0, f"advisory mode failed the run — off CI it must report and pass: {err}"
    assert ADVISORY_MODE in out, out
    assert RED_BUDGET in err, "the advisory run dropped its findings — that is the off-switch it must not become"
    assert ADVISORY_CLOSER in err, err


@pytest.mark.parametrize(("ci", "mode"), [(True, HARD_MODE), (False, ADVISORY_MODE)], ids=["hard", "advisory"])
def test_4b_green_runs_name_their_mode(judge, ci: bool, mode: str) -> None:
    status, out, _ = judge(f"{SLOW_TEST}\n", ci=ci)
    assert status == 0, out
    assert mode in out, out


def test_5_pattern_entry_is_green(judge) -> None:
    status, out, _ = judge("ocx_fixture slow::*\n")
    green = out.splitlines()[-1]
    assert status == 0, out
    assert GREEN_SLOWEST in green and GREEN_MATCHED in green, green


def test_6_pattern_matching_nothing_stays_red(judge) -> None:
    status, _, err = judge("ocx_other other::*\n")
    assert status == 1, "a pattern matching nothing allowed the 7.500s test — patterns allow whatever they are handed"
    assert RED_BUDGET in err, err


def test_7_unbounded_pattern_is_refused(judge) -> None:
    with pytest.raises(SystemExit, match=r"1 unbounded pattern\(s\)"):
        judge("*\n")


def test_7_shipped_allowlist_survives_its_own_reader(judge) -> None:
    """At an unreachable budget only the reader can speak."""
    status, out, _ = judge(None, budget=1e9)
    assert status == 0, out
