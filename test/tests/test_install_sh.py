# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the install.sh profile-modification and env-file
generation logic (plan_self_activate.md Phase D).

These tests exercise the install.sh shell functions in an isolated
``OCX_HOME``/``HOME`` without hitting the network (no binary download).
They call ``create_env_sh``, ``modify_shell_profile``, ``remove_shell_profile``,
and ``remove_legacy_init_lines`` directly by sourcing the script with a no-op
``main()`` override.

Test matrix:
  (a) env.sh uses env-var-with-fallback ``${OCX_HOME:=$HOME/.ocx}`` — NOT a
      literal substituted path.  Byte-identical across different installs.
  (b) Install twice → block-marker appears exactly once (idempotent).
  (c) Seed legacy ``. "$OCX_HOME/init.bash"`` + benign comment containing
      ``.ocx/init.`` → install → legacy source line removed, comment preserved.
  (d) Uninstall → BEGIN/END block AND legacy source line both gone.
  (e) Re-source env.sh twice in the same session → ``_OCX_ENV_LOADED`` guard
      prevents double-apply (PATH not duplicated).
  (f) env.sh byte-identical across different OCX_HOME directories (Phase D).

All tests:
  - Use isolated temp directories as OCX_HOME / HOME.
  - Never modify the real user HOME or shell profiles.
  - Are deterministic and independent.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

# Shell scenarios are Linux/macOS only.
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="install.sh tests require POSIX sh — skipped on Windows.",
)

INSTALL_SH = Path(__file__).resolve().parents[2] / "website" / "src" / "public" / "install.sh"


def _sh(script: str, env: dict[str, str]) -> "subprocess.CompletedProcess[str]":
    """Run ``script`` in a POSIX ``sh`` subprocess with the given environment."""
    return subprocess.run(
        ["sh", "-c", script],
        capture_output=True,
        text=True,
        env=env,
    )


def _get_install_sh_functions() -> str:
    """Return the install.sh content with the trailing ``main "$@"`` call stripped.

    The script ends with ``main "$@"`` as its last line.  Sourcing the functions-
    only version loads all helper functions without running the network-dependent
    main installer.  We strip only the last line so no structural assumptions are
    made about the script body.
    """
    lines = INSTALL_SH.read_text().splitlines()
    # Strip the trailing ``main "$@"`` invocation (last non-empty line).
    while lines and not lines[-1].strip():
        lines.pop()
    if lines and lines[-1].strip().startswith("main"):
        lines.pop()
    return "\n".join(lines)


# Cache the functions body once per module (pure read, no side effects).
_INSTALL_SH_FUNCTIONS: str | None = None


def _install_sh_functions() -> str:
    global _INSTALL_SH_FUNCTIONS
    if _INSTALL_SH_FUNCTIONS is None:
        _INSTALL_SH_FUNCTIONS = _get_install_sh_functions()
    return _INSTALL_SH_FUNCTIONS


def _source_install_sh(extra: str, env: dict[str, str]) -> "subprocess.CompletedProcess[str]":
    """Source install.sh functions (without the ``main "$@"`` invocation) then
    run ``extra`` shell code."""
    script = f"""
{_install_sh_functions()}
{extra}
"""
    return _sh(script, env)


# ---------------------------------------------------------------------------
# (a) env.sh uses env-var-with-fallback, NOT a literal substituted path
# ---------------------------------------------------------------------------


