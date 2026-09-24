"""`suite_ceilings.py`'s proofs, as pytest.

The red cases carry a skipped count no ceiling will ever permit, so they cannot
go vacuous when test/SKIP_CEILING moves, and each asserts the comparison's own
wording rather than the exit status: a parser that never reaches the comparison
fails for some other reason, and that must not read as gating. The green case
carries `1 skipped, 1 xfailed`, never zeroes — an extraction that stopped
matching reports 0, so a zero-count fixture is green whether or not it works.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

import pytest
import suite_ceilings as ceilings

TEST_DIR = ceilings.REPO_ROOT / "test"
RED_SUMMARY = "1 passed, 999999 skipped in 1.00s"


def _run(tmp_path: Path, summary: str, skip: str = "SKIP_CEILING", xfail: str = "XFAIL_CEILING") -> int:
    log = tmp_path / "run.log"
    log.write_text(f"collected 3 items\n{summary}\n", encoding="utf-8")
    return ceilings.run(log, TEST_DIR / skip, TEST_DIR / xfail)


def test_red_on_999999_skipped(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, RED_SUMMARY) == 1
    assert "999999 skipped exceeds test/SKIP_CEILING" in capsys.readouterr().err


def test_green_reads_1_skipped_and_1_xfailed(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, "10 passed, 1 skipped, 1 xfailed in 1.00s") == 0
    out = capsys.readouterr().out
    assert "suite ceilings: 1 skipped (ceiling " in out
    assert ", 1 xfailed (ceiling " in out


def test_red_on_xfailed_over_ceiling(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, "1 passed, 999999 xfailed in 1.00s") == 1
    assert "999999 xfailed exceeds test/XFAIL_CEILING" in capsys.readouterr().err


def test_escape_codes_are_stripped(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    """FORCE_COLOR wraps the summary in escape codes; one between the count and
    its word would make a parser that did not strip them read the count as
    absent and pass."""
    assert _run(tmp_path, "\x1b[32m1 passed\x1b[0m, \x1b[33m999999\x1b[0m skipped\x1b[32m in 1.00s\x1b[0m") == 1
    err = capsys.readouterr().err
    assert "999999 skipped exceeds test/SKIP_CEILING" in err
    assert "\x1b" not in err, "the summary was quoted with its escape codes"


def test_the_last_summary_line_is_the_verdict(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, f"{RED_SUMMARY}\n10 passed, 1 skipped in 1.00s") == 0
    assert "suite ceilings: 1 skipped" in capsys.readouterr().out


def test_a_line_that_starts_with_its_count(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    """The `-q` shape when every test skipped: no non-digit before the count."""
    assert _run(tmp_path, "999999 skipped in 1.00s") == 1
    assert "999999 skipped exceeds test/SKIP_CEILING" in capsys.readouterr().err


def test_red_on_a_missing_summary_line(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, "tests/test_x.py ....") == 1
    assert "suite ceilings: no pytest summary line in" in capsys.readouterr().err


def test_red_on_a_missing_log(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert ceilings.run(tmp_path / "absent.log", TEST_DIR / "SKIP_CEILING", TEST_DIR / "XFAIL_CEILING") == 1
    assert "suite ceilings: no pytest summary line in" in capsys.readouterr().err


@pytest.mark.parametrize(
    ("summary", "expect_status", "expect"),
    [
        (RED_SUMMARY, 1, r"999999 skipped exceeds test/LINT_SKIP_CEILING"),
        ("10 passed in 1.00s", 0, r"suite ceilings: 0 skipped \(ceiling \d+\), 0 xfailed"),
    ],
    ids=["red", "green"],
)
def test_lint_ceiling_files_through_the_cli(tmp_path: Path, summary: str, expect_status: int, expect: str) -> None:
    """`test:lint:structure` passes its own ceiling pair; the default files never
    prove that the override reaches the comparison, so this goes through argv."""
    log = tmp_path / "lint.log"
    log.write_text(f"{summary}\n", encoding="utf-8")
    shown = subprocess.run(
        [
            sys.executable,
            str(ceilings.REPO_ROOT / "scripts" / "suite_ceilings.py"),
            "--log",
            str(log),
            "--skip-ceiling-file",
            "LINT_SKIP_CEILING",
            "--xfail-ceiling-file",
            "LINT_XFAIL_CEILING",
        ],
        cwd=TEST_DIR,
        capture_output=True,
        encoding="utf-8",
        check=False,
    )
    assert shown.returncode == expect_status, shown.stderr
    assert re.search(expect, shown.stdout + shown.stderr), shown.stdout + shown.stderr
