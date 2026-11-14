"""`release_provenance_check.py`: every marker red, the reader floor, the
report predicates and the `--exec` end-to-end fakes (see the script's
docstring for what each proof covers)."""

from __future__ import annotations

import importlib.util
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pytest
import release_provenance_check as check_mod
from release_provenance_check import (
    _GOOD_READELF,
    _GOOD_REPORT,
    _LINK_GREENS,
    _LINK_REDS,
    _REPORT_REDS,
    EPOCH_PREFIX,
    EXIT_CLEAN,
    EXIT_FINDING,
    EXIT_USAGE,
    GLIBC_FLOOR,
    MARKERS,
    NEEDED_ALLOWED,
    SHA,
    _broken,
    evaluate_readelf,
    evaluate_report,
    scan,
)


@pytest.mark.parametrize("marker", MARKERS, ids=lambda m: m.decode())
def test_scan_finds_each_marker_at_its_offset(tmp_path: Path, marker: bytes) -> None:
    fixture = tmp_path / f"red-{marker.decode().replace('/', '_')}"
    offset = 4096 + len(marker)
    fixture.write_bytes(b"\x7fELF" + b"\0" * (offset - 4) + marker + b"\0" * 64)
    findings = scan([fixture])
    assert len(findings) == 1
    assert marker.decode() in findings[0]
    assert f"offset {offset}" in findings[0]
    assert str(fixture) in findings[0]
    assert check_mod.main(["--scan", str(fixture)]) == EXIT_FINDING


@pytest.mark.parametrize(
    "name", ["__OCX_TESTING_FORGE_BASE_URL", "__OCX_TESTING_FAULT", "__OCX_TESTING_GITHUB_API_URL"]
)
def test_scan_flags_a_declared_testing_variable(tmp_path: Path, name: str) -> None:
    # The registry declares these only under `__testing`: their names in a binary mean its seams are compiled in.
    fixture = tmp_path / "red-seam"
    fixture.write_bytes(b"\x7fELF" + b"\0" * 32 + name.encode() + b"\0" * 8)
    findings = scan([fixture])
    assert len(findings) == 1
    assert "__OCX_TESTING_" in findings[0]
    assert "offset 36" in findings[0]
    assert check_mod.main(["--scan", str(fixture)]) == EXIT_FINDING


def test_scan_flags_the_fault_variable_followed_by_a_value(tmp_path: Path) -> None:
    # No exemption: the name followed by `=` is a seam string too, so a release binary must not carry it.
    planted = tmp_path / "red-hook-text"
    planted.write_bytes(b"\x7fELF" + b"\0" * 32 + b"__OCX_TESTING_FAULT=after_lock_write" + b"\0" * 8)
    assert len(scan([planted])) == 1
    assert check_mod.main(["--scan", str(planted)]) == EXIT_FINDING


def test_scan_reports_every_occurrence_not_only_the_first(tmp_path: Path) -> None:
    twice = tmp_path / "red-twice"
    twice.write_bytes(MARKERS[1] + b"\0" * 10 + MARKERS[1])
    assert len(scan([twice])) == 2


def test_scan_is_clean_on_near_misses(tmp_path: Path) -> None:
    # The all-zero SHA and near-misses of each marker are not markers.
    clean = tmp_path / "clean"
    clean.write_bytes(
        b"\x7fELF" + b"0" * 40 + b"\0" * 32 + b"placeholder-g0000000\0" + b"placeholder/\0" + b"ci.inval1d"
    )
    assert scan([clean]) == []
    assert check_mod.main(["--scan", str(clean)]) == EXIT_CLEAN


@pytest.fixture
def clean_file(tmp_path: Path) -> Path:
    clean = tmp_path / "clean"
    clean.write_bytes(
        b"\x7fELF" + b"0" * 40 + b"\0" * 32 + b"placeholder-g0000000\0" + b"placeholder/\0" + b"ci.inval1d"
    )
    return clean


