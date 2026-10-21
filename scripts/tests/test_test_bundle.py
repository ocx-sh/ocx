"""`test_bundle.py run`'s proofs: green on a full bundle, red on each under-run shape."""

from __future__ import annotations

import io
import json
import tarfile
from argparse import Namespace
from pathlib import Path

import pytest
import test_bundle as tb

# Stands in for a libtest binary: the verdict comes from the env the manifest sets.
FAKE = """#!/bin/sh
case " $* " in *" --exact "*) n=$EXACT_PASSED;; *) n=$MAIN_PASSED;; esac
echo "running $n tests"
echo "test result: ok. $n passed; 0 failed; 0 ignored"
"""


def make_bundle(
    tmp: Path,
    tests: list[dict],
    *,
    candidates: int | None = None,
    target: str = "x86_64-unknown-linux-gnu",
) -> Path:
    entries = []
    for i, t in enumerate(tests):
        slot = f"t{i}"
        entries.append(
            {
                "label": t["label"],
                "dir": slot,
                "exe": "bin/test",
                "args": [f"--skip={s}" for s in t.get("skips", [])],
                "env": {"MAIN_PASSED": str(t.get("main", 3)), "EXACT_PASSED": str(t.get("exact", 1))},
                "skips": t.get("skips", []),
            }
        )
    manifest = {"target": target, "candidates": len(tests) if candidates is None else candidates, "tests": entries}
    out = tmp / "unit-tests.tar.gz"
    with tarfile.open(out, "w:gz") as tar:
        for i, t in enumerate(tests):
            if t.get("missing"):
                continue
            info = tarfile.TarInfo(f"t{i}/_main/bin/test")
            data = FAKE.encode()
            info.size, info.mode = len(data), 0o755
            tar.addfile(info, io.BytesIO(data))
        data = json.dumps(manifest).encode()
        info = tarfile.TarInfo("manifest.json")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    return out


def run(tmp: Path, bundle: Path, *, source_root: Path | None = None, flt: str = "") -> str | None:
    """None when the run is green, else the `FAILED: ...` message."""
    args = Namespace(
        bundle=str(bundle),
        junit=str(tmp / "junit.xml"),
        timeout=60,
        filter=flt,
        source_root=str(source_root) if source_root else None,
    )
    try:
        tb.cmd_run(args)
    except SystemExit as e:
        return str(e.code)
    return None


def test_a_full_bundle_is_green(tmp_path: Path) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a"}, {"label": "//b:b"}])
    assert run(tmp_path, bundle) is None


def test_a_missing_executable_is_red(tmp_path: Path) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a"}, {"label": "//b:b", "missing": True}])
    failed = run(tmp_path, bundle)
    assert failed is not None
    assert "//b:b" in failed


def test_a_target_with_zero_cases_is_red(tmp_path: Path) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a"}, {"label": "//b:b", "main": 0}])
    failed = run(tmp_path, bundle)
    assert failed is not None
    assert "//b:b" in failed


def test_a_cfg_gated_target_may_be_empty_only_off_its_platform(tmp_path: Path) -> None:
    label = "//crates/ocx_cli:macos_self_contained"
    empty = [{"label": "//a:a"}, {"label": label, "main": 0}]
    assert run(tmp_path, make_bundle(tmp_path, empty)) is None
    on_mac = make_bundle(tmp_path, empty, target="aarch64-apple-darwin")
    failed = run(tmp_path, on_mac)
    assert failed is not None
    assert label in failed


def test_a_manifest_short_of_its_candidates_is_red(tmp_path: Path) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a"}], candidates=2)
    failed = run(tmp_path, bundle)
    assert failed is not None
    assert "1 tests for 2 candidates" in failed


def test_a_filtered_run_skips_the_candidate_count(tmp_path: Path) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a"}, {"label": "//b:b"}], candidates=3)
    assert run(tmp_path, bundle, flt="//a:") is None


def test_skipped_testcases_run_when_a_source_root_is_given(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a", "skips": ["m::one", "m::two"]}])
    assert run(tmp_path, bundle, source_root=tmp_path) is None
    assert "skipped pass: 2 skipped testcases run (2 passed" in capsys.readouterr().out


def test_without_a_source_root_the_summary_says_the_skipped_pass_did_not_run(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a", "skips": ["m::one"]}])
    assert run(tmp_path, bundle) is None
    assert "skipped pass: NOT run" in capsys.readouterr().out


def test_a_skip_naming_no_testcase_is_red(tmp_path: Path) -> None:
    bundle = make_bundle(tmp_path, [{"label": "//a:a", "skips": ["m::gone"], "exact": 0}])
    failed = run(tmp_path, bundle, source_root=tmp_path)
    assert failed is not None
    assert "m::gone" in failed


def test_a_workspace_reading_skip_is_not_run_even_with_cargo_on_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    # `exact: 0` stands in for `cargo metadata` failing on a checkout without submodules.
    monkeypatch.setattr(tb.shutil, "which", lambda _name: "/usr/bin/cargo")
    bundle = make_bundle(tmp_path, [{"label": "//a:a", "skips": ["deps_direction"], "exact": 0}])
    assert run(tmp_path, bundle, source_root=tmp_path) is None
    assert "not run (reads the workspace)" in capsys.readouterr().out
