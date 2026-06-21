# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""All-shell activation matrix: prove ``ocx self setup`` activation survives an
unset ``OCX_HOME`` in every supported login shell.

This is the durable net for a bug class that has regressed repeatedly: the
managed shell block must source ``env.*`` to *locate* ocx, but ``env.*`` is the
file that *sets* ``OCX_HOME`` — so a fresh login shell (where ``OCX_HOME`` is not
yet exported) must not depend on it. A bare ``. "$OCX_HOME/env.sh"`` resolves to
``. "/env.sh"`` and fails on every shell start. This module runs the **real**
activation path per shell and asserts it survives ``OCX_HOME`` unset.

Self-contained on purpose (stdlib + pytest only, no ``src.runner`` / registry
fixtures): the same file runs both under the repo's ``uv run pytest`` on the
host (``shutil.which`` skips shells absent there) and inside the shell-zoo Docker
image (where every shell is present and the whole matrix runs). The ocx binary
is taken from ``$OCX_ACTIVATION_BINARY`` (falls back to ``$OCX_COMMAND``, then
``test/bin/ocx``); if none resolves the module skips, so a host run without a
build stays green.

Per shell the test:

1. seeds an install candidate under an isolated ``$HOME/.ocx`` so the offline
   bootstrap resolves ``already_present`` (no registry needed),
2. runs ``ocx --offline self setup`` targeting that shell's profile / dedicated
   file,
3. launches the shell with ``OCX_HOME`` **unset** and a *clean* environment (no
   ``OCX_*`` leakage from the parent, e.g. a stale ``OCX_HOME``), sourcing the
   managed block twice, and asserts: exit 0, no "No such file"/"not found" for
   ``env.*`` on stderr, the ocx bin dir lands on ``PATH`` (activation actually
   ran), and a second source does not duplicate it (idempotent move-to-front).
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="shell-activation matrix assumes POSIX-family / container shells.",
)

# The install-layout path the bootstrap candidate lives at, relative to OCX_HOME.
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"

# The PATH segment env.* prepends — the bin dir of the `current` install symlink.
_BIN_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin"

# A clean, minimal base PATH so the parent shell's PATH never pre-contains the
# ocx bin dir (which would make the "activation ran" assertion vacuous).
_BASE_PATH = os.pathsep.join(["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"])

# Substrings that signal a missing-file failure when paired with an `env.` ref —
# the exact symptom of the unset-OCX_HOME regression.
_MISSING_FILE_MARKERS = ("no such file", "not found", "cannot find", "does not exist")


def _ocx_binary() -> Path | None:
    """Resolve the ocx binary under test, or ``None`` to skip the module."""
    for key in ("OCX_ACTIVATION_BINARY", "OCX_COMMAND"):
        value = os.environ.get(key)
        if value and Path(value).is_file():
            return Path(value)
    fallback = Path(__file__).resolve().parents[1] / "bin" / ("ocx.exe" if os.name == "nt" else "ocx")
    return fallback if fallback.is_file() else None


_OCX = _ocx_binary()

pytestmark = [
    pytestmark,
    pytest.mark.skipif(_OCX is None, reason="no ocx binary (set OCX_ACTIVATION_BINARY / OCX_COMMAND, or build test/bin/ocx)."),
]


def _seed_candidate(ocx_home: Path, binary: Path) -> None:
    """Place a real ocx binary as the install candidate so offline bootstrap is a no-op."""
    candidate = ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, candidate)
    candidate.chmod(0o755)


def _clean_env(home: Path, shell_abs: str, *, ocx_home: Path | None = None, shell_name: str | None = None) -> dict[str, str]:
    """Build a clean child env: HOME + minimal PATH only, no OCX_* leakage.

    The shell's own directory is appended to PATH so a shell that re-execs a
    helper still resolves it; the ocx bin dir is deliberately NOT present so the
    activation prepend is observable.
    """
    path = _BASE_PATH
    shell_dir = str(Path(shell_abs).parent)
    if shell_dir not in path.split(os.pathsep):
        path = path + os.pathsep + shell_dir
    env = {"HOME": str(home), "PATH": path}
    if ocx_home is not None:
        env["OCX_HOME"] = str(ocx_home)
    if shell_name is not None:
        env["SHELL"] = shell_abs
    return env


