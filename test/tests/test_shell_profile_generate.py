# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx shell profile generate`` (plan Phase 9).

The new ``generate`` subcommand is the file-generating sibling of
``shell profile load``: it emits the same export lines, but writes them
to a file (default ``$OCX_HOME/init.<shell>``) so the user can ``source``
the file once from their shell rc instead of running ``eval`` every
shell startup.

Specification mode (contract-first TDD)
---------------------------------------
``ShellProfileGenerate::execute`` is currently ``unimplemented!()``.
Every test below is expected to FAIL at the stub panic; Phase 5
implementation flips them to passing.

Plan reference: plan_project_toolchain.md Phase 9 (lines 840–844, 853).
"""
from __future__ import annotations

import platform
import shutil
import subprocess
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Default output path
# ---------------------------------------------------------------------------


def test_generate_writes_default_path(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``ocx shell profile generate --shell bash`` writes
    ``$OCX_HOME/init.bash`` containing the package's exports.
    """
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "shell", "profile", "generate", "--shell", "bash", format=None
    )
    assert result.returncode == 0, (
        f"generate should succeed; rc={result.returncode} "
        f"stderr={result.stderr!r}"
    )

    init_path = Path(ocx.env["OCX_HOME"]) / "init.bash"
    assert init_path.is_file(), (
        f"default output should be $OCX_HOME/init.bash; "
        f"OCX_HOME contents: "
        f"{sorted(p.name for p in Path(ocx.env['OCX_HOME']).iterdir())}"
    )
    body = init_path.read_text()
    assert "export" in body, f"init.bash should contain export lines: {body!r}"
    assert "PATH" in body, f"init.bash should set PATH: {body!r}"


# ---------------------------------------------------------------------------
# Stdout escape hatch
# ---------------------------------------------------------------------------


