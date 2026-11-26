"""`shim_inputs.py`'s proofs, on a copy of the inputs it reads: each one changed reds, a release bump does not."""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "shim_inputs.py"
FILES = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".github/workflows/build-windows-shims.yml",
    "taskfiles/rust.taskfile.yml",
    "crates/ocx_shim/Cargo.toml",
    "crates/ocx_env_apply/Cargo.toml",
)


def run(root: Path, mode: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-I", str(SCRIPT), "--root", str(root), mode],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )


@pytest.fixture
def tree(tmp_path: Path) -> Path:
    """A copy of the inputs with the digest of that copy recorded, so the green does not depend on the live tree."""
    for name in FILES:
        (tmp_path / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, tmp_path / name)
    printed = run(tmp_path, "--print")
    assert printed.returncode == 0, printed.stderr
    recorded = tmp_path / "crates" / "ocx_store" / "src" / "shim_inputs.sha256"
    recorded.parent.mkdir(parents=True)
    recorded.write_text(printed.stdout, encoding="utf-8")
    return tmp_path


def edit(root: Path, name: str, pattern: str, replacement: str) -> None:
    path = root / name
    text = path.read_text(encoding="utf-8")
    changed, count = re.subn(pattern, replacement, text, count=1, flags=re.MULTILINE)
    assert count == 1, f"the mutation's anchor {pattern!r} no longer matches {name}"
    path.write_text(changed, encoding="utf-8")


def test_an_unchanged_tree_matches(tree: Path) -> None:
    result = run(tree, "--check")
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize(
    ("name", "pattern", "replacement"),
    [
        pytest.param(
            "rust-toolchain.toml",
            r'^channel = "[^"]+"',
            'channel = "1.0.0"',
            id="toolchain",
        ),
        pytest.param(
            "Cargo.toml", r'^opt-level = "z"', 'opt-level = "s"', id="shim-profile"
        ),
        pytest.param(
            "Cargo.toml",
            r'"Win32_Security"',
            '"Win32_Security", "Win32_Networking"',
            id="dependency-features",
        ),
        pytest.param(
            "Cargo.lock",
            r'(name = "serde_json"\nversion = "[^"]+"\nsource = "[^"]+"\nchecksum = ")[0-9a-f]',
            r"\g<1>0",
            id="lock-closure",
        ),
        pytest.param(
            ".github/workflows/build-windows-shims.yml",
            r'ZIG_VERSION: "[^"]+"',
            'ZIG_VERSION: "0.99.0"',
            id="zig",
        ),
        pytest.param(
            ".github/workflows/build-windows-shims.yml",
            r"cargo-zigbuild@[0-9.]+",
            "cargo-zigbuild@0.99.0",
            id="zigbuild",
        ),
        pytest.param(
            "taskfiles/rust.taskfile.yml",
            r"--profile shim",
            "--profile shim -v",
            id="build-command",
        ),
    ],
)
def test_a_changed_input_fails(
    tree: Path, name: str, pattern: str, replacement: str
) -> None:
    edit(tree, name, pattern, replacement)
    result = run(tree, "--check")
    assert result.returncode == 1, result.stdout + result.stderr
    assert "Re-cut every blob" in result.stderr, result.stderr


def test_a_release_version_bump_is_not_an_input(tree: Path) -> None:
    edit(tree, "Cargo.lock", r'(name = "ocx_shim"\nversion = ")[^"]+', r"\g<1>9.9.9")
    result = run(tree, "--check")
    assert result.returncode == 0, result.stderr


def test_a_lock_without_the_launcher_reads_as_nothing(tree: Path) -> None:
    edit(tree, "Cargo.lock", r'^name = "ocx_shim"$', 'name = "ocx_shim_renamed"')
    result = run(tree, "--check")
    assert result.returncode == 2, result.stdout + result.stderr


def test_a_workflow_without_the_zig_pin_reads_as_nothing(tree: Path) -> None:
    edit(
        tree,
        ".github/workflows/build-windows-shims.yml",
        r'^\s*ZIG_VERSION: "[^"]+"\n',
        "",
    )
    result = run(tree, "--check")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "ZIG_VERSION" in result.stderr, result.stderr
