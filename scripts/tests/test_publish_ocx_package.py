"""`publish_ocx_package.sh`'s proofs, against a stub `ocx` that records its argv."""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "publish_ocx_package.sh"
TOKEN = "PLANTED_FAKE_TOKEN_0123456789"
BASE = [
    "--identifier",
    "localhost:15000/ocx-sh/dev/cli:1.0.0",
    "--platform",
    "linux/amd64",
]

STUB = """#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$STUB_LOG"
# `create` is the last moment the staged tree exists; record the binary it holds.
if [[ "$1 $2" == "package create" ]]; then
    printf 'layout: %s\\n' "$(cd "$3" && find bin -type f | sort | tr '\\n' ' ')" >> "$STUB_LOG"
fi
"""


@pytest.fixture
def run(tmp_path: Path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    stub = bin_dir / "ocx"
    stub.write_text(STUB, encoding="utf-8")
    stub.chmod(0o755)
    binary = tmp_path / "ocx-binary"
    binary.write_bytes(b"x")
    log = tmp_path / "argv.log"

    def go(*args: str) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        env = {
            "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            "STUB_LOG": str(log),
            "GITHUB_TOKEN": TOKEN,
            "OCX_AUTH_ghcr_io_PASSWORD": TOKEN,
        }
        result = subprocess.run(
            [
                shutil.which("bash") or "bash",
                str(SCRIPT),
                *BASE,
                "--binary",
                str(binary),
                *args,
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=env,
            check=False,
        )
        calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        return result, calls

    return go


def push_line(calls: list[str]) -> str:
    pushes = [c for c in calls if c.startswith("package push")]
    assert len(pushes) == 1, calls
    return pushes[0]


def test_a_bare_publish_creates_then_pushes_with_no_extra_flags(run) -> None:
    result, calls = run()
    assert result.returncode == 0, result.stderr
    assert (
        calls[0].startswith("package create ")
        and "-p linux/amd64" in calls[0]
        and "-m " in calls[0]
    )
    push = push_line(calls)
    assert push.startswith(
        "package push --identifier localhost:15000/ocx-sh/dev/cli:1.0.0 --platform linux/amd64 "
    )
    for flag in ("--sign", "--sbom", "--cascade", "--no-keep-tag", "--tags-file"):
        assert flag not in push


@pytest.mark.parametrize(
    ("args", "expected"),
    [
        (["--no-keep-tag", "--cascade"], ["--no-keep-tag", "--cascade"]),
        (["--sign"], ["--sign"]),
        (["--sign", "--sbom", "/tmp/s.json"], ["--sign", "--sbom /tmp/s.json"]),
        (["--tags-file", "/tmp/tags"], ["--tags-file /tmp/tags"]),
        (["--annotation", "a=b"], ["--annotation a=b"]),
    ],
)
def test_each_flag_reaches_the_push(run, args: list[str], expected: list[str]) -> None:
    result, calls = run(*args)
    assert result.returncode == 0, result.stderr
    push = push_line(calls)
    for fragment in expected:
        assert fragment in push


@pytest.mark.parametrize(
    ("platform", "binary"),
    [
        ("windows/amd64", "bin/ocx.exe"),
        ("windows/arm64", "bin/ocx.exe"),
        ("linux/amd64", "bin/ocx"),
        ("darwin/arm64", "bin/ocx"),
    ],
)
def test_the_binary_is_staged_under_the_name_its_platform_runs(
    run, platform: str, binary: str
) -> None:
    result, calls = run("--platform", platform)
    assert result.returncode == 0, result.stderr
    assert [c for c in calls if c.startswith("layout: ")] == [f"layout: {binary} "]


def test_the_metadata_flag_reaches_create(run) -> None:
    _, calls = run("--metadata", "/tmp/meta.json")
    assert "-m /tmp/meta.json" in calls[0]


def test_no_environment_value_is_printed(run) -> None:
    result, calls = run("--sign", "--cascade")
    assert TOKEN not in result.stdout + result.stderr + "\n".join(calls)


def test_a_missing_identifier_is_a_usage_error(run, tmp_path: Path) -> None:
    result = subprocess.run(
        ["bash", str(SCRIPT), "--platform", "linux/amd64", "--binary", str(tmp_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    assert result.returncode == 64
