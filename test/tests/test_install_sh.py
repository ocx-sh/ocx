# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the install.sh profile-modification and env-file
generation logic (plan_toolchain_cli.md Phase 5 / C6 / Warn A6).

These tests exercise the install.sh shell functions in an isolated
``OCX_HOME``/``HOME`` without hitting the network (no binary download).
They call ``create_env_sh``, ``modify_shell_profile``, ``remove_shell_profile``,
and ``remove_legacy_init_lines`` directly by sourcing the script with a no-op
``main()`` override.

Test matrix:
  (a) Non-default OCX_HOME → generated env.sh contains the LITERAL resolved root,
      not a runtime ``${OCX_HOME:-...}`` fallback.  Activation works without
      OCX_HOME exported.
  (b) Install twice → block-marker appears exactly once (idempotent).
  (c) Seed legacy ``. "$OCX_HOME/init.bash"`` + benign comment containing
      ``.ocx/init.`` → install → legacy source line removed, comment preserved.
  (d) Uninstall → BEGIN/END block AND legacy source line both gone.
  (e) Re-source env.sh twice in the same session → ``_OCX_ENV_LOADED`` guard
      prevents double-apply (PATH not duplicated).

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
# (a) Non-default OCX_HOME → env.sh contains literal root, not ${OCX_HOME:-...}
# ---------------------------------------------------------------------------


def test_env_sh_contains_literal_ocx_home(tmp_path: Path) -> None:
    """``create_env_sh`` writes the LITERAL resolved OCX_HOME path into env.sh,
    not a runtime ``${OCX_HOME:-...}`` fallback.

    C6 contract: the generated file must activate in a shell that does NOT have
    OCX_HOME exported — the literal path is the sole source of truth.
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
    # Must contain the literal path, not a runtime fallback variable.
    assert str(ocx_home) in content, (
        f"env.sh must embed the literal OCX_HOME path ({ocx_home}); "
        f"got:\n{content}"
    )
    # Must NOT contain ${OCX_HOME:-...} fallback that would break without OCX_HOME.
    assert "${OCX_HOME:-" not in content, (
        "env.sh must NOT use a runtime ${OCX_HOME:-...} fallback; "
        "literal path must be embedded at install time"
    )


def test_env_sh_activates_without_ocx_home_exported(tmp_path: Path) -> None:
    """Sourcing env.sh activates the same-session guard even when OCX_HOME
    is NOT in the environment (literal path embedded at install time).

    The ``_OCX_ENV_LOADED`` guard fires and then ``OCX_HOME`` is set from the
    literal embedded in the file.
    """
    ocx_home = tmp_path / "isolated_ocx"
    ocx_home.mkdir()
    env = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    # Write env.sh with literal path.
    _source_install_sh("create_env_sh", env)
    env_sh = ocx_home / "env.sh"
    assert env_sh.exists()

    # Source env.sh WITHOUT OCX_HOME in the environment.
    env_no_ocx_home = {
        "HOME": str(tmp_path / "home"),
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
    # The env.sh exports OCX_HOME from the embedded literal.
    assert str(ocx_home) in result.stdout, (
        f"env.sh must export OCX_HOME from the embedded literal path; "
        f"got stdout:\n{result.stdout}"
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
    """Sourcing env.sh twice in the same shell session must not add duplicate
    entries to PATH (the ``_OCX_ENV_LOADED`` guard prevents double-apply).

    C6 contract: env.sh guards with:
      ``if [ -n "${_OCX_ENV_LOADED:-}" ]; then return 0 2>/dev/null || true; fi``
      ``export _OCX_ENV_LOADED=1``

    A POSIX ``export PATH="<new>:${PATH}"`` in env.sh would duplicate the
    entry on re-source.  The guard prevents this.
    """
    ocx_home = tmp_path / "ocx"
    ocx_home.mkdir()
    env_gen = {
        "HOME": str(tmp_path / "home"),
        "OCX_HOME": str(ocx_home),
        "PATH": "/usr/bin:/bin",
    }
    _source_install_sh("create_env_sh", env_gen)
    env_sh = ocx_home / "env.sh"
    assert env_sh.exists()

    # Source env.sh twice and count how many times _OCX_ENV_LOADED appears
    # in the child's environment — if the guard works it must be exactly "1".
    result = _sh(
        f"""
