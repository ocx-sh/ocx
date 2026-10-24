"""`windows_local.sh`'s refusals and its staging, driven with a stub `cmd.exe` and no Windows host.

What the Windows side then does with the staged tree (zot, `task test:accept:native`) is proven
on the Windows host by `task test:windows-local`, not here.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "windows_local.sh"


def _run(*args: str, **env: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        env={**os.environ, **env},
    )


@pytest.mark.parametrize(
    "args",
    [
        [],
        ["--mode", "bogus"],
        ["--mode"],
        ["--mode", "accept", "--nope"],
        ["--mode", "unit", "--exe", "x.exe"],
        ["--mode", "unit", "--shard-count", "2", "--shard-index", "0"],
        ["--mode", "unit", "--", "one", "two"],
    ],
    ids=["no-mode", "bad-mode", "mode-without-value", "unknown-flag", "unit-exe", "unit-shard", "unit-two-filters"],
)
def test_a_malformed_call_exits_64(args: list[str]) -> None:
    done = _run(*args)
    assert done.returncode == 64, done.stderr
    assert "windows_local:" in done.stderr


@pytest.mark.parametrize(
    "args",
    [
        ["--shard-count", "4", "--shard-index", "4"],
        ["--shard-count", "0", "--shard-index", "0"],
        ["--shard-count", "4"],
        ["--shard-index", "1"],
        ["--shard-count", "four", "--shard-index", "0"],
    ],
    ids=["index-eq-count", "count-0", "count-only", "index-only", "count-nan"],
)
def test_a_shard_the_selector_refuses_exits_2(args: list[str]) -> None:
    done = _run("--mode", "accept", *args)
    assert done.returncode == 2, done.stderr
    assert "accept shard:" in done.stderr


def test_a_missing_exe_exits_66(tmp_path: Path) -> None:
    done = _run("--mode", "accept", "--exe", str(tmp_path / "ocx.exe"))
    assert done.returncode == 66, done.stderr


def test_without_a_cmd_exe_it_exits_69_before_building_anything(tmp_path: Path) -> None:
    done = _run("--mode", "accept", WINDOWS_LOCAL_CMD_EXE=str(tmp_path / "no-cmd.exe"))
    assert done.returncode == 69, done.stderr
    assert "WSL" in done.stderr


TRIPLE = "x86_64-pc-windows-gnullvm"

# Answers the %USERPROFILE% probe; any other call is a batch step, logged and run as the host would:
# the reports a run leaves in the stage, a CRLF line of output, $STUB_EXIT as the status.
CMD_EXE = r"""#!/usr/bin/env bash
set -u
if [ "$2" = 'echo %USERPROFILE%' ]; then
    printf '%s\r\n' "$STUB_PROFILE"
    exit 0
fi
step=${2//\\//}
{ printf -- '--- step\n'; cat "$step"; } >>"$STUB_LOG/steps.log"
case $(cat "$step") in
    *test:accept:native*) mkdir -p target/accept/junit && echo '<testsuites/>' >target/accept/junit/shard-0.xml ;;
    *cross:test:run*) mkdir -p target/windows-local && echo '<testsuites/>' >target/windows-local/unit-junit.xml ;;