def test_reader_floor_reds_under_min_files(clean_file: Path) -> None:
    assert check_mod.main(["--scan", str(clean_file), "--min-files", "2"]) == EXIT_FINDING


def test_reader_floor_does_not_count_one_file_named_twice(clean_file: Path) -> None:
    assert check_mod.main(["--scan", str(clean_file), str(clean_file), "--min-files", "2"]) == EXIT_FINDING


def test_reader_floor_passes_two_distinct_clean_files(tmp_path: Path, clean_file: Path) -> None:
    second = tmp_path / "clean-2"
    second.write_bytes(clean_file.read_bytes())
    assert check_mod.main(["--scan", str(clean_file), str(second), "--min-files", "2"]) == EXIT_CLEAN


def test_scan_over_a_missing_file_is_usage_error(tmp_path: Path) -> None:
    assert check_mod.main(["--scan", str(tmp_path / "absent")]) == EXIT_USAGE


def test_scan_over_a_directory_is_usage_error(tmp_path: Path) -> None:
    assert check_mod.main(["--scan", str(tmp_path)]) == EXIT_USAGE


def test_min_files_zero_is_usage_error(clean_file: Path) -> None:
    assert check_mod.main(["--scan", str(clean_file), "--min-files", "0"]) == EXIT_USAGE


def test_min_files_beside_exec_is_usage_error(clean_file: Path) -> None:
    assert check_mod.main(["--exec", str(clean_file), "--min-files", "2"]) == EXIT_USAGE


@pytest.mark.parametrize("name,report,needle", _REPORT_REDS, ids=[case[0] for case in _REPORT_REDS])
def test_evaluate_report_reds(name: str, report: object, needle: str) -> None:
    findings = evaluate_report(report)
    assert findings
    assert any(needle in finding for finding in findings)


def test_evaluate_report_is_clean_on_a_good_report() -> None:
    assert evaluate_report(_GOOD_REPORT) == []


def test_evaluate_report_is_clean_with_a_dev_channel() -> None:
    with_channel = _broken(("channel",), "dev")
    assert evaluate_report(with_channel) == []


def _write_fake_binary(path: Path, body: str) -> None:
    path.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
    path.chmod(0o755)


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
@pytest.mark.parametrize(
    "name,report,expected",
    [
        ("good", _GOOD_REPORT, EXIT_CLEAN),
        ("placeholder", _broken(("channel",), "test"), EXIT_FINDING),
    ],
)
def test_exec_end_to_end_over_a_fake_binary(tmp_path: Path, name: str, report: object, expected: int) -> None:
    fake = tmp_path / f"fake-ocx-{name}"
    _write_fake_binary(
        fake,
        "import sys\n"
        "assert sys.argv[1:] == ['--format', 'json', 'version'], sys.argv\n"
        f"print({json.dumps(json.dumps(report))})\n",
    )
    assert check_mod.main(["--exec", str(fake)]) == expected


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_exec_reds_on_a_failing_binary(tmp_path: Path) -> None:
    # A release-shaped report on stdout, so only the exit status can red it.
    broken = tmp_path / "fake-ocx-fails"
    _write_fake_binary(
        broken,
        f"import sys\nprint({json.dumps(json.dumps(_GOOD_REPORT))})\nsys.exit(3)\n",
    )
    assert check_mod.main(["--exec", str(broken)]) == EXIT_FINDING


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_exec_reds_on_non_json_output(tmp_path: Path) -> None:
    garbage = tmp_path / "fake-ocx-garbage"
    _write_fake_binary(garbage, "print('not json')\n")
    assert check_mod.main(["--exec", str(garbage)]) == EXIT_FINDING


@pytest.mark.parametrize("name,text,needle", _LINK_REDS, ids=[case[0] for case in _LINK_REDS])
def test_evaluate_readelf_reds(name: str, text: str, needle: str) -> None:
    findings = evaluate_readelf(text)
    assert findings
    assert any(needle in finding for finding in findings)


