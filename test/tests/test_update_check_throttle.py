# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the update-check throttle mechanism.

Exercises the throttle contracts on a real terminal:

- The state-file name contains no dots.
- Two invocations within the window probe once (the second leaves the state
  file's mtime alone).
- Backdating the state file past the interval re-probes.
- OCX_UPDATE_CHECK_INTERVAL=0 bypasses the throttle, 3600 keeps it.

The background gate skips when stderr is not a terminal, so these cases run on
a pty (``run_on_a_terminal``) with a command that reaches the check:
``ocx clean --dry-run``. ``version`` is in the skip list and never did, which is
why the earlier pipe-based versions of these tests always skipped themselves.
The self check is redirected to a loopback repository with the
``__OCX_SELF_IMAGE`` seam, so no case reaches the public registry; the probe
touches its state file whether or not that repository exists.

The kill switch and a malformed interval are covered on a terminal in
``test_update_config.py``.
"""

from __future__ import annotations

import os
import shlex
import sys
import time
from pathlib import Path
from uuid import uuid4

import pytest

from src.runner import OcxRunner
from src.terminal import requires_pty, run_on_a_terminal

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="State-file path assertions use POSIX paths.",
)

_STATE_DIR_SUFFIX = Path("state") / "update-check"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _state_dir(ocx_home: Path) -> Path:
    return ocx_home / _STATE_DIR_SUFFIX


def _state_files(ocx: OcxRunner) -> list[Path]:
    directory = _state_dir(ocx.ocx_home)
    if not directory.is_dir():
        return []
    return sorted(path for path in directory.iterdir() if path.is_file())


def _probe_on_terminal(
    ocx: OcxRunner, tmp_path: Path, self_repo: str, extra_env: dict[str, str] | None = None
) -> None:
    """One `ocx clean --dry-run` with stderr on a pty, asserted to exit 0."""
    script = tmp_path / f"run-{uuid4().hex[:8]}.sh"
    script.write_text(f"{shlex.quote(str(ocx.binary))} clean --dry-run >/dev/null\n")
    env = {**ocx.env, "__OCX_SELF_IMAGE": f"{ocx.registry}/{self_repo}", **(extra_env or {})}
    status, terminal = run_on_a_terminal(script, cwd=tmp_path, env=env)
    assert status == 0, terminal


def _seed(ocx: OcxRunner, tmp_path: Path, self_repo: str, extra_env: dict[str, str] | None = None) -> Path:
    """First probe on a fresh home: no state file before, exactly one after."""
    assert not _state_files(ocx), "fixture must start without a state file"
    _probe_on_terminal(ocx, tmp_path, self_repo, extra_env)
    files = _state_files(ocx)
    assert len(files) == 1, f"the probe must leave exactly one state file; got {files}"
    return files[0]


# ---------------------------------------------------------------------------
# Slug shape
# ---------------------------------------------------------------------------


@requires_pty
def test_throttle_state_file_path_has_no_dots_in_name(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """The state file name is a strict slug of the identifier: no dots, slashes or colons.

    The identifier here carries a dot in its repository name, so a name that kept
    it verbatim would fail where a slug of `ocx.sh/ocx/cli` alone would not.
    """
    repo = f"{unique_repo}.dotted"
    state = _seed(ocx, tmp_path, repo)

    assert "." not in state.name, f"state file name must contain no dots; got {state.name!r}"
    assert "/" not in state.name and ":" not in state.name, state.name
    assert "dotted" in state.name, f"the name must derive from the identifier; got {state.name!r}"


# ---------------------------------------------------------------------------
# Two consecutive invocations within window -> one probe
# ---------------------------------------------------------------------------


@requires_pty
def test_two_consecutive_invocations_query_once_within_window(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The second run inside the window short-circuits and leaves the mtime alone.

    Red control on the same fixture: once the file is backdated past the window
    the very same command moves the mtime, so an unchanged mtime above is the
    throttle and not a run that never reached the check.
    """
    repo = f"{unique_repo}_self"
    state = _seed(ocx, tmp_path, repo)
    after_first = state.stat().st_mtime_ns

    _probe_on_terminal(ocx, tmp_path, repo)
    assert state.stat().st_mtime_ns == after_first, "a run inside the window must not touch the state file"

    stamp = time.time() - 48 * 3600
    os.utime(state, (stamp, stamp))
    backdated = state.stat().st_mtime_ns
    _probe_on_terminal(ocx, tmp_path, repo)
    assert state.stat().st_mtime_ns != backdated, "control: past the window the same command must probe"


# ---------------------------------------------------------------------------
# Backdate state file -> re-probe on next invocation
# ---------------------------------------------------------------------------


@requires_pty
def test_invocation_after_window_re_queries(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """Backdating the state file past the 1 d default makes the next run touch it again."""
    repo = f"{unique_repo}_self"
    state = _seed(ocx, tmp_path, repo)

    stamp = time.time() - 48 * 3600
    os.utime(state, (stamp, stamp))
    backdated = state.stat().st_mtime_ns

    _probe_on_terminal(ocx, tmp_path, repo)

    assert state.stat().st_mtime_ns > backdated, "the mtime must advance once the interval has elapsed"


# ---------------------------------------------------------------------------
# OCX_UPDATE_CHECK_INTERVAL=0 bypasses throttle; 3600 keeps it
# ---------------------------------------------------------------------------


@requires_pty
def test_zero_interval_env_bypasses_throttle(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """With `OCX_UPDATE_CHECK_INTERVAL=0` a state file a minute old is probed again.

    Control on the same state file: `3600` leaves it alone.
    """
    repo = f"{unique_repo}_self"
    state = _seed(ocx, tmp_path, repo)

    stamp = time.time() - 60
    os.utime(state, (stamp, stamp))
    backdated = state.stat().st_mtime_ns
    _probe_on_terminal(ocx, tmp_path, repo, {"OCX_UPDATE_CHECK_INTERVAL": "3600"})
    assert state.stat().st_mtime_ns == backdated, "control: a 3600 s window must throttle a one-minute-old file"

    _probe_on_terminal(ocx, tmp_path, repo, {"OCX_UPDATE_CHECK_INTERVAL": "0"})
    assert state.stat().st_mtime_ns > backdated, "interval 0 must probe on every command"


@requires_pty
def test_positive_interval_env_throttles_within_window(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`OCX_UPDATE_CHECK_INTERVAL=3600`: two runs inside the hour probe once.

    Control on the same file: backdated two hours, the same setting probes.
    """
    repo = f"{unique_repo}_self"
    env = {"OCX_UPDATE_CHECK_INTERVAL": "3600"}
    state = _seed(ocx, tmp_path, repo, env)
    first = state.stat().st_mtime_ns

    _probe_on_terminal(ocx, tmp_path, repo, env)
    assert state.stat().st_mtime_ns == first, "inside the 3600 s window the second run must not touch the file"

    stamp = time.time() - 2 * 3600
    os.utime(state, (stamp, stamp))
    backdated = state.stat().st_mtime_ns
    _probe_on_terminal(ocx, tmp_path, repo, env)
    assert state.stat().st_mtime_ns > backdated, "control: past the 3600 s window the same setting must probe"


# ---------------------------------------------------------------------------
# Throttle short-circuit must not touch state file
# ---------------------------------------------------------------------------


@requires_pty
def test_throttle_does_not_touch_state_file(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """A throttled run leaves the mtime exactly as it was.

    Touching on a short-circuit would extend the window indefinitely. Control on
    the same file: backdated past the window, the same command moves the mtime,
    so the unchanged mtime is the short-circuit and not a run that skipped the check.
    """
    repo = f"{unique_repo}_self"
    state = _seed(ocx, tmp_path, repo)
    stamp = time.time() - 3600
    os.utime(state, (stamp, stamp))
    pinned = state.stat().st_mtime_ns

    _probe_on_terminal(ocx, tmp_path, repo)
    assert state.stat().st_mtime_ns == pinned, "the throttle short-circuit must not touch the state file"

    stamp = time.time() - 48 * 3600
    os.utime(state, (stamp, stamp))
    backdated = state.stat().st_mtime_ns
    _probe_on_terminal(ocx, tmp_path, repo)
    assert state.stat().st_mtime_ns != backdated, "control: past the window the same command must probe"
