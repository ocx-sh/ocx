# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests — `ocx launcher exec` internal subcommand.

These tests exercise the hidden `ocx launcher exec` subcommand that generated
entry-point launchers call at runtime.  The stable wire ABI is:

    ocx launcher exec '<pkg-root>' -- <argv0> [args...]

Scenarios covered:
- Success: valid installed package root, entrypoint binary resolves and runs.
- Missing pkg-root: non-existent path → exit 64 (UsageError).
- pkg-root outside $OCX_HOME/packages/: path escapes packages root → exit 64.
- Missing metadata.json: path inside packages/ but no metadata.json → exit 64.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src import exec_state
from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner

pytestmark = pytest.mark.command("launcher*")

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _get_package_root(ocx: OcxRunner, pkg_short: str) -> Path:
    """Return the on-disk package root for an installed package."""
    result = ocx.json("package", "which", pkg_short)["paths"]
    return Path(result[pkg_short]["path"])


# ---------------------------------------------------------------------------
# Success path
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_exec_success(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx launcher exec <pkg_root> -- hello` resolves and runs the entrypoint binary."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", pkg.short)

    pkg_root = _get_package_root(ocx, pkg.short)

    result = ocx.run("launcher", "exec", str(pkg_root), "--", "hello", format=None, check=False)
    assert result.returncode == 0, (
        f"launcher exec with valid pkg-root must succeed; "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert pkg.marker in result.stdout, (
        f"launcher exec must run the resolved binary — marker missing; "
        f"stdout={result.stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_exec_forwards_args(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx launcher exec <pkg_root> -- hello extra` forwards extra args to the binary."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", pkg.short)

    pkg_root = _get_package_root(ocx, pkg.short)
    extra_arg = "extra-from-launcher-exec"

    result = ocx.run("launcher", "exec", str(pkg_root), "--", "hello", extra_arg, format=None, check=False)
    assert result.returncode == 0, (
        f"launcher exec with extra args must succeed; "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert extra_arg in result.stdout, (
        f"launcher exec must forward extra args verbatim; stdout={result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------


def test_launcher_exec_missing_pkg_root_exits_64(ocx: OcxRunner) -> None:
    """`ocx launcher exec /nonexistent/path -- hello` must exit 64 (UsageError)."""
    nonexistent = "/nonexistent/pkg-root-does-not-exist"
    result = ocx.run("launcher", "exec", nonexistent, "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"non-existent pkg-root must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )


def test_launcher_exec_relative_path_exits_64(ocx: OcxRunner) -> None:
    """`ocx launcher exec relative/path -- hello` must exit 64 (non-absolute)."""
    result = ocx.run("launcher", "exec", "relative/path", "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"relative pkg-root must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "absolute" in result.stderr.lower(), (
        f"error must mention 'absolute'; stderr={result.stderr.strip()!r}"
    )


def test_launcher_exec_outside_packages_root_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """`ocx launcher exec` with a path outside $OCX_HOME/packages/ must exit 64."""
    outside = tmp_path / "outside-packages-root"
    outside.mkdir()

    result = ocx.run("launcher", "exec", str(outside), "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"pkg-root outside OCX_HOME/packages/ must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )


def test_launcher_exec_missing_metadata_json_exits_64(ocx: OcxRunner) -> None:
    """`ocx launcher exec` on a path inside packages/ with no metadata.json must exit 64."""
    # Construct a fake path inside OCX_HOME/packages/; the directory exists
    # but contains no metadata.json — validate_launcher_pkg_root rejects it.
    fake = Path(str(ocx.ocx_home)) / "packages" / "fake.example.com" / "sha256" / "ab" / "fake"
    fake.mkdir(parents=True, exist_ok=True)

    result = ocx.run("launcher", "exec", str(fake), "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"pkg-root without metadata.json must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "metadata.json" in result.stderr, (
        f"error must mention 'metadata.json'; stderr={result.stderr.strip()!r}"
    )


# ---------------------------------------------------------------------------
# Shadow rule at the launcher hop
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX executable bit has no Windows analogue")
def test_launcher_hop_refuses_a_non_executable_dispatch_target(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A package's own non-executable target must not be passed over here either.

    A generated entrypoint re-enters `ocx launcher exec` as a *fresh* process,
    so this hop makes its own resolution decision. Resolving it the way the OS
    would means a package shipping a 0644 dispatch target silently runs the
    host's same-named binary — #268, one process later, and invisible to the
    `ocx package test` coverage because a different code path decides it.

    The decoy is what makes this discriminating: it prints a marker no package
    binary ever prints, so a green run that executed it is visible.
    """
    decoy_dir = tmp_path / "host-bin"
    decoy_dir.mkdir()
    decoy_marker = f"decoy-{uuid4().hex[:12]}"
    decoy = decoy_dir / "hello"
    decoy.write_text(f"#!/bin/sh\necho {decoy_marker}\n")
    decoy.chmod(decoy.stat().st_mode | stat.S_IEXEC)

    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
        bin_exec={"hello": False},
    )
    ocx.plain("package", "install", "--select", pkg.short)

    launcher = _get_package_root(ocx, pkg.short) / "entrypoints" / "hello"
    assert launcher.exists(), f"the generated launcher must exist: {launcher}"

    launcher_env = dict(ocx.env)
    # The decoy leads PATH; the ocx dir follows so the launcher can re-enter.
    launcher_env["PATH"] = os.pathsep.join(
        [str(decoy_dir), str(ocx.binary.parent), launcher_env.get("PATH", "")]
    )
    completed = subprocess.run(
        [str(launcher)],
        capture_output=True,
        text=True,
        env=launcher_env,
        timeout=30,
        check=False,
    )

    assert completed.returncode == 65, (
        f"a non-executable dispatch target must exit 65 (DataError), got "
        f"{completed.returncode}\nstdout: {completed.stdout}\nstderr: {completed.stderr}"
    )
    assert decoy_marker not in completed.stdout, (
        f"the host decoy must never run; stdout:\n{completed.stdout}"
    )
    assert f"bin{os.sep}hello" in completed.stderr, (
        f"stderr must name the package path of the offending file, got:\n{completed.stderr}"
    )


# ---------------------------------------------------------------------------
# Layering at the launcher hop
# ---------------------------------------------------------------------------


def _dumped(stdout: str, key: str) -> str | None:
    """The value of ``key`` in an ``env`` dump, or None when absent."""
    prefix = f"{key}="
    return next((line[len(prefix) :] for line in stdout.splitlines() if line.startswith(prefix)), None)


def _state_dumped(stdout: str) -> dict:
    state = exec_state.decode(_dumped(stdout, "__OCX_ENV_STATE"))
    assert isinstance(state, dict), f"the child must receive a readable execution state; got {state!r}"
    return state


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_reentry_layers_the_state_like_exec_does(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A launcher hands its child the state it received, layered, with the replay unchanged.

    The same ``package exec`` runs ``env`` directly and through the package's
    generated launcher. The launcher re-applies the received ``replay.env`` after
    the package's own value, so the override wins on both paths, and what it
    passes on keeps every record the outer frame wrote and the replay as it was.
    """
    var_name = f"T_{unique_repo.upper().replace('-', '_')}_LAYER"
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        env=[
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
            {"key": var_name, "type": "constant", "value": "package-value", "visibility": "public"},
        ],
    )
    ocx.plain("package", "install", pkg.short)
    override = f"{var_name}=flag-value"

    direct = ocx.run("package", "exec", "--env", override, pkg.short, "--", "env", format=None, check=False)
    hopped = ocx.run("package", "exec", "--env", override, pkg.short, "--", "showenv", format=None, check=False)
    assert direct.returncode == 0, direct.stderr
    assert hopped.returncode == 0, hopped.stderr

    assert _dumped(direct.stdout, var_name) == "flag-value", direct.stdout
    assert _dumped(hopped.stdout, var_name) == "flag-value", (
        f"the received replay must be applied after the launcher's package env; got:\n{hopped.stdout}"
    )

    outer, inner = _state_dumped(direct.stdout), _state_dumped(hopped.stdout)
    assert inner["replay"]["env"] == outer["replay"]["env"], (
        f"the launcher passes the received replay on unchanged; {json.dumps(inner['replay'])}"
    )
    assert inner["replay"]["env"], "the --env override must be in the replay, or this proves nothing"
    assert inner["inherited_scopes"] == outer["inherited_scopes"]
    outer_keys = {(record["key"], record["kind"]) for record in outer["applied"]}
    inner_keys = {(record["key"], record["kind"]) for record in inner["applied"]}
    assert outer_keys <= inner_keys, (
        f"the launcher layers on the received records and drops none; missing {sorted(outer_keys - inner_keys)}"
    )