def test_env_sh_uses_envvar_fallback_not_literal(tmp_path: Path) -> None:
    """``create_env_sh`` writes a runtime ``${OCX_HOME:=...}`` fallback into
    env.sh, NOT the literal resolved OCX_HOME path (Phase D shim model).

    Phase D contract: env.sh must be byte-identical across users.  The file
    must contain the env-var-with-fallback form and must NOT embed any
    literal home-directory path.
    """
    ocx_home = tmp_path / "custom_ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    # Call create_env_sh via source.
    result = _source_install_sh("create_env_sh", env)
    assert result.returncode == 0, (
        f"create_env_sh must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    env_sh = ocx_home / "env.sh"
    assert env_sh.exists(), f"env.sh must be created at {env_sh}"

    content = env_sh.read_text()
    # Must contain the env-var-with-fallback expression.
    assert "${OCX_HOME:=" in content, (
        "env.sh must use the ${OCX_HOME:=...} assign-if-unset form; "
        f"got:\n{content}"
    )
    # Must NOT embed the literal install-time OCX_HOME path.
    assert str(ocx_home) not in content, (
        f"env.sh must NOT embed the literal OCX_HOME path ({ocx_home}); "
        "the file must be byte-identical across users.\n"
        f"got:\n{content}"
    )
    # Must NOT contain any literal home-directory prefix that would make files differ.
    assert str(tmp_path) not in content, (
        f"env.sh must NOT contain the tmp_path prefix ({tmp_path}); "
        f"got:\n{content}"
    )
    # Must delegate to `ocx self activate`.
    assert "self activate" in content, (
        f"env.sh must delegate to 'ocx self activate'; got:\n{content}"
    )


def test_env_sh_activates_without_ocx_home_exported(tmp_path: Path) -> None:
    """Sourcing env.sh activates the same-session guard even when OCX_HOME
    is NOT in the environment (env-var-with-fallback form: ${OCX_HOME:=$HOME/.ocx}).

    The ``_OCX_ENV_LOADED`` guard fires and OCX_HOME is set from the
    ``${OCX_HOME:=$HOME/.ocx}`` fallback using the shell's ``$HOME``.
    """
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    ocx_home = tmp_path / "ocx_gen"
    ocx_home.mkdir()
    env_gen = {
        "HOME": str(fake_home),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    # Write env.sh (byte-identical shim, no literal path).
    _source_install_sh("create_env_sh", env_gen)
    env_sh = ocx_home / "env.sh"
    assert env_sh.exists()

    # Source env.sh WITHOUT OCX_HOME in the environment.
    # The fallback will derive OCX_HOME from $HOME.
    env_no_ocx_home = {
        "HOME": str(fake_home),
        "PATH": "/usr/bin:/bin",
    }
    result = _sh(
        f'. "{env_sh}" && echo "OCX_HOME=$OCX_HOME"',
        env_no_ocx_home,
    )
    assert result.returncode == 0, (
        f"env.sh must source successfully without OCX_HOME; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # The env.sh sets OCX_HOME via the $HOME/.ocx fallback.
    expected_ocx_home = str(fake_home / ".ocx")
    assert expected_ocx_home in result.stdout, (
        f"env.sh must set OCX_HOME to $HOME/.ocx when OCX_HOME is unset; "
        f"expected '{expected_ocx_home}' in stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# (b) Install twice → block-marker appears exactly once (idempotent)
# ---------------------------------------------------------------------------


def test_modify_shell_profile_idempotent(tmp_path: Path) -> None:
    """Running ``modify_shell_profile`` twice must produce exactly one
    ``# BEGIN ocx`` / ``# END ocx`` block — never two.

    C6 contract: the grep guard in modify_shell_profile prevents double-append.
    """
    home = tmp_path / "home"
    home.mkdir()
    profile = home / ".profile"
    profile.write_text("# existing profile\n")
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/bin/sh",
        "PATH": "/usr/bin:/bin",
    }

    # First call.
    r1 = _source_install_sh("modify_shell_profile", env)
    assert r1.returncode == 0, f"first modify_shell_profile failed: {r1.stderr}"

    # Second call — must be idempotent.
    r2 = _source_install_sh("modify_shell_profile", env)
    assert r2.returncode == 0, f"second modify_shell_profile failed: {r2.stderr}"

    content = profile.read_text()
    begin_count = content.count("# BEGIN ocx")
    assert begin_count == 1, (
        f"# BEGIN ocx must appear exactly once (idempotent); "
        f"found {begin_count} occurrences in:\n{content}"
    )
    end_count = content.count("# END ocx")
    assert end_count == 1, (
        f"# END ocx must appear exactly once; found {end_count} in:\n{content}"
    )


# ---------------------------------------------------------------------------
# (c) Legacy source line removed, benign comment preserved
# ---------------------------------------------------------------------------


def test_legacy_init_line_removed_on_install(tmp_path: Path) -> None:
    """A legacy ``. "$OCX_HOME/init.bash"`` source line is removed during install;
    a user comment that merely mentions ``.ocx/init.`` is NOT touched.

    C6 contract (W6): the installer detects and strips any old-model
    ``. "$OCX_HOME/init.<shell>"`` lines written by the deleted ``ocx shell init``.
    The detection is anchored to leading-whitespace + dot-command so comments
    like ``# my backup at .ocx/init.bash`` survive.

    The OCX_HOME must use the ``.ocx`` name convention (matching real-world
    ``~/.ocx``) so the grep-vE filter ``'^[[:space:]]*\\. .*\\.ocx/init\\.'``
    matches the legacy source line for removal.
    """
    home = tmp_path / "home"
    home.mkdir()
    # Must use ".ocx" name: the removal filter regex is anchored to \.ocx/init\.
    ocx_home = tmp_path / ".ocx"
    ocx_home.mkdir()

    # Simulate a profile that has:
    # - a legacy source line (written by deleted `ocx shell init`)
    # - a benign comment mentioning the path (must NOT be removed)
    profile = home / ".profile"
    profile.write_text(
        "# existing profile\n"
        f'. "{ocx_home}/init.bash"\n'
        "# note: old backup at /some/path/.ocx/init.bash\n"
        "export PATH=$PATH:/usr/local/bin\n"
    )

    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/bin/sh",
        "PATH": "/usr/bin:/bin",
    }

    result = _source_install_sh("modify_shell_profile", env)
    assert result.returncode == 0, f"modify_shell_profile failed: {result.stderr}"

    content = profile.read_text()

    # Legacy source line must be gone.
    assert f'. "{ocx_home}/init.bash"' not in content, (
        f"legacy `. \"$OCX_HOME/init.bash\"` line must be removed; "
        f"profile is now:\n{content}"
    )

    # Benign comment must be preserved.
    assert "# note: old backup at /some/path/.ocx/init.bash" in content, (
        f"benign comment mentioning .ocx/init.bash must NOT be removed; "
        f"profile is now:\n{content}"
    )

    # New block must be present.
    assert "# BEGIN ocx" in content, (
        f"# BEGIN ocx block must be added after legacy line removal; "
        f"profile is now:\n{content}"
    )


# ---------------------------------------------------------------------------
# (d) Uninstall → block AND legacy gone
# ---------------------------------------------------------------------------


def test_remove_shell_profile_strips_block_and_legacy(tmp_path: Path) -> None:
    """``remove_shell_profile`` strips both the BEGIN/END block and any
    remaining legacy ``. "$OCX_HOME/init.*"`` lines.

    C6 contract (W6): uninstall must clean up both the current block-marker
    form and any old-model source lines the user hasn't upgraded yet.

    The OCX_HOME must use the ``.ocx`` name convention so the removal filter
    regex ``'^[[:space:]]*\\. .*\\.ocx/init\\.'`` matches the legacy line.
    """
    home = tmp_path / "home"
    home.mkdir()
    # Must use ".ocx" name: the removal filter regex is anchored to \.ocx/init\.
    ocx_home = tmp_path / ".ocx"
    ocx_home.mkdir()

    profile = home / ".profile"
    profile.write_text(
        "# header\n"
        f'. "{ocx_home}/init.bash"\n'
        "\n# BEGIN ocx\n"
        f'. "{ocx_home}/env.sh"\n'
        "# END ocx\n"
        "export KEEP=1\n"
    )

    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/bin/sh",
        "PATH": "/usr/bin:/bin",
    }

    result = _source_install_sh("remove_shell_profile", env)
    assert result.returncode == 0, f"remove_shell_profile failed: {result.stderr}"

    content = profile.read_text()

    # BEGIN/END block must be gone.
    assert "# BEGIN ocx" not in content, (
        f"# BEGIN ocx must be stripped on uninstall; profile:\n{content}"
    )
    assert "# END ocx" not in content, (
        f"# END ocx must be stripped on uninstall; profile:\n{content}"
    )

    # Legacy source line must be gone.
    assert f'. "{ocx_home}/init.bash"' not in content, (
        f"legacy init line must be stripped on uninstall; profile:\n{content}"
    )

    # User content outside the block must be preserved.
    assert "export KEEP=1" in content, (
        f"user content outside the block must be preserved; profile:\n{content}"
    )


# ---------------------------------------------------------------------------
# (e) Re-source env.sh twice → _OCX_ENV_LOADED guard prevents double-apply
# ---------------------------------------------------------------------------


def test_env_sh_same_session_guard_prevents_double_apply(tmp_path: Path) -> None:
    """Sourcing env.sh twice in the same shell session must not re-run activation
    (the ``_OCX_ENV_LOADED`` guard prevents double-apply).

    Current env.sh guard shape (fires unconditionally before OCX_HOME assignment):

    .. code-block:: sh

        if [ -n "${_OCX_ENV_LOADED:-}" ]; then
            return 0 2>/dev/null || exit 0
        fi
        _OCX_ENV_LOADED=1
        export _OCX_ENV_LOADED

    The guard runs at the top of env.sh, outside and before the
    ``if [ -x "$_ocx_bin" ]`` block, so a fake binary is created to
    reach the activation path and confirm ``_OCX_ENV_LOADED`` is set.
    """
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    fake_home = tmp_path / "home"
    fake_home.mkdir()

    env_gen = {
        "HOME": str(fake_home),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    _source_install_sh("create_env_sh", env_gen)
    env_sh = ocx_home / "env.sh"
    assert env_sh.exists()

    # Create a minimal fake `ocx` binary at the expected symlink path.
    # The fake binary emits a no-op comment — eval of a comment is safe in sh.
    bin_dir = ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin"
    bin_dir.mkdir(parents=True)
    fake_ocx = bin_dir / "ocx"
    fake_ocx.write_text("#!/bin/sh\n# no-op fake ocx for test\n")
    fake_ocx.chmod(0o755)

    # Source env.sh twice with OCX_HOME set so the binary is found via the
    # env-var-with-fallback (fallback would use $HOME/.ocx where no bin exists).
    result = _sh(
        f"""
. "{env_sh}"
. "{env_sh}"
echo "_OCX_ENV_LOADED=${{_OCX_ENV_LOADED:-}}"
""",
        {
            "HOME": str(fake_home),
            "OCX_HOME": str(ocx_home),
            "PATH": "/usr/bin:/bin",
        },
    )
    assert result.returncode == 0, (
        f"double-source must not fail; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # The guard variable must be set after the first source.
    assert "_OCX_ENV_LOADED=1" in result.stdout, (
        f"_OCX_ENV_LOADED must be '1' after double-source; stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# (f) PATH de-duplication: double-source env.sh does not repeat OCX bin dir
# ---------------------------------------------------------------------------


def test_env_sh_double_source_no_path_duplication(tmp_path: Path) -> None:
    """Sourcing env.sh twice in the same shell session must not duplicate the
    OCX bin directory in PATH.

    Validates the ``_OCX_ENV_LOADED`` guard (see test (e)) from the PATH
    perspective: the second source must skip the ``export PATH=...`` line,
    so the OCX bin dir appears in PATH exactly once.

    Strategy:
    - Write env.sh via ``create_env_sh`` (generates the thin shim form).
    - Create a minimal fake ``ocx`` binary so env.sh can reach the activation
      path (the inner ``if [ -x "$_ocx_bin" ]`` guard).
    - Source env.sh twice in a single ``sh`` subprocess with an initial PATH
      set to a known value.
    - Print PATH after double-source and count occurrences of the OCX bin
      directory segment (``symlinks``).
    - Assert the count is exactly 1.
    """
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    fake_home = tmp_path / "home"
    fake_home.mkdir()

    env_gen = {
        "HOME": str(fake_home),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    _source_install_sh("create_env_sh", env_gen)
    env_sh = ocx_home / "env.sh"
    assert env_sh.exists()

    # Create a minimal fake `ocx` binary at the expected current symlink path.
    # The fake binary must emit a valid `export PATH=...` line when invoked as
    # `ocx self activate --shell=sh`, because env.sh does:
    #   eval "$("$_ocx_bin" self activate --shell=sh 2>/dev/null)" || true
    # Without a PATH-prepend line in the output, the guard test would vacuously
    # pass (0 occurrences ≠ 1).  We emit an explicit prepend using the bin dir
    # that env.sh itself passes via $OCX_HOME.
    bin_dir = ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin"
    bin_dir.mkdir(parents=True)
    fake_ocx = bin_dir / "ocx"
    # The fake ocx emits the POSIX export PATH=... line so the first source
    # actually mutates PATH.  This mirrors what the real `ocx self activate
    # --shell=sh` emits, letting the guard test exercise PATH de-duplication.
    fake_ocx.write_text(
        "#!/bin/sh\n"
        f'printf \'export PATH="%s:${{PATH}}"\' "$(dirname "$0")"\n'
    )
    fake_ocx.chmod(0o755)

    # Source env.sh twice, print PATH after both sources.
    result = _sh(
        f"""
. "{env_sh}"
. "{env_sh}"
printf '%s' "$PATH"
""",
        {
            "HOME": str(fake_home),
            "OCX_HOME": str(ocx_home),
            "PATH": "/usr/bin:/bin",
        },
    )
    assert result.returncode == 0, (
        f"double-source must not fail; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    path_value = result.stdout
    # Count occurrences of the OCX bin dir in PATH.  Must be exactly 1 —
    # the _OCX_ENV_LOADED guard skips the second source so the prepend
    # runs once only.
    ocx_bin_path = str(bin_dir)
    occurrence_count = path_value.count(ocx_bin_path)
    assert occurrence_count == 1, (
        f"OCX bin dir must appear in PATH exactly once after double-source; "
        f"found {occurrence_count} occurrence(s).\n"
        f"bin_dir={ocx_bin_path!r}\n"
        f"PATH={path_value!r}"
    )


# ---------------------------------------------------------------------------
# Nushell — create_env_nu / create_nu_autoload / detect_profile / uninstall
# ---------------------------------------------------------------------------


def test_create_env_nu_uses_envvar_fallback_not_literal(tmp_path: Path) -> None:
    """``create_env_nu`` writes $OCX_HOME/env.nu using the env-var-with-fallback
    form, NOT a literal install-time path (Phase D shim model).

    The file must NOT contain the literal OCX_HOME install path and must use
    the Nushell env-var-with-fallback form to set OCX_HOME at runtime.
    The shim delegates to ``ocx self activate --shell=nushell``.
    """
    ocx_home = tmp_path / "custom_ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("create_env_nu", env)
    assert result.returncode == 0, (
        f"create_env_nu must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    env_nu = ocx_home / "env.nu"
    assert env_nu.exists(), f"env.nu must be created at {env_nu}"

    content = env_nu.read_text()
    # Must NOT embed the literal OCX_HOME path (byte-identity requirement).
    assert str(ocx_home) not in content, (
        f"env.nu must NOT embed the literal OCX_HOME path ({ocx_home}); "
        "the file must be byte-identical across users.\n"
        f"got:\n{content}"
    )
    # Must use the env-var-with-fallback form for OCX_HOME.
    assert "OCX_HOME?" in content, (
        "env.nu must use the Nushell env-var-with-fallback form ($env.OCX_HOME?); "
        f"got:\n{content}"
    )
    # Must delegate to `ocx self activate`.
    assert "self activate" in content, (
        f"env.nu must delegate to 'ocx self activate'; got:\n{content}"
    )


def test_create_nu_autoload_uses_runtime_ocx_home_not_literal(tmp_path: Path) -> None:
    """``create_nu_autoload`` writes the vendor/autoload/ocx.nu file with a
    runtime OCX_HOME computation, NOT a literal install-time path (Phase D).

    The autoload file sets OCX_HOME via env-var-with-fallback and computes the
    env.nu path from it at runtime.  No literal home-directory path is embedded.
    """
    home = tmp_path / "home"
    home.mkdir()
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("create_nu_autoload", env)
    assert result.returncode == 0, (
        f"create_nu_autoload must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    autoload_file = home / ".local" / "share" / "nushell" / "vendor" / "autoload" / "ocx.nu"
    assert autoload_file.exists(), f"ocx.nu must be created at {autoload_file}"

    content = autoload_file.read_text()
    # Must NOT contain a literal install-time path (byte-identity requirement).
    assert str(ocx_home) not in content, (
        f"ocx.nu must NOT embed the literal OCX_HOME path ({ocx_home}); "
        "the file must be byte-identical across users.\n"
        f"got:\n{content}"
    )
    assert str(home) not in content, (
        f"ocx.nu must NOT embed the literal HOME path ({home}); "
        f"got:\n{content}"
    )
    # Must use the runtime env-var-with-fallback for OCX_HOME.
    assert "OCX_HOME?" in content or "$env.OCX_HOME" in content, (
        "ocx.nu must use a runtime OCX_HOME reference ($env.OCX_HOME? or similar); "
        f"got:\n{content}"
    )
    # Must reference env.nu for the actual source.
    assert "env.nu" in content, (
        f"ocx.nu must reference env.nu; got:\n{content}"
    )


def test_detect_profile_nu_returns_empty(tmp_path: Path) -> None:
    """``detect_profile`` must return an empty string when ``$SHELL`` is ``nu``.

    Nushell uses vendor/autoload, not a block-marker profile edit.
    """
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        "SHELL": "/usr/bin/nu",
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("detect_profile", env)
    assert result.returncode == 0, (
        f"detect_profile must succeed for nu; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stdout.strip() == "", (
        f"detect_profile must return empty for nu shell; got: '{result.stdout.strip()}'"
    )


def test_remove_nu_autoload(tmp_path: Path) -> None:
    """``remove_shell_profile`` removes the vendor/autoload/ocx.nu file when
    ``$SHELL`` is ``nu``.
    """
    home = tmp_path / "home"
    home.mkdir()
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()

    # Pre-create the autoload file so removal has something to remove.
    autoload_dir = home / ".local" / "share" / "nushell" / "vendor" / "autoload"
    autoload_dir.mkdir(parents=True)
    autoload_file = autoload_dir / "ocx.nu"
    autoload_file.write_text(f'source "{ocx_home}/env.nu"\n')

    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/usr/bin/nu",
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("remove_shell_profile", env)
    assert result.returncode == 0, (
        f"remove_shell_profile must succeed for nu; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not autoload_file.exists(), (
        f"vendor/autoload/ocx.nu must be removed on uninstall; still exists at {autoload_file}"
    )


# ---------------------------------------------------------------------------
# Elvish — create_env_elv / detect_profile / modify / remove profile
# ---------------------------------------------------------------------------


def test_create_env_elv_uses_envvar_fallback_not_literal(tmp_path: Path) -> None:
    """``create_env_elv`` writes $OCX_HOME/env.elv using the env-var-with-fallback
    form, NOT a literal install-time path (Phase D shim model).

    The file must NOT contain the literal OCX_HOME install path and must use
    the Elvish env-var form to set OCX_HOME at runtime.
    The shim delegates to ``ocx self activate --shell=elvish``.
    """
    ocx_home = tmp_path / "custom_ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("create_env_elv", env)
    assert result.returncode == 0, (
        f"create_env_elv must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    env_elv = ocx_home / "env.elv"
    assert env_elv.exists(), f"env.elv must be created at {env_elv}"

    content = env_elv.read_text()
    # Must NOT embed the literal OCX_HOME path (byte-identity requirement).
    assert str(ocx_home) not in content, (
        f"env.elv must NOT embed the literal OCX_HOME path ({ocx_home}); "
        "the file must be byte-identical across users.\n"
        f"got:\n{content}"
    )
    # Must use the Elvish env-var fallback for OCX_HOME.
    assert "has-env OCX_HOME" in content or "OCX_HOME" in content, (
        "env.elv must use a runtime OCX_HOME reference (has-env OCX_HOME / set-env); "
        f"got:\n{content}"
    )
    # Must delegate to `ocx self activate --shell=elvish`.
    assert "self activate" in content, (
        f"env.elv must delegate to 'ocx self activate'; got:\n{content}"
    )
    assert "slurp" in content, (
        f"env.elv must use slurp to read shell output; got:\n{content}"
    )


def test_detect_profile_elvish_returns_rc_elv(tmp_path: Path) -> None:
    """``detect_profile`` must return ``~/.config/elvish/rc.elv`` when
    ``$SHELL`` is ``elvish``.
    """
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        "SHELL": "/usr/bin/elvish",
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("detect_profile", env)
    assert result.returncode == 0, (
        f"detect_profile must succeed for elvish; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    expected = str(home / ".config" / "elvish" / "rc.elv")
    assert result.stdout.strip() == expected, (
        f"detect_profile must return '{expected}' for elvish; got: '{result.stdout.strip()}'"
    )


def test_modify_shell_profile_elvish_block(tmp_path: Path) -> None:
    """``modify_shell_profile`` writes a ``# BEGIN ocx`` / ``# END ocx`` block
    with Elvish ``eval (slurp < ...)`` syntax when ``$SHELL`` is ``elvish``.

    The block must be idempotent: a second call must not add a second block.
    """
    home = tmp_path / "home"
    home.mkdir()
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()

    rc_elv = home / ".config" / "elvish" / "rc.elv"
    rc_elv.parent.mkdir(parents=True)
    rc_elv.write_text("# existing elvish config\n")

    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/usr/bin/elvish",
        "PATH": "/usr/bin:/bin",
    }

    # First call — must add the block.
    r1 = _source_install_sh("modify_shell_profile", env)
    assert r1.returncode == 0, f"first modify_shell_profile (elvish) failed: {r1.stderr}"

    content = rc_elv.read_text()
    assert "# BEGIN ocx" in content, (
        f"# BEGIN ocx must be added for elvish; profile:\n{content}"
    )
    assert "# END ocx" in content, (
        f"# END ocx must be added for elvish; profile:\n{content}"
    )
    # Must use Elvish eval-slurp syntax, not POSIX dot-source.
    assert "eval (slurp <" in content, (
        f"elvish block must use eval (slurp < ...) syntax; got:\n{content}"
    )
    assert str(ocx_home) in content, (
        f"elvish block must embed literal OCX_HOME path; got:\n{content}"
    )
    # Must contain the env.elv filename.
    assert "env.elv" in content, (
        f"elvish block must reference env.elv; got:\n{content}"
    )

    # Second call — must be idempotent (no duplicate block).
    r2 = _source_install_sh("modify_shell_profile", env)
    assert r2.returncode == 0, f"second modify_shell_profile (elvish) failed: {r2.stderr}"

    content2 = rc_elv.read_text()
    begin_count = content2.count("# BEGIN ocx")
    assert begin_count == 1, (
        f"# BEGIN ocx must appear exactly once (idempotent); found {begin_count} in:\n{content2}"
    )


def test_modify_shell_profile_posix_block_unchanged(tmp_path: Path) -> None:
    """Regression: POSIX sh/bash/zsh block form must remain byte-identical to the
    expected form — ``\\n# BEGIN ocx\\n. "<root>/env.sh"\\n# END ocx\\n``.

    Ensures the Elvish generalisation did not alter the POSIX path.
    """
    home = tmp_path / "home"
    home.mkdir()
    profile = home / ".profile"
    profile.write_text("# header\n")
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/bin/sh",
        "PATH": "/usr/bin:/bin",
    }

    result = _source_install_sh("modify_shell_profile", env)
    assert result.returncode == 0, f"modify_shell_profile (sh) failed: {result.stderr}"

    content = profile.read_text()
    expected_block = f'\n# BEGIN ocx\n. "{ocx_home}/env.sh"\n# END ocx\n'
    assert expected_block in content, (
        f"POSIX block must be byte-identical to expected form; "
        f"expected:\n{expected_block!r}\ngot profile:\n{content!r}"
    )
    # Must NOT contain any Elvish or Nushell syntax.
    assert "slurp" not in content, (
        f"POSIX profile must not contain Elvish slurp syntax; got:\n{content}"
    )


# ---------------------------------------------------------------------------
# (Phase D) env.sh byte-identical across different OCX_HOME directories
# ---------------------------------------------------------------------------


def test_env_sh_byte_identical_across_install_dirs(tmp_path: Path) -> None:
    """After Phase D, env.sh must be byte-identical regardless of which
    ``OCX_HOME`` path was used during install.

    Plan decision: "OCX_HOME resolution: Env-var-with-fallback
    (${OCX_HOME:=$HOME/.ocx}), no install-time substitution. Env files
    byte-identical across users."

    This test is a regression guard.  Currently install.sh embeds the literal
    OCX_HOME into env.sh (failing the byte-identity check), which means this
    test will FAIL until Phase D rewrites `create_env_sh` to emit the thin
    shim.  That failure is expected and signals to the implementer that the
    byte-identity property is not yet satisfied.
    """
    home_a = tmp_path / "home_a"
    home_a.mkdir()
    ocx_home_a = tmp_path / ".ocx_a"
    ocx_home_a.mkdir()

    home_b = tmp_path / "home_b"
    home_b.mkdir()
    ocx_home_b = tmp_path / ".ocx_b"
    ocx_home_b.mkdir()

    env_a = {
        "HOME": str(home_a),
        "OCX_HOME": str(ocx_home_a),
        "PATH": "/usr/bin:/bin",
    }
    env_b = {
        "HOME": str(home_b),
        "OCX_HOME": str(ocx_home_b),
        "PATH": "/usr/bin:/bin",
    }

    result_a = _source_install_sh("create_env_sh", env_a)
    assert result_a.returncode == 0, (
        f"create_env_sh (a) must succeed; rc={result_a.returncode}\nstderr:\n{result_a.stderr}"
    )
    result_b = _source_install_sh("create_env_sh", env_b)
    assert result_b.returncode == 0, (
        f"create_env_sh (b) must succeed; rc={result_b.returncode}\nstderr:\n{result_b.stderr}"
    )

    env_sh_a = (ocx_home_a / "env.sh").read_bytes()
    env_sh_b = (ocx_home_b / "env.sh").read_bytes()

    assert env_sh_a == env_sh_b, (
        "env.sh must be byte-identical across different OCX_HOME directories "
        "(Phase D: no OCX_HOME literal substitution at install time).\n"
        "This failure is expected until Phase D implements the thin shim model.\n"
        f"env.sh from OCX_HOME_A:\n{env_sh_a.decode(errors='replace')}\n"
        f"env.sh from OCX_HOME_B:\n{env_sh_b.decode(errors='replace')}"
    )


def test_remove_shell_profile_elvish(tmp_path: Path) -> None:
    """``remove_shell_profile`` strips the ``# BEGIN ocx`` / ``# END ocx`` block
    from ``rc.elv`` when ``$SHELL`` is ``elvish``.
    """
    home = tmp_path / "home"
    home.mkdir()
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()

    rc_elv = home / ".config" / "elvish" / "rc.elv"
    rc_elv.parent.mkdir(parents=True)
    rc_elv.write_text(
        "# header\n"
        "\n# BEGIN ocx\n"
        f'eval (slurp < "{ocx_home}/env.elv")\n'
        "# END ocx\n"
        "# user config\n"
    )

    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "SHELL": "/usr/bin/elvish",
        "PATH": "/usr/bin:/bin",
    }

    result = _source_install_sh("remove_shell_profile", env)
    assert result.returncode == 0, (
        f"remove_shell_profile must succeed for elvish; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    content = rc_elv.read_text()
    # BEGIN/END block must be gone.
    assert "# BEGIN ocx" not in content, (
        f"# BEGIN ocx must be stripped for elvish; profile:\n{content}"
    )
    assert "# END ocx" not in content, (
        f"# END ocx must be stripped for elvish; profile:\n{content}"
    )
    # User content outside the block must survive.
    assert "# user config" in content, (
        f"user content outside the block must be preserved; profile:\n{content}"
    )


# ---------------------------------------------------------------------------
# TC-B4 — Fish env file byte-identity
#
# Parallel coverage to test_env_sh_uses_envvar_fallback_not_literal and
# test_create_env_nu_uses_envvar_fallback_not_literal.  Without these tests,
# a copy-paste regression in `create_env_fish` that bakes the literal OCX_HOME
# into the heredoc body would land green (and break the byte-identity contract
# that lets the installer ship the same env.fish to every user).
# ---------------------------------------------------------------------------


def test_create_env_fish_uses_envvar_fallback_not_literal(tmp_path: Path) -> None:
    """``create_env_fish`` writes ``$OCX_HOME/env.fish`` using the
    env-var-with-fallback form, NOT a literal install-time path (Phase D
    shim model).

    The file must NOT contain the literal OCX_HOME install path and must use
    the fish env-var-with-fallback form to set OCX_HOME at runtime.  The shim
    delegates to ``ocx self activate --shell=fish``.

    Byte-identity contract: every user receives the same env.fish bytes so
    the installer can ship a stable shim.  Embedding the literal install path
    would defeat that contract and break atomic upgrades.
    """
    ocx_home = tmp_path / "custom_ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("create_env_fish", env)
    assert result.returncode == 0, (
        f"create_env_fish must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    env_fish = ocx_home / "env.fish"
    assert env_fish.exists(), f"env.fish must be created at {env_fish}"

    content = env_fish.read_text()
    # Must NOT embed the literal OCX_HOME path (byte-identity requirement).
    assert str(ocx_home) not in content, (
        f"env.fish must NOT embed the literal OCX_HOME path ({ocx_home}); "
        "the file must be byte-identical across users.\n"
        f"got:\n{content}"
    )
    # Must NOT embed the literal HOME path either.
    assert str(tmp_path) not in content, (
        f"env.fish must NOT embed the tmp_path prefix ({tmp_path}); "
        f"got:\n{content}"
    )
    # Must use the fish env-var-with-fallback form for OCX_HOME.
    assert "set -q OCX_HOME" in content, (
        "env.fish must use the fish env-var-with-fallback form "
        "(if not set -q OCX_HOME ... end); "
        f"got:\n{content}"
    )
    # Must delegate to `ocx self activate --shell=fish`.
    assert "self activate --shell=fish" in content, (
        f"env.fish must delegate to 'ocx self activate --shell=fish'; got:\n{content}"
    )


def test_env_fish_byte_identical_across_install_dirs(tmp_path: Path) -> None:
    """Byte-identity gate: env.fish must be identical regardless of OCX_HOME.

    A regression in ``create_env_fish`` that bakes the literal OCX_HOME path
    into the heredoc body would produce different bytes per install — this
    gate catches that case at install time.
    """
    home_a = tmp_path / "home_a"
    home_a.mkdir()
    ocx_home_a = tmp_path / ".ocx_a"
    ocx_home_a.mkdir()

    home_b = tmp_path / "home_b"
    home_b.mkdir()
    ocx_home_b = tmp_path / ".ocx_b"
    ocx_home_b.mkdir()

    env_a = {"HOME": str(home_a), "OCX_HOME": str(ocx_home_a), "PATH": "/usr/bin:/bin"}
    env_b = {"HOME": str(home_b), "OCX_HOME": str(ocx_home_b), "PATH": "/usr/bin:/bin"}

    r_a = _source_install_sh("create_env_fish", env_a)
    assert r_a.returncode == 0, f"create_env_fish (a) failed: {r_a.stderr}"
    r_b = _source_install_sh("create_env_fish", env_b)
    assert r_b.returncode == 0, f"create_env_fish (b) failed: {r_b.stderr}"

    bytes_a = (ocx_home_a / "env.fish").read_bytes()
    bytes_b = (ocx_home_b / "env.fish").read_bytes()
    assert bytes_a == bytes_b, (
        "env.fish must be byte-identical across different OCX_HOME directories "
        "(no install-time substitution).\n"
        f"env.fish from OCX_HOME_A:\n{bytes_a.decode(errors='replace')}\n"
        f"env.fish from OCX_HOME_B:\n{bytes_b.decode(errors='replace')}"
    )


# ---------------------------------------------------------------------------
# TC-B4 — PowerShell env file byte-identity
#
# Parallel coverage to env.sh / env.nu / env.elv / env.fish.  Windows is
# skipped at module scope (pytestmark) but the heredoc body itself is text —
# we exercise it from POSIX sh just like the other env files.
# ---------------------------------------------------------------------------


def test_create_env_ps1_uses_envvar_fallback_not_literal(tmp_path: Path) -> None:
    """``create_env_ps1`` writes ``$OCX_HOME/env.ps1`` using the
    env-var-with-fallback form, NOT a literal install-time path.

    The file must NOT contain the literal OCX_HOME install path and must use
    the PowerShell ``if (-not $env:OCX_HOME) { ... }`` env-var-with-fallback
    form to set OCX_HOME at runtime.  The shim delegates to
    ``ocx self activate --shell=powershell``.
    """
    ocx_home = tmp_path / "custom_ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    result = _source_install_sh("create_env_ps1", env)
    assert result.returncode == 0, (
        f"create_env_ps1 must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    env_ps1 = ocx_home / "env.ps1"
    assert env_ps1.exists(), f"env.ps1 must be created at {env_ps1}"

    content = env_ps1.read_text()
    # Must NOT embed the literal OCX_HOME path (byte-identity requirement).
    assert str(ocx_home) not in content, (
        f"env.ps1 must NOT embed the literal OCX_HOME path ({ocx_home}); "
        "the file must be byte-identical across users.\n"
        f"got:\n{content}"
    )
    # Must NOT embed the literal HOME path either.
    assert str(tmp_path) not in content, (
        f"env.ps1 must NOT embed the tmp_path prefix ({tmp_path}); "
        f"got:\n{content}"
    )
    # Must use the PowerShell env-var-with-fallback form for OCX_HOME.
    assert "$env:OCX_HOME" in content, (
        "env.ps1 must use the PowerShell env-var-with-fallback form "
        "(if (-not $env:OCX_HOME) ...); "
        f"got:\n{content}"
    )
    # Must delegate to `ocx self activate --shell=powershell`.
    assert "self activate --shell=powershell" in content, (
        f"env.ps1 must delegate to 'ocx self activate --shell=powershell'; got:\n{content}"
    )


def test_env_ps1_byte_identical_across_install_dirs(tmp_path: Path) -> None:
    """Byte-identity gate: env.ps1 must be identical regardless of OCX_HOME.

    Catches a copy-paste regression in ``create_env_ps1`` that bakes the
    literal install path into the heredoc body.
    """
    home_a = tmp_path / "home_a"
    home_a.mkdir()
    ocx_home_a = tmp_path / ".ocx_a"
    ocx_home_a.mkdir()

    home_b = tmp_path / "home_b"
    home_b.mkdir()
    ocx_home_b = tmp_path / ".ocx_b"
    ocx_home_b.mkdir()

    env_a = {"HOME": str(home_a), "OCX_HOME": str(ocx_home_a), "PATH": "/usr/bin:/bin"}
    env_b = {"HOME": str(home_b), "OCX_HOME": str(ocx_home_b), "PATH": "/usr/bin:/bin"}

    r_a = _source_install_sh("create_env_ps1", env_a)
    assert r_a.returncode == 0, f"create_env_ps1 (a) failed: {r_a.stderr}"
    r_b = _source_install_sh("create_env_ps1", env_b)
    assert r_b.returncode == 0, f"create_env_ps1 (b) failed: {r_b.stderr}"

    bytes_a = (ocx_home_a / "env.ps1").read_bytes()
    bytes_b = (ocx_home_b / "env.ps1").read_bytes()
    assert bytes_a == bytes_b, (
        "env.ps1 must be byte-identical across different OCX_HOME directories "
        "(no install-time substitution).\n"
        f"env.ps1 from OCX_HOME_A:\n{bytes_a.decode(errors='replace')}\n"
        f"env.ps1 from OCX_HOME_B:\n{bytes_b.decode(errors='replace')}"
    )


# ---------------------------------------------------------------------------
# TC-B5 — detect_profile ZDOTDIR="/" rejection (CWE-22 defense)
#
# install.sh detect_profile (around lines 673-679) refuses ZDOTDIR="/" to
# prevent the installer writing /.zprofile (path-traversal class — CWE-22).
# Removal of the guard would let the installer write under /, escalating from
# a misconfigured env var into a root-owned config file under /.
# ---------------------------------------------------------------------------


def test_detect_profile_rejects_zdotdir_root(tmp_path: Path) -> None:
    """``detect_profile`` with ``ZDOTDIR=/`` must fall back to ``$HOME``
    and never emit a path under ``/``.

    CWE-22 defense: writing ``/.zprofile`` (filesystem root) is the failure
    we are guarding against — a misconfigured or malicious ZDOTDIR would
    otherwise direct the installer at the wrong directory.

    The guard implementation (install.sh lines 673-679) is:

        _zdotdir="${ZDOTDIR:-$HOME}"
        if [ "$_zdotdir" = "/" ]; then
            warn "ZDOTDIR is '/' — refusing to write under /; falling back to $HOME"
            _zdotdir="$HOME"
        fi

    Assertions:
      - exit code 0 (warn, do not abort — graceful degradation)
      - stdout contains $HOME/.zprofile and $HOME/.zshrc (the fallback paths)
      - stdout does NOT contain `/.zprofile` or `/.zshrc` rooted at filesystem root
      - stderr carries the warning marker so removal of the guard fails the test
    """
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        # CWE-22 attack vector: ZDOTDIR pointing at filesystem root.
        "ZDOTDIR": "/",
        "SHELL": "/usr/bin/zsh",
        "PATH": "/usr/bin:/bin",
    }

    result = _source_install_sh("detect_profile", env)
    assert result.returncode == 0, (
        f"detect_profile must exit 0 even with ZDOTDIR='/' (warn + fallback); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    stdout_lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]

    # Fallback paths must appear (anchored at $HOME, not /).
    expected_zprofile = f"{home}/.zprofile"
    expected_zshrc = f"{home}/.zshrc"
    assert expected_zprofile in stdout_lines, (
        f"detect_profile must fall back to $HOME/.zprofile when ZDOTDIR='/'; "
        f"expected '{expected_zprofile}' in stdout lines: {stdout_lines!r}"
    )
    assert expected_zshrc in stdout_lines, (
        f"detect_profile must fall back to $HOME/.zshrc when ZDOTDIR='/'; "
        f"expected '{expected_zshrc}' in stdout lines: {stdout_lines!r}"
    )

    # The dangerous root-rooted paths must NEVER appear in stdout.
    # Use exact line match because '/.zprofile' is a substring of any
    # path-under-HOME line; we want the literal filesystem-root form.
    assert "/.zprofile" not in stdout_lines, (
        f"detect_profile must NOT emit '/.zprofile' (CWE-22 root write); "
        f"stdout lines: {stdout_lines!r}"
    )
    assert "/.zshrc" not in stdout_lines, (
        f"detect_profile must NOT emit '/.zshrc' (CWE-22 root write); "
        f"stdout lines: {stdout_lines!r}"
    )

    # Warning must fire — security log so removal of the guard fails the test.
    assert "ZDOTDIR" in result.stderr and "refusing" in result.stderr, (
        f"detect_profile must emit the ZDOTDIR='/' warning to stderr "
        f"(security log — removing the guard would silence this); "
        f"stderr:\n{result.stderr}"
    )


def test_detect_profile_rejects_zdotdir_root_writes_no_file_under_root(
    tmp_path: Path,
) -> None:
    """End-to-end: running ``modify_shell_profile`` with ZDOTDIR='/' must
    write its block under ``$HOME``, not under ``/``.

    This is the integration variant of the previous test — it exercises the
    full profile-modification path (detect_profile → modify_shell_profile)
    to prove the guard short-circuits before any write under ``/``.

    The test cannot directly assert ``/.zprofile`` was not created (we are
    not root and lack write permission to ``/``); instead it asserts:
      - the expected $HOME/.zprofile and $HOME/.zshrc were written
      - their content contains the BEGIN ocx marker (proves the write reached
        the fallback path)
      - exit code 0
    """
    home = tmp_path / "home"
    home.mkdir()
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(home),
        "OCX_HOME": str(ocx_home),
        "ZDOTDIR": "/",
        "SHELL": "/usr/bin/zsh",
        "PATH": "/usr/bin:/bin",
    }

    result = _source_install_sh("modify_shell_profile", env)
    assert result.returncode == 0, (
        f"modify_shell_profile must exit 0 with ZDOTDIR='/' fallback; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # The fallback targets must exist under $HOME.
    zprofile = home / ".zprofile"
    zshrc = home / ".zshrc"
    assert zprofile.exists(), (
        f"$HOME/.zprofile must be written (ZDOTDIR='/' fallback); "
        f"not found at {zprofile}\nstderr:\n{result.stderr}"
    )
    assert zshrc.exists(), (
        f"$HOME/.zshrc must be written (ZDOTDIR='/' fallback); "
        f"not found at {zshrc}\nstderr:\n{result.stderr}"
    )

    # And they must contain the BEGIN ocx block (proves the write reached
    # the fallback, not silently dropped).
    assert "# BEGIN ocx" in zprofile.read_text(), (
        f"$HOME/.zprofile must contain '# BEGIN ocx' block:\n{zprofile.read_text()}"
    )


# verify_checksum — sha256.sum format parsing


def _make_archive(path: Path, content: bytes = b"payload") -> str:
    import hashlib

    path.write_bytes(content)
    return hashlib.sha256(content).hexdigest()


def test_verify_checksum_accepts_binary_mode_format(tmp_path: Path) -> None:
    """Regression: parser must accept '<hash> *<name>' (cargo-dist default)."""
    archive = tmp_path / "ocx-x86_64-unknown-linux-gnu.tar.xz"
    digest = _make_archive(archive)
    (tmp_path / "sha256.sum").write_text(f"{digest} *{archive.name}\n")

    env = {"HOME": str(tmp_path / "home"), "PATH": "/usr/bin:/bin"}
    result = _source_install_sh(
        f'verify_checksum "{tmp_path}" "{archive.name}"', env
    )
    assert result.returncode == 0, result.stderr
    assert "Checksum verified" in result.stdout


def test_verify_checksum_accepts_text_mode_format(tmp_path: Path) -> None:
    """Parser must also accept '<hash>  <name>' (text mode, no asterisk)."""
    archive = tmp_path / "ocx-x86_64-unknown-linux-gnu.tar.xz"
    digest = _make_archive(archive)
    (tmp_path / "sha256.sum").write_text(f"{digest}  {archive.name}\n")

    env = {"HOME": str(tmp_path / "home"), "PATH": "/usr/bin:/bin"}
    result = _source_install_sh(
        f'verify_checksum "{tmp_path}" "{archive.name}"', env
    )
    assert result.returncode == 0, result.stderr


def test_verify_checksum_rejects_substring_match(tmp_path: Path) -> None:
    """Exact filename match: 'foo.tar.xz' must not match 'foo.tar.xz.sig'."""
    archive = tmp_path / "ocx-x86_64-unknown-linux-gnu.tar.xz"
    digest = _make_archive(archive)
    (tmp_path / "sha256.sum").write_text(f"{digest} *{archive.name}.sig\n")

    env = {"HOME": str(tmp_path / "home"), "PATH": "/usr/bin:/bin"}
    result = _source_install_sh(
        f'verify_checksum "{tmp_path}" "{archive.name}"', env
    )
    assert result.returncode != 0
    assert "not found" in result.stderr


def test_verify_checksum_detects_mismatch(tmp_path: Path) -> None:
    archive = tmp_path / "ocx-x86_64-unknown-linux-gnu.tar.xz"
    _make_archive(archive, content=b"real content")
    wrong_digest = "0" * 64
    (tmp_path / "sha256.sum").write_text(f"{wrong_digest} *{archive.name}\n")

    env = {"HOME": str(tmp_path / "home"), "PATH": "/usr/bin:/bin"}
    result = _source_install_sh(
        f'verify_checksum "{tmp_path}" "{archive.name}"', env
    )
    assert result.returncode != 0
    assert "checksum mismatch" in result.stderr
