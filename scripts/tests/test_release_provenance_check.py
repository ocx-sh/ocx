"""`release_provenance_check.py`: every marker red, the reader floor, the
report predicates and the `--exec` end-to-end fakes (see the script's
docstring for what each proof covers)."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest
import release_provenance_check as check_mod
from release_provenance_check import (
    _GOOD_REPORT,
    _REPORT_REDS,
    EXIT_CLEAN,
    EXIT_FINDING,
    EXIT_USAGE,
    MARKERS,
    _broken,
    evaluate_report,
    scan,
)


@pytest.mark.parametrize("marker", MARKERS, ids=lambda m: m.decode())
def test_scan_finds_each_marker_at_its_offset(tmp_path: Path, marker: bytes) -> None:
    fixture = tmp_path / f"red-{marker.decode().replace('/', '_')}"
    offset = 4096 + len(marker)
    fixture.write_bytes(b"\x7fELF" + b"\0" * (offset - 4) + marker + b"\0" * 64)
    findings = scan([fixture])
    assert len(findings) == 1
    assert marker.decode() in findings[0]
    assert f"offset {offset}" in findings[0]
    assert str(fixture) in findings[0]
    assert check_mod.main(["--scan", str(fixture)]) == EXIT_FINDING


def test_scan_reports_every_occurrence_not_only_the_first(tmp_path: Path) -> None:
    twice = tmp_path / "red-twice"
    twice.write_bytes(MARKERS[1] + b"\0" * 10 + MARKERS[1])
    assert len(scan([twice])) == 2


def test_scan_is_clean_on_near_misses(tmp_path: Path) -> None:
    # The all-zero SHA and near-misses of each marker are not markers.
    clean = tmp_path / "clean"
    clean.write_bytes(
        b"\x7fELF" + b"0" * 40 + b"\0" * 32 + b"placeholder-g0000000\0" + b"placeholder/\0" + b"ci.inval1d"
    )
    assert scan([clean]) == []
    assert check_mod.main(["--scan", str(clean)]) == EXIT_CLEAN


@pytest.fixture
def clean_file(tmp_path: Path) -> Path:
    clean = tmp_path / "clean"
    clean.write_bytes(
        b"\x7fELF" + b"0" * 40 + b"\0" * 32 + b"placeholder-g0000000\0" + b"placeholder/\0" + b"ci.inval1d"
    )
    return clean


def test_reader_floor_reds_under_min_files(clean_file: Path) -> None:
    assert check_mod.main(["--scan", str(clean_file), "--min-files", "2"]) == EXIT_FINDING


def test_reader_floor_does_not_count_one_file_named_twice(clean_file: Path) -> None:
    assert check_mod.main(["--scan", str(clean_file), str(clean_file), "--min-files", "2"]) == EXIT_FINDING


def test_reader_floor_passes_two_distinct_clean_files(tmp_path: Path, clean_file: Path) -> None:
    second = tmp_path / "clean-2"
    second.write_bytes(clean_file.read_bytes())
    assert check_mod.main(["--scan", str(clean_file), str(second), "--min-files", "2"]) == EXIT_CLEAN


def test_scan_over_a_missing_file_is_usage_error(tmp_path: Path) -> None:
    assert check_mod.main(["--scan", str(tmp_path / "absent")]) == EXIT_USAGE


def test_scan_over_a_directory_is_usage_error(tmp_path: Path) -> None:
    assert check_mod.main(["--scan", str(tmp_path)]) == EXIT_USAGE


def test_min_files_zero_is_usage_error(clean_file: Path) -> None:
    assert check_mod.main(["--scan", str(clean_file), "--min-files", "0"]) == EXIT_USAGE


def test_min_files_beside_exec_is_usage_error(clean_file: Path) -> None:
    assert check_mod.main(["--exec", str(clean_file), "--min-files", "2"]) == EXIT_USAGE


@pytest.mark.parametrize("name,report,needle", _REPORT_REDS, ids=[case[0] for case in _REPORT_REDS])
def test_evaluate_report_reds(name: str, report: object, needle: str) -> None:
    findings = evaluate_report(report)
    assert findings
    assert any(needle in finding for finding in findings)


def test_evaluate_report_is_clean_on_a_good_report() -> None:
    assert evaluate_report(_GOOD_REPORT) == []


def test_evaluate_report_is_clean_with_a_dev_channel() -> None:
    with_channel = _broken(("channel",), "dev")
    assert evaluate_report(with_channel) == []


def _write_fake_binary(path: Path, body: str) -> None:
    path.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
    path.chmod(0o755)


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
@pytest.mark.parametrize(
    "name,report,expected",
    [
        ("good", _GOOD_REPORT, EXIT_CLEAN),
        ("placeholder", _broken(("channel",), "test"), EXIT_FINDING),
    ],
)
def test_exec_end_to_end_over_a_fake_binary(
    tmp_path: Path, name: str, report: object, expected: int
) -> None:
    fake = tmp_path / f"fake-ocx-{name}"
    _write_fake_binary(
        fake,
        "import sys\n"
        "assert sys.argv[1:] == ['--format', 'json', 'version'], sys.argv\n"
        f"print({json.dumps(json.dumps(report))})\n",
    )
    assert check_mod.main(["--exec", str(fake)]) == expected


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_exec_reds_on_a_failing_binary(tmp_path: Path) -> None:
    # A release-shaped report on stdout, so only the exit status can red it.
    broken = tmp_path / "fake-ocx-fails"
    _write_fake_binary(
        broken, f"import sys\nprint({json.dumps(json.dumps(_GOOD_REPORT))})\nsys.exit(3)\n"
    )
    assert check_mod.main(["--exec", str(broken)]) == EXIT_FINDING


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_exec_reds_on_non_json_output(tmp_path: Path) -> None:
    garbage = tmp_path / "fake-ocx-garbage"
    _write_fake_binary(garbage, "print('not json')\n")
    assert check_mod.main(["--exec", str(garbage)]) == EXIT_FINDING
