"""`dead_path_sweep.py`: every shape the sweep decides, shown red and green on
a tree built under `tmp_path` (see the script's docstring for what each proof
covers)."""

from __future__ import annotations

import subprocess
from pathlib import Path

import dead_path_sweep as sweep_mod
import pytest


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A live/dead corpus covering every shape `sweep` judges."""
    root = tmp_path
    (root / "crates/ocx_util/src").mkdir(parents=True)
    (root / "crates/ocx_util/src/live.rs").write_text("pub fn x() {}\n", encoding="utf-8")
    (root / "crates/ocx_util/src/subdir").mkdir()
    (root / "crates/ocx_util/src/subdir/k.rs").write_text("\n", encoding="utf-8")
    (root / "crates/ocx_util/Cargo.toml").write_text("[package]\n", encoding="utf-8")
    (root / ".claude/rules").mkdir(parents=True)
    (root / "scripts").mkdir()
    (root / ".gitignore").write_text("*.cdx.json\n", encoding="utf-8")

    def write(rel: str, text: str) -> None:
        (root / rel).write_text(text, encoding="utf-8")

    # A live file literal, a live DIRECTORY literal (the DEC-52 blind spot), a
    # dead file literal, a dead directory literal, and the crate root, which is
    # judged like anything else and happens to be live.
    write(
        ".claude/rules/subsystem-x.md",
        "see `crates/ocx_util/src/live.rs` and `crates/ocx_util/src/subdir/**`\n"
        "and `crates/ocx_util/src/gone.rs` and `crates/ocx_util/src/nodir/**`\n"
        "and `crates/ocx_util/src` which is the crate root\n",
    )
    # An INTERIOR glob, live and dead. `exists()` answers False for both, so
    # only matching the pattern tells them apart.
    write(
        ".claude/rules/subsystem-glob.md",
        "paths: `crates/ocx_util/src/**/*.rs` and `crates/ocx_util/src/**/*.toml`\n",
    )
    # OUTSIDE `src/`, live and dead. A manifest and a `tests/` tree move in an
    # extraction exactly as often as a source file does.
    write(
        ".claude/rules/subsystem-manifest.md",
        "see `crates/ocx_util/Cargo.toml` and `crates/ocx_gone/Cargo.toml`\n",
    )
    # A path `.gitignore` declares a build output: absent by design, so asking
    # whether it exists is a category error.
    write("scripts/emit.sh", "# writes crates/ocx_util/ocx.cdx.json\n")
    # SEGMENTED: the same path as adjacent literals joined by `/`, live and
    # dead — no token here resembles a repo path. The third line derives the
    # crate from a variable and must stay invisible.
    write(
        "scripts/bench.py",
        'SRC = base / "crates" / "ocx_util" / "src" / "live.rs"\n'
        'GONE = base / "crates" / "ocx_util" / "src" / "vanished.rs"\n'
        'ANY = base / "crates" / name / "src"\n',
    )
    # Off a live prefix entirely: an artifact records what was true when
    # written, so its dead literal must NOT be reported.
    (root / ".claude/artifacts").mkdir()
    write(".claude/artifacts/old_plan.md", "`crates/ocx_util/src/ancient.rs`\n")
    # Exempt by what the file IS, not by naming a path.
    write("scripts/lint_ratchet.py", "# crates/ocx_util/src/fixture_only.rs\n")

    subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
    return root


def test_reader_saw_the_live_corpus(repo: Path) -> None:
    _, read = sweep_mod.sweep(repo)
    assert read >= 3, "the reader saw too few live file(s), so nothing else means anything"


def test_dead_shapes_are_reported(repo: Path) -> None:
    dead, _ = sweep_mod.sweep(repo)
    assert sorted(dead) == [
        "crates/ocx_gone/Cargo.toml",
        "crates/ocx_util/src/**/*.toml",
        "crates/ocx_util/src/gone.rs",
        "crates/ocx_util/src/nodir",
        "crates/ocx_util/src/vanished.rs",
    ]


@pytest.mark.parametrize(
    "green",
    [
        "crates/ocx_util/src/live.rs",  # live file
        "crates/ocx_util/src/subdir",  # live directory
        "crates/ocx_util/src",  # crate root
        "crates/ocx_util/src/**/*.rs",  # live interior glob
        "crates/ocx_util/Cargo.toml",  # live, outside src/
        "crates/ocx_util/ocx.cdx.json",  # declared build output
        "crates/ocx_util/src/ancient.rs",  # off a live prefix
        "crates/ocx_util/src/fixture_only.rs",  # exempt by what the file is
    ],
)
def test_green_shapes_are_not_reported(repo: Path, green: str) -> None:
    dead, _ = sweep_mod.sweep(repo)
    assert green not in dead


def test_segmented_reader_rebuilds_a_literal_crate_segment() -> None:
    # A derived path cannot go stale, and claiming it would report every
    # glob-driven walk in the tree as a path.
    assert sweep_mod._segmented_candidates(
        'base / "crates" / "ocx_util" / "src" / "x.rs"'
    ) == {"crates/ocx_util/src/x.rs"}


def test_segmented_reader_ignores_a_variable_crate_segment() -> None:
    assert sweep_mod._segmented_candidates('base / "crates" / name / "src"') == set()


def test_reader_floor_refuses_an_empty_tree(tmp_path: Path) -> None:
    subprocess.run(["git", "-C", str(tmp_path), "init", "-q"], check=True)
    _, read = sweep_mod.sweep(tmp_path)
    assert read == 0
    assert read < sweep_mod.MIN_FILES_READ


def test_verdict_reds_both_directions_on_an_equal_cardinality_swap() -> None:
    # One tolerated survivor fixed, one new literal arrived: the total is
    # unchanged, which was the whole defect this shows red.
    one_out = next(iter(sorted(sweep_mod.BASELINE_DEAD)))
    swapped = (set(sweep_mod.BASELINE_DEAD) - {one_out}) | {"crates/ocx_util/src/arrived.rs"}
    assert len(swapped) == len(sweep_mod.BASELINE_DEAD)
    lines = sweep_mod.verdict(swapped)
    assert len(lines) == 2
    assert any("crates/ocx_util/src/arrived.rs" in line for line in lines)
    assert any(one_out in line for line in lines)


def test_verdict_is_green_on_the_baseline_itself() -> None:
    assert sweep_mod.verdict(set(sweep_mod.BASELINE_DEAD)) == []


def test_verdict_reds_once_on_an_arrival_alone() -> None:
    assert len(sweep_mod.verdict(set(sweep_mod.BASELINE_DEAD) | {"crates/ocx_x/src/new.rs"})) == 1


def test_verdict_reds_once_on_a_departure_alone() -> None:
    one_out = next(iter(sorted(sweep_mod.BASELINE_DEAD)))
    assert len(sweep_mod.verdict(set(sweep_mod.BASELINE_DEAD) - {one_out})) == 1
