# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the CLI plugin dispatch contract.

Contract source: ``.claude/artifacts/adr_cli_plugin_pattern.md`` (decisions 1–7)
and ``plan_cli_plugin_pattern.md`` Phase 3 acceptance table (9 cases).

Implementation is complete — all 10 tests pass against the production binary.
"""
from __future__ import annotations

import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Shared constants
# ---------------------------------------------------------------------------

# sysexits.h EX_USAGE (64) — expected exit code for plugin-not-found
EX_USAGE = 64


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def plugin_bin_dir(tmp_path: Path) -> Path:
    """Stage ``ocx-fixture`` and ``ocx-do-thing`` shell scripts in a temp dir.

    The fixture echoes its argv layout and selected OCX env vars to stdout,
    then exits with ``$OCX_FIXTURE_EXIT_CODE`` if set, else 0.  Tests
    prepend the returned directory to PATH when invoking ocx.

    Script contract:
      stdout line 1: ``ARGV:<argv0>|<argv1>|...``  (space-joined remainder)
      stdout line 2: ``ENV:OCX_HOME=...|OCX_OFFLINE=...|...``
    """
    plugin_body = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # ocx-fixture / ocx-do-thing — test plugin stub for acceptance tests.
        # Echoes argv layout and selected OCX env vars; exits with configurable code.
        echo "ARGV:$0|$*"
        echo "ENV:OCX_HOME=${OCX_HOME:-}|OCX_OFFLINE=${OCX_OFFLINE:-}|OCX_REMOTE=${OCX_REMOTE:-}|OCX_DEFAULT_REGISTRY=${OCX_DEFAULT_REGISTRY:-}|OCX_BINARY_PIN=${OCX_BINARY_PIN:-}"
        exit "${OCX_FIXTURE_EXIT_CODE:-0}"
        """
    )

    for name in ("ocx-fixture", "ocx-do-thing"):
        script_path = tmp_path / name
        script_path.write_text(plugin_body)
        script_path.chmod(
            script_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
        )

    return tmp_path


