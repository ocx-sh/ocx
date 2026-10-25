"""`cross_matrix.py`'s proofs: the default is every target, a subset is exact, and a stray triple is red."""

from __future__ import annotations

import json
from pathlib import Path

import cross_matrix as cm
import pytest
import yaml

ROOT = Path(__file__).resolve().parents[2]
EVERY = yaml.safe_load((ROOT / "taskfiles" / "cross.taskfile.yml").read_text(encoding="utf-8"))["vars"]["TARGETS"].split()


def legs(only: list[str]) -> dict[str, dict[str, object]]:
    return {leg["target"]: leg for leg in cm.matrix(EVERY, only)["include"]}  # type: ignore[index]


def test_the_default_is_every_supported_target_with_its_flags() -> None:
    found = legs([])
    assert sorted(found) == sorted(EVERY) and len(EVERY) >= 8
    assert {t for t, leg in found.items() if leg.get("bundle")} == cm.BUNDLE
    assert {t for t, leg in found.items() if leg.get("test-bin")} == cm.TEST_BIN
    # A bundle leg also stages the acceptance binaries: `cross:build:bundle` does both in one invocation.
    assert cm.BUNDLE <= cm.TEST_BIN


def test_a_subset_keeps_only_those_legs_and_their_flags() -> None:
    found = legs(["x86_64-pc-windows-gnullvm", "aarch64-apple-darwin"])
    assert sorted(found) == ["aarch64-apple-darwin", "x86_64-pc-windows-gnullvm"]
    assert all(leg.get("bundle") and leg.get("test-bin") for leg in found.values())
    assert legs(["aarch64-unknown-linux-musl"]) == {"aarch64-unknown-linux-musl": {"target": "aarch64-unknown-linux-musl"}}


def test_a_triple_outside_the_supported_list_is_refused() -> None:
    with pytest.raises(ValueError, match=r"unsupported target\(s\) \['x86_64-pc-windows-msvc'\]"):
        cm.matrix(EVERY, ["x86_64-pc-windows-msvc"])


def test_a_flag_on_an_unsupported_target_is_refused() -> None:
    with pytest.raises(ValueError, match="not supported targets"):
        cm.matrix([t for t in EVERY if t != "aarch64-apple-darwin"], [])


def test_main_prints_compact_json_and_exits_1_on_a_refusal(capsys: pytest.CaptureFixture[str]) -> None:
    assert cm.main(["--all", " ".join(EVERY), "--only", "aarch64-apple-darwin"]) == 0
    assert json.loads(capsys.readouterr().out) == {"include": [{"target": "aarch64-apple-darwin", "bundle": True, "test-bin": True}]}
    assert cm.main(["--all", " ".join(EVERY), "--only", "nope"]) == 1
    assert "unsupported target" in capsys.readouterr().err
