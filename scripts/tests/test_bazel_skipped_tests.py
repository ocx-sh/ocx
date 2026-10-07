"""`bazel_skipped_tests.py`'s proofs, as pytest.

The reader is checked against synthetic crates, so a mapping bug shows red here
and not only in the cargo run; the live tree must parse to a nonzero set.
"""

from __future__ import annotations

from pathlib import Path

import bazel_skipped_tests as skipped
import pytest


def _crate(root: Path, name: str, package: str, build: str) -> None:
    crate = root / "crates" / name
    crate.mkdir(parents=True)
    (crate / "Cargo.toml").write_text(f'[package]\nname = "{package}"\n', encoding="utf-8")
    (crate / "BUILD.bazel").write_text(build, encoding="utf-8")


def test_maps_lib_and_integration_targets_to_package_and_binary(tmp_path: Path) -> None:
    _crate(
        tmp_path,
        "ocx_cli",
        "ocx",
        'rust_test(\n    name = "t",\n    args = [\n        "--exact",\n        "--skip=a::b",\n    ],\n    crate = ":ocx_cli",\n)\n'
        'rust_test(\n    name = "other",\n    args = ["--exact"],\n    crate = ":ocx_cli",\n)\n',
    )
    _crate(
        tmp_path,
        "ocx_test_support",
        "ocx_test_support",
        'rust_test(\n    name = "ws",\n    srcs = ["tests/ws.rs"],\n    args = [\n        # "--skip=commented",\n        "--skip=c",\n    ],\n)\n',
    )
    assert skipped.parse(tmp_path) == [
        skipped.Skip("ocx", None, "a::b"),
        skipped.Skip("ocx_test_support", "ws", "c"),
    ]


def test_a_tree_without_skips_parses_to_nothing_and_run_reds(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _crate(tmp_path, "ocx_util", "ocx_util", 'rust_test(\n    name = "t",\n    crate = ":ocx_util",\n)\n')
    assert skipped.parse(tmp_path) == []
    assert skipped.run([]) == 1
    assert "parsed 0 --skip args" in capsys.readouterr().err


def test_the_live_tree_parses_a_nonzero_set() -> None:
    assert len(skipped.parse(skipped.REPO_ROOT)) > 0