def _run_setup(binary: Path, env: dict[str, str], *extra: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(binary), "--offline", "self", "setup", *extra]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def _assert_no_missing_env_error(stderr: str, shell: str) -> None:
    for line in stderr.splitlines():
        low = line.lower()
        if "env." in low and any(marker in low for marker in _MISSING_FILE_MARKERS):
            pytest.fail(f"{shell}: activation reported a missing env.* file (unset-OCX_HOME regression):\n{line}")


def _assert_activation(shell: str, result: subprocess.CompletedProcess[str], bin_seg: str) -> None:
    assert result.returncode == 0, (
        f"{shell}: sourcing the managed block must exit 0; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    _assert_no_missing_env_error(result.stderr, shell)
    segments = [seg for seg in result.stdout.strip().split(os.pathsep) if seg]
    count = segments.count(bin_seg)
    assert count == 1, (
        f"{shell}: the ocx bin dir must appear exactly once on PATH after a double source "
        f"(activation ran + idempotent); found {count} in:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# POSIX-fence shells: sh (dash/ash), bash, zsh
# ---------------------------------------------------------------------------

_POSIX_SHELLS = ["sh", "dash", "ash", "bash", "zsh"]


@pytest.mark.parametrize("shell", _POSIX_SHELLS)
def test_posix_fence_activation_survives_unset_ocx_home(shell: str, tmp_path: Path) -> None:
    """A POSIX shell sourcing the managed fence with OCX_HOME unset activates cleanly."""
    shell_abs = shutil.which(shell)
    if shell_abs is None:
        pytest.skip(f"{shell} not installed on this host")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    _seed_candidate(ocx_home, _OCX)
    bin_seg = str(ocx_home / _BIN_REL)

    profile = home / "profile"
    profile.write_text("# pre-existing user content\n")

    setup = _run_setup(
        _OCX,
        _clean_env(home, shell_abs, ocx_home=ocx_home),
        "--profile",
        str(profile),
    )
    assert setup.returncode == 0, f"{shell}: setup must exit 0; stderr:\n{setup.stderr}"

    # Source the managed block twice with OCX_HOME unset, then print PATH.
    script = f'. "{profile}"; . "{profile}"; printf "%s" "$PATH"'
    result = subprocess.run(
        [shell_abs, "-c", script],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs),
    )
    _assert_activation(shell, result, bin_seg)


# ---------------------------------------------------------------------------
# Dedicated-file / non-POSIX-fence shells: fish, nushell, elvish, pwsh
# ---------------------------------------------------------------------------


def _dedicated_setup(shell: str, shell_abs: str, home: Path, ocx_home: Path) -> Path:
    """Run auto-detect setup for a dedicated-file / elvish / pwsh shell and return its file."""
    _seed_candidate(ocx_home, _OCX)
    setup = _run_setup(_OCX, _clean_env(home, shell_abs, ocx_home=ocx_home, shell_name=shell))
    assert setup.returncode == 0, f"{shell}: setup must exit 0; stderr:\n{setup.stderr}"

    if shell == "fish":
        return home / ".config" / "fish" / "conf.d" / "ocx.fish"
    if shell == "nu":
        return home / ".local" / "share" / "nushell" / "vendor" / "autoload" / "ocx.nu"
    if shell == "elvish":
        return home / ".config" / "elvish" / "rc.elv"
    raise AssertionError(f"unexpected dedicated shell {shell}")


def test_fish_dedicated_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = shutil.which("fish")
    if shell_abs is None:
        pytest.skip("fish not installed on this host")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    bin_seg = str(ocx_home / _BIN_REL)
    conf = _dedicated_setup("fish", shell_abs, home, ocx_home)

    script = f"source '{conf}'; source '{conf}'; string join : $PATH"
    result = subprocess.run(
        [shell_abs, "-c", script],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs),
    )
    _assert_activation("fish", result, bin_seg)


def test_nushell_dedicated_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = shutil.which("nu")
    if shell_abs is None:
        pytest.skip("nushell not installed on this host")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    bin_seg = str(ocx_home / _BIN_REL)
    autoload = _dedicated_setup("nu", shell_abs, home, ocx_home)

    script = f"source '{autoload}'; source '{autoload}'; $env.PATH | str join (char esep)"
    result = subprocess.run(
        [shell_abs, "--no-config-file", "-c", script],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs),
    )
    _assert_activation("nu", result, bin_seg)


