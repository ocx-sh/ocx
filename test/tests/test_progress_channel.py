# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Progress paints only while bytes move.

A progress bar is a promise that a transfer is in flight. Every assertion here
is on the bytes a **controlling terminal** received, because that is the only
observer a bar can reach: with stderr piped ocx renders nothing, so a
pipe-based test is green whether or not a bar would have been drawn.

Three properties:

- a fully cached execution writes nothing to the terminal — no spinner around
  the store lookup, no frame that is drawn and cleared in the same tick;
- a real transfer draws the download bar, and `--quiet` (or `OCX_QUIET`)
  suppresses it;
- a shim's `lazy-report` governs the *download* channel: `progress` opens the
  controlling terminal only when a transfer starts, never on a re-entry that
  finds the package in the store, and `silent` keeps the bar off fd 2 even
  when fd 2 is a terminal.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo
from src.helpers import assert_shim_dir_exists, make_package, write_ocx_toml
from src.terminal import requires_pty, run_on_a_terminal

EXIT_SUCCESS = 0

# What a rendered guard looks like: the byte bar's label and the task
# spinners' labels, each with the opening quote of the identifier they wrap.
# The quote is what separates them from the INFO log lines a real transfer
# writes to stderr regardless (`Downloading layer sha256:…`, `Package '…' not
# found locally, pulling.`), which are logging, not progress.
PROGRESS_FRAMES = ("Downloading '", "Resolving '", "Pulling '", "Finding '")

# A consumer-visible `bin/` with no `${installPath}`-rooted constant, so the
# lazy fixtures below raise no advisory that would itself reach the terminal.
PUBLIC_BIN_PATH = [
    {
        "key": "PATH",
        "type": "path",
        "required": True,
        "value": "${installPath}/bin",
        "visibility": "public",
    }
]

pytestmark = requires_pty


def _script(tmp_path: Path, name: str, body: str) -> Path:
    script = tmp_path / f"{name}.sh"
    script.write_text(body)
    return script


def _assert_terminal_untouched(terminal: str, why: str) -> None:
    assert terminal.strip() == "", f"{why}; the pty saw:\n{terminal!r}"


def _assert_no_progress_frame(terminal: str, why: str) -> None:
    drawn = [frame for frame in PROGRESS_FRAMES if frame in terminal]
    assert not drawn, f"{why}; frames {drawn} were drawn, the pty saw:\n{terminal!r}"


