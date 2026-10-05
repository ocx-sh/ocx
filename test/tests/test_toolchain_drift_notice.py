# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the toolchain drift notice (``.claude/artifacts/plan_update_family.md``).

Every case runs on a real terminal (``run_on_a_terminal``): the background gate skips when
stderr is not a terminal, so over pipes a "no notice, no marker" assertion is green in every
state. Each silent case therefore shows its red control: the same command without the silencing
condition prints the notice and writes the marker. ``ocx status`` reaches the check (outside a
project it exits 64, so no-project cases use ``ocx clean --dry-run``). Drift is a tag moved with
``index=False``, so the stale local index proves the probe reads the live registry; the self check
is off (``OCX_SELF_UPDATE=manual``) so only drift writes under ``state/update-check/``.
"""

from __future__ import annotations

import os
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner
from src.terminal import requires_pty, run_on_a_terminal

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="State-file path assertions and the pty driver use POSIX paths and `bash`.",
)
pytestmark = [pytestmark, pytest.mark.command("status")]

_ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
_MARKER_NAME = re.compile(r"[0-9a-f]{16}")
_HEADER = "ocx: updates available"
# A notice row as the terminal shows it, indentation stripped: `<scope>: <names> — run `<command>``.
_ROW = re.compile(r"(?:project \(.*\)|global): .+ — run `[^`]+`|details: `.+`")
_DAY = 24 * 3600

# Throttle and self-check noise out of the way; the interval is overridden per run.
_BASE_ENV = {"OCX_SELF_UPDATE": "manual"}
_ALWAYS = {"OCX_UPDATE_CHECK_INTERVAL": "0"}
# `status` exits 64 with no `ocx.toml`; `clean --dry-run` is local-only, exits 0 anywhere and reaches the check.
_NO_PROJECT_COMMAND = ("clean", "--dry-run")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run(ocx: OcxRunner, cwd: Path, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        env={**ocx.env, **_BASE_ENV, **(env or {})},
        stdin=subprocess.DEVNULL,
        check=False,
    )


def _ok(result: subprocess.CompletedProcess[str]) -> None:
    assert result.returncode == 0, f"rc={result.returncode}\nstderr:\n{result.stderr}"


def _on_terminal(
    ocx: OcxRunner,
    tmp_path: Path,
    cwd: Path,
    env: dict[str, str] | None = None,
    command: tuple[str, ...] = ("status",),
    stdout: Path | None = None,
) -> str:
    """Runs a check-reaching command with stderr on a pty; returns the terminal text, ANSI stripped.

    The env is explicit: pexpect replaces the environment, so a `CI` set on the
    host running the suite cannot reach the child and trip the CI gate.
    """
    script = tmp_path / f"run-{uuid4().hex[:8]}.sh"
    args = " ".join(shlex.quote(arg) for arg in command)
    sink = shlex.quote(str(stdout)) if stdout else "/dev/null"
    script.write_text(f"{shlex.quote(str(ocx.binary))} {args} >{sink}\n", encoding="utf-8")
    status, terminal = run_on_a_terminal(script, cwd=cwd, env={**ocx.env, **_BASE_ENV, **(env or {})})
    assert status == 0, terminal
    return _ANSI.sub("", terminal)


def _notices(terminal: str) -> list[str]:
    """The notice block's lines: the header, every toolchain row and the closing details line, in order."""
    lines = (line.strip() for line in terminal.splitlines())
    return [line for line in lines if line == _HEADER or _ROW.fullmatch(line)]


def _update_check_files(ocx: OcxRunner) -> list[Path]:
    root = ocx.ocx_home / "state" / "update-check"
    return sorted(path for path in root.rglob("*") if path.is_file()) if root.is_dir() else []


def _markers(ocx: OcxRunner) -> list[Path]:
    """The drift throttle markers: one per project key, plus `global`."""
    root = ocx.ocx_home / "state" / "update-check" / "toolchain"
    return sorted(path for path in root.iterdir() if path.is_file()) if root.is_dir() else []


def _only_marker(ocx: OcxRunner) -> Path:
    markers = _markers(ocx)
    assert len(markers) == 1, f"expected exactly one marker, got {markers}"
    return markers[0]


def _index_snapshot(ocx: OcxRunner) -> dict[str, bytes]:
    root = ocx.ocx_home / "index"
    snapshot = {str(path.relative_to(root)): path.read_bytes() for path in root.rglob("*") if path.is_file()}
    assert snapshot, "fixture must have populated the local index, or an unchanged index proves nothing"
    return snapshot


