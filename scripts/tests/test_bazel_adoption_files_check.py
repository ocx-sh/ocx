"""`bazel_adoption_files_check.py`: C-001 and C-002, each shown red and green
on copies of the shipped artifacts, with every mutation proven to have landed
before its result is trusted (see the module docstring for what each covers)."""

from __future__ import annotations

from pathlib import Path

import bazel_adoption_files_check as gate
import pytest


@pytest.fixture(scope="module")
def original_decision() -> str:
    return gate.DECISION.read_text(encoding="utf-8")


@pytest.fixture(scope="module")
def original_migration() -> str:
    return gate.MIGRATION.read_text(encoding="utf-8")


def test_exit_check_verbatim_from_go_no_go() -> None:
    if not gate.GO_NO_GO.is_file():
        pytest.skip(f"{gate.GO_NO_GO} absent, verbatim-equality not asserted this run")
    shipped = gate.GO_NO_GO.read_text(encoding="utf-8").splitlines()
    assert gate.EXIT_CHECK in shipped, (
        f"EXIT_CHECK is no longer a verbatim line of {gate.GO_NO_GO} — re-copy it, never re-spell it"
    )


def test_decision_green(original_decision: str, tmp_path: Path) -> None:
    decision = tmp_path / "decision_bazel_adoption.md"
    decision.write_text(original_decision, encoding="utf-8")
    assert gate.check_decision(decision) == 0, "the shipped decision file must pass C-001"
    assert gate.empty_cell_rows(decision) == [], "the shipped decision file must print no rows"


@pytest.mark.parametrize("signal", gate.SIGNALS)
def test_decision_red_per_signal(signal: str, original_decision: str, tmp_path: Path) -> None:
    decision = tmp_path / "decision_bazel_adoption.md"
    mutated = gate.blank_measured_value(original_decision, signal)
    decision.write_text(mutated, encoding="utf-8")
    landed = decision.read_text(encoding="utf-8")
    assert f"| {signal} |  |" in landed, f"mutation for {signal!r} did not land on disk"
    assert landed != original_decision, f"mutation for {signal!r} was a no-op"

    rows = gate.empty_cell_rows(decision)
    assert len(rows) == 1, f"blanking {signal!r} printed {len(rows)} row(s), expected 1"
    assert signal in rows[0], f"the printed row does not name {signal!r}: {rows[0]}"
    assert gate.check_decision(decision) == 1, f"blanking {signal!r} must exit 1"

    # Restore, from our own saved bytes, and prove the restore landed.
    decision.write_text(original_decision, encoding="utf-8")
    assert decision.read_text(encoding="utf-8") == original_decision, f"restore after {signal!r} did not land"
    assert gate.check_decision(decision) == 0, f"restoring after {signal!r} must green again"


def test_decision_reader_floor_on_table_less_file(original_decision: str, tmp_path: Path) -> None:
    decision = tmp_path / "decision_bazel_adoption.md"
    decision.write_text("# Bazel adoption decision\n", encoding="utf-8")
    assert gate.empty_cell_rows(decision) == [], "the shipped grep is green on a table-less file"
    assert gate.check_decision(decision) == 1, "the reader floor must red on a table-less file"


def test_migration_green(original_migration: str, tmp_path: Path) -> None:
    migration = tmp_path / "migration_plan_bazel.md"
    migration.write_text(original_migration, encoding="utf-8")
    assert gate.check_migration(migration) == 0, "the shipped migration plan must pass C-002"


@pytest.mark.parametrize("heading", gate.SECTIONS)
def test_migration_red_per_section(heading: str, original_migration: str, tmp_path: Path) -> None:
    migration = tmp_path / "migration_plan_bazel.md"
    mutated = gate.drop_section(original_migration, heading)
    migration.write_text(mutated, encoding="utf-8")
    landed = migration.read_text(encoding="utf-8")
    assert heading not in landed.splitlines(), f"deletion of {heading!r} did not land on disk"
    assert len(landed) < len(original_migration), f"deleting {heading!r} was a no-op"
    assert gate.check_migration(migration) == 1, f"deleting {heading!r} must exit 1"

    migration.write_text(original_migration, encoding="utf-8")
    assert migration.read_text(encoding="utf-8") == original_migration, f"restore after {heading!r} did not land"
    assert gate.check_migration(migration) == 0, f"restoring after {heading!r} must green again"


def test_originals_untouched(original_decision: str, original_migration: str) -> None:
    """The fixtures above only ever touch `tmp_path` copies."""
    assert gate.DECISION.read_text(encoding="utf-8") == original_decision
    assert gate.MIGRATION.read_text(encoding="utf-8") == original_migration
