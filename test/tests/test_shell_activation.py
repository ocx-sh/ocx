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

Self-contained on purpose (stdlib + pytest + the stdlib-only ``shell_matrix``
helper, no ``src.runner`` / registry fixtures): the same file runs both under
the repo's ``uv run pytest`` on the host (``shutil.which`` skips shells absent
there) and inside the shell-zoo Docker image (where every shell is present and
the whole matrix runs). ``shell_matrix`` is imported as a top-level module,
which is why every runner puts it beside this file — ``test/pyproject.toml``'s
``pythonpath`` on the host, a bind mount in the zoo, a flat copy on the macOS
leg. The ocx binary is taken from ``$OCX_ACTIVATION_BINARY`` (falls back to
``$OCX_COMMAND``, then ``test/bin/ocx``); if none resolves the module skips, so
a host run without a build stays green.

Where an interpreter or tool IS expected to exist — the zoo images, a CI leg
that installs it — ``__OCX_TESTING_REQUIRE_LIVE_SHELLS`` turns "absent → skip"
into a failure, because a skip and a pass carry the same evidence and only the
first is honest. Everything here goes through :func:`_require`, so that holds
for every row rather than for the rows somebody remembered.

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
from dataclasses import dataclass
from pathlib import Path

import pytest

import shell_matrix as matrix

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


def _require(binary: str) -> str:
    """Absolute path of ``binary``, or skip naming exactly what is missing.

    The skip is a **failure** wherever ``__OCX_TESTING_REQUIRE_LIVE_SHELLS``
    names it: on an image that ships the tool, "skipped" and "passed" carry the
    same evidence, and a regression gate that vanishes with its interpreter is
    the shape this module exists to refuse. Off by default, so a developer host
    that has three of the nine shells still runs the three.
    """
    resolved = shutil.which(binary)
    if resolved is None:
        assert not matrix.missing_tool_is_fatal(binary), (
            f"{binary} is not installed, so this test asserted nothing — and "
            "__OCX_TESTING_REQUIRE_LIVE_SHELLS names it as one that must be live here"
        )
        pytest.skip(f"{binary} is not installed on this host (shutil.which returned None)")
    return resolved


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
    return subprocess.run(cmd, capture_output=True, text=True, env=env, check=False)


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
    shell_abs = _require(shell)

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
        env=_clean_env(home, shell_abs), check=False,
    )
    _assert_activation(shell, result, bin_seg)


def test_dash_login_activates_via_dot_profile_when_bash_profile_exists(tmp_path: Path) -> None:
    """A skel shipping ``~/.bash_profile`` must still activate a dash/ksh login.

    dash/ksh/sh login shells read ``~/.profile`` and never ``~/.bash_profile``.
    When auto-detect setup runs on a home that already ships ``~/.bash_profile``
    (e.g. the Fedora skel), the managed fence must land in ``~/.profile`` too, or
    a dash login never activates ocx. Auto-detect (no ``--profile``) is used on
    purpose so the profile-target detection is exercised end-to-end.
    """
    shell_abs = _require("dash")

    home = tmp_path / "home"
    home.mkdir()
    # Simulate a skel that ships ~/.bash_profile (the case that stranded dash).
    (home / ".bash_profile").write_text("# pre-existing bash_profile\n")
    ocx_home = home / ".ocx"
    _seed_candidate(ocx_home, _OCX)
    bin_seg = str(ocx_home / _BIN_REL)

    setup = _run_setup(_OCX, _clean_env(home, shell_abs, ocx_home=ocx_home, shell_name="dash"))
    assert setup.returncode == 0, f"dash: setup must exit 0; stderr:\n{setup.stderr}"

    profile = home / ".profile"
    assert profile.is_file(), (
        "auto-detect setup must write the managed fence to ~/.profile so dash/ksh "
        "login shells (which never read ~/.bash_profile) activate ocx"
    )

    # A dash login sourcing ~/.profile twice with OCX_HOME unset must activate
    # cleanly and idempotently.
    script = f'. "{profile}"; . "{profile}"; printf "%s" "$PATH"'
    result = subprocess.run(
        [shell_abs, "-c", script],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs), check=False,
    )
    _assert_activation("dash", result, bin_seg)


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
    shell_abs = _require("fish")

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
        env=_clean_env(home, shell_abs), check=False,
    )
    _assert_activation("fish", result, bin_seg)