@pytest.fixture()
def fake_ocx_package_dir(tmp_path: Path) -> Path:
    """Stage a fake ``ocx-package`` binary in an isolated temp dir.

    This binary writes a sentinel marker to stdout and exits 99.  Tests use
    it to verify that built-in ``package`` commands are never shadowed.
    """
    body = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        echo "SHADOW_MARKER:ocx-package-invoked"
        exit 99
        """
    )
    script_path = tmp_path / "ocx-package"
    script_path.write_text(body)
    script_path.chmod(
        script_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
    )
    return tmp_path


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


def _run_ocx(
    ocx: OcxRunner,
    *args: str,
    extra_path_dir: Path | None = None,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run the ocx binary directly (no --format wrapper) and return the result.

    ``check=False`` — callers assert on returncode themselves.
    ``timeout=30`` — prevents worker hang if a fixture script bugs out.
    """
    env = dict(ocx.env)
    if extra_path_dir is not None:
        env["PATH"] = str(extra_path_dir) + os.pathsep + env.get("PATH", "")
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=30,
    )


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_unknown_subcommand_execs_plugin(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """``ocx fixture --foo bar`` with ``ocx-fixture`` on PATH → exec'd; exit 0.

    ADR decision 1 (External clap variant) + decision 3 (dispatch before
    Context::try_init).  The plugin is exec'd and its stdout contains the
    expected ARGV layout.

    Contract (ADR decision 4, git convention): argv[0]=<path-to-ocx-fixture>,
    user args follow with the subcommand name DROPPED — so the plugin sees
    ``--foo bar`` (NOT ``fixture --foo bar``), identical to running the
    standalone binary directly.  The fixture echoes ``ARGV:<argv0>|<space-joined
    user args>``, so parsing the portion after ``|`` locks the convention; a
    bare substring check would not (``fixture`` also appears in argv0's path).
    """
    result = _run_ocx(ocx, "fixture", "--foo", "bar", extra_path_dir=plugin_bin_dir)
    assert result.returncode == 0, (
        f"expected exit 0 from plugin, got {result.returncode}\n"
        f"stdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )
    assert "ARGV:" in result.stdout, f"expected ARGV: line in stdout; got {result.stdout!r}"
    argv_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("ARGV:")), ""
    )
    argv0, _, forwarded = argv_line.removeprefix("ARGV:").partition("|")
    # argv0 is the resolved plugin binary path (proves the right plugin ran).
    assert argv0.endswith("ocx-fixture"), (
        f"expected argv0 to be the ocx-fixture binary path; got {argv0!r}"
    )
    # Git convention: only the user args reach the plugin, name dropped.
    assert forwarded.split() == ["--foo", "bar"], (
        f"git convention: plugin must receive only user args ['--foo', 'bar'], "
        f"not the subcommand name; got forwarded args {forwarded!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_env_forwarded_to_plugin(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """``ocx --offline fixture`` forwards ``OCX_OFFLINE=true`` + ``OCX_HOME`` to plugin env.

    ADR decision 3 (env forward via Env::apply_ocx_config).  OCX_OFFLINE must
    be non-empty/true; OCX_HOME must equal the test-isolated OCX_HOME value.
    OCX_BINARY_PIN must be set non-empty (always set by apply_ocx_config).
    """
    ocx_home_val = str(ocx.ocx_home)
    result = _run_ocx(
        ocx,
        "--offline",
        "fixture",
        extra_path_dir=plugin_bin_dir,
    )
    assert result.returncode == 0, (
        f"expected exit 0 from plugin, got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    env_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("ENV:")), ""
    )
    assert env_line, f"expected ENV: line in stdout; got {result.stdout!r}"
    # OCX_OFFLINE must be forwarded and truthy
    assert "OCX_OFFLINE=true" in env_line or "OCX_OFFLINE=1" in env_line, (
        f"expected OCX_OFFLINE=true or OCX_OFFLINE=1 in ENV line; got {env_line!r}"
    )
    # OCX_HOME must be forwarded with the correct value
    assert f"OCX_HOME={ocx_home_val}" in env_line, (
        f"expected OCX_HOME={ocx_home_val!r} in ENV line; got {env_line!r}"
    )
    # OCX_BINARY_PIN must be set non-empty — apply_ocx_config always sets it so
    # nested ocx invocations inside the plugin pin to the same binary.
    assert "OCX_BINARY_PIN=" in env_line, (
        f"expected OCX_BINARY_PIN key in ENV line; got {env_line!r}"
    )
    # Extract value after OCX_BINARY_PIN= and verify it is non-empty
    binary_pin_part = next(
        (part for part in env_line.split("|") if part.startswith("OCX_BINARY_PIN=")), ""
    )
    binary_pin_val = binary_pin_part.removeprefix("OCX_BINARY_PIN=")
    assert binary_pin_val, (
        f"expected OCX_BINARY_PIN to be non-empty; got {binary_pin_part!r} in {env_line!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_remote_env_forwarded_to_plugin(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """``ocx --remote fixture`` forwards ``OCX_REMOTE=true`` to plugin env.

    Verifies that resolution-affecting flags forwarded by apply_ocx_config
    reach the child plugin process (TC F1 — OCX_REMOTE coverage).
    """
    result = _run_ocx(
        ocx,
        "--remote",
        "fixture",
        extra_path_dir=plugin_bin_dir,
    )
    assert result.returncode == 0, (
        f"expected exit 0 from plugin, got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    env_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("ENV:")), ""
    )
    assert env_line, f"expected ENV: line in stdout; got {result.stdout!r}"
    assert "OCX_REMOTE=true" in env_line or "OCX_REMOTE=1" in env_line, (
        f"expected OCX_REMOTE=true or OCX_REMOTE=1 in ENV line; got {env_line!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_exit_code_propagation(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """Plugin exits 42 → ``ocx`` exits 42.

    ADR decision 3 (exit code via propagate_exit_code).
    """
    result = _run_ocx(
        ocx,
        "fixture",
        extra_path_dir=plugin_bin_dir,
        extra_env={"OCX_FIXTURE_EXIT_CODE": "42"},
    )
    assert result.returncode == 42, (
        f"expected ocx to propagate plugin exit code 42; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )


def test_plugin_not_found(ocx: OcxRunner) -> None:
    """``ocx nonexistent-plugin-xyz-12345`` → exit 64; stderr contains "unknown subcommand"
    and the install hint mentioning ``ocx --global add``.

    ADR decision 3 (plugin-not-found exits EX_USAGE=64 with install hint).
    No plugin dir on PATH — ``which::which`` will find nothing.
    """
    result = _run_ocx(ocx, "nonexistent-plugin-xyz-12345")
    assert result.returncode == EX_USAGE, (
        f"expected exit 64 (EX_USAGE) for unknown subcommand; got {result.returncode}\n"
        f"stdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )
    combined_output = result.stdout + result.stderr
    assert "unknown subcommand" in combined_output, (
        f"expected 'unknown subcommand' in output; got {combined_output!r}"
    )
    assert "ocx --global add" in combined_output, (
        f"expected install hint mentioning 'ocx --global add'; got {combined_output!r}"
    )
    # Correct three-segment OCI identifier (registry/namespace/package), not the
    # GitHub org slug. First-party plugins publish under ocx.sh/ocx/<name>.
    assert "ocx.sh/ocx/nonexistent-plugin-xyz-12345" in combined_output, (
        f"expected 'ocx.sh/ocx/<name>' identifier in hint; got {combined_output!r}"
    )
    assert "ocx-sh/ocx-" not in combined_output, (
        f"hint must not use the GitHub org slug 'ocx-sh/ocx-'; got {combined_output!r}"
    )
    # Honest framing: the add form is the official-plugin path, with a generic
    # PATH fallback for third-party plugins — not an unconditional promise.
    assert "official ocx plugin" in combined_output.lower(), (
        f"expected official-plugin framing in hint; got {combined_output!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_builtin_shadow_guard(
    ocx: OcxRunner, fake_ocx_package_dir: Path, plugin_bin_dir: Path
) -> None:
    """Built-in ``package`` command takes precedence over a fake ``ocx-package`` on PATH.

    ADR decision 1: built-ins always shadow plugins (clap matches built-ins first;
    External only fires for unrecognised names).

    Staged fake ``ocx-package`` exits 99 and emits ``SHADOW_MARKER``.  If the
    built-in is correctly shadowed, running ``ocx package --help`` exits 0 and
    stdout does NOT contain ``SHADOW_MARKER``.
    """
    # Put the fake ocx-package BEFORE anything else on PATH
    result = _run_ocx(
        ocx,
        "package",
        "--help",
        extra_path_dir=fake_ocx_package_dir,
    )
    assert result.returncode == 0, (
        f"expected built-in 'package --help' to exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert "SHADOW_MARKER" not in combined, (
        f"fake ocx-package was invoked instead of built-in; output: {combined!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_builtin_shadow_guard_at_subcommand_depth(
    ocx: OcxRunner, fake_ocx_package_dir: Path
) -> None:
    """Built-in ``package install`` (subcommand depth) is not intercepted by a fake ``ocx-package``.

    Sec W1: deeper shadow case that locks the invariant at subcommand depth.
    Even with a rogue ``ocx-package`` binary first on PATH, ``ocx package install --help``
    must route to the built-in ``package`` group (exit 0, no SHADOW_MARKER).

    Uses ``--help`` form to avoid touching the real registry while still exercising
    the full built-in dispatch path.
    """
    result = _run_ocx(
        ocx,
        "package",
        "install",
        "--help",
        extra_path_dir=fake_ocx_package_dir,
    )
    assert result.returncode == 0, (
        f"expected built-in 'package install --help' to exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert "SHADOW_MARKER" not in combined, (
        f"fake ocx-package was invoked instead of built-in at subcommand depth; output: {combined!r}"
    )
    # Confirm the built-in help text mentions the install subcommand
    assert "install" in combined.lower(), (
        f"expected 'install' in package install --help output; got {combined!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_help_forwarding(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """``ocx fixture --help`` → exec'd plugin receives only ``--help`` in argv.

    ADR decision 5 (``ocx <plugin> --help`` exec-forwards to the plugin) +
    decision 4 (git convention): the plugin must see exactly ``--help``, never
    ``fixture --help`` — a clap plugin would reject its own name as an unknown
    subcommand.  Regression guard for the bug where the name was passed through.
    """
    result = _run_ocx(ocx, "fixture", "--help", extra_path_dir=plugin_bin_dir)
    assert result.returncode == 0, (
        f"expected exit 0 from plugin --help; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    argv_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("ARGV:")), ""
    )
    _, _, forwarded = argv_line.removeprefix("ARGV:").partition("|")
    assert forwarded.split() == ["--help"], (
        f"git convention: 'ocx fixture --help' must forward only ['--help'], "
        f"not the subcommand name; got forwarded args {forwarded!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_help_subcommand_form(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """``ocx help fixture`` rewrites to ``ocx-fixture --help`` and exits 0.

    ADR open question #3 / plan decision: ``argv == ["help", X]`` rewrites to
    ``[X, "--help"]`` so ``ocx help foo`` dispatches ``ocx-foo --help`` instead
    of ``ocx-help foo``.

    Unit tests cover the rewrite logic in isolation (``rewrite_help_invocation``).
    This acceptance test verifies the rewrite is wired end-to-end.
    """
    result = _run_ocx(ocx, "help", "fixture", extra_path_dir=plugin_bin_dir)
    assert result.returncode == 0, (
        f"expected exit 0 from 'ocx help fixture'; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    argv_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("ARGV:")), ""
    )
    _, _, forwarded = argv_line.removeprefix("ARGV:").partition("|")
    # Rewrite + git convention: 'ocx help fixture' dispatches 'ocx-fixture --help'.
    assert forwarded.split() == ["--help"], (
        f"'ocx help fixture' must dispatch 'ocx-fixture --help' (only ['--help'] "
        f"forwarded); got forwarded args {forwarded!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_plugin_with_hyphenated_name(
    ocx: OcxRunner, plugin_bin_dir: Path
) -> None:
    """``ocx do-thing`` with ``ocx-do-thing`` on PATH → exec'd; exit 0.

    ADR edge case: hyphenated subcommand names.  ``ocx do-thing`` looks for
    ``ocx-do-thing`` on PATH.
    """
    result = _run_ocx(ocx, "do-thing", extra_path_dir=plugin_bin_dir)
    assert result.returncode == 0, (
        f"expected exit 0 from hyphenated plugin; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    argv_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("ARGV:")), ""
    )
    assert "do-thing" in argv_line, (
        f"expected 'do-thing' in ARGV line for hyphenated plugin; got {argv_line!r}"
    )


def test_help_list_regression(ocx: OcxRunner) -> None:
    """``ocx --help`` lists built-in commands ``package`` and ``env``; exits 0.

    Smoke-checks that adding the ``External`` clap variant did not disrupt the
    built-in command listing.  No plugin staged on PATH.
    """
    result = _run_ocx(ocx, "--help")
    assert result.returncode == 0, (
        f"expected exit 0 from 'ocx --help'; got {result.returncode}\n"
        f"stderr: {result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert "package" in combined, (
        f"expected 'package' in --help output; got {combined!r}"
    )
    assert "env" in combined, (
        f"expected 'env' in --help output; got {combined!r}"
    )


def test_help_builtin_form_not_mistaken_for_plugin(ocx: OcxRunner) -> None:
    """``ocx help package`` must show built-in help, not the plugin install hint.

    Regression guard for the disable_help_subcommand fix: ensures built-in
    subcommand names are detected BEFORE PATH lookup for ``ocx-<name>``.

    Without the fix, ``ocx help package`` is routed through External as
    ``["package", "--help"]`` after the rewrite, then ``resolve_plugin_binary``
    finds no ``ocx-package`` on PATH and emits the misleading install hint
    ``ocx --global add ocx.sh/ocx/package``.

    With the fix, ``find_subcommand("package")`` recognises the built-in and
    clap's own help printer is called directly, exiting 0 with the subcommand
    help text.
    """
    result = _run_ocx(ocx, "help", "package")
    assert result.returncode == 0, (
        f"exit {result.returncode}\nstdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    # Must NOT contain the install hint produced by the not-found path
    # (check both cases since message is lowercase)
    assert "install with:" not in combined.lower(), (
        f"install hint must not appear for a built-in subcommand; got {combined!r}"
    )
    assert "ocx --global add" not in combined, (
        f"install hint must not appear for a built-in subcommand; got {combined!r}"
    )
    # Must contain something recognisable as the package subcommand's help —
    # at minimum the subcommand name itself (clap always includes it in Usage).
    assert "package" in combined.lower(), (
        f"expected 'package' in help output; got {combined!r}"
    )


def test_bare_help_prints_top_level(ocx: OcxRunner) -> None:
    """``ocx help`` (no subcommand argument) must print top-level help, exit 0.

    Regression guard for ``disable_help_subcommand = true`` interaction with
    bare ``help``: without the special-case in ``dispatch``, ``["help"]``
    routes through External and ``resolve_plugin_binary("help")`` looks for
    ``ocx-help`` on PATH — wrong. With the special-case, clap's top-level
    ``print_help`` is invoked directly and the user sees the same output as
    ``ocx --help``.

    Co-guards ``test_help_survives_invalid_ambient_config`` in test_config.py
    which exercises this path with a malformed config file (the bare ``help``
    bypass of ``Context::try_init`` is precisely what lets help survive
    broken config).
    """
    result = _run_ocx(ocx, "help")
    assert result.returncode == 0, (
        f"exit {result.returncode}\nstdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert "install with:" not in combined.lower(), (
        f"install hint must not appear for bare 'ocx help'; got {combined!r}"
    )
    assert "package" in combined, (
        f"expected built-in 'package' in top-level help; got {combined!r}"
    )
