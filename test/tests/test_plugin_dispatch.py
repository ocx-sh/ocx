# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the CLI plugin dispatch contract.

Contract source: ``.claude/artifacts/adr_cli_plugin_pattern.md`` (decisions 1–7).

Implementation is complete — all 10 tests pass against the production binary.
"""
from __future__ import annotations

import json
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
def reporter_bin_dir(tmp_path: Path) -> Path:
    """Stage ``ocx-reporter``: prints what reached it, then runs its own ``ocx`` child.

    Output: ``SET:<name>`` for each probed variable that is set (never its value), then
    ``ocx --format json shell state`` of the child ocx on one line behind ``STATE:``.
    The child is the pinned binary, as a real plugin's would be.
    """
    body = textwrap.dedent(
        r"""
        #!/usr/bin/env bash
        for name in OCX_OFFLINE OCX_FROZEN OCX_REMOTE OCX_BINARY_PIN OCX_TOOLCHAIN_DIR \
                    OCX_IDENTITY_TOKEN OCX_KEY_PASSWORD OCX_SIGNING_KEY OCX_ANNOUNCE_GIT_TOKEN \
                    OCX_ANNOUNCE_TOKEN; do
            if [ -n "${!name+x}" ]; then echo "SET:$name"; fi
        done
        echo "HEADER:${__OCX_ENV_STATE%%.*}"
        state=$("${OCX_BINARY_PIN:-ocx}" --format json shell state) || exit 3
        echo "STATE:$(printf '%s' "$state" | tr -d '\n')"
        """
    ).lstrip("\n")
    script_path = tmp_path / "ocx-reporter"
    script_path.write_text(body)
    script_path.chmod(script_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
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
    """``ocx --offline fixture``: ``OCX_HOME`` and ``OCX_BINARY_PIN`` reach the plugin; ``OCX_OFFLINE`` does not.

    The narrowing travels in the inherited execution state (see
    ``test_plugins_own_ocx_child_honours_the_dispatching_parents_narrowing``), not as a public variable.
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
    parts = dict(part.split("=", 1) for part in env_line.removeprefix("ENV:").split("|"))
    assert parts["OCX_OFFLINE"] == "", f"OCX_OFFLINE must not be written for the plugin; got {env_line!r}"
    assert parts["OCX_HOME"] == ocx_home_val, f"expected OCX_HOME={ocx_home_val!r}; got {env_line!r}"
    # Nested ocx invocations inside the plugin pin to the dispatching binary.
    assert parts["OCX_BINARY_PIN"], f"expected a non-empty OCX_BINARY_PIN; got {env_line!r}"


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_remote_flag_reaches_the_plugins_ocx_child(
    ocx: OcxRunner, reporter_bin_dir: Path
) -> None:
    """``ocx --remote reporter``: the plugin's ocx child sees ``remote`` in the received policy."""
    result = _run_ocx(ocx, "--remote", "reporter", extra_path_dir=reporter_bin_dir, extra_env={"OCX_NO_PROJECT": "1"})
    set_names, _, report = _report(result)
    assert "OCX_REMOTE" not in set_names, f"OCX_REMOTE must not be written for the plugin: {set_names}"
    assert report["env_state"]["policy"]["remote"] is True, report["env_state"]


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


# ---------------------------------------------------------------------------
# Credential exemption — S-025
# ---------------------------------------------------------------------------