def test_nushell_dedicated_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = _require("nu")

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
        env=_clean_env(home, shell_abs), check=False,
    )
    _assert_activation("nu", result, bin_seg)


# Marker the appended elvish probe prints so the PATH line is unambiguous amid any
# terminal-init escapes the interactive pty session emits.
_ELVISH_PATH_PROBE = "OCX_ELVISH_PATH_PROBE:"


def test_elvish_fence_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = _require("elvish")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    bin_seg = str(ocx_home / _BIN_REL)
    rc = _dedicated_setup("elvish", shell_abs, home, ocx_home)

    # rc.elv is sourced top-to-bottom during elvish's *interactive* startup (its
    # completion block needs the interactive-only `edit:` module), before the REPL
    # begins. Rather than drive the REPL with fed keystrokes, append a PATH probe
    # plus `exit` to the rc: the managed fence above sets PATH, the probe echoes
    # it, and elvish exits before any REPL timing matters. A tty is still required
    # for rc.elv to be sourced at all, which is what the pty supplies.
    with rc.open("a", encoding="utf-8") as handle:
        handle.write(f'\necho "{_ELVISH_PATH_PROBE}" $E:PATH\nexit\n')

    env = _clean_env(home, shell_abs)
    env["TERM"] = "dumb"
    combined = matrix.pty_session([shell_abs, "-rc", str(rc)], [], cwd=home, env=env, timeout=60)
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
    assert _ELVISH_PATH_PROBE in combined, (
        f"elvish: the appended rc probe did not run — rc.elv was not sourced; "
        f"session output:\n{combined}"
    )
    assert bin_seg in combined, (
        f"elvish: the ocx bin dir must land on PATH after the fence sources env.elv "
        f"with OCX_HOME unset; not found in:\n{combined}"
    )


# The two verdicts every hook probe prints, and the PATH line that keeps the
# negative arm from passing in the "activation never ran" state.
_HOOK_PRESENT = "OCX_HOOK_PRESENT"
_HOOK_ABSENT = "OCX_HOOK_ABSENT"
_HOOK_PATH_PROBE = "OCX_HOOK_PATH:"


@dataclass(frozen=True)
class _HookArm:
    """One shell's spelling of "is a per-prompt hook registered right now?".

    The probe asks the **running shell** what it has, never what the shim
    emitted: the shims are free to change how they say `--interactive`, and a
    test pinned to their text would go red for a non-reason.
    """

    binary: str
    """Interpreter looked up with ``shutil.which``."""

    setup_shell: str
    """``$SHELL`` basename `ocx self setup` detects the profile targets from."""

    profile: str
    """Startup file, relative to ``$HOME``, an interactive login shell reads
    LAST — where the probe is appended so it observes post-activation state.
    Empty for pwsh, whose ``$PROFILE`` is asked of the host at run time."""

    login: tuple[str, ...]
    """Argv tail for an interactive login shell (driven on a pty)."""

    quiet: tuple[str, ...]
    """Argv tail for the tty-less control; the probe is appended to it."""

    probe: str
    """Prints one verdict line plus the ``OCX_HOOK_PATH:`` line."""

    exit_command: str = "exit"
    """How this shell's startup file ends the **session**, not just itself.

    Every arm appends the probe plus this line to a startup file, so the pty
    session has to terminate on its own. In every POSIX shell and in fish,
    ``exit`` in an rc file exits the shell. PowerShell does not: ``exit`` inside
    ``$PROFILE`` leaves the profile *script* and drops the host into its
    interactive REPL, which then waits for input forever. That is why pwsh
    spells it ``[Environment]::Exit(0)``.

    Before `pty_session` raised on timeout, the pwsh arm reached its prompt and
    was killed at the deadline; the assertions then ran over a partial
    transcript that happened to contain the markers, so the row passed while
    proving nothing about the session ever ending.
    """


