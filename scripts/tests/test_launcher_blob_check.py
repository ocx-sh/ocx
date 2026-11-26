"""`launcher_blob_check.sh`'s proofs: green on the committed blob this host runs, red on every input it must refuse."""

from __future__ import annotations

import platform
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "launcher_blob_check.sh"
HOST_TARGET = {
    "x86_64": "x86_64-unknown-linux-musl",
    "aarch64": "aarch64-unknown-linux-musl",
}.get(platform.machine())

pytestmark = pytest.mark.skipif(
    not sys.platform.startswith("linux") or HOST_TARGET is None,
    reason="the committed Linux blobs run on a Linux x86_64 or aarch64 host only",
)


def host_blob() -> Path:
    return ROOT / "crates" / "ocx_store" / "src" / "shims" / f"ocx-shim-{HOST_TARGET}"


def check(blob: Path, script: Path = SCRIPT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [shutil.which("bash") or "bash", str(script), str(blob), str(HOST_TARGET)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        timeout=60,
    )


def test_the_committed_host_blob_passes_every_case() -> None:
    result = check(host_blob())
    assert result.returncode == 0, result.stdout + result.stderr
    assert "4 cases passed" in result.stdout, result.stdout


def test_a_blob_with_a_changed_byte_fails(tmp_path: Path) -> None:
    blob = tmp_path / "flipped"
    data = bytearray(host_blob().read_bytes())
    data[1] ^= 0xFF
    blob.write_bytes(bytes(data))
    blob.chmod(0o755)
    result = check(blob)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "4 failed" in result.stderr, result.stderr


def test_a_launcher_that_accepts_any_sidecar_fails(tmp_path: Path) -> None:
    """A stand-in that exits 0 whatever it finds: every refusal case must catch it."""
    blob = tmp_path / "accepts-everything"
    blob.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    blob.chmod(0o755)
    result = check(blob)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "FAIL malformed-sidecar: exit 0, want 78" in result.stdout, result.stdout


def test_a_missing_blob_fails_before_any_case(tmp_path: Path) -> None:
    result = check(tmp_path / "absent")
    assert result.returncode == 66, result.stdout + result.stderr
    assert "ok " not in result.stdout, result.stdout


def test_a_dropped_case_fails_the_floor(tmp_path: Path) -> None:
    """The reader floor: a run of fewer cases than the target owes is red even when each one passed."""
    text = SCRIPT.read_text(encoding="utf-8")
    call = "\n    case_malformed_sidecar\n"
    assert text.count(call) == 1, "the mutation's anchor no longer matches the script"
    mutated = tmp_path / "launcher_blob_check.sh"
    mutated.write_text(text.replace(call, "\n"), encoding="utf-8")
    result = check(host_blob(), mutated)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "3 of 4 cases ran, 0 failed" in result.stderr, result.stderr