def _locked_project(ocx: OcxRunner, tmp_path: Path, repo: str, *, name: str = "tool", consent: bool = True) -> Path:
    """A project whose lock pins `repo:3` at 3.0.0, with the tool bound under `name`.

    `ocx lock` itself stamps consent; `consent=False` locks under `OCX_NO_CONSENT`,
    leaving the project as a fresh clone arrives: `ocx.toml` and `ocx.lock`, no stamp.
    """
    make_package(ocx, repo, "3.0.0", tmp_path, cascade=True)
    project = tmp_path / "drift_proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\n{name} = "{ocx.registry}/{repo}:3"\n', encoding="utf-8")
    _ok(_run(ocx, project, "lock", env=None if consent else {"OCX_NO_CONSENT": "1"}))
    return project


def _locked_global(ocx: OcxRunner, tmp_path: Path, repo: str, *, name: str = "tool") -> None:
    """The global toolchain pinned to `repo:3` at 3.0.0; needs no consent."""
    (ocx.ocx_home / "ocx.toml").write_text(f'[tools]\n{name} = "{ocx.registry}/{repo}:3"\n', encoding="utf-8")
    _ok(_run(ocx, tmp_path, "--global", "lock"))


def _move_tag(ocx: OcxRunner, tmp_path: Path, repo: str) -> None:
    """`repo:3` now points at 3.1.0; `index=False` leaves the local index stale."""
    make_package(ocx, repo, "3.1.0", tmp_path, cascade=True, index=False)


def _assert_fires(terminal: str, ocx: OcxRunner, *, name: str = "tool", global_tier: bool = False) -> None:
    """The control: one block (header, one row naming `name` and the run command, the details command)
    and a throttle marker."""
    lines = _notices(terminal)
    assert len(lines) == 3 and lines[0] == _HEADER, f"expected header, row and details, got {lines}\n{terminal}"
    scope = "global" if global_tier else r"project \(.*\)"
    run_command = "ocx --global update" if global_tier else "ocx update"
    assert re.fullmatch(rf"{scope}: {name} — run `{re.escape(run_command)}`", lines[1]), lines[1]
    assert lines[2] == f"details: `{run_command} --check`", lines[2]
    assert _markers(ocx), "a probe must leave a throttle marker"


# ---------------------------------------------------------------------------
# the notice
# ---------------------------------------------------------------------------


@requires_pty
def test_moved_tag_prints_one_notice_naming_the_binding_and_ocx_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The lock pins `3` at 3.0.0 and the tag moved to 3.1.0: one stderr block names the binding,
    the project directory and `ocx update`.

    The notice is read-only: stdout carries none of it, the lock is byte-identical,
    and the local index (stale on purpose, so only a live read can see the move) is
    byte-identical. The marker is keyed by the 16-hex project key.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)
    assert not _markers(ocx), "fixture must start without a marker"
    index_before = _index_snapshot(ocx)
    lock_before = (project / "ocx.lock").read_bytes()
    stdout = tmp_path / "stdout.txt"

    terminal = _on_terminal(ocx, tmp_path, project, stdout=stdout)

    _assert_fires(terminal, ocx)
    line = _notices(terminal)[1]
    assert re.fullmatch(rf"project \(.*{re.escape(project.name)}\): tool — run `ocx update`", line), line
    assert _MARKER_NAME.fullmatch(_only_marker(ocx).name), _markers(ocx)
    assert _HEADER not in stdout.read_text(encoding="utf-8"), "the notice belongs on stderr only"
    assert (project / "ocx.lock").read_bytes() == lock_before, "the notice must never rewrite the lock"
    assert _index_snapshot(ocx) == index_before, "the probe must not write the local index"


# ---------------------------------------------------------------------------
# the throttle
# ---------------------------------------------------------------------------


@requires_pty
def test_second_run_within_the_interval_is_silent_and_a_zero_interval_fires_again(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Inside the window a run is silent and leaves the marker's mtime alone.

    Red controls on the same marker: `OCX_UPDATE_CHECK_INTERVAL=0`, and the
    default interval once the marker is backdated past it, both fire again.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)

    _assert_fires(_on_terminal(ocx, tmp_path, project), ocx)
    marker = _only_marker(ocx)
    first = marker.stat().st_mtime_ns

    assert _notices(_on_terminal(ocx, tmp_path, project)) == [], "a run inside the interval must be silent"
    assert marker.stat().st_mtime_ns == first, "a throttled run must not touch the marker"

    _assert_fires(_on_terminal(ocx, tmp_path, project, _ALWAYS), ocx)
    assert marker.stat().st_mtime_ns > first, "a zero interval must probe, and so touch the marker"

    stamp = time.time() - 2 * _DAY
    os.utime(marker, (stamp, stamp))
    backdated = marker.stat().st_mtime_ns
    _assert_fires(_on_terminal(ocx, tmp_path, project), ocx)
    assert marker.stat().st_mtime_ns > backdated, "past the default interval the same command must probe"