@pytest.fixture()
def credential_probe_dir(tmp_path: Path) -> Path:
    """Stage ``ocx-credprobe``: a plugin that reports what it inherited.

    Its own fixture rather than a line added to ``plugin_bin_dir``: the probe
    must dump *every* credential name plus one ordinary variable, and folding
    that into the shared fixture would make eight unrelated tests depend on the
    credential list.
    """
    body = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # ocx-credprobe — reports which credential vars reached a plugin.
        echo "SIGNING_KEY:${OCX_SIGNING_KEY:-<absent>}"
        echo "KEY_PASSWORD:${OCX_KEY_PASSWORD:-<absent>}"
        echo "IDENTITY_TOKEN:${OCX_IDENTITY_TOKEN:-<absent>}"
        echo "ORDINARY:${OCX_CREDPROBE_CONTROL:-<absent>}"
        """
    )
    script_path = tmp_path / "ocx-credprobe"
    script_path.write_text(body)
    script_path.chmod(
        script_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
    )
    return tmp_path


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_plugin_never_inherits_a_credential_variable(
    ocx: OcxRunner, credential_probe_dir: Path
) -> None:
    """S-025: ``OCX_SIGNING_KEY`` does not reach a dispatched ``ocx-<plugin>``.

    A plugin is third-party code and inherits the ambient environment on
    purpose (trust boundary, ``app/plugin_dispatch.rs``), so ``Command::envs``
    dropping a key from the composed map is not enough — the removal has to be
    an explicit ``env_remove`` for every ``CREDENTIAL_KEYS`` entry. This drives
    that through the real binary with all three set in the parent env.

    ``OCX_SIGNING_KEY`` is the one that matters here: it holds a raw private
    key PEM rather than a short-lived token, and ``--key file:<path>`` — the
    spelling ``env://`` replaces — hands a plugin no pointer at all.

    ``ORDINARY`` is the positive control, and it is what makes the three
    absences evidence. Without it, a plugin that failed to launch, or one that
    inherited nothing whatsoever, would report every credential absent and pass
    for entirely the wrong reason.
    """
    secret = "-----BEGIN ENCRYPTED SIGSTORE PRIVATE KEY-----\nnot-a-real-key\n"
    result = _run_ocx(
        ocx,
        "credprobe",
        extra_path_dir=credential_probe_dir,
        extra_env={
            "OCX_SIGNING_KEY": secret,
            "OCX_KEY_PASSWORD": "probe-password",
            "OCX_IDENTITY_TOKEN": "probe-token",
            "OCX_CREDPROBE_CONTROL": "inherited",
        },
    )
    assert result.returncode == 0, (
        f"the probe plugin must run; exit {result.returncode}\n"
        f"stdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )
    lines = dict(
        line.split(":", 1) for line in result.stdout.splitlines() if ":" in line
    )
    assert lines.get("ORDINARY") == "inherited", (
        "positive control: a plugin inherits the ambient environment, so the "
        "absences below mean the credentials were removed rather than that "
        f"nothing was passed at all; got {result.stdout!r}"
    )
    for name in ("SIGNING_KEY", "KEY_PASSWORD", "IDENTITY_TOKEN"):
        assert lines.get(name) == "<absent>", (
            f"OCX_{name} reached the plugin; got {result.stdout!r}"
        )
    assert secret.strip() not in result.stdout, (
        "the key material itself must not appear anywhere in the plugin's output"
    )


def _report(result: subprocess.CompletedProcess[str]) -> tuple[set[str], str, dict]:
    """(probed variables that were set, the received header, the child's ``shell state`` report)."""
    assert result.returncode == 0, f"plugin failed: {result.returncode}\nstderr: {result.stderr!r}"
    set_names = {line.removeprefix("SET:") for line in result.stdout.splitlines() if line.startswith("SET:")}
    header = next(line.removeprefix("HEADER:") for line in result.stdout.splitlines() if line.startswith("HEADER:"))
    state = next(line.removeprefix("STATE:") for line in result.stdout.splitlines() if line.startswith("STATE:"))
    return set_names, header, json.loads(state)


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
def test_plugin_receives_ambient_input_but_no_credentials(ocx: OcxRunner, reporter_bin_dir: Path) -> None:
    """Ambient ``OCX_TOOLCHAIN_DIR`` and ``OCX_ANNOUNCE_TOKEN`` reach a plugin; bearer credentials do not."""
    toolchain = ocx.ocx_home / "toolchain-fixture"
    toolchain.mkdir(mode=0o700)
    result = _run_ocx(
        ocx,
        "reporter",
        extra_path_dir=reporter_bin_dir,
        extra_env={
            "OCX_NO_PROJECT": "1",
            "OCX_TOOLCHAIN_DIR": str(toolchain),
            "OCX_ANNOUNCE_TOKEN": "fixture-secret-value",
            "OCX_IDENTITY_TOKEN": "fixture-secret-value",
            "OCX_KEY_PASSWORD": "fixture-secret-value",
            "OCX_SIGNING_KEY": "fixture-secret-value",
            "OCX_ANNOUNCE_GIT_TOKEN": "fixture-secret-value",
        },
    )
    set_names, _, _ = _report(result)
    assert {"OCX_TOOLCHAIN_DIR", "OCX_ANNOUNCE_TOKEN"} <= set_names, f"inherited input missing: {set_names}"
    leaked = set_names & {"OCX_IDENTITY_TOKEN", "OCX_KEY_PASSWORD", "OCX_SIGNING_KEY", "OCX_ANNOUNCE_GIT_TOKEN"}
    assert not leaked, f"credentials reached the plugin: {leaked}"


@pytest.mark.skipif(sys.platform == "win32", reason="bash plugin scripts require POSIX shell")
@pytest.mark.parametrize("flag", ["--offline", "--frozen"])
def test_plugins_own_ocx_child_honours_the_dispatching_parents_narrowing(
    ocx: OcxRunner, reporter_bin_dir: Path, flag: str
) -> None:
    """``ocx --offline|--frozen <plugin>``: the plugin's ocx child reports it through the inherited state.

    The narrowing travels in ``__OCX_ENV_STATE``'s header; the public ``OCX_OFFLINE`` / ``OCX_FROZEN``
    are not written for the plugin.
    """
    name = flag.removeprefix("--")
    result = _run_ocx(ocx, flag, "reporter", extra_path_dir=reporter_bin_dir, extra_env={"OCX_NO_PROJECT": "1"})
    set_names, header, report = _report(result)
    assert not {"OCX_OFFLINE", "OCX_FROZEN", "OCX_REMOTE"} & set_names, f"public switches written: {set_names}"
    assert name in header, f"the plugin's header must name {name}: {header!r}"
    env_state = report["env_state"]
    assert env_state["status"] == "readable", env_state
    assert name in env_state["narrowing"], f"the child ocx does not report {name}: {env_state}"
