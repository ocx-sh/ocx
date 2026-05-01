"""Acceptance tests for OCX configuration forwarding across subprocess spawns.

`ocx exec` materializes the running ocx's resolution-affecting policy as
`OCX_*` env vars on the child, so a subsequent `ocx exec` invocation (most
commonly an entrypoint launcher's re-entry into ocx) sees the same policy
the parent saw — even under `--clean` and even when the parent shell
exports stale `OCX_*` values.

Per `subsystem-cli.md` "Cross-Cutting: OCX Configuration Forwarding".
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner


pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="OCX configuration forwarding is exercised via POSIX shell helpers (`sh`, `cat`); Windows has the same Rust surface covered by unit tests",
)


def _exec_capture(
    ocx: OcxRunner,
    pkg_short: str,
    cmd: list[str],
    *outer_flags: str,
    extra_env: dict[str, str] | None = None,
    stdin: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx [outer_flags] exec pkg -- cmd...` capturing stdout/stderr."""
    full = [str(ocx.binary), *outer_flags, "exec", pkg_short, "--", *cmd]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=env,
        input=stdin,
        timeout=30,
        check=False,
    )


def test_ocx_binary_set_on_child_env(
    ocx: OcxRunner, published_package
) -> None:
    """`ocx exec` writes `OCX_BINARY_PIN=<absolute-path-to-running-ocx>` onto the child."""
    ocx.plain("install", published_package.short)
    result = _exec_capture(
        ocx, published_package.short, ["sh", "-c", 'printf "%s" "$OCX_BINARY_PIN"']
    )
    assert result.returncode == 0, result.stderr
    assert Path(result.stdout).resolve() == ocx.binary.resolve(), (
        f"OCX_BINARY_PIN must equal the running ocx binary; got {result.stdout!r}, expected {ocx.binary}"
    )