# `command -v` finds a function in bash and zsh; fish needs `functions -q`; pwsh
# names its hook `__ocxReconcile` (its `prompt` wrapper calls through to it) and
# tests for it as a function-drive path.
_POSIX_HOOK_PROBE = (
    f"if command -v __ocx_prompt_hook >/dev/null 2>&1; then echo {_HOOK_PRESENT}; else echo {_HOOK_ABSENT}; fi; "
    f'printf "{_HOOK_PATH_PROBE}%s\\n" "$PATH"'
)
_FISH_HOOK_PROBE = (
    f"if functions -q __ocx_prompt_hook; echo {_HOOK_PRESENT}; else; echo {_HOOK_ABSENT}; end; "
    f'printf "{_HOOK_PATH_PROBE}%s\\n" (string join : $PATH)'
)
_PWSH_HOOK_PROBE = (
    f"if (Test-Path function:global:__ocxReconcile) {{ Write-Output '{_HOOK_PRESENT}' }} "
    f"else {{ Write-Output '{_HOOK_ABSENT}' }}; "
    f"Write-Output ('{_HOOK_PATH_PROBE}' + $env:PATH)"
)

_HOOK_ARMS: dict[str, _HookArm] = {
    # bash reads ~/.profile on login (no ~/.bash_profile in a fresh arena home).
    "bash": _HookArm("bash", "bash", ".profile", ("-l", "-i"), ("-l", "-c"), _POSIX_HOOK_PROBE),
    # zsh: ~/.zprofile on login, ~/.zshrc additionally when interactive — so the
    # interactive arm probes ~/.zshrc and the control still sources ~/.zprofile.
    "zsh": _HookArm("zsh", "zsh", ".zshrc", ("-l", "-i"), ("-l", "-c"), _POSIX_HOOK_PROBE),
    # fish reads conf.d for every session, interactive or not.
    "fish": _HookArm("fish", "fish", ".config/fish/conf.d/ocx.fish", ("-l", "-i"), ("-l", "-c"), _FISH_HOOK_PROBE),
    # pwsh has no `-l`/`-i`: bare `pwsh` IS the interactive REPL, and `-Command`
    # is the non-interactive form. Both load $PROFILE (no `-NoProfile` here).
    "pwsh": _HookArm(
        "pwsh", "pwsh", "", ("-NoLogo",), ("-NoLogo", "-Command"), _PWSH_HOOK_PROBE, "[Environment]::Exit(0)"
    ),
}


def _pwsh_profile(shell_abs: str, home: Path) -> Path:
    """Ask the pwsh host for ``$PROFILE.CurrentUserAllHosts``, as setup does."""
    query = subprocess.run(
        [shell_abs, "-NoProfile", "-NonInteractive", "-Command", "$PROFILE.CurrentUserAllHosts"],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs), check=False,
    )
    return Path(query.stdout.strip())


def _hook_arm_home(arm: _HookArm, shell_abs: str, tmp_path: Path, name: str) -> tuple[Path, Path, Path]:
    """A fresh arena home with the managed block installed; returns its paths.

    Each arm of the test gets its own home so the two runs cannot influence one
    another through an appended probe, in either order.
    """
    home = tmp_path / name
    home.mkdir()
    ocx_home = home / ".ocx"
    _seed_candidate(ocx_home, _OCX)
    setup = _run_setup(_OCX, _clean_env(home, shell_abs, ocx_home=ocx_home, shell_name=arm.setup_shell))
    assert setup.returncode == 0, f"{arm.binary}: setup must exit 0; stderr:\n{setup.stderr}"
    profile = _pwsh_profile(shell_abs, home) if arm.binary == "pwsh" else home / arm.profile
    assert profile.is_file(), f"{arm.binary}: setup must write the managed block to {profile}"
    return home, ocx_home, profile


def _hook_verdict(transcript: str) -> str | None:
    """The last verdict line in ``transcript``, ignoring the shell's own echo.

    A pty session echoes the probe's source text, which contains both verdict
    words, so only a line that is *exactly* one of them counts.
    """
    verdicts = [line.strip() for line in transcript.splitlines() if line.strip() in (_HOOK_PRESENT, _HOOK_ABSENT)]
    return verdicts[-1] if verdicts else None


