"""`run_each_case.sh` and `isolated.bzl`: the runner red on every way a case can go missing, the crate list whole."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "crates/ocx_test_support/run_each_case.sh"
BZL = ROOT / "crates/ocx_test_support/isolated.bzl"

# A stand-in libtest binary: `<case>` prefixed `ok_` passes, `none_` matches nothing, `bad_` fails.
FAKE = """#!/bin/sh
case $2 in
    ok_*) printf 'running 1 test\\ntest %s ... ok\\n\\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\\n' "$2" ;;
    none_*) printf 'running 0 tests\\n\\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out\\n' ;;
    *) printf 'test %s ... FAILED\\n\\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 9 filtered out\\n' "$2"; exit 101 ;;
esac
"""


def _run(tmp_path: Path, *cases: str) -> subprocess.CompletedProcess[str]:
    binary = tmp_path / "fake_test"
    binary.write_text(FAKE, encoding="utf-8")
    binary.chmod(0o755)
    return subprocess.run(
        ["sh", str(RUNNER), str(binary), *cases],
        env={"TEST_TMPDIR": str(tmp_path), "PATH": "/usr/bin:/bin"},
        capture_output=True, text=True, check=False,
    )  # fmt: skip


def test_green_passes_every_case_through_in_order(tmp_path: Path) -> None:
    ran = _run(tmp_path, "ok_a", "ok_b")
    assert ran.returncode == 0, ran.stderr
    assert ran.stdout.index("test ok_a ... ok") < ran.stdout.index("test ok_b ... ok")
    assert ran.stdout.count("test result: ok. 1 passed;") == 2


def test_red_on_a_case_that_matches_no_test(tmp_path: Path) -> None:
    ran = _run(tmp_path, "ok_a", "none_b")
    assert ran.returncode == 1
    assert "none_b did not run exactly one passing test" in ran.stderr
    assert "ok_a did not" not in ran.stderr


def test_red_on_a_failing_case(tmp_path: Path) -> None:
    ran = _run(tmp_path, "bad_a", "ok_b")
    assert ran.returncode == 1
    assert "bad_a did not run exactly one passing test" in ran.stderr


def test_red_without_a_case(tmp_path: Path) -> None:
    assert _run(tmp_path).returncode == 2


def test_the_crate_list_is_every_crate_with_a_tree() -> None:
    listed = set(re.findall(r'^    "(ocx_[a-z_]+)",$', BZL.read_text(encoding="utf-8"), re.MULTILINE))
    builds = {p.parent.name for p in (ROOT / "crates").glob("*/BUILD.bazel")}
    assert len(builds) >= 24, "the crates glob stopped matching"
    assert listed == builds
    bare = sorted(
        name for name in builds if "crate_tree()" not in (ROOT / "crates" / name / "BUILD.bazel").read_text(encoding="utf-8")
    )
    assert bare == [], f"no `crate_tree()` in {bare}: its sources are invisible to the isolated cases"


@pytest.mark.parametrize("build", sorted((ROOT / "crates").glob("*/BUILD.bazel")), ids=lambda p: p.parent.name)
def test_no_skip_is_spelled_apart_from_its_isolated_target(build: Path) -> None:
    """A `--skip` literal would leave its case running nowhere on Linux; `_SKIPPED` feeds both."""
    text = "\n".join(line for line in build.read_text(encoding="utf-8").splitlines() if not line.lstrip().startswith("#"))
    assert '"--skip=' not in text.replace('"--skip=" + case', ""), f"{build}: a literal --skip"
    if "_SKIPPED" in text:
        assert re.search(r"isolated_cases\(\n(?:.*\n)*?    cases = ", text), f"{build}: _SKIPPED without isolated_cases"