esac
printf 'child said hello\r\n'
exit "${STUB_EXIT:-0}"
"""
WSLPATH = '#!/usr/bin/env bash\nprintf "%s\\n" "$2"\n'
TASK = '#!/usr/bin/env bash\nprintf "%s\\n" "$*" >>"$STUB_LOG/task.log"\n'

linux_only = pytest.mark.skipif(
    sys.platform != "linux", reason="windows_local.sh needs GNU comm and sed -z; it runs from WSL"
)


class Done(NamedTuple):
    code: int
    out: str
    err: str


class Rig:
    """A throwaway checkout named `repo` holding a copy of the script, and the stubs and stage it runs against.

    Bytes in, bytes out: text mode would turn the CRLFs the CR-stripping proof reads into LFs.
    """

    def __init__(self, root: Path) -> None:
        self.root = root
        self.repo = root / "repo"
        self.profile = root / "profile"
        self.stage = self.profile / "ocx-wintest" / "repo"
        self.own = self.profile / "ocx-wintest" / "_wl"
        self.log = root / "log"
        self.stubs = root / "stubs"
        for directory in (self.repo / "scripts", self.log, self.stubs, self.own / "py" / "Scripts"):
            directory.mkdir(parents=True)
        shutil.copy(SCRIPT, self.repo / "scripts" / "windows_local.sh")
        # The venv step is skipped when its python3.exe is there.
        (self.own / "py" / "Scripts" / "python3.exe").write_bytes(b"")
        for name, body in (("cmd.exe", CMD_EXE), ("wslpath", WSLPATH), ("task", TASK)):
            (self.stubs / name).write_text(body, encoding="utf-8")
            (self.stubs / name).chmod(0o755)
        self.git("init", "-q")

    def git(self, *args: str) -> None:
        env = {
            **os.environ,
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "XDG_CONFIG_HOME": str(self.root),
        }
        subprocess.run(["git", "-C", str(self.repo), *args], check=True, capture_output=True, env=env)

    def track(self, **files: str) -> None:
        for name, content in files.items():
            (self.repo / name).write_text(content, encoding="utf-8")
        self.git("add", "-A")

    def binaries(self, *, shim: bool = True, schema: bool = True, text: str = "new") -> Path:
        """A directory holding `ocx.exe` and, unless left out, its two siblings; returns `ocx.exe`."""
        directory = self.root / "bins"
        (directory / "schema").mkdir(parents=True, exist_ok=True)
        (directory / "ocx.exe").write_text(text, encoding="utf-8")
        if shim:
            (directory / "ocx-shim.exe").write_text(text, encoding="utf-8")
        if schema:
            (directory / "schema" / "ocx_schema.exe").write_text(text, encoding="utf-8")
        return directory / "ocx.exe"

    def steps(self) -> str:
        log = self.log / "steps.log"
        return log.read_bytes().decode("utf-8") if log.exists() else ""

    def run(self, *args: str, **env: str) -> Done:
        stub_env = {
            "PATH": f"{self.stubs}{os.pathsep}{os.environ['PATH']}",
            "WSL_DISTRO_NAME": "stub",
            "WINDOWS_LOCAL_CMD_EXE": str(self.stubs / "cmd.exe"),
            "WINDOWS_LOCAL_DRIVE_ROOT": str(self.root),
            "TASK_EXE": str(self.stubs / "task"),
            "STUB_PROFILE": str(self.profile),
            "STUB_LOG": str(self.log),
            "TMPDIR": str(self.root),
        }
        done = subprocess.run(
            ["bash", str(self.repo / "scripts" / "windows_local.sh"), *args],
            capture_output=True,
            check=False,
            env={**os.environ, **stub_env, **env},
        )
        return Done(done.returncode, done.stdout.decode("utf-8"), done.stderr.decode("utf-8"))

    def accept(self, *args: str, **env: str) -> Done:
        return self.run("--mode", "accept", "--exe", str(self.binaries()), *args, **env)


@pytest.fixture
def rig(tmp_path: Path) -> Rig:
    if shutil.which("rsync") is None:
        pytest.fail("no rsync on PATH: windows_local.sh stages with it, and a skip here would hide every staging proof")
    return Rig(tmp_path)


@linux_only
def test_a_tracked_file_deleted_from_the_working_tree_neither_fails_the_stage_nor_survives_in_it(rig: Rig) -> None:
    rig.track(**{"keep.txt": "k", "gone.txt": "g"})
    assert rig.accept().code == 0
    assert (rig.stage / "gone.txt").is_file()

    (rig.repo / "gone.txt").unlink()
    done = rig.accept()

    assert done.code == 0, done.err
    assert (rig.stage / "keep.txt").is_file()
    assert not (rig.stage / "gone.txt").exists()


@linux_only
def test_the_sweep_deletes_what_git_does_not_list_and_keeps_the_directories_a_run_leaves(rig: Rig) -> None:
    rig.track(**{"listed.txt": "l"})
    kept = ["test/bin/keep.dll", "target/report.xml", ".venv/lib.py", "pkg/__pycache__/m.pyc", "test/results/r.xml"]
    swept = ["stale.txt", "pkg/old.py"]
    for name in kept + swept:
        (rig.stage / name).parent.mkdir(parents=True, exist_ok=True)
        (rig.stage / name).write_text("seed", encoding="utf-8")

    done = rig.accept()

    assert done.code == 0, done.err
    assert (rig.stage / "listed.txt").is_file()
    assert [name for name in swept if (rig.stage / name).exists()] == []
    assert [name for name in kept if not (rig.stage / name).exists()] == []


@linux_only
@pytest.mark.parametrize(("mode", "status"), [("accept", 7), ("unit", 3)])
def test_the_childs_status_is_the_scripts_and_the_reports_still_leave_the_stage(
    rig: Rig, mode: str, status: int
) -> None:
    if mode == "unit":
        bundle = rig.repo / "target" / "cross" / TRIPLE / "unit-tests.tar.gz"
        bundle.parent.mkdir(parents=True)
        bundle.write_bytes(b"bundle")
        done = rig.run("--mode", "unit", STUB_EXIT=str(status))
        report = rig.repo / "target" / "windows-local" / "unit-junit.xml"
    else:
        done = rig.accept(STUB_EXIT=str(status))
        report = rig.repo / "target" / "windows-local" / "junit" / "shard-0.xml"

    assert done.code == status, done.err
    assert report.is_file(), "a failing run's reports are the ones to read"


@linux_only
def test_the_childs_output_reaches_the_caller_without_carriage_returns(rig: Rig) -> None:
    done = rig.accept()

    assert done.code == 0, done.err
    assert "child said hello\n" in done.out
    assert "\r" not in done.out


@linux_only
@pytest.mark.parametrize(
    ("filter_text", "quoted"),
    [("persist_temp_file", '"persist_temp_file"'), ("100%PATH%", '"100%%PATH%%"'), ("a&b|c>d", '"a&b|c>d"')],
    ids=["plain", "percent-doubled", "metacharacters-stay-inside-the-quotes"],
)
def test_the_unit_filter_reaches_the_batch_file_in_quotes_with_percent_doubled(
    rig: Rig, filter_text: str, quoted: str
) -> None:
    bundle = rig.repo / "target" / "cross" / TRIPLE / "unit-tests.tar.gz"
    bundle.parent.mkdir(parents=True)
    bundle.write_bytes(b"bundle")

    done = rig.run("--mode", "unit", "--", filter_text)

    assert done.code == 0, done.err
    assert f" FILTER={quoted}\r\n" in rig.steps()


@linux_only
@pytest.mark.parametrize(
    "filter_text",
    ['a"&calc&"b', "a\nb", "a\rb", "$(whoami)", "`whoami`", "tail\\"],
    ids=["quote", "line-feed", "carriage-return", "command-substitution", "backtick", "backslash"],
)
def test_a_unit_filter_cmd_exe_or_the_taskfile_shell_would_act_on_exits_64_before_cmd_runs(
    rig: Rig, filter_text: str
) -> None:
    done = rig.run("--mode", "unit", "--", filter_text)

    assert done.code == 64, done.err
    assert "windows_local:" in done.err
    assert rig.steps() == ""


@linux_only
def test_an_accept_argument_holding_a_quote_exits_64_and_a_percent_is_doubled(rig: Rig) -> None:
    refused = rig.accept("--", 'say "hi"')
    assert refused.code == 64, refused.err
    assert rig.steps() == ""

    done = rig.accept("--", "-k", "50%")
    assert done.code == 0, done.err
    assert ' "-k" "50%%"' in rig.steps()


@linux_only
def test_a_staging_directory_that_is_a_symlink_is_refused_before_anything_in_it_is_swept(rig: Rig) -> None:
    outside = rig.root / "outside"
    outside.mkdir()
    (outside / "precious.txt").write_text("mine", encoding="utf-8")
    rig.stage.symlink_to(outside)

    done = rig.accept()

    assert done.code == 77, done.err
    assert (outside / "precious.txt").read_text(encoding="utf-8") == "mine"
    assert rig.steps() == ""


@linux_only
def test_the_scripts_own_directory_being_a_symlink_is_refused(rig: Rig) -> None:
    outside = rig.root / "outside"
    shutil.move(rig.own, outside)
    rig.own.symlink_to(outside)

    assert rig.accept().code == 77


@linux_only
def test_a_staging_directory_that_is_not_a_link_but_resolves_outside_ocx_wintest_is_refused(rig: Rig) -> None:
    elsewhere = rig.root / "elsewhere"
    (elsewhere / "repo").mkdir(parents=True)
    (elsewhere / "repo" / "precious.txt").write_text("mine", encoding="utf-8")
    shutil.rmtree(rig.profile / "ocx-wintest")
    (rig.profile / "ocx-wintest").symlink_to(elsewhere)
    (elsewhere / "_wl").mkdir()

    done = rig.accept()

    assert done.code == 77, done.err
    assert (elsewhere / "repo" / "precious.txt").is_file()


@linux_only
@pytest.mark.parametrize("missing", ["ocx-shim.exe", "schema/ocx_schema.exe"])
def test_an_exe_without_its_siblings_fails_naming_the_missing_one_and_stages_nothing(rig: Rig, missing: str) -> None:
    exe = rig.binaries(shim=missing != "ocx-shim.exe", schema=missing != "schema/ocx_schema.exe")
    old = rig.stage / "test" / "bin" / "ocx-shim.exe"
    old.parent.mkdir(parents=True)
    old.write_text("old", encoding="utf-8")

    done = rig.run("--mode", "accept", "--exe", str(exe))

    assert done.code == 66, done.err
    assert missing in done.err
    assert old.read_text(encoding="utf-8") == "old"
    assert not (rig.stage / "test" / "bin" / "ocx.exe").exists()


@linux_only
def test_a_complete_exe_set_replaces_every_staged_binary(rig: Rig) -> None:
    for name in ("ocx.exe", "ocx-shim.exe", "schema/ocx_schema.exe"):
        old = rig.stage / "test" / "bin" / name
        old.parent.mkdir(parents=True, exist_ok=True)
        old.write_text("old", encoding="utf-8")

    done = rig.run("--mode", "accept", "--exe", str(rig.binaries(text="new")))

    assert done.code == 0, done.err
    staged = [
        (rig.stage / "test" / "bin" / name).read_text(encoding="utf-8")
        for name in ("ocx.exe", "ocx-shim.exe", "schema/ocx_schema.exe")
    ]
    assert staged == ["new", "new", "new"]


@linux_only
def test_a_cross_build_that_lacks_a_sibling_fails_the_same_way(rig: Rig) -> None:
    built = rig.repo / "target" / "cross" / TRIPLE / "test-bin"
    built.mkdir(parents=True)
    (built / "ocx.exe").write_text("built", encoding="utf-8")

    done = rig.run("--mode", "accept")

    assert done.code == 66, done.err
    assert "ocx-shim.exe" in done.err
