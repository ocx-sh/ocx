"""`launcher_blob_check.sh`'s proofs: green on the committed blob this host runs, red on every input it must refuse."""

from __future__ import annotations

import os
import platform
import re
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


REFUSAL_CASES = ["no-record", "old-sidecar", "malformed-record", "direct-starts-child"]
HOME_CASES = ["package", "deferred", "toolchain-project", "toolchain-global"]
HOST_CASES = [*REFUSAL_CASES, "direct-args-env-exit", *HOME_CASES]

# A stand-in launcher that reads its record and prints the argv the stub `ocx` would print; `@SEP@` is the
# `--` separator, dropped by the wrong variant.
HOME_STAND_IN = """#!/bin/sh
rec="$0.launch"
field() { sed -n "s/.*\\"$1\\":\\"\\([^\\"]*\\)\\".*/\\1/p" "$rec" 2>/dev/null; }
case "$(field kind)" in
package) set -- launcher exec "$(field package_root)" @SEP@ tool "$@" ;;
deferred) set -- launcher shim "$(field identifier)" @SEP@ tool "$@" ;;
toolchain)
    if [ "$(field tier)" = global ]; then
        set -- --global exec @SEP@ tool "$@"
    else
        set -- --project "$(field project_root)" exec @SEP@ tool "$@"
    fi ;;
*) exit 78 ;;
esac
printf '%s\\n' "$@"
echo depth=1
"""


def host_blob() -> Path:
    return ROOT / "crates" / "ocx_store" / "src" / "shims" / f"ocx-shim-{HOST_TARGET}"


def check(
    blob: Path,
    script: Path = SCRIPT,
    target: str | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [shutil.which("bash") or "bash", str(script), str(blob), target or str(HOST_TARGET)],
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",  # env falls back to sh on a mangled image and prints its bytes
        check=False,
        timeout=60,
    )


def stand_in(tmp_path: Path, text: str) -> Path:
    blob = tmp_path / "stand-in"
    blob.write_text(text, encoding="utf-8")
    blob.chmod(0o755)
    return blob


@pytest.fixture(scope="module")
def accept_all_run(tmp_path_factory: pytest.TempPathFactory) -> subprocess.CompletedProcess[str]:
    """A stand-in that exits 0 whatever it finds: every case must catch it."""
    return check(stand_in(tmp_path_factory.mktemp("accept-all"), "#!/bin/sh\nexit 0\n"))


@pytest.fixture(scope="module")
def correct_home_run(tmp_path_factory: pytest.TempPathFactory) -> subprocess.CompletedProcess[str]:
    return check(stand_in(tmp_path_factory.mktemp("correct-home"), HOME_STAND_IN.replace("@SEP@", "--")))


@pytest.fixture(scope="module")
def no_separator_run(tmp_path_factory: pytest.TempPathFactory) -> subprocess.CompletedProcess[str]:
    return check(stand_in(tmp_path_factory.mktemp("no-separator"), HOME_STAND_IN.replace("@SEP@ ", "")))


def test_the_committed_host_blob_passes_every_case() -> None:
    result = check(host_blob())
    assert result.returncode == 0, result.stdout + result.stderr
    assert f"{len(HOST_CASES)} cases passed" in result.stdout, result.stdout


def test_a_blob_with_a_changed_byte_fails(tmp_path: Path) -> None:
    blob = tmp_path / "flipped"
    data = bytearray(host_blob().read_bytes())
    data[1] ^= 0xFF
    blob.write_bytes(bytes(data))
    blob.chmod(0o755)
    result = check(blob)
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"{len(HOST_CASES)} failed" in result.stderr, result.stderr


@pytest.mark.parametrize("name", HOST_CASES)
def test_a_launcher_that_accepts_any_record_fails_every_case(
    accept_all_run: subprocess.CompletedProcess[str], name: str
) -> None:
    assert accept_all_run.returncode == 1, accept_all_run.stdout + accept_all_run.stderr
    assert re.search(rf"^FAIL {re.escape(name)}: ", accept_all_run.stdout, re.MULTILINE), accept_all_run.stdout