@pytest.mark.parametrize("shell", sorted(_HOOK_ARMS))
def test_interactive_login_shell_registers_the_prompt_hook(shell: str, tmp_path: Path) -> None:
    """The install path must actually register the per-prompt hook.

    This is the gate the whole per-prompt feature was missing. Every shim invokes
    ``self activate`` inside a command substitution **and** redirects its stderr
    (``2>/dev/null``), so a stderr-only interactivity probe answered ``false`` in
    every real shell and the hook's ``auto`` rung resolved to off — no shell ever
    registered a hook through the shipped install, while every test that passed
    ``--hook`` explicitly stayed green. Nothing observed that, because nothing
    drove the *shim*.

    That root cause answered ``false`` in **every** shell, so a bash-only gate
    would have the same blind spot the defect shipped through: the row runs on
    every arm whose shim registers a hook. (Elvish's shim-driven arm is
    ``test_elvish_prompt_hook_fires_on_cd_and_stays_quiet_otherwise``, which is
    strictly stronger — it drives the registered hook until it fires.)

    Both colours, on inputs this test controls and in two independent homes: an
    interactive login shell on a pty must have the hook; the same managed block
    read by a shell with no terminal must not. The negative arm also asserts the
    ocx bin dir IS on ``PATH``, so "absent" cannot be satisfied by a managed
    block that did nothing at all.

    The probe interrogates the live shell, never the emitted text: a shim that
    starts stating ``--interactive`` explicitly changes what it emits and
    nothing here.
    """
    arm = _HOOK_ARMS[shell]
    shell_abs = _require(arm.binary)

    # --- interactive login shell on a pty: the hook must be registered --------
    home, _, profile = _hook_arm_home(arm, shell_abs, tmp_path, "home_tty")
    # Appending the probe to the profile rather than typing it at the prompt is
    # what keeps this arm shell-agnostic: no line editor, no keystroke timing,
    # and the observation point is exactly "after the managed block ran".
    with profile.open("a", encoding="utf-8") as handle:
        handle.write(f"\n{arm.probe}\n{arm.exit_command}\n")
    env = _clean_env(home, shell_abs)
    env["TERM"] = "dumb"
    transcript = matrix.pty_session([shell_abs, *arm.login], [], cwd=home, env=env, timeout=60)
    assert _hook_verdict(transcript) == _HOOK_PRESENT, (
        f"{shell}: an interactive login shell must register the per-prompt hook through the "
        f"managed profile; saw {_hook_verdict(transcript)!r}\nsession output:\n{transcript}"
    )

    # --- no terminal anywhere: the auto rung must resolve to off --------------
    # Without this, "present" could be an unconditional registration rather than
    # the auto rung answering correctly.
    quiet_home, quiet_ocx_home, _ = _hook_arm_home(arm, shell_abs, tmp_path, "home_quiet")
    quiet = subprocess.run(
        [shell_abs, *arm.quiet, arm.probe],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        env=_clean_env(quiet_home, shell_abs), check=False,
    )
    output = quiet.stdout + quiet.stderr
    # Load-bearing on its own: a managed block that failed outright also prints
    # no hook, so "absent" only means something once activation demonstrably ran.
    bin_seg = str(quiet_ocx_home / _BIN_REL)
    path_lines = [line for line in output.splitlines() if _HOOK_PATH_PROBE in line]
    assert path_lines and bin_seg in path_lines[-1], (
        f"{shell}: the managed block must still put the ocx bin dir ({bin_seg}) on PATH without a "
        f"terminal — otherwise the absent-hook verdict below is satisfied by activation never "
        f"running;\nstdout:\n{quiet.stdout}\nstderr:\n{quiet.stderr}"
    )
    assert _hook_verdict(output) == _HOOK_ABSENT, (
        f"{shell}: a shell with no terminal must not register a prompt hook; "
        f"saw {_hook_verdict(output)!r}\nstdout:\n{quiet.stdout}\nstderr:\n{quiet.stderr}"
    )


# Markers the elvish prompt-hook session prints. Distinctive so they survive the
# terminal-init escapes an interactive pty emits around them.
_ELVISH_FIRE_PROBE = "OCX_ELVISH_FIRES:"

# The counting shim the elvish hook session puts at the resolved binary path.
# Counting by file append rather than by inspecting a process list is
# deliberate: a detector that can match its own invocation answers the same in
# every state.
_ELVISH_COUNTER_SHIM = """#!/bin/sh
printf 'x' >> {counter}
exec {real} "$@"
"""