# ---------------------------------------------------------------------------
# policy and kill switch
# ---------------------------------------------------------------------------


@requires_pty
@pytest.mark.parametrize("silencer", ["env_manual", "config_manual", "kill_switch"])
def test_policy_manual_and_the_kill_switch_stay_silent_and_write_no_marker(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, silencer: str
) -> None:
    """`OCX_TOOLCHAIN_UPDATE=manual`, `[update] toolchain = "manual"` and `OCX_NO_UPDATE_CHECK=1`
    each print nothing and leave no state file; the same fixture without the silencer fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)
    config = ocx.ocx_home / "config.toml"
    env = {
        "env_manual": {"OCX_TOOLCHAIN_UPDATE": "manual"},
        "config_manual": {},
        "kill_switch": {"OCX_NO_UPDATE_CHECK": "1"},
    }[silencer]
    if silencer == "config_manual":
        config.write_text('[update]\ntoolchain = "manual"\n', encoding="utf-8")

    silent = _on_terminal(ocx, tmp_path, project, env)

    assert _notices(silent) == [], f"{silencer} must stay silent\n{silent}"
    assert _markers(ocx) == [], f"{silencer} must write no marker"
    if silencer == "kill_switch":
        assert _update_check_files(ocx) == [], "the kill switch must write no state file at all"

    if silencer == "config_manual":
        config.unlink()
    _assert_fires(_on_terminal(ocx, tmp_path, project), ocx)


# ---------------------------------------------------------------------------
# silent cases
# ---------------------------------------------------------------------------


@requires_pty
def test_no_drift_is_silent_but_the_probe_ran(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """With the tag unmoved the run prints nothing, yet the marker proves the probe reached the registry.

    Red control on the same fixture: move the tag, and the next run fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)

    silent = _on_terminal(ocx, tmp_path, project)

    assert _notices(silent) == [], f"no drift must stay silent\n{silent}"
    assert len(_markers(ocx)) == 1, "a clean probe still touches the marker, which is what shows the check ran"

    _move_tag(ocx, tmp_path, repo)
    _assert_fires(_on_terminal(ocx, tmp_path, project, _ALWAYS), ocx)


