"""`test_bundle.py run`'s proofs: green on a full bundle, red on each under-run shape."""

from __future__ import annotations

import io
import json
import re
import subprocess
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


def _bazel_argv(monkeypatch: pytest.MonkeyPatch, rc: str) -> list[str]:
    """The argv `bazel()` hands to `subprocess.run` for a given `--cache-rc` value."""
    seen: list[list[str]] = []

    def fake_run(cmd: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        seen.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

    monkeypatch.setattr(tb.subprocess, "run", fake_run)
    tb.bazel("build", "//a:a", rc=rc)
    assert len(seen) == 1
    return seen[0]


def test_the_cache_rc_the_task_passes_is_the_first_startup_option(monkeypatch: pytest.MonkeyPatch) -> None:
    argv = _bazel_argv(monkeypatch, "--bazelrc=/runner/bazel-cache.rc")
    assert argv == [*tb.BAZEL, "--bazelrc=/runner/bazel-cache.rc", "build", "//a:a"]


def test_no_cache_rc_flag_when_the_task_passes_none(monkeypatch: pytest.MonkeyPatch) -> None:
    # The task's `CACHE_RC` is empty on fork PRs and developer shells; `--bazelrc` naming a missing file is a hard bazel error.
    assert _bazel_argv(monkeypatch, "") == [*tb.BAZEL, "build", "//a:a"]


def test_the_script_derives_no_cache_rc_of_its_own(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    # `CACHE_RC` is decided once, in the root taskfile; an env var and a file on disk change nothing here.
    (tmp_path / "bazel-cache.rc").write_text("build --remote_header='x=y'\n", encoding="utf-8")
    monkeypatch.setenv("RUNNER_TEMP", str(tmp_path))
    assert _bazel_argv(monkeypatch, "") == [*tb.BAZEL, "build", "//a:a"]


def test_a_query_below_the_rust_test_floor_is_red() -> None:
    from bazel_gate_proofs import CRATES_TEST_TARGETS

    full = " ".join(f"//c{i}:t" for i in range(CRATES_TEST_TARGETS))
    assert len(tb.rust_test_labels(full)) == CRATES_TEST_TARGETS
    short = " ".join(f"//c{i}:t" for i in range(CRATES_TEST_TARGETS - 1))
    with pytest.raises(SystemExit, match=rf"found {CRATES_TEST_TARGETS - 1} rust_test targets"):
        tb.rust_test_labels(short)
    with pytest.raises(SystemExit, match="found 0 rust_test targets"):
        tb.rust_test_labels("")


def test_every_bazel_call_goes_through_the_seam() -> None:
    # `BAZEL` appears in its definition and inside `bazel()` only; a second use is a call that skips the rc.
    assert len(re.findall(r"\bBAZEL\b", Path(tb.__file__).read_text(encoding="utf-8"))) == 2


class _BuildSeen(Exception):
    """Raised by the fake `bazel` once `build` ran, so `cmd_bundle` stops before it needs a real graph."""


def _built_labels(monkeypatch: pytest.MonkeyPatch, **flags_: bool) -> tuple[list[str], list[str]]:
    """`(build argv, labels in its --target_pattern_file)` of the one `bazel build` `cmd_bundle` issues."""
    from bazel_gate_proofs import CRATES_TEST_TARGETS

    tests = [f"//crates/c{i}:t" for i in range(CRATES_TEST_TARGETS)]
    seen: dict[str, list[str]] = {}

    def fake_bazel(*args: str, rc: str = "", capture: bool = False) -> str:
        del rc, capture
        if args[0] == "query":
            return "\n".join(tests)
        assert args[0] == "build", args
        pattern = next(a for a in args if a.startswith("--target_pattern_file="))
        seen["argv"] = list(args)
        seen["labels"] = Path(pattern.split("=", 1)[1]).read_text(encoding="utf-8").split()
        raise _BuildSeen

    monkeypatch.setattr(tb, "bazel", fake_bazel)
    ns = Namespace(target="x86_64-pc-windows-gnullvm", out=None, cache_rc="", with_ocx=False, with_test_bin=False)
    for name, value in flags_.items():
        setattr(ns, name, value)
    with pytest.raises(_BuildSeen):
        tb.cmd_bundle(ns)
    return seen["argv"], [label for label in seen["labels"] if label not in tests]


def test_the_bundle_alone_builds_only_the_tests(monkeypatch: pytest.MonkeyPatch) -> None:
    _, extra = _built_labels(monkeypatch)
    assert extra == []


def test_with_ocx_and_test_bin_the_same_build_also_produces_the_binary_and_the_acceptance_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    argv, extra = _built_labels(monkeypatch, with_ocx=True, with_test_bin=True)
    # One invocation; `ocx` is in both sets and is built once.
    assert extra == [tb.OCX, "//crates/ocx_shim:ocx_shim", "//crates/ocx_schema:ocx_schema_bin"]
    # The unit-test flags are unchanged by the extra labels (-c opt, the target platform, the test toolchain flag).
    assert [*argv[2:4]] == ["-c", "opt"] and tb.NO_TEST_TOOLCHAIN in argv


def test_with_ocx_alone_adds_only_ocx(monkeypatch: pytest.MonkeyPatch) -> None:
    _, extra = _built_labels(monkeypatch, with_ocx=True)
    assert extra == [tb.OCX]


@pytest.mark.parametrize(("target", "ocx_name"), [("x86_64-pc-windows-gnullvm", "ocx.exe"), ("aarch64-apple-darwin", "ocx")])
def test_stage_copies_each_built_label_executable_to_its_layout_path(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, target: str, ocx_name: str
) -> None:
    built = tmp_path / "bazel-out" / "bin"
    built.mkdir(parents=True)
    for name in ("ocx", "ocx_shim", "ocx_schema_bin"):
        (built / name).write_bytes(name.encode())
        (built / name).chmod(0o444)  # Bazel's outputs are read-only
    monkeypatch.setattr(tb, "cquery", lambda label, _t, *_o, rc="": str(built / label.rsplit(":", 1)[1]) + "\n")
    out = tmp_path / "test-bin"
    tb.stage(tb.bin_layout(target), out, target, "")
    tb.stage(tb.bin_layout(target)[:1], out, target, "")  # a second run replaces what the first left read-only
    assert (out / ocx_name).read_bytes() == b"ocx"
    assert (out / "schema" / tb.exe_name("ocx_schema", target)).read_bytes() == b"ocx_schema_bin"
    assert (out / ocx_name).stat().st_mode & 0o111


def test_stage_refuses_a_label_that_built_nothing(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setattr(tb, "cquery", lambda *_a, **_k: "\n")
    with pytest.raises(SystemExit, match="produced no file"):
        tb.stage([(tb.OCX, "ocx")], tmp_path, "aarch64-apple-darwin", "")