def test_elvish_prompt_hook_fires_on_cd_and_stays_quiet_otherwise(tmp_path: Path) -> None:
    """C-019 member 7 on elvish: ``cd`` reconciles, a still prompt does not.

    Elvish's guard is the one arm with **no watch-set term** — elvish 0.21 has no
    in-shell mtime — so ``$pwd`` is doing all the work here and this is the test
    that says whether it does. Red state: drop ``(!=s $E:__OCX_ENV_PWD (to-string $pid)' '$pwd)``
    from ``elvish_registration``'s guard and the counts go ``1 1 1`` instead of
    ``1 1 2`` — entering a different project never applies its environment, which
    is the regression ocx-sh/ocx#341 was filed for.

    The hook rides ``$edit:before-readline``, which fires only when the *line
    editor* runs, so unlike the other elvish tests in this module the REPL has to
    be driven with real keystrokes through the pty rather than by appending to
    ``rc.elv``.
    """
    shell_abs = _require("elvish")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    rc = _dedicated_setup("elvish", shell_abs, home, ocx_home)

    # The hook invokes the resolved absolute binary (C-045), which is the
    # candidate seeded by `_dedicated_setup`. Replacing it with a counting shim
    # that hands over to the real binary makes every hook fire observable
    # without changing what the fire does.
    counter = tmp_path / "fires"
    counter.write_bytes(b"")
    real = tmp_path / "ocx-real"
    candidate = ocx_home / _CANDIDATE_REL
    shutil.move(str(candidate), str(real))
    candidate.write_text(
        _ELVISH_COUNTER_SHIM.format(counter=_sh_quote(str(counter)), real=_sh_quote(str(real))),
        encoding="utf-8",
    )
    candidate.chmod(0o755)

    alpha = tmp_path / "alpha"
    beta = tmp_path / "beta"
    alpha.mkdir()
    beta.mkdir()

    # rc.elv itself invokes ocx (the activation stream), so the counter is
    # truncated at the end of rc — after startup, before the first prompt — and
    # counts prompt fires only.
    with rc.open("a", encoding="utf-8") as handle:
        handle.write(f"\ncd {_elvish_quote(str(alpha))}\nprint '' > {_elvish_quote(str(counter))}\n")

    session = [
        # Fire 1: the recorded directory is unset, so the first prompt
        # reconciles — C-044's own first-prompt boundary.
        f"echo {_ELVISH_FIRE_PROBE} A (wc -c < {_elvish_quote(str(counter))})",
        # Nothing moved: the guard must not exec.
        f"echo {_ELVISH_FIRE_PROBE} B (wc -c < {_elvish_quote(str(counter))})",
        f"cd {_elvish_quote(str(beta))}",
        # The prompt after the `cd` must reconcile.
        f"echo {_ELVISH_FIRE_PROBE} C (wc -c < {_elvish_quote(str(counter))})",
    ]

    env = _clean_env(home, shell_abs)
    env["TERM"] = "dumb"
    combined = matrix.pty_session([shell_abs, "-i"], session, cwd=home, env=env, timeout=120)
    observed = _elvish_probe_counts(combined)
    assert observed == ["1", "1", "2"], (
        f"elvish: prompt 1 must fire on the unset recorded directory, prompt 2 must be quiet, and "
        f"`cd` must fire prompt 3 (C-019 member 7); saw {observed}\nsession output:\n{combined}"
    )


def _elvish_probe_counts(output: str) -> list[str]:
    """The three fire counts the session printed, in order.

    The pty interleaves terminal escapes and the echoed input line with the
    output line, so the marker is searched for on a line that is NOT the echo:
    the echoed one still carries the unexpanded `(wc -c ...)` call.
    """
    counts = []
    for line in output.splitlines():
        if _ELVISH_FIRE_PROBE not in line or "wc -c" in line:
            continue
        counts.append(line.rsplit(" ", 1)[-1].strip())
    return counts


def _sh_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _elvish_quote(value: str) -> str:
    """An elvish single-quoted raw string: a quote is written by doubling it."""
    return "'" + value.replace("'", "''") + "'"


