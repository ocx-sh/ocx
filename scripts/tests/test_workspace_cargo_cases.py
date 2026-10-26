"""`workspace_cargo_cases.py`: the verdict red and green, and the case list held to the BUILD file's."""

from __future__ import annotations

import re
from pathlib import Path

import workspace_cargo_cases as lane
from test_bundle import NEEDS_CARGO

ROOT = Path(__file__).resolve().parents[2]


def test_green_when_every_named_case_passed() -> None:
    out = "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 44 filtered out; finished in 1.01s\n"
    assert lane.verdict(0, out, 5) is None


def test_red_when_a_name_matches_nothing() -> None:
    out = "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 45 filtered out; finished in 1.01s\n"
    assert "4 testcase(s) passed for 5 named" in (lane.verdict(0, out, 5) or "")


def test_red_when_nothing_ran() -> None:
    assert "0 testcase(s) passed for 5 named" in (lane.verdict(0, "", 5) or "")


def test_red_on_a_failing_binary() -> None:
    out = "test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 44 filtered out; finished in 1.0s\n"
    assert "exited 101" in (lane.verdict(101, out, 5) or "")


def test_the_cargo_cases_are_the_ones_bazel_leaves_out_of_the_isolated_target() -> None:
    """`NEEDS_CARGO` (Python) and `_NEEDS_CARGO` (Starlark) are one set, and each is a `--skip`ped case."""
    build = (ROOT / "crates/ocx_test_support/BUILD.bazel").read_text(encoding="utf-8")
    block = re.search(r"^_NEEDS_CARGO = \[\n(?P<names>(?:    \"[^\"]+\",\n)+)\]", build, re.MULTILINE)
    assert block is not None, "`_NEEDS_CARGO` not found in crates/ocx_test_support/BUILD.bazel"
    declared = set(re.findall(r'"([^"]+)"', block["names"]))
    assert declared == NEEDS_CARGO
    skipped = re.search(r"^_SKIPPED = \[\n(?P<names>(?:    \"[^\"]+\",\n)+)\]", build, re.MULTILINE)
    assert skipped is not None
    assert declared <= set(re.findall(r'"([^"]+)"', skipped["names"]))
