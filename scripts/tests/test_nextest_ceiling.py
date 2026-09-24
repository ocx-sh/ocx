"""`nextest_ceiling.py`'s proofs, as pytest.

Every fixture is a nextest 0.9.144 Summary line, so the cases exercise
`GRAMMAR`, not only the comparison. The red one carries a skipped count no
ceiling will ever permit, so it cannot go vacuous when
crates/NEXTEST_SKIP_CEILING moves; the green one carries `1 skipped`, not `0`,
and asserts the parser SAYS 1 — an extraction that stopped matching reports 0,
so a zero-skip fixture is green whether or not it works. Each red asserts the
gate's own wording, never merely the exit status.
"""

from __future__ import annotations

from pathlib import Path

import nextest_ceiling as ceiling
import pytest


def _run(tmp_path: Path, *lines: str) -> int:
    log = tmp_path / "run.log"
    log.write_text("".join(f"{line}\n" for line in lines), encoding="utf-8")
    return ceiling.run(log)


def test_red_on_999999_skipped(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, "    Summary [   1.234s] 100 tests run: 1 passed, 999999 skipped") == 1
    assert "999999 skipped exceeds crates/NEXTEST_SKIP_CEILING" in capsys.readouterr().err


def test_green_reads_1_skipped(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, "    Summary [   1.234s] 100 tests run: 99 passed, 1 skipped") == 0
    assert "nextest ceiling: 1 skipped (ceiling " in capsys.readouterr().out


def test_green_through_every_optional_token(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    summary = (
        "\x1b[32;1m     Summary\x1b[0m [  81.000s] 8215/8223 tests run: 8200 passed (3 slow, 1 flaky), "
        "2 failed (1 due to being leaky), 1 exec failed, 1 timed out, 1 skipped"
    )
    assert _run(tmp_path, summary) == 0
    assert "nextest ceiling: 1 skipped (ceiling " in capsys.readouterr().out


def test_red_on_a_reshaped_summary(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path, "    Summary [   1.234s] 100 tests run: 99 passed, 1 skipped, 1 mystery") == 1
    assert "Summary line is not nextest's grammar" in capsys.readouterr().err


def test_the_last_summary_line_is_the_verdict(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert (
        _run(
            tmp_path,
            "    Summary [   1.234s] 100 tests run: 1 passed, 999999 skipped",
            "    Summary [   1.234s] 100 tests run: 99 passed, 1 skipped",
        )
        == 0
    )
    assert "nextest ceiling: 1 skipped" in capsys.readouterr().out


@pytest.mark.parametrize("present", [True, False], ids=["no-summary", "missing-log"])
def test_red_when_no_run_happened(tmp_path: Path, capsys: pytest.CaptureFixture[str], present: bool) -> None:
    log = tmp_path / "run.log"
    if present:
        log.write_text("    PASS [   0.003s] ocx_fixture a::b\n", encoding="utf-8")
    assert ceiling.run(log) == 1
    assert "nextest ceiling: no Summary line in" in capsys.readouterr().err
