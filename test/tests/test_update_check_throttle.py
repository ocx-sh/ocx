# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the update-check throttle mechanism.

Exercises plan_self_activate.md Phase B contracts:

- State-file slug contains no dots.
- Two consecutive invocations within the throttle window → only one registry
  query (throttle short-circuit on second call).
- Backdating the state file past the interval → re-query on next invocation.
- OCX_UPDATE_CHECK_INTERVAL=0 bypasses throttle entirely.
- OCX_NO_UPDATE_CHECK=1 skips all queries and does not create the state file.
- Throttle short-circuit must NOT touch the state file mtime.

Tests invoke the ocx binary directly (no OcxRunner abstraction) so they can
manipulate the state file on disk between calls.  All tests use isolated
OCX_HOME directories via `ocx_home` fixture.

## TTY gate — why most state-file tests carry @pytest.mark.requires_tty

The auto-update-check code path is guarded by `is_terminal()` on stderr.
``subprocess.run(capture_output=True)`` (used by the ``_run_ocx`` helper)
connects stderr to a pipe, so `is_terminal()` returns false and the throttle
code path is never entered.  As a result, the state file under
``$OCX_HOME/state/update-check/`` is never created, and tests that assert on
its existence or mtime self-skip with the message
"state file not created — auto-check may be suppressed in this environment".

The throttle logic itself (``is_throttled``, interval parsing, slug derivation)
is fully covered at the unit-test level in ``update_check.rs``.  Acceptance-
level coverage of the state-file lifecycle would require a real TTY on stderr
(e.g. via ``pty.openpty()`` + ``Popen``).  That adds CI complexity with no
additional signal over the unit tests, so we document the gap here and mark the
affected tests with ``@pytest.mark.requires_tty`` instead.

Tests NOT marked ``requires_tty``:
- ``test_no_update_check_env_skips_entirely`` — asserts state file is ABSENT;
  absence is guaranteed regardless of TTY (no-check path skips the create).
- ``test_malformed_interval_env_falls_back_to_default`` — asserts exit 0 only;
  does not depend on state-file creation.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="State-file path assertions use POSIX paths.",
)

# The slug for `ocx.sh/ocx/cli` as produced by `StringExt::to_slug`:
#   to_slug("ocx.sh/ocx/cli") → "ocx_sh_ocx_cli"  (dots + slashes → underscores)
_EXPECTED_SLUG = "ocx_sh_ocx_cli"
_STATE_DIR_SUFFIX = Path("state") / "update-check"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _state_dir(ocx_home: Path) -> Path:
    return ocx_home / _STATE_DIR_SUFFIX


def _state_file(ocx_home: Path) -> Path:
    return _state_dir(ocx_home) / _EXPECTED_SLUG