def test_elvish_hook_registration_is_inert_without_a_tty(tmp_path: Path) -> None:
    """The hook registration must not break a non-interactive elvish.

    ``$edit:before-readline`` exists only in an interactive elvish, and elvish
    resolves every variable in a chunk *before* executing any of it — so a direct
    reference is a compile error that kills the whole unit, including any
    ``try`` around it. The registration therefore rides inside ``eval`` of a
    string, which turns that into a catchable runtime exception. This asserts the
    catch actually holds: a non-interactive elvish sourcing the same rc exits 0
    and reaches the line after it.
    """
    shell_abs = _require("elvish")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    _dedicated_setup("elvish", shell_abs, home, ocx_home)

    # The registration lives in the emitted activation stream, not in rc.elv —
    # rc.elv only sources `env.elv`, which runs `ocx self activate`. `--hook`
    # forces the rung a tty-less harness would otherwise resolve to `auto`.
    stream_path = tmp_path / "activation.elv"
    stream = subprocess.run(
        [str(_OCX), "--offline", "self", "activate", "--shell=elvish", "--hook", "--no-completion"],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs, ocx_home=ocx_home), check=False,
    )
    assert stream.returncode == 0, f"elvish: activation must exit 0; stderr:\n{stream.stderr}"
    assert "edit:before-readline" in stream.stdout, (
        f"elvish: the activation stream must carry the prompt-hook registration; got:\n{stream.stdout}"
    )
    stream_path.write_text(stream.stdout, encoding="utf-8")

    probe = f"eval (slurp < {_elvish_quote(str(stream_path))}); echo '{_ELVISH_PATH_PROBE}' reached"
    result = subprocess.run(
        [shell_abs, "-c", probe],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs), check=False,
    )
    combined = result.stdout + result.stderr
    assert result.returncode == 0, (
        f"elvish: a non-interactive shell must survive the hook registration; rc={result.returncode}\n{combined}"
    )
    assert f"{_ELVISH_PATH_PROBE} reached" in result.stdout, (
        f"elvish: the line after the registration must still run; got:\n{combined}"
    )


def test_elvish_path_activation_survives_completion_failure_without_tty(tmp_path: Path) -> None:
    """PATH activation must not depend on the completion block compiling.

    clap_complete's elvish completer needs the interactive-only ``edit:`` module,
    which is bound only with a real TTY. In a non-TTY elvish that block raises a
    compile error; because elvish compiles an ``eval`` unit as a whole, the old
    coupled ``eval (… --completion | slurp)`` form lost the PATH prepend with it.

    This drives ``env.elv`` in a NON-interactive ``elvish -c`` (no ``edit:``, no
    pty at all) and asserts the ocx bin dir still lands on PATH —
    proving the PATH and completion eval units are decoupled. On the old shim this
    raises a compile error and leaves ocx off PATH.
    """
    shell_abs = _require("elvish")

    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    bin_seg = str(ocx_home / _BIN_REL)
    _dedicated_setup("elvish", shell_abs, home, ocx_home)  # writes rc.elv + env.* shims
    env_elv = ocx_home / "env.elv"
    assert env_elv.is_file(), "setup must write the elvish env shim"

    # Non-interactive elvish: edit: is absent, so the completion block fails to
    # compile. With decoupled eval units, the PATH eval still runs. OCX_HOME is
    # unset in the child; env.elv computes it from $HOME.
    probe = f"eval (slurp < '{env_elv}'); echo '{_ELVISH_PATH_PROBE}' $E:PATH"
    result = subprocess.run(
        [shell_abs, "-c", probe],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs), check=False,
    )
    combined = result.stdout + result.stderr
    # The try/catch must swallow the completion compile error so the whole eval
    # exits 0; a non-zero exit means the error escaped and aborted the shim.
    assert result.returncode == 0, (
        f"elvish: eval of env.elv must exit 0 in a non-TTY shell (try/catch must swallow the "
        f"completion compile error); rc={result.returncode}\nstderr:\n{combined}"
    )
    _assert_no_missing_env_error(combined, "elvish")
    assert bin_seg in result.stdout, (
        f"elvish: PATH activation must survive a completion compile error in a non-TTY shell "
        f"(decoupled eval units); bin dir not on PATH:\n{result.stdout}\nstderr:\n{combined}"
    )


def test_powershell_fence_activation_survives_unset_ocx_home(tmp_path: Path) -> None:
    shell_abs = _require("pwsh")

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
        env=_clean_env(home, shell_abs), check=False,
    )
    profile = Path(query.stdout.strip())
    assert profile.is_file(), f"pwsh: setup must write the managed block to $PROFILE ({profile})"

    script = f". '{profile}'; . '{profile}'; $env:PATH"
    result = subprocess.run(
        [shell_abs, "-NoProfile", "-Command", script],
        capture_output=True,
        text=True,
        env=_clean_env(home, shell_abs), check=False,
    )
    _assert_activation("pwsh", result, bin_seg)