def test_offline_flag_propagates_via_env(
    ocx: OcxRunner, published_package
) -> None:
    """`ocx --offline exec pkg -- ...` sets `OCX_OFFLINE=1` on the child env."""
    ocx.plain("install", published_package.short)
    result = _exec_capture(
        ocx,
        published_package.short,
        ["sh", "-c", 'printf "%s" "$OCX_OFFLINE"'],
        "--offline",
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "1", (
        f"`OCX_OFFLINE` must be `1` on child env when outer is --offline; got {result.stdout!r}"
    )


def test_clean_env_writes_ocx_config_explicitly(
    ocx: OcxRunner, published_package
) -> None:
    """Under `--clean`, the child env starts empty but still receives explicit
    `OCX_BINARY_PIN` + `OCX_OFFLINE` written from the outer ocx's parsed state.
    No ambient parent-shell export can leak in or override this."""
    ocx.plain("install", published_package.short)
    # `sh` resolution requires PATH lookup, but `--clean` drops the inherited
    # PATH and only the package's bin/ is exposed, so we hand `Command::new`
    # an absolute interpreter path. The contract under test is what `OCX_*`
    # keys land on the child env — interpreter discovery is incidental.
    sh_path = "/bin/sh"
    full = [
        str(ocx.binary),
        "--offline",
        "exec",
        "--clean",
        published_package.short,
        "--",
        sh_path,
        "-c",
        'printf "binary=%s offline=%s" "$OCX_BINARY_PIN" "$OCX_OFFLINE"',
    ]
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "offline=1" in result.stdout, (
        f"`--offline` must propagate via env into a --clean child; got {result.stdout!r}"
    )
    assert (
        f"binary={ocx.binary}" in result.stdout
        or f"binary={ocx.binary.resolve()}" in result.stdout
    ), (
        f"`OCX_BINARY_PIN` must be written explicitly under --clean; got {result.stdout!r}"
    )


def test_clean_encapsulates_parent_env(
    ocx: OcxRunner, published_package
) -> None:
    """Under `--clean`, parent-shell vars not in `apply_ocx_config` or the
    package env do not leak past the clean boundary. Regression guard for
    `Command::env_clear()` on the spawn/exec helper — without it,
    `Command::envs()` merges into the inherited parent env and `--clean`
    becomes a no-op for arbitrary `STRAY_VAR` exports."""
    ocx.plain("install", published_package.short)
    # `--clean` drops PATH; hand `Command::new` an absolute interpreter path
    # for the same reason as `test_clean_env_writes_ocx_config_explicitly`.
    sh_path = "/bin/sh"
    full = [
        str(ocx.binary),
        "exec",
        "--clean",
        published_package.short,
        "--",
        sh_path,
        "-c",
        'printf "%s" "${STRAY_VAR-unset}"',
    ]
    env = dict(ocx.env)
    env["STRAY_VAR"] = "leak"
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "unset", (
        f"`STRAY_VAR` from parent shell must not leak past `--clean`; got {result.stdout!r}"
    )


def test_exec_inherits_stdin_by_default(
    ocx: OcxRunner, published_package
) -> None:
    """Stdin is inherited by `ocx exec`'s child unconditionally; the
    `--interactive` flag was removed because the new default matches shell
    exec semantics."""
    ocx.plain("install", published_package.short)
    payload = "hello-from-stdin"
    result = _exec_capture(
        ocx, published_package.short, ["cat"], stdin=payload
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == payload, (
        f"stdin must flow through `ocx exec` to the child; got stdout={result.stdout!r}"
    )


def test_interactive_flag_rejected(
    ocx: OcxRunner, published_package
) -> None:
    """The `--interactive` / `-i` flag was removed; clap rejects it as a
    usage error (sysexits.h `EX_USAGE = 64`)."""
    ocx.plain("install", published_package.short)
    full = [
        str(ocx.binary),
        "exec",
        "--interactive",
        published_package.short,
        "--",
        "true",
    ]
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        timeout=30,
        check=False,
    )
    assert result.returncode == 64, (
        f"removed `--interactive` flag must surface as UsageError (64); got rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_generated_launcher_uses_ocx_binary_and_silences_presentation(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The Unix launcher template uses `${OCX_BINARY_PIN:-ocx}` and bakes the
    `launcher exec` subcommand so the entrypoint chain is opaque to the
    surrounding tool. Presentation overrides (--log-level=off / --color=never /
    --format=plain) and self-view are forced inside `launcher exec` and are
    NOT baked per-launcher; the launcher body must therefore contain neither
    presentation flags nor `--self`. Inspecting the generated file is the
    cheapest assertion of the One-Way Door wire vocabulary committed in
    `adr_package_entry_points.md` §Stable Surfaces."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    from src.runner import registry_dir
    reg = registry_dir(ocx.registry)
    launcher = (
        Path(str(ocx.ocx_home))
        / "symlinks"
        / reg
        / pkg.repo
        / "current"
        / "entrypoints"
        / "hello"
    )
    assert launcher.exists(), f"launcher must exist: {launcher}"
    body = launcher.read_text()
    assert '"${OCX_BINARY_PIN:-ocx}"' in body, (
        f"launcher must invoke inner ocx via `${{OCX_BINARY_PIN:-ocx}}`: {body!r}"
    )
    assert "launcher exec" in body, (
        f"launcher must call the `launcher exec` subcommand: {body!r}"
    )
    for forbidden in ("--log-level", "--color", "--format", "--self", "file://"):
        assert forbidden not in body, (
            f"launcher must NOT bake `{forbidden}` (hidden inside `launcher exec`): {body!r}"
        )


def test_exec_child_exit_code_propagates(
    ocx: OcxRunner, published_package
) -> None:
    """Non-zero child exit codes propagate verbatim through `ocx exec` on Unix.

    On Unix, `ocx exec` uses `execvp`-style process replacement, so the child's
    exit code becomes the ocx process's exit code with no wrapping or saturation.
    The canonical case is exit code 42 — an application-specific code above the
    shell-reserved range (1–2) and below the sysexits range (64+).

    Windows note: Windows spawn uses `.status()` with `.unwrap_or(1)` fallback,
    so non-zero codes above 1 are not guaranteed to propagate identically. That
    path is covered by the `utility::child_process` unit tests; this module is
    Unix-only per the module-level `pytestmark`.
    """
    ocx.plain("install", published_package.short)
    result = _exec_capture(
        ocx, published_package.short, ["sh", "-c", "exit 42"]
    )
    assert result.returncode == 42, (
        f"`ocx exec` must propagate child exit code 42 verbatim (Unix execvp passthrough); "
        f"got rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_launcher_chain_offline_propagates(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """End-to-end: `ocx --offline exec pkg -- hello` flows through the
    generated launcher's inner `ocx launcher exec` call, and the
    inner ocx sees `OCX_OFFLINE=1` from the outer's `apply_ocx_config`.

    The launcher's wrapped target is the package's `bin/hello` script; we
    do not need a separate observation point — what we are asserting is that
    `--offline` does not break the launcher chain (i.e. the outer's offline
    policy reaches the inner ocx without forcing it into a no-network
    failure mode for purely local resolution work).
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    full = [
        str(ocx.binary),
        "--offline",
        "exec",
        pkg.short,
        "--",
        "hello",
    ]
    env = dict(ocx.env)
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, (
        f"launcher chain must succeed under outer --offline (purely local resolution work); rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert pkg.marker in result.stdout, (
        f"launcher must invoke wrapped target; stdout={result.stdout!r}"
    )