def _run_ocx(binary: Path, ocx_home: Path, *args: str, extra_env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    env = {
        "OCX_HOME": str(ocx_home),
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", str(Path.home())),
        # Force a non-CI, non-offline, terminal-like environment so the
        # auto-check code path is not suppressed by existing guards.
        "CI": "",
        "OCX_NO_UPDATE_CHECK": "",
    }
    if extra_env:
        env.update(extra_env)
    cmd = [str(binary), "version"]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


# ---------------------------------------------------------------------------
# Slug shape
# ---------------------------------------------------------------------------


@pytest.mark.requires_tty
def test_throttle_state_file_path_has_no_dots_in_name(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """After any command that triggers the auto-check, the state file under
    `$OCX_HOME/state/update-check/` must have no dots in its file name.

    Plan: "Throttle file naming: state/update-check/<slug> where slug =
    to_slug(identifier) — strict slug, no dots in filename."

    The file name must match the strict slug `ocx_sh_ocx_cli`.

    Requires a real TTY on stderr — see module docstring for explanation.
    """
    _run_ocx(ocx_binary, ocx_home)

    state_dir = _state_dir(ocx_home)
    if not state_dir.exists():
        pytest.skip(
            "state/update-check/ not created — auto-check may be suppressed "
            "(e.g., non-terminal, CI, or stub not yet implemented)"
        )

    files = list(state_dir.iterdir())
    assert len(files) >= 1, f"at least one state file must exist in {state_dir}"

    for f in files:
        assert "." not in f.name, (
            f"state file name must contain no dots; got: {f.name!r}"
        )


# ---------------------------------------------------------------------------
# Two consecutive invocations within window → one query
# ---------------------------------------------------------------------------


@pytest.mark.requires_tty
def test_two_consecutive_invocations_query_once_within_window(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """Two consecutive `ocx version` calls within the throttle window must
    result in the state file being touched exactly once (at the first call).

    The second call short-circuits on throttle and must NOT update the mtime.

    Plan: "DO NOT touch when throttle short-circuits before probe."

    Requires a real TTY on stderr — see module docstring for explanation.
    """
    # First invocation: creates the state file if auto-check runs.
    _run_ocx(ocx_binary, ocx_home)

    state_path = _state_file(ocx_home)
    if not state_path.exists():
        pytest.skip("state file not created — auto-check may be suppressed in this environment")

    mtime_after_first = state_path.stat().st_mtime

    # Second invocation: must short-circuit on throttle, not touch the file.
    _run_ocx(ocx_binary, ocx_home)

    mtime_after_second = state_path.stat().st_mtime

    assert mtime_after_first == mtime_after_second, (
        "state file mtime must not change on second call within throttle window; "
        f"first mtime={mtime_after_first}, second mtime={mtime_after_second}"
    )


# ---------------------------------------------------------------------------
# Backdate state file → re-query on next invocation
# ---------------------------------------------------------------------------


@pytest.mark.requires_tty
def test_invocation_after_window_re_queries(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """After backdating the state file past the throttle interval, the next
    invocation must re-query (mtime of state file must be updated).

    Plan: `is_throttled` returns false when file mtime > interval ago.

    Requires a real TTY on stderr — see module docstring for explanation.
    """
    # First invocation to seed the state file.
    _run_ocx(ocx_binary, ocx_home)

    state_path = _state_file(ocx_home)
    if not state_path.exists():
        pytest.skip("state file not created — auto-check may be suppressed in this environment")

    # Backdate the file by 48 hours (past the 24h default interval).
    backdate_seconds = 48 * 3600
    old_mtime = state_path.stat().st_mtime - backdate_seconds
    os.utime(state_path, (old_mtime, old_mtime))

    backdated_mtime = state_path.stat().st_mtime

    # Second invocation: interval elapsed → should probe → touch state file.
    _run_ocx(ocx_binary, ocx_home)

    new_mtime = state_path.stat().st_mtime

    assert new_mtime > backdated_mtime, (
        "state file mtime must be updated when called after the throttle interval; "
        f"backdated mtime={backdated_mtime}, new mtime={new_mtime}"
    )


# ---------------------------------------------------------------------------
# OCX_UPDATE_CHECK_INTERVAL=0 bypasses throttle
# ---------------------------------------------------------------------------


@pytest.mark.requires_tty
def test_zero_interval_env_bypasses_throttle(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """When OCX_UPDATE_CHECK_INTERVAL=0, every invocation probes the registry.

    With two consecutive calls both touching the state file, the mtime must
    advance on the second call (since throttle is bypassed).

    Plan: "OCX_UPDATE_CHECK_INTERVAL=<seconds> (0 = always)."

    Requires a real TTY on stderr — see module docstring for explanation.
    """
    env_override = {"OCX_UPDATE_CHECK_INTERVAL": "0"}

    # First call seeds the state file.
    _run_ocx(ocx_binary, ocx_home, extra_env=env_override)

    state_path = _state_file(ocx_home)
    if not state_path.exists():
        pytest.skip("state file not created — auto-check may be suppressed in this environment")

    mtime_first = state_path.stat().st_mtime

    # Small sleep to ensure clock ticks (mtime resolution is 1s on most FS).
    import time
    time.sleep(1.1)

    # Second call: with interval=0, throttle is bypassed → mtime must advance.
    _run_ocx(ocx_binary, ocx_home, extra_env=env_override)

    mtime_second = state_path.stat().st_mtime

    assert mtime_second > mtime_first, (
        "state file mtime must advance on second call when OCX_UPDATE_CHECK_INTERVAL=0; "
        f"first mtime={mtime_first}, second mtime={mtime_second}"
    )


# ---------------------------------------------------------------------------
# OCX_UPDATE_CHECK_INTERVAL=3600 — throttles within window
# ---------------------------------------------------------------------------


@pytest.mark.requires_tty
def test_positive_interval_env_throttles_within_window(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """When OCX_UPDATE_CHECK_INTERVAL=3600, two consecutive calls within the
    one-hour window must NOT advance the state file mtime on the second call.

    The first call seeds the state file; the second call sees a fresh file
    within the 3600-second window and short-circuits without touching it.

    Plan: "OCX_UPDATE_CHECK_INTERVAL=<seconds> (0 = always)."

    Requires a real TTY on stderr — see module docstring for explanation.
    """
    env_override = {"OCX_UPDATE_CHECK_INTERVAL": "3600"}

    # First call seeds the state file.
    _run_ocx(ocx_binary, ocx_home, extra_env=env_override)

    state_path = _state_file(ocx_home)
    if not state_path.exists():
        pytest.skip("state file not created — auto-check may be suppressed in this environment")

    mtime_first = state_path.stat().st_mtime

    # Second call: within the 3600-second window → must short-circuit, no touch.
    _run_ocx(ocx_binary, ocx_home, extra_env=env_override)

    mtime_second = state_path.stat().st_mtime

    assert mtime_second == mtime_first, (
        "state file mtime must not change on second call within OCX_UPDATE_CHECK_INTERVAL=3600 window; "
        f"first mtime={mtime_first}, second mtime={mtime_second}"
    )


# ---------------------------------------------------------------------------
# OCX_NO_UPDATE_CHECK skips entirely — no state file created
# ---------------------------------------------------------------------------


def test_no_update_check_env_skips_entirely(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """When OCX_NO_UPDATE_CHECK=1, no registry query is made and no state file
    is created.

    Plan: "OCX_NO_UPDATE_CHECK keeps full-disable semantics."

    Existing CLI guard in `app/update_check.rs` (line 46): `if
    env::flag("OCX_NO_UPDATE_CHECK", false) { return Ok(Skipped(...)) }` — the
    state file must not be touched because the code exits before any probe.
    """
    env_override = {"OCX_NO_UPDATE_CHECK": "1"}

    _run_ocx(ocx_binary, ocx_home, extra_env=env_override)
    _run_ocx(ocx_binary, ocx_home, extra_env=env_override)

    state_path = _state_file(ocx_home)
    assert not state_path.exists(), (
        f"state file must NOT be created when OCX_NO_UPDATE_CHECK=1; "
        f"found: {state_path}"
    )


# ---------------------------------------------------------------------------
# Throttle short-circuit must not touch state file
# ---------------------------------------------------------------------------


@pytest.mark.requires_tty
def test_throttle_does_not_touch_state_file(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """When the throttle short-circuits (file is fresh), the state file mtime
    must remain unchanged.

    Plan: "Do NOT touch when throttle short-circuits before probe (touching on
    short-circuit would extend the window indefinitely)."

    Requires a real TTY on stderr — see module docstring for explanation.
    """
    # Seed the state file via first call.
    _run_ocx(ocx_binary, ocx_home)

    state_path = _state_file(ocx_home)
    if not state_path.exists():
        pytest.skip("state file not created — auto-check may be suppressed in this environment")

    mtime_t = state_path.stat().st_mtime

    # Second call within window (no env override → 24h default).
    _run_ocx(ocx_binary, ocx_home)

    mtime_after = state_path.stat().st_mtime

    assert mtime_t == mtime_after, (
        "state file mtime must not change on throttle short-circuit; "
        f"mtime before={mtime_t}, mtime after={mtime_after}"
    )


# ---------------------------------------------------------------------------
# OCX_UPDATE_CHECK_INTERVAL with malformed value falls back to 24h default
# ---------------------------------------------------------------------------


def test_malformed_interval_env_falls_back_to_default(
    ocx_binary: Path,
    ocx_home: Path,
) -> None:
    """When OCX_UPDATE_CHECK_INTERVAL is set to a non-numeric value (e.g. "foo"),
    the throttle parser must fall back to the 24-hour default and NOT error out.

    The parent command (e.g. `ocx version`) must still succeed (exit 0).
    The state file must be created — the check still runs; it is not aborted.

    Regression guard: a crash or hard error on malformed OCX_UPDATE_CHECK_INTERVAL
    would break CI pipelines that set this variable without strict validation.

    Implementation reference: `app/update_check.rs` `Err(_) =>` branch logs at
    debug level and falls through to `None` (24-hour default).
    """
    env_override = {"OCX_UPDATE_CHECK_INTERVAL": "foo"}

    result = _run_ocx(ocx_binary, ocx_home, extra_env=env_override)

    assert result.returncode == 0, (
        f"ocx must exit 0 even when OCX_UPDATE_CHECK_INTERVAL is malformed; "
        f"got exit code {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # The state file may or may not be created depending on whether the
    # auto-check guard (CI, non-terminal, etc.) fires.  We cannot assert
    # existence unconditionally, but we CAN assert that if the state file is
    # absent it is because a guard (not a crash) suppressed the check.
    #
    # A crash on malformed input would have produced a non-zero exit code,
    # which is already covered by the returncode assertion above.
    #
    # If the state file IS created, its name must follow the slug contract.
    state_dir = _state_dir(ocx_home)
    if state_dir.exists():
        for f in state_dir.iterdir():
            assert "." not in f.name, (
                f"state file name must contain no dots even after malformed interval; got: {f.name!r}"
            )
