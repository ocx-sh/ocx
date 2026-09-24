"""Every gate script has its tests here, and none keeps a homemade harness.

This replaces `scripts:self-test:complete`, which checked a hand-kept list in
`taskfiles/scripts.taskfile.yml`. The subject is now the directory itself, and
the reader is floored so a glob that matched nothing cannot read as complete.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent

# Scripts that are not gates, or whose proof lives elsewhere, each with its reason.
EXEMPT = {
    "_git.py": "a shared helper imported by gate scripts, proven through them",
    "sbom-to-markdown.py": "a release-notes formatter, not a gate",
}
FLOOR = 20


def gate_scripts() -> list[Path]:
    return sorted(p for p in SCRIPTS.glob("*.py") if p.name not in EXEMPT)


def test_the_reader_sees_the_scripts() -> None:
    found = gate_scripts()
    assert len(found) >= FLOOR, f"read {len(found)} gate scripts, expected >= {FLOOR}: the glob stopped matching"


@pytest.mark.parametrize("script", gate_scripts(), ids=lambda p: p.name)
def test_every_gate_script_has_tests(script: Path) -> None:
    tests = SCRIPTS / "tests" / f"test_{script.stem}.py"
    assert tests.is_file(), f"{script.name} has no {tests.relative_to(ROOT)} — a gate nobody tests is a check nobody runs"


@pytest.mark.parametrize("script", gate_scripts(), ids=lambda p: p.name)
def test_no_homemade_self_test_harness(script: Path) -> None:
    text = script.read_text(encoding="utf-8")
    assert '"--self-test"' not in text, f"{script.name} still parses `--self-test`; its proofs belong in scripts/tests/"


def test_the_exemptions_exist() -> None:
    missing = sorted(name for name in EXEMPT if not (SCRIPTS / name).exists())
    assert not missing, f"exempt script(s) no longer exist: {missing} — drop them from EXEMPT"


def test_the_action_selftests_run_here() -> None:
    """The one shell self-test outside scripts/: `.github/actions/*/selftest.sh`."""
    found = sorted(ROOT.glob(".github/actions/*/selftest.sh"))
    assert found, "no .github/actions/*/selftest.sh found — the glob stopped matching"
    for selftest in found:
        run = subprocess.run(
            ["bash", str(selftest)], cwd=ROOT, capture_output=True, text=True, encoding="utf-8", check=False
        )
        assert run.returncode == 0, f"{selftest.relative_to(ROOT)} failed:\n{run.stdout}\n{run.stderr}"