@requires_pty
def test_stale_lock_is_silent(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """A lock older than `ocx.toml` is not probed: its pins describe a declaration that no longer exists.

    Red control on the same fixture: restoring the original `ocx.toml` makes the lock current and the run fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)
    manifest = project / "ocx.toml"
    original = manifest.read_text(encoding="utf-8")
    manifest.write_text(original + f'extra = "{ocx.registry}/{repo}:3.0.0"\n', encoding="utf-8")

    silent = _on_terminal(ocx, tmp_path, project, _ALWAYS)

    assert _notices(silent) == [], f"a stale lock must stay silent\n{silent}"

    manifest.write_text(original, encoding="utf-8")
    _assert_fires(_on_terminal(ocx, tmp_path, project, _ALWAYS), ocx)


@requires_pty
def test_no_project_is_silent(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """Outside any project there is nothing to probe and nothing is written.

    Red control on the same fixture: the same command from inside the project fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()

    silent = _on_terminal(ocx, tmp_path, elsewhere, command=_NO_PROJECT_COMMAND)

    assert _notices(silent) == [], f"no project must stay silent\n{silent}"
    assert _markers(ocx) == [], "no project means no marker"

    _assert_fires(_on_terminal(ocx, tmp_path, project, command=_NO_PROJECT_COMMAND), ocx)


@requires_pty
def test_non_tty_run_is_silent(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """With stderr on a pipe the check never runs: no notice, no marker.

    Red control on the same fixture: the same command on a terminal fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)

    piped = _run(ocx, project, "status")

    assert piped.returncode == 0, piped.stderr
    assert _HEADER not in piped.stderr, f"a piped run must stay silent\n{piped.stderr}"
    assert _markers(ocx) == [], "a piped run must write no marker"

    _assert_fires(_on_terminal(ocx, tmp_path, project), ocx)


@requires_pty
def test_unconsented_project_is_never_probed(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """A project nobody consented to is not probed, so a cloned repository's lock cannot make
    every ocx command contact a registry host of its choosing. No marker either.

    Red control on the same fixture: after `ocx shell allow` the same command fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo, consent=False)
    _move_tag(ocx, tmp_path, repo)

    silent = _on_terminal(ocx, tmp_path, project, _ALWAYS)

    assert _notices(silent) == [], f"an unconsented project must stay silent\n{silent}"
    assert _markers(ocx) == [], "an unconsented project must leave no state file"

    _ok(_run(ocx, project, "shell", "allow"))
    _assert_fires(_on_terminal(ocx, tmp_path, project, _ALWAYS), ocx)


@requires_pty
@pytest.mark.parametrize("flag", ["--frozen", "--offline"])
def test_frozen_and_offline_runs_are_silent(ocx: OcxRunner, unique_repo: str, tmp_path: Path, flag: str) -> None:
    """`--frozen` and `--offline` forbid discovering a new tag mapping, so the check does not run.

    Red control on the same fixture: the same command without the flag fires.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)

    silent = _on_terminal(ocx, tmp_path, project, _ALWAYS, command=(flag, "status"))

    assert _notices(silent) == [], f"{flag} must stay silent\n{silent}"
    assert _markers(ocx) == [], f"{flag} must write no marker"

    _assert_fires(_on_terminal(ocx, tmp_path, project, _ALWAYS), ocx)


# ---------------------------------------------------------------------------
# the global toolchain
# ---------------------------------------------------------------------------


@requires_pty
def test_global_toolchain_drift_names_ocx_global_update(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """The global toolchain needs no consent and no project: drift names `ocx --global update`
    and throttles under the fixed `global` marker.

    Red control on the same fixture: before the tag moves the same run is silent but still
    writes the marker, which is what shows the probe reached the registry.
    """
    repo = f"{unique_repo}_drift"
    make_package(ocx, repo, "3.0.0", tmp_path, cascade=True)
    _locked_global(ocx, tmp_path, repo)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()

    clean = _on_terminal(ocx, tmp_path, elsewhere, command=_NO_PROJECT_COMMAND)

    assert _notices(clean) == [], f"an undrifted global toolchain must stay silent\n{clean}"
    assert [marker.name for marker in _markers(ocx)] == ["global"], "the global marker proves the probe ran"

    _move_tag(ocx, tmp_path, repo)
    drifted = _on_terminal(ocx, tmp_path, elsewhere, _ALWAYS, command=_NO_PROJECT_COMMAND)

    _assert_fires(drifted, ocx, global_tier=True)
    assert [marker.name for marker in _markers(ocx)] == ["global"]


@requires_pty
def test_global_flag_notices_the_global_toolchain_once(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`--global` selects the global toolchain as the project too; it is probed and printed once."""
    repo = f"{unique_repo}_drift"
    make_package(ocx, repo, "3.0.0", tmp_path, cascade=True)
    _locked_global(ocx, tmp_path, repo)
    _move_tag(ocx, tmp_path, repo)

    terminal = _on_terminal(ocx, tmp_path, tmp_path, command=("--global", "status"))

    _assert_fires(terminal, ocx, global_tier=True)


@requires_pty
def test_project_and_global_drift_print_one_line_each(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """A moved tag under both toolchains prints one block: one header, then one row per file, each
    naming its own binding and its own run command; each file throttles under its own marker.
    """
    repo = f"{unique_repo}_drift"
    project = _locked_project(ocx, tmp_path, repo, name="proj_tool")
    _locked_global(ocx, tmp_path, repo, name="glob_tool")
    _move_tag(ocx, tmp_path, repo)

    terminal = _on_terminal(ocx, tmp_path, project)

    lines = _notices(terminal)
    assert len(lines) == 4 and lines[0] == _HEADER, f"expected header, a row per file, details; got {lines}\n{terminal}"
    project_line, global_line = lines[1], lines[2]
    assert re.fullmatch(r"project \(.*\): proj_tool — run `ocx update`", project_line), project_line
    assert global_line == "global: glob_tool — run `ocx --global update`", global_line
    assert lines[3] == "details: `ocx update --check`, `ocx --global update --check`", lines[3]
    names = sorted(marker.name for marker in _markers(ocx))
    assert len(names) == 2 and "global" in names, f"expected a project marker and `global`, got {names}"