def _deferred_launcher(
    ocx: OcxRunner, pkg: PackageInfo, tmp_path: Path, report: str
) -> tuple[Path, Path]:
    """A project deferring `pkg` under `lazy-report = <report>`, and its `hello` launcher.

    `lock --no-pull` leaves the store cold; `env` generates the shim tree. The
    launcher is exec'd by path rather than through `PATH` so a second
    invocation re-enters the shim after the package is materialized — via
    `PATH` the real `bin/` shadows the shim, which is S-004's subject, not
    this file's.
    """
    project = tmp_path / "project"
    project.mkdir()
    write_ocx_toml(
        project,
        f'lazy-mode = "always"\nlazy-report = "{report}"\n[tools]\nhello = "{pkg.fq}"\n',
    )
    lock = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert lock.returncode == EXIT_SUCCESS, f"ocx lock --no-pull failed:\n{lock.stderr}"
    export = subprocess.run(
        [str(ocx.binary), "env", "--shell=sh"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert export.returncode == EXIT_SUCCESS, (
        f"ocx env --shell=sh failed:\n{export.stderr}"
    )
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "the trigger goes through a shim")
    launcher = shim_bin / ("hello.exe" if sys.platform == "win32" else "hello")
    return project, launcher


def _shim_env(ocx: OcxRunner) -> dict[str, str]:
    """The launcher re-enters `"${OCX_BINARY_PIN:-ocx}"`; pin it to the binary under test."""
    return {**ocx.env, "OCX_BINARY_PIN": str(ocx.binary)}


# ---------------------------------------------------------------------------
# Cached execution
# ---------------------------------------------------------------------------


def test_a_cached_exec_paints_nothing_on_the_terminal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package exec` against a package already in the store leaves the terminal untouched.

    stderr stays attached to the pty — that is the condition under which ocx
    renders at all — and only the tool's stdout is redirected, so the terminal
    holds exactly what ocx itself drew.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    warm = subprocess.run(
        [str(ocx.binary), "package", "pull", pkg.fq],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert warm.returncode == EXIT_SUCCESS, f"warm-up pull failed:\n{warm.stderr}"

    out = tmp_path / "exec.out"
    script = _script(
        tmp_path, "exec", f'"{ocx.binary}" package exec {pkg.fq} -- hello >"{out}"\n'
    )
    status, terminal = run_on_a_terminal(script, cwd=tmp_path, env=ocx.env)

    assert status == EXIT_SUCCESS, (
        f"cached exec must succeed; status={status}\nterminal:\n{terminal}"
    )
    assert pkg.marker in out.read_text(), (
        f"the tool must have run; marker={pkg.marker!r}"
    )
    _assert_terminal_untouched(
        terminal, "a cached exec must draw nothing — no spinner, no cleared frame"
    )


# ---------------------------------------------------------------------------
# A real transfer
# ---------------------------------------------------------------------------


def test_a_real_transfer_paints_a_download_bar(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Positive control: the first pull of a package draws the download bar.

    Without this the negative tests above and below could pass by ocx never
    rendering anything at all.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])

    out = tmp_path / "pull.out"
    script = _script(
        tmp_path, "pull", f'"{ocx.binary}" package pull {pkg.fq} >"{out}"\n'
    )
    status, terminal = run_on_a_terminal(script, cwd=tmp_path, env=ocx.env)

    assert status == EXIT_SUCCESS, (
        f"pull must succeed; status={status}\nterminal:\n{terminal}"
    )
    assert "Downloading '" in terminal, (
        f"a real transfer must draw the download bar on a terminal stderr; the pty saw:\n{terminal!r}"
    )


@pytest.mark.parametrize(
    ("argv", "env"),
    [(["--quiet"], {}), ([], {"OCX_QUIET": "1"})],
    ids=["flag", "env"],
)
def test_quiet_suppresses_the_download_bar(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    argv: list[str],
    env: dict[str, str],
) -> None:
    """`--quiet` / `OCX_QUIET` disable progress; the transfer still happens."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])

    out = tmp_path / "pull.out"
    flags = " ".join(argv)
    script = _script(
        tmp_path, "pull", f'"{ocx.binary}" {flags} package pull {pkg.fq} >"{out}"\n'
    )
    status, terminal = run_on_a_terminal(script, cwd=tmp_path, env={**ocx.env, **env})

    assert status == EXIT_SUCCESS, (
        f"quiet pull must succeed; status={status}\nterminal:\n{terminal}"
    )
    _assert_no_progress_frame(terminal, "quiet must suppress the download bar")


# ---------------------------------------------------------------------------
# Shims: `lazy-report` governs the download channel
# ---------------------------------------------------------------------------


def test_a_cached_shim_invocation_opens_no_terminal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`lazy-report = progress` renders the first materialization and nothing on a re-entry.

    Both standard streams are redirected, so the only route to the pty is the
    controlling terminal the `progress` arm opens. The first invocation is the
    control — the download bar reaches the pty — and the second, which finds
    the package in the store, must open nothing.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["hello"],
        binaries=["hello"],
        env=PUBLIC_BIN_PATH,
    )
    project, launcher = _deferred_launcher(ocx, pkg, tmp_path, report="progress")

    first = _script(
        tmp_path,
        "first",
        f'"{launcher}" >"{tmp_path / "first.out"}" 2>"{tmp_path / "first.err"}"\n',
    )
    status, terminal = run_on_a_terminal(first, cwd=project, env=_shim_env(ocx))
    assert status == EXIT_SUCCESS, (
        f"the first trigger must materialize; status={status}\n"
        f"stderr:\n{(tmp_path / 'first.err').read_text()}"
    )
    assert pkg.repo in terminal, (
        f"control: the first invocation downloads and must render on the controlling terminal; "
        f"the pty saw:\n{terminal!r}"
    )

    second = _script(
        tmp_path,
        "second",
        f'"{launcher}" >"{tmp_path / "second.out"}" 2>"{tmp_path / "second.err"}"\n',
    )
    status, terminal = run_on_a_terminal(second, cwd=project, env=_shim_env(ocx))
    assert status == EXIT_SUCCESS, (
        f"the re-entry must succeed; status={status}\nstderr:\n{(tmp_path / 'second.err').read_text()}"
    )
    assert pkg.marker in (tmp_path / "second.out").read_text(), (
        "the tool must have run on the re-entry"
    )
    _assert_terminal_untouched(
        terminal,
        "a re-entry that finds the package in the store must open no controlling terminal",
    )


def test_silent_report_paints_nothing_even_when_stderr_is_the_terminal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`lazy-report = silent` keeps the download bar off fd 2, terminal or not.

    A shim runs inside another tool's process tree and fd 2 belongs to whoever
    invoked it — `silent` means the transfer renders nowhere, which a stderr
    that happens to be a terminal must not override.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["hello"],
        binaries=["hello"],
        env=PUBLIC_BIN_PATH,
    )
    project, launcher = _deferred_launcher(ocx, pkg, tmp_path, report="silent")

    out = tmp_path / "trigger.out"
    script = _script(tmp_path, "trigger", f'"{launcher}" >"{out}"\n')
    status, terminal = run_on_a_terminal(script, cwd=project, env=_shim_env(ocx))

    assert status == EXIT_SUCCESS, (
        f"the trigger must materialize; status={status}\nterminal:\n{terminal}"
    )
    assert pkg.marker in out.read_text(), "the tool must have run"
    _assert_no_progress_frame(
        terminal, "lazy-report=silent must draw no bar on a terminal stderr"
    )