@pytest.mark.parametrize("name", HOME_CASES)
def test_a_stand_in_that_starts_the_stub_correctly_passes_each_home_case(
    correct_home_run: subprocess.CompletedProcess[str], name: str
) -> None:
    assert re.search(rf"^ok +{re.escape(name)}$", correct_home_run.stdout, re.MULTILINE), correct_home_run.stdout


@pytest.mark.parametrize("name", HOME_CASES)
def test_a_stand_in_that_drops_the_separator_fails_each_home_case(
    no_separator_run: subprocess.CompletedProcess[str], name: str
) -> None:
    assert re.search(rf"^FAIL {re.escape(name)}: ", no_separator_run.stdout, re.MULTILINE), no_separator_run.stdout


def test_a_missing_blob_fails_before_any_case(tmp_path: Path) -> None:
    result = check(tmp_path / "absent")
    assert result.returncode == 66, result.stdout + result.stderr
    assert "ok " not in result.stdout, result.stdout


def test_a_dropped_case_fails_the_floor(tmp_path: Path) -> None:
    """The reader floor: a run of fewer cases than the target owes is red, whatever the blob does."""
    text = SCRIPT.read_text(encoding="utf-8")
    call = "\n    case_malformed_record\n"
    assert text.count(call) == 1, "the mutation's anchor no longer matches the script"
    mutated = tmp_path / "launcher_blob_check.sh"
    mutated.write_text(text.replace(call, "\n"), encoding="utf-8")
    assert call not in mutated.read_text(encoding="utf-8"), "the mutation did not land"
    blob = stand_in(tmp_path, "#!/bin/sh\nexit 0\n")
    result = check(blob, mutated)
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"{len(HOST_CASES) - 1} of {len(HOST_CASES)} cases ran" in result.stderr, result.stderr


def fake_apple_tools(tmp_path: Path, xattr_exit: int) -> dict[str, str]:
    """`codesign` and `xattr` stand-ins on PATH so the Apple-silicon arm runs on Linux: codesign rejects a
    file whose byte 4096 is 0xff (the changed-byte copy), xattr exits `xattr_exit`."""
    bin_dir = tmp_path / "fakebin"
    bin_dir.mkdir()
    (bin_dir / "codesign").write_text(
        '#!/bin/sh\n[ "$(dd if="$3" bs=1 skip=4096 count=1 2>/dev/null | od -An -tx1 | tr -d " \\n")" != ff ]\n',
        encoding="utf-8",
    )
    (bin_dir / "xattr").write_text(f"#!/bin/sh\nexit {xattr_exit}\n", encoding="utf-8")
    for tool in bin_dir.iterdir():
        tool.chmod(0o755)
    path = f"{bin_dir}:{os.environ['PATH']}"
    return {"PATH": path}


def apple_run(tmp_path: Path, xattr_exit: int) -> subprocess.CompletedProcess[str]:
    blob = stand_in(tmp_path, HOME_STAND_IN.replace("@SEP@", "--") + "#" * 5000)
    return check(blob, target="aarch64-apple-darwin", env=fake_apple_tools(tmp_path, xattr_exit))


def test_the_quarantine_start_is_reported_and_owes_no_case(tmp_path: Path) -> None:
    """The quarantine start asserts nothing about Gatekeeper, so it must not count as a passed case."""
    result = apple_run(tmp_path, xattr_exit=0)
    assert "info quarantine-start: exit " in result.stdout, result.stdout
    assert "ok   quarantine-start" not in result.stdout, result.stdout
    assert "ok   signed-copies-start" in result.stdout, result.stdout


def test_a_quarantine_attribute_that_cannot_be_set_fails(tmp_path: Path) -> None:
    result = apple_run(tmp_path, xattr_exit=1)
    assert "FAIL quarantine-start: xattr cannot set" in result.stdout, result.stdout
