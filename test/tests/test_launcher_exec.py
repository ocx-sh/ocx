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
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src import exec_state
from src.helpers import make_package_with_entrypoints
from src.launcher_fixtures import (
    DEPTH_VAR,
    install_entrypoint_package,
    launcher_env,
    only_generation,
    run_launcher,
    stub_calls,
    write_stub_ocx,
)
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

    launcher = (
        only_generation(_get_package_root(ocx, pkg.short) / "launchers") / "hello"
    )
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


# ---------------------------------------------------------------------------
# The home launcher program: frozen argv, program search, loop limit
# ---------------------------------------------------------------------------

unix_only = pytest.mark.skipif(
    sys.platform == "win32",
    reason="the stub ocx is a /bin/sh script; a Windows launcher refuses batch programs",
)


@unix_only
def test_package_launcher_builds_the_frozen_argv_and_counts_one_hop(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """a `package` launcher starts `ocx launcher exec <package_root> -- NAME args…` at depth 1."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(
        installed.launcher,
        "one",
        "two words",
        env=launcher_env(ocx, OCX_BINARY_PIN=str(stub)),
    )

    assert result.returncode == 0, result.stderr
    calls = stub_calls(log)
    assert [call["argv"] for call in calls] == [
        [
            "launcher",
            "exec",
            str(installed.package_root),
            "--",
            "hello",
            "one",
            "two words",
        ]
    ]
    assert calls[0]["depth"] == "1", (
        f"an unset counter becomes 1; got {calls[0]['depth']!r}"
    )


@unix_only
def test_package_launcher_record_is_a_package_record_for_the_installed_root(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """the launcher is published with `NAME.launch`, kind `package`, naming the package root."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)

    record = json.loads(installed.record.read_text(encoding="utf-8"))

    assert record == {
        "kind": "package",
        "schema_version": 1,
        "package_root": str(installed.package_root),
    }


@unix_only
def test_package_launcher_increments_an_existing_depth_counter(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """every home kind adds one to `__OCX_LAUNCHER_DEPTH`."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(
        installed.launcher,
        env=launcher_env(ocx, OCX_BINARY_PIN=str(stub), **{DEPTH_VAR: "5"}),
    )

    assert result.returncode == 0, result.stderr
    assert [call["depth"] for call in stub_calls(log)] == ["6"]


@unix_only
def test_package_launcher_keeps_the_tier_selectors_in_the_environment(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """only the `toolchain` kind removes OCX_GLOBAL and OCX_PROJECT; `package` passes them through."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(
        installed.launcher,
        env=launcher_env(
            ocx, OCX_BINARY_PIN=str(stub), OCX_GLOBAL="1", OCX_PROJECT=str(tmp_path)
        ),
    )

    assert result.returncode == 0, result.stderr
    (call,) = stub_calls(log)
    assert (call["global"], call["project"]) == ("1", str(tmp_path))


@unix_only
def test_launcher_depth_of_32_exits_78_naming_the_loop(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """a counter of 32 ends the chain with exit 78 and `launcher loop`, starting nothing."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(
        installed.launcher,
        env=launcher_env(ocx, OCX_BINARY_PIN=str(stub), **{DEPTH_VAR: "32"}),
    )

    assert result.returncode == 78, f"rc={result.returncode} stderr={result.stderr!r}"
    assert "launcher loop" in result.stderr, result.stderr
    assert stub_calls(log) == [], "the guard fires before the program starts"


@unix_only
def test_launcher_depth_of_31_still_starts_the_program(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """the limit is exactly 32; 31 runs and hands on 32."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(
        installed.launcher,
        env=launcher_env(ocx, OCX_BINARY_PIN=str(stub), **{DEPTH_VAR: "31"}),
    )

    assert result.returncode == 0, result.stderr
    assert [call["depth"] for call in stub_calls(log)] == ["32"]


def _copied_launcher(installed, destination: Path) -> Path:
    """A private copy of a published launcher and its record (the published directory is never edited)."""
    destination.mkdir(parents=True, exist_ok=True)
    copy = destination / installed.launcher.name
    shutil.copy2(installed.launcher, copy)
    shutil.copy2(installed.record, destination / installed.record.name)
    return copy


@unix_only
def test_a_relative_pin_exits_78(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`OCX_BINARY_PIN` must be absolute."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)

    result = run_launcher(
        installed.launcher, env=launcher_env(ocx, OCX_BINARY_PIN="relative/ocx")
    )

    assert result.returncode == 78, f"rc={result.returncode} stderr={result.stderr!r}"


@unix_only
def test_an_empty_pin_counts_as_unset_and_the_path_walk_finds_ocx(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """an empty `OCX_BINARY_PIN` falls through to the `PATH` walk."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    copy = _copied_launcher(installed, tmp_path / "home-bin")
    _, log = write_stub_ocx(tmp_path / "stub-bin")

    result = run_launcher(
        copy,
        env=launcher_env(ocx, OCX_BINARY_PIN="", PATH=str(tmp_path / "stub-bin")),
    )

    assert result.returncode == 0, result.stderr
    assert len(stub_calls(log)) == 1


@unix_only
def test_the_path_walk_skips_the_launchers_own_directory_and_other_launchers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """own directory, empty and relative entries, and a sibling `ocx.launch` are skipped.

    Every skipped candidate is a decoy that records itself in `decoy.log`; the
    stub in the last entry must be the one that runs.
    """
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    own = tmp_path / "own"
    copy = _copied_launcher(installed, own)
    decoy_log = tmp_path / "decoy.log"
    decoy_body = f'#!/bin/sh\necho decoy >> "{decoy_log}"\n'

    def decoy(path: Path) -> None:
        path.write_text(decoy_body, encoding="utf-8")
        path.chmod(0o755)

    decoy(own / "ocx")  # the launcher's own directory
    other = tmp_path / "other-launcher"
    other.mkdir()
    decoy(other / "ocx")  # another launcher: has a sibling ocx.launch
    (other / "ocx.launch").write_text(
        '{"kind":"package","schema_version":1,"package_root":"/x"}\n'
    )
    cwd = tmp_path / "cwd"
    cwd.mkdir()
    decoy(cwd / "ocx")  # what an empty PATH entry would mean
    (cwd / "rel").mkdir()
    decoy(cwd / "rel" / "ocx")  # what a relative PATH entry would mean
    _, log = write_stub_ocx(tmp_path / "stub-bin")
    path = os.pathsep.join(
        ["", "rel", str(own), str(other), str(tmp_path / "stub-bin")]
    )

    result = run_launcher(
        copy, env=launcher_env(ocx, OCX_BINARY_PIN=None, PATH=path), cwd=cwd
    )

    assert result.returncode == 0, result.stderr
    assert not decoy_log.exists(), f"a skipped candidate ran: {decoy_log.read_text()!r}"
    assert len(stub_calls(log)) == 1


@unix_only
def test_the_path_walk_with_no_ocx_exits_78(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """no pin, no record `ocx`, nothing on `PATH` is exit 78 naming the search."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    copy = _copied_launcher(installed, tmp_path / "home-bin")

    result = run_launcher(
        copy,
        env=launcher_env(ocx, OCX_BINARY_PIN=None, PATH=str(tmp_path / "home-bin")),
    )

    assert result.returncode == 78, f"rc={result.returncode} stderr={result.stderr!r}"
    assert "no ocx found on PATH" in result.stderr, result.stderr


@unix_only
@pytest.mark.parametrize("extension", ["shim", "shimref", "exec", "direct"])
def test_a_launcher_beside_only_a_retired_sidecar_exits_78_naming_the_record(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, extension: str
) -> None:
    """a launcher with `NAME.<retired sidecar>` and no `NAME.launch` exits 78 naming `NAME.launch`."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    directory = tmp_path / "retired"
    directory.mkdir()
    copy = directory / "hello"
    shutil.copy2(installed.launcher, copy)
    (directory / f"hello.{extension}").write_text("{}\n", encoding="utf-8")
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(copy, env=launcher_env(ocx, OCX_BINARY_PIN=str(stub)))

    assert result.returncode == 78, f"rc={result.returncode} stderr={result.stderr!r}"
    assert "hello.launch" in result.stderr, result.stderr
    assert stub_calls(log) == []


@unix_only
def test_a_launcher_reached_through_a_symlink_passes_the_targets_name(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """the record and `NAME` come from the image path, not from the symlink that named it."""
    installed = install_entrypoint_package(ocx, unique_repo, tmp_path)
    alias_dir = tmp_path / "aliases"
    alias_dir.mkdir()
    alias = alias_dir / "renamed"
    alias.symlink_to(installed.launcher)
    stub, log = write_stub_ocx(tmp_path / "stub")

    result = run_launcher(alias, env=launcher_env(ocx, OCX_BINARY_PIN=str(stub)))

    assert result.returncode == 0, result.stderr
    (call,) = stub_calls(log)
    assert call["argv"][-1] == "hello", (
        f"NAME must be the target's file name; got {call['argv']}"
    )