. "{env_sh}"
. "{env_sh}"
echo "_OCX_ENV_LOADED=${{_OCX_ENV_LOADED:-}}"
""",
        {
            "HOME": str(tmp_path / "home"),
            "PATH": "/usr/bin:/bin",
        },
    )
    assert result.returncode == 0, (
        f"double-source must not fail; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # The guard variable must be set exactly once.
    assert "_OCX_ENV_LOADED=1" in result.stdout, (
        f"_OCX_ENV_LOADED must be '1' after double-source; stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# Nushell — create_env_nu / create_nu_autoload / detect_profile / uninstall
# ---------------------------------------------------------------------------


def test_create_env_nu_writes_literal_root(tmp_path: Path) -> None:
    """``create_env_nu`` writes $OCX_HOME/env.nu with the literal install root
    embedded, not a runtime ``$env.OCX_HOME`` reference.

    The file must contain the literal OCX_HOME path and the JSON-ingest body
    (``from json``, ``load-env``) but must NOT use ``$env.OCX_HOME`` as the
    source of the binary path.
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
    # Must embed the literal OCX_HOME path.
    assert str(ocx_home) in content, (
        f"env.nu must embed the literal OCX_HOME path ({ocx_home}); got:\n{content}"
    )
    # Must NOT use a runtime env variable as the binary root.
    assert "$env.OCX_HOME" not in content, (
        "env.nu must NOT use $env.OCX_HOME runtime var; literal path must be embedded"
    )
    # Must contain the JSON ingest body.
    assert "from json" in content, (
        f"env.nu must contain 'from json' for JSON ingest; got:\n{content}"
    )
    assert "load-env" in content, (
        f"env.nu must contain 'load-env' for env application; got:\n{content}"
    )


def test_create_nu_autoload_writes_literal_source(tmp_path: Path) -> None:
    """``create_nu_autoload`` writes the vendor/autoload/ocx.nu file with a
    literal ``source`` line pointing at ``$OCX_HOME/env.nu``.

    Nushell ``source`` is parse-time and cannot accept a runtime variable; the
    path must be a literal resolved at install time.
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
    # Must contain a literal source line with the OCX_HOME path embedded.
    assert f'source "{ocx_home}/env.nu"' in content, (
        f"ocx.nu must contain literal source line with {ocx_home}/env.nu; got:\n{content}"
    )
    # Must NOT use a runtime variable as the source path.
    assert "$env.OCX_HOME" not in content, (
        "ocx.nu must NOT use $env.OCX_HOME in the source line — parse-time constraint"
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


def test_create_env_elv_writes_literal_root(tmp_path: Path) -> None:
    """``create_env_elv`` writes $OCX_HOME/env.elv with the literal install root
    embedded, not a runtime ``$E:OCX_HOME`` reference.

    The file must contain the literal OCX_HOME path and the eval-slurp body.
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
    # Must embed the literal OCX_HOME path.
    assert str(ocx_home) in content, (
        f"env.elv must embed the literal OCX_HOME path ({ocx_home}); got:\n{content}"
    )
    # Must NOT use a runtime env variable as the binary root.
    assert "$E:OCX_HOME" not in content, (
        "env.elv must NOT use $E:OCX_HOME runtime var; literal path must be embedded"
    )
    # Must contain the elvish eval-slurp activation pattern.
    assert "--shell=elvish" in content, (
        f"env.elv must invoke ocx with --shell=elvish; got:\n{content}"
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