@pytest.mark.parametrize("name,text", _LINK_GREENS, ids=[case[0] for case in _LINK_GREENS])
def test_evaluate_readelf_greens(name: str, text: str) -> None:
    assert evaluate_readelf(text) == []


def test_link_constants_match_the_release_bytes() -> None:
    # v0.6.5: the highest GLIBC_ either gnu binary needs, and the sonames they link, loaders included.
    assert GLIBC_FLOOR == (2, 34)
    assert NEEDED_ALLOWED == {
        "libc.so.6",
        "libm.so.6",
        "libgcc_s.so.1",
        "ld-linux-x86-64.so.2",
        "ld-linux-aarch64.so.1",
        "librt.so.1",
        "libpthread.so.0",
        "libdl.so.2",
    }


def _write_fake_readelf(path: Path, output: str, status: int = 0) -> None:
    path.write_text(
        f"#!{sys.executable}\nimport sys\nprint({output!r})\nsys.exit({status})\n",
        encoding="utf-8",
    )
    path.chmod(0o755)


@pytest.fixture
def elf_file(tmp_path: Path) -> Path:
    binary = tmp_path / "ocx"
    binary.write_bytes(b"\x7fELF" + b"\0" * 64)
    return binary


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
@pytest.mark.parametrize(
    "name,output,expected",
    [
        ("good", _GOOD_READELF, EXIT_CLEAN),
        ("glibc 2.99", _LINK_REDS[0][1], EXIT_FINDING),
    ],
)
def test_link_end_to_end_over_a_fake_readelf(
    tmp_path: Path,
    elf_file: Path,
    monkeypatch: pytest.MonkeyPatch,
    name: str,
    output: str,
    expected: int,
) -> None:
    fake = tmp_path / "fake-readelf"
    _write_fake_readelf(fake, output)
    monkeypatch.setattr(check_mod, "READELF", str(fake))
    assert check_mod.main(["--link", str(elf_file)]) == expected


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_link_reds_when_readelf_fails_on_a_file(
    tmp_path: Path, elf_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Clean-looking stdout, so only the exit status can red it.
    fake = tmp_path / "fake-readelf"
    _write_fake_readelf(fake, _GOOD_READELF, status=1)
    monkeypatch.setattr(check_mod, "READELF", str(fake))
    assert check_mod.main(["--link", str(elf_file)]) == EXIT_FINDING


def test_link_without_readelf_is_usage_error_never_a_pass(
    tmp_path: Path, elf_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(check_mod, "READELF", str(tmp_path / "absent-readelf"))
    assert check_mod.main(["--link", str(elf_file)]) == EXIT_USAGE


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_link_reader_floor_reds_under_min_files(
    tmp_path: Path, elf_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = tmp_path / "fake-readelf"
    _write_fake_readelf(fake, _GOOD_READELF)
    monkeypatch.setattr(check_mod, "READELF", str(fake))
    assert check_mod.main(["--link", str(elf_file), "--min-files", "1"]) == EXIT_CLEAN
    assert check_mod.main(["--link", str(elf_file), "--min-files", "2"]) == EXIT_FINDING


def test_link_over_a_missing_file_is_usage_error(tmp_path: Path) -> None:
    assert check_mod.main(["--link", str(tmp_path / "absent")]) == EXIT_USAGE


# ---------------------------------------------------------------------------
# The stamped / unstamped pair (`task cross:build:release` against `task cross:build`)
# ---------------------------------------------------------------------------

_STATUS_SCRIPT = Path(__file__).resolve().parents[2] / "bazel" / "release" / "workspace_status.py"
_SPEC = importlib.util.spec_from_file_location("workspace_status", _STATUS_SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
workspace_status = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(workspace_status)

_CI_ENV = {
    "GITHUB_SERVER_URL": "https://github.com",
    "GITHUB_REPOSITORY": "ocx-sh/ocx",
    "GITHUB_RUN_ID": "35032267742",
}
_STATUS_LINE = re.compile(r"STABLE_OCX_(\S+) (.*)")


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A one-commit git checkout on branch `feature/x`, with no CI variable in the environment."""
    root = tmp_path / "checkout"
    root.mkdir()
    # Every variable the status script reads, so a CI runner's GITHUB_WORKFLOW / GITHUB_SHA cannot leak in.
    for name in ("GITHUB_REF_NAME", *workspace_status.PASS_THROUGH):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", os.devnull)
    monkeypatch.setenv("GIT_CONFIG_SYSTEM", os.devnull)

    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", *args],
            cwd=root,
            check=True,
            capture_output=True,
        )

    git("init", "-q", "-b", "feature/x")
    (root / "tracked").write_text("one\n", encoding="utf-8")
    git("add", "tracked")
    git("commit", "-q", "-m", "one")
    monkeypatch.chdir(root)
    return root


def _status(monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], **env: str) -> dict[str, str]:
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    assert workspace_status.main() == 0
    pairs = (_STATUS_LINE.fullmatch(line) for line in capsys.readouterr().out.splitlines())
    return {m[1]: m[2] for m in pairs if m}


def test_status_reports_the_checkout(
    repo: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    status = _status(monkeypatch, capsys)
    assert SHA.fullmatch(status["VERGEN_GIT_SHA"])
    assert status["VERGEN_GIT_DIRTY"] == "false"
    assert status["__OCX_BUILD_BRANCH"] == "feature/x"
    assert not status["VERGEN_BUILD_TIMESTAMP"].startswith(EPOCH_PREFIX)
    assert status["VERGEN_GIT_COMMIT_TIMESTAMP"].endswith("Z")
    # Nothing the environment did not set is invented.
    assert not any(key.startswith("GITHUB_") for key in status)


def test_status_dirty_follows_tracked_files_only(
    repo: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    (repo / "untracked").write_text("x\n", encoding="utf-8")
    assert _status(monkeypatch, capsys)["VERGEN_GIT_DIRTY"] == "false"
    (repo / "tracked").write_text("two\n", encoding="utf-8")
    assert _status(monkeypatch, capsys)["VERGEN_GIT_DIRTY"] == "true"


@pytest.mark.parametrize(
    "env,expected",
    [
        ({"GITHUB_REF": "refs/heads/main", "GITHUB_REF_NAME": "main"}, "main"),
        (
            {"GITHUB_REF": "refs/heads/release/1.x", "GITHUB_REF_NAME": "release/1.x"},
            "release/1.x",
        ),
        # A tag ref names no branch: the checkout's own HEAD answers.
        ({"GITHUB_REF": "refs/tags/v1.2.3", "GITHUB_REF_NAME": "v1.2.3"}, "feature/x"),
        (
            {"GITHUB_REF": "refs/pull/7/merge", "GITHUB_REF_NAME": "7/merge"},
            "feature/x",
        ),
        ({}, "feature/x"),
    ],
)
def test_status_branch_rule(
    repo: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    env: dict[str, str],
    expected: str,
) -> None:
    assert _status(monkeypatch, capsys, **env)["__OCX_BUILD_BRANCH"] == expected


def test_status_omits_the_branch_on_a_detached_head(
    repo: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    subprocess.run(["git", "checkout", "-q", "--detach"], cwd=repo, check=True, capture_output=True)
    assert "__OCX_BUILD_BRANCH" not in _status(monkeypatch, capsys)


def test_status_passes_ci_channel_and_version_through(
    repo: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    status = _status(
        monkeypatch,
        capsys,
        __OCX_BUILD_CHANNEL="dev",
        __OCX_BUILD_VERSION="0.6.7-dev+1",
        **_CI_ENV,
    )
    assert status["__OCX_BUILD_CHANNEL"] == "dev"
    assert status["__OCX_BUILD_VERSION"] == "0.6.7-dev+1"
    assert status["GITHUB_RUN_ID"] == _CI_ENV["GITHUB_RUN_ID"]


def test_status_refuses_a_multiline_value(
    repo: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("GITHUB_WORKFLOW", "a\nVERGEN_GIT_SHA=" + "f" * 40)
    assert workspace_status.main() == 0
    captured = capsys.readouterr()
    assert "GITHUB_WORKFLOW" not in captured.out
    assert "skipping GITHUB_WORKFLOW" in captured.err


def _report_from_status(status: dict[str, str]) -> dict:
    """The version report `build_info.rs` renders from those baked values."""
    report: dict = {
        "version": status["__OCX_BUILD_VERSION"],
        "channel": status["__OCX_BUILD_CHANNEL"],
        "commit": {
            "sha": status["VERGEN_GIT_SHA"],
            "dirty": status["VERGEN_GIT_DIRTY"] == "true",
            "timestamp": status["VERGEN_GIT_COMMIT_TIMESTAMP"],
            "branch": status["__OCX_BUILD_BRANCH"],
        },
        "build": {"timestamp": status["VERGEN_BUILD_TIMESTAMP"]},
    }
    if "GITHUB_RUN_ID" in status:
        run_url = f"{status['GITHUB_SERVER_URL']}/{status['GITHUB_REPOSITORY']}/actions/runs/{status['GITHUB_RUN_ID']}"
        report["ci"] = {"run_url": run_url}
    return report


_PLACEHOLDER_REPORT = {
    "version": "0.6.2",
    "channel": "test",
    "commit": {"sha": "0" * 40, "dirty": True, "branch": "placeholder"},
    "build": {"timestamp": "1970-01-01T00:00:00.000000000Z"},
    "ci": {"run_url": "https://ci.invalid/placeholder/placeholder/actions/runs/0"},
}


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_unstamped_build_is_red_and_stamped_build_is_green(
    repo: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    unstamped = tmp_path / "unstamped-ocx"
    _write_fake_binary(unstamped, f"print({json.dumps(json.dumps(_PLACEHOLDER_REPORT))})\n")
    # The shebang fake carries the markers as a comment, as the real binary carries them as data.
    unstamped.write_bytes(unstamped.read_bytes() + b"# " + b" ".join(MARKERS) + b"\n")

    status = _status(
        monkeypatch,
        capsys,
        __OCX_BUILD_CHANNEL="dev",
        __OCX_BUILD_VERSION="0.6.7-dev+1",
        **_CI_ENV,
    )
    stamped = tmp_path / "stamped-ocx"
    _write_fake_binary(stamped, f"print({json.dumps(json.dumps(_report_from_status(status)))})\n")

    assert check_mod.main(["--scan", str(unstamped)]) == EXIT_FINDING
    assert check_mod.main(["--exec", str(unstamped)]) == EXIT_FINDING
    assert check_mod.main(["--scan", str(stamped)]) == EXIT_CLEAN
    assert check_mod.main(["--exec", str(stamped)]) == EXIT_CLEAN


@pytest.mark.skipif(os.name != "posix", reason="a shebang fake cannot execute outside posix")
def test_stamped_build_without_a_ci_run_is_still_red_under_exec(
    repo: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    # A local `cross:build:release` carries real git state but no run URL: `--scan` clean, `--exec` red.
    status = _status(
        monkeypatch,
        capsys,
        __OCX_BUILD_CHANNEL="dev",
        __OCX_BUILD_VERSION="0.6.7-dev+1",
    )
    local = tmp_path / "local-ocx"
    _write_fake_binary(local, f"print({json.dumps(json.dumps(_report_from_status(status)))})\n")
    assert check_mod.main(["--scan", str(local)]) == EXIT_CLEAN
    assert check_mod.main(["--exec", str(local)]) == EXIT_FINDING
