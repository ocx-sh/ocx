"""`satellite_contract.py`'s proofs, as pytest.

The floor cases feed hand-built JUnit; the plugin cases run a real pytest
under xdist, as `satellite:contract` does, and floor the report it wrote.
Each red asserts the gate's own wording, never merely the exit status.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest
import satellite_contract as contract

SCRIPTS = Path(contract.__file__).resolve().parent


def _case(name: str, site: str | None, outcome: str = "") -> str:
    props = f'<properties><property name="{contract.MARKER}" value="{site}"/></properties>' if site else ""
    child = f'<{outcome} message="x"/>' if outcome else ""
    return f'<testcase classname="t" name="{name}">{props}{child}</testcase>'


def _floor(tmp_path: Path, *cases: str) -> int:
    report = tmp_path / "report.xml"
    report.write_text(f'<testsuites><testsuite name="pytest">{"".join(cases)}</testsuite></testsuites>')
    return contract.check(report)


def _six() -> list[str]:
    return [_case(f"p{n}", f"site-{n}") for n in range(6)]


def test_green_on_six_distinct_sites(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _floor(tmp_path, *_six()) == 0
    assert "6 distinct ocx_contract_site sites passed across 6 testcases" in capsys.readouterr().out


@pytest.mark.parametrize("outcome", ["failure", "error", "skipped"])
def test_red_on_any_case_not_passed(tmp_path: Path, capsys: pytest.CaptureFixture[str], outcome: str) -> None:
    assert _floor(tmp_path, *_six(), _case("extra", "site-6", outcome)) == 1
    assert "failed, errored or skipped: t::extra" in capsys.readouterr().err


def test_red_on_six_cases_under_one_site(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _floor(tmp_path, *[_case(f"p{n}", "site-0") for n in range(6)]) == 1
    assert "below the floor of 6 distinct" in capsys.readouterr().err


def test_a_case_without_a_site_counts_for_nothing(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _floor(tmp_path, *_six()[:5], _case("bare", None)) == 1
    assert "below the floor of 6 distinct" in capsys.readouterr().err


def test_red_on_an_empty_report(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _floor(tmp_path) == 1
    assert "0 distinct" in capsys.readouterr().out


def test_red_on_a_missing_report(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert contract.check(tmp_path / "absent.xml") == 1
    assert "no JUnit report at" in capsys.readouterr().err


def _plugin_run(tmp_path: Path, body: str) -> Path:
    (tmp_path / "test_sites.py").write_text(f"import pytest\n\n{body}")
    report = tmp_path / "junit.xml"
    env = {**os.environ, "PYTHONPATH": str(SCRIPTS)}
    subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-n", "2", "-p", "no:cacheprovider", "-p", "satellite_contract",
         "-m", contract.MARKER, f"--junitxml={report}", "--rootdir", str(tmp_path), "-c", os.devnull, str(tmp_path)],
        cwd=tmp_path, env=env, capture_output=True, text=True, check=False,
    )  # fmt: skip
    return report


def test_the_plugin_records_each_site_through_xdist(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    body = (
        '@pytest.mark.parametrize("n", [pytest.param(n, marks=pytest.mark.ocx_contract_site(f"site-{n}"))'
        " for n in range(6)])\ndef test_site(n):\n    pass\n\ndef test_unmarked():\n    pass\n"
    )
    assert contract.check(_plugin_run(tmp_path, body)) == 0
    assert "6 distinct ocx_contract_site sites passed across 6 testcases" in capsys.readouterr().out


def test_six_parametrized_cases_under_one_site_red(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    body = '@pytest.mark.ocx_contract_site("one")\n@pytest.mark.parametrize("n", range(6))\ndef test_site(n):\n    pass\n'
    assert contract.check(_plugin_run(tmp_path, body)) == 1
    assert "1 distinct ocx_contract_site sites passed across 6 testcases" in capsys.readouterr().out