def test_generate_to_stdout(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``--output -`` routes the generated init script to stdout instead
    of writing a file.
    """
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "shell",
        "profile",
        "generate",
        "--shell",
        "bash",
        "--output",
        "-",
        format=None,
    )
    assert result.returncode == 0, (
        f"generate to stdout should succeed; rc={result.returncode} "
        f"stderr={result.stderr!r}"
    )
    assert "export" in result.stdout, (
        f"--output - should print exports to stdout: {result.stdout!r}"
    )
    # No file written to the default location.
    init_path = Path(ocx.env["OCX_HOME"]) / "init.bash"
    assert not init_path.exists(), (
        f"--output - must not write the default file; found {init_path}"
    )


# ---------------------------------------------------------------------------
# Explicit output path
# ---------------------------------------------------------------------------


def test_generate_to_explicit_path(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``--output <PATH>`` writes to the caller-chosen location."""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    custom = tmp_path / "custom-init.sh"
    result = ocx.run(
        "shell",
        "profile",
        "generate",
        "--shell",
        "bash",
        "--output",
        str(custom),
        format=None,
    )
    assert result.returncode == 0, (
        f"generate --output <PATH> should succeed; rc={result.returncode} "
        f"stderr={result.stderr!r}"
    )
    assert custom.is_file(), f"custom path should exist: {custom}"
    body = custom.read_text()
    assert "export" in body, f"custom file should contain exports: {body!r}"


# ---------------------------------------------------------------------------
# Generated bash file is syntactically valid
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    platform.system() == "Windows", reason="bash -n parser not available"
)
def test_generate_bash_syntax_valid(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """Generated bash file passes ``bash -n`` (parse-only, no execute).

    Plan line 853 — "writes syntactically valid bash file".
    """
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    out_file = tmp_path / "init.bash"
    result = ocx.run(
        "shell",
        "profile",
        "generate",
        "--shell",
        "bash",
        "--output",
        str(out_file),
        format=None,
    )
    assert result.returncode == 0, (
        f"generate should succeed; rc={result.returncode} "
        f"stderr={result.stderr!r}"
    )

    parse = subprocess.run(
        ["bash", "-n", str(out_file)],
        capture_output=True,
        text=True,
    )
    assert parse.returncode == 0, (
        f"bash -n must accept the generated file; rc={parse.returncode}, "
        f"stderr={parse.stderr!r}, body={out_file.read_text()!r}"
    )


# ---------------------------------------------------------------------------
# Idempotent overwrite
# ---------------------------------------------------------------------------


def test_generate_overwrites_existing(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Generating twice in a row succeeds; the second invocation
    overwrites the first.

    The plan does not explicitly call out overwrite semantics, but file
    generators in OCX (e.g. ``ocx generate direnv``) refuse-on-existing
    by default and require ``--force``.  ``shell profile generate``
    differs because the file is regenerated on every shell-config update
    by design — overwrite is the natural behavior.  If Phase 5 chooses
    refuse-on-existing instead, the second invocation should fail and
    this test should be flipped to assert that.
    """
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    init_path = Path(ocx.env["OCX_HOME"]) / "init.bash"

    first = ocx.run(
        "shell", "profile", "generate", "--shell", "bash", format=None
    )
    assert first.returncode == 0, first.stderr
    assert init_path.is_file(), "first generate should write the file"
    first_body = init_path.read_text()

    second = ocx.run(
        "shell", "profile", "generate", "--shell", "bash", format=None
    )
    assert second.returncode == 0, (
        f"second generate must overwrite, not refuse; "
        f"rc={second.returncode} stderr={second.stderr!r}"
    )
    assert init_path.is_file(), "init.bash should still exist after rerun"
    second_body = init_path.read_text()
    # Identical inputs → identical outputs (the profile manifest is
    # unchanged between the two invocations).
    assert first_body == second_body, (
        "regenerating with the same profile must produce the same body"
    )


# ---------------------------------------------------------------------------
# Deprecation note absence
# ---------------------------------------------------------------------------


def test_generate_does_not_emit_deprecation_note(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``generate`` is the recommended (forward-looking) path — it must
    NOT carry the deprecation note that the four legacy shell-profile
    commands (load/add/remove/list) emit.

    The deprecation note in those legacy commands references
    ``shell init`` and ``shell profile generate`` as alternatives;
    ``generate`` itself emitting that note would be self-referential
    nonsense. Use ``--output -`` to avoid touching the filesystem and
    keep stdout/stderr easy to inspect."""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "shell",
        "profile",
        "generate",
        "--shell",
        "bash",
        "--output",
        "-",
        format=None,
    )
    assert result.returncode == 0, (
        f"generate should succeed; rc={result.returncode} "
        f"stderr={result.stderr!r}"
    )
    assert "Note:" not in result.stderr, (
        f"generate must not emit the deprecation note on stderr; "
        f"got: {result.stderr!r}"
    )
    assert "shell init" not in result.stderr, (
        f"generate stderr must not reference 'shell init'; "
        f"got: {result.stderr!r}"
    )
    assert "shell profile generate" not in result.stderr, (
        f"generate stderr must not reference 'shell profile generate' "
        f"(self-referential); got: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Non-bash shell coverage
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    platform.system() == "Windows", reason="zsh -n parser not available"
)
def test_generate_zsh_syntax_valid(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """Generated zsh file passes ``zsh -n`` (parse-only, no execute).

    Mirrors ``test_generate_bash_syntax_valid`` for zsh to ensure the
    generator emits syntactically valid output for at least one
    non-bash shell. Skip if zsh is not installed in the environment.
    """
    if shutil.which("zsh") is None:
        pytest.skip("zsh not installed in this environment")

    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    out_file = tmp_path / "init.zsh"
    result = ocx.run(
        "shell",
        "profile",
        "generate",
        "--shell",
        "zsh",
        "--output",
        str(out_file),
        format=None,
    )
    assert result.returncode == 0, (
        f"generate --shell zsh should succeed; rc={result.returncode} "
        f"stderr={result.stderr!r}"
    )

    parse = subprocess.run(
        ["zsh", "-n", str(out_file)],
        capture_output=True,
        text=True,
    )
    assert parse.returncode == 0, (
        f"zsh -n must accept the generated file; rc={parse.returncode}, "
        f"stderr={parse.stderr!r}, body={out_file.read_text()!r}"
    )
