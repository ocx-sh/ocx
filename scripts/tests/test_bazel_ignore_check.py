"""`bazel_ignore_check.py`: C-006's declared red state, both reader floors, and
the path-boundary control, shown red and green on the live `buildfiles(//...)`
answer (see the module docstring for what each proof covers)."""

from __future__ import annotations

import bazel_ignore_check as guard
import pytest
from bazel_gate_proofs import codes

# One worker owns the live capture: a second `bazel query` on the same server
# only queues behind the first.
pytestmark = pytest.mark.xdist_group("bazel_ignore_check")


@pytest.fixture(scope="module")
def entries() -> list[str]:
    return guard.read_entries(guard.BAZELIGNORE.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def live(bazel: str) -> list[str]:
    """The live `buildfiles(//...)` answer, queried once for every proof below."""
    stdout, rc, stderr = guard.run_query(bazel)
    assert rc == 0, (
        "`bazel query 'buildfiles(//...)'` exited "
        f"{rc} — this gate's subject is the live graph. stderr: {stderr}"
    )
    paths = guard.workspace_paths(stdout)
    assert len(paths) >= guard.BUILDFILES_FLOOR
    return paths


def test_entries_above_floor(entries: list[str]) -> None:
    assert len(entries) >= guard.IGNORE_ENTRY_FLOOR


def test_entries_are_relative_directories(entries: list[str]) -> None:
    assert all(not entry.startswith("/") and "*" not in entry for entry in entries)


def test_live_graph_is_clean(live: list[str], entries: list[str]) -> None:
    assert guard.ignore_findings(paths=live, entries=entries) == []


def test_red_package_under_ignored_directory(live: list[str], entries: list[str]) -> None:
    planted = f"{entries[0]}/probe/BUILD.bazel"
    assert planted not in live
    findings = guard.ignore_findings(paths=[*live, planted], entries=entries)
    assert codes(findings) == ["ignore-violated"]
    assert planted in findings[0].message


def test_red_ignored_directory_itself(live: list[str], entries: list[str]) -> None:
    findings = guard.ignore_findings(paths=[*live, f"{entries[0]}/BUILD.bazel"], entries=entries)
    assert codes(findings) == ["ignore-violated"]


def test_control_sibling_prefix_is_silent(live: list[str], entries: list[str]) -> None:
    """A prefix that is not a path boundary must NOT fire."""
    sibling = f"{entries[0]}s/probe/BUILD.bazel"
    assert guard.ignore_findings(paths=[*live, sibling], entries=entries) == []


def test_red_reader_floor_on_empty_paths(entries: list[str]) -> None:
    """A zero-match query exits 0, so the count is what must gate."""
    findings = guard.ignore_findings(paths=[], entries=entries)
    assert codes(findings) == ["ignore-reader-floor"]


def test_red_entries_floor_on_empty_list(live: list[str]) -> None:
    """An emptied ignore list — the sweep has nothing to sweep for."""
    findings = guard.ignore_findings(paths=live, entries=[])
    assert codes(findings) == ["ignore-entries-floor"]


def test_red_manual_ocx_home_entry(live: list[str], entries: list[str]) -> None:
    """The entry `bazel build --nobuild //...` does not catch (non-fatal ERROR)."""
    manual = "test/manual/.ocx-home"
    assert manual in entries
    findings = guard.ignore_findings(paths=[*live, f"{manual}/BUILD.bazel"], entries=entries)
    assert codes(findings) == ["ignore-violated"]


def test_red_dropping_manual_entry_reds_on_entry_floor(live: list[str], entries: list[str]) -> None:
    """Dropping the entry must not escape the gate the other way: the entry
    floor is what catches a deletion."""
    manual = "test/manual/.ocx-home"
    thinned = [entry for entry in entries if entry != manual]
    findings = guard.ignore_findings(paths=[*live, f"{manual}/BUILD.bazel"], entries=thinned)
    assert codes(findings) == ["ignore-entries-floor"]