def _script_pty_command(script_abs: str, command: list[str]) -> list[str]:
    """Wrap ``command`` in a ``script(1)`` pty invocation, portable across flavors.

    elvish only sources ``rc.elv`` for an *interactive* shell, so it must run on a
    tty; ``script(1)`` supplies one. The two ``script`` flavors disagree on how the
    command is passed, so the invocation cannot be shared verbatim:

    * util-linux (Linux):  ``script -qec "<cmd string>" <typescript-file>``
    * BSD (macOS):         ``script -q <typescript-file> <cmd> [args...]``

    The Linux form keeps the original ``-c`` shape; macOS gets the trailing-argv
    form because BSD ``script`` has no ``-c`` flag.
    """
    if sys.platform == "darwin":
        return [script_abs, "-q", "/dev/null", *command]
    return [script_abs, "-qec", " ".join(command), "/dev/null"]


# Marker the appended elvish probe prints so the PATH line is unambiguous amid any
# terminal-init escapes the interactive pty session emits.
_ELVISH_PATH_PROBE = "OCX_ELVISH_PATH_PROBE:"


def test_elvish_fence_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = shutil.which("elvish")
    if shell_abs is None:
        pytest.skip("elvish not installed on this host")
    script_abs = shutil.which("script")
    if script_abs is None:
        pytest.skip("script(1) is needed to drive elvish in a pty")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    bin_seg = str(ocx_home / _BIN_REL)
    rc = _dedicated_setup("elvish", shell_abs, home, ocx_home)

    # rc.elv is sourced top-to-bottom during elvish's *interactive* startup (its
    # completion block needs the interactive-only `edit:` module), before the REPL
    # begins. Rather than drive the REPL with fed keystrokes — racy across script(1)
    # flavors, since BSD script forwards a closed stdin as an immediate Ctrl-D so the
    # line editor never runs the input — append a PATH probe plus `exit` to the rc.
    # The managed fence above sets PATH; the probe echoes it and elvish exits before
    # any REPL/paste/EOF timing matters. A tty is still required for rc.elv to be
    # sourced at all, so it runs under script(1).
    with rc.open("a", encoding="utf-8") as handle:
        handle.write(f'\necho "{_ELVISH_PATH_PROBE}" $E:PATH\nexit\n')

    env = _clean_env(home, shell_abs)
    env["TERM"] = "dumb"
    result = subprocess.run(
        _script_pty_command(script_abs, [shell_abs, "-rc", str(rc)]),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        env=env,
    )
    combined = result.stdout + result.stderr
    # Scope: prove the unset-OCX_HOME regression is gone — the fence located and
    # ran env.elv (bin dir on PATH) without a missing-file error.
    _assert_no_missing_env_error(combined, "elvish")
    # Regression: the global-env eval now captures the exporter output
    # (`eval (… | slurp)`) instead of piping it (`… | slurp | eval`), so an empty
    # global toolchain no longer raises "arity mismatch" on startup.
    assert "arity mismatch" not in combined, (
        f"elvish: global-env eval must not raise an arity mismatch (pipe-to-eval bug); "
        f"got:\n{combined}"
    )
    # The probe runs only if rc.elv was actually sourced in the pty session.
    assert _ELVISH_PATH_PROBE in result.stdout, (
        f"elvish: the appended rc probe did not run — rc.elv was not sourced; "
        f"got:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert bin_seg in result.stdout, (
        f"elvish: the ocx bin dir must land on PATH after the fence sources env.elv "
        f"with OCX_HOME unset; not found in:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_powershell_fence_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = shutil.which("pwsh")
    if shell_abs is None:
        pytest.skip("pwsh not installed on this host")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    bin_seg = str(ocx_home / _BIN_REL)
    _seed_candidate(ocx_home, _OCX)

    # Detect the host $PROFILE the way `ocx self setup` does, then run setup so it
    # writes the managed PowerShell fence into that file.
    setup = _run_setup(_OCX, _clean_env(home, shell_abs, ocx_home=ocx_home, shell_name="pwsh"))
    assert setup.returncode == 0, f"pwsh: setup must exit 0; stderr:\n{setup.stderr}"
    query = subprocess.run(
        [shell_abs, "-NoProfile", "-NonInteractive", "-Command", "$PROFILE.CurrentUserAllHosts"],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs),
    )
    profile = Path(query.stdout.strip())
    assert profile.is_file(), f"pwsh: setup must write the managed block to $PROFILE ({profile})"

    script = f". '{profile}'; . '{profile}'; $env:PATH"
    result = subprocess.run(
        [shell_abs, "-NoProfile", "-Command", script],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs),
    )
    _assert_activation("pwsh", result, bin_seg)
