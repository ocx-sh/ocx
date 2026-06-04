# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the slimmed install.sh bootstrap (plan_self_setup.md Phase E).

install.sh is now a thin bootstrap: it detects the platform, downloads and
verifies the release archive, then hands off to the downloaded binary's
``ocx self setup``. ``ocx self setup`` owns everything that touches the user's
machine - the self-install into the content store, the per-shell env shims
under ``$OCX_HOME``, and the managed shell-profile activation block. The
scaffold generators that install.sh used to carry (``create_env_*``,
``modify_shell_profile``, ``detect_profile``, ``remove_shell_profile``,
``create_nu_autoload``) were deleted in Phase E.

What survived the slim - and what this module covers:

  - ``verify_checksum`` - install.sh still verifies the downloaded archive
    before handing off.
  - the ``__OCX_TESTING_INSTALL_BINARY`` hatch + ``ocx self setup`` handoff -
    placing a pre-built binary as the candidate, then letting setup write the
    env shims.
  - the ASCII guard on the Windows-read surface (install.sh / install.ps1).

The env-shim and shell-profile *content* is now produced by ``ocx self setup``;
that behavior is covered by ``tests/test_self_setup.py`` (the env.* shims, the
versioned fence, idempotency, format-upgrade, dirty-edit, legacy migration) and
by the Rust ``setup::{rc_block, shims, profiles}`` unit tests (the fence state
machine, byte-identity, target detection incl. the ZDOTDIR='/' guard). The
former in-script-generator tests were deleted here, not silently dropped - the
equivalent coverage moved to those two layers.

All tests:
  - Use isolated temp directories as OCX_HOME / HOME.
  - Never modify the real user HOME or shell profiles.
  - Are deterministic and independent.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

# Shell scenarios are Linux/macOS only.
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="install.sh tests require POSIX sh - skipped on Windows.",
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
    only version loads all helper functions (e.g. ``verify_checksum``) without
    running the network-dependent main installer.  We strip only the last line so
    no structural assumptions are made about the script body.
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
# verify_checksum - sha256.sum format parsing (bootstrap surface, survives slim)
# ---------------------------------------------------------------------------


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


# ---------------------------------------------------------------------------
# __OCX_TESTING_INSTALL_BINARY install path + `ocx self setup` handoff
#
# Setting __OCX_TESTING_INSTALL_BINARY=/path/to/ocx skips the normal download +
# registry bootstrap and places the given binary directly as the candidate
# under OCX_HOME.  install.sh then runs `ocx self setup --offline` against that
# candidate, which writes the env shims.  Re-pointed from the former
# finalize_install assertion: the env.* files now appear because of the setup
# handoff, not an in-script generator.  The candidate-placement half is
# unchanged.
# ---------------------------------------------------------------------------


def test_testing_install_binary_places_candidate_and_writes_env_files(
    tmp_path: Path,
) -> None:
    """``__OCX_TESTING_INSTALL_BINARY`` install mode places binary as candidate
    and the ``ocx self setup`` handoff writes the env shims.

    Running install.sh with ``__OCX_TESTING_INSTALL_BINARY=/path/to/ocx`` and
    ``OCX_HOME=<tmp>`` must:

    - Exit with code 0.
    - Place the binary at
      ``<OCX_HOME>/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx``
      and make it executable.
    - Hand off to ``ocx self setup --offline``, which writes the env shims
      (``env.sh`` and ``env.ps1`` among them) to OCX_HOME.
    - Require no network access (the hatch skips download + registry bootstrap;
      ``ocx self setup --offline`` resolves the seeded candidate as already
      present).

    The stable prebuilt binary at ``/tmp/ocx-test-bin`` is used as the candidate
    to avoid a network dependency and keep the test hermetic.  Skips when that
    binary is not staged on the runner.
    """
    test_binary = Path("/tmp/ocx-test-bin")
    if not test_binary.exists():
        pytest.skip("/tmp/ocx-test-bin not available on this runner")

    ocx_home = tmp_path / "ocx_install"

    env = {
        # Hatch env var - double-underscore prefix = test-only.
        "__OCX_TESTING_INSTALL_BINARY": str(test_binary),
        "OCX_HOME": str(ocx_home),
        "HOME": str(tmp_path / "home"),
        "PATH": "/usr/bin:/bin",
        # Signal to test infrastructure that no registry is available.
        "OCX_TESTS_NO_REGISTRY": "1",
    }

    result = subprocess.run(
        ["sh", str(INSTALL_SH), "--no-modify-path"],
        capture_output=True,
        text=True,
        env=env,
    )

    assert result.returncode == 0, (
        f"install.sh must exit 0 in __OCX_TESTING_INSTALL_BINARY mode; "
        f"rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )

    # Candidate binary must exist and be executable at the standard path.
    candidate = (
        ocx_home
        / "symlinks"
        / "ocx.sh"
        / "ocx"
        / "cli"
        / "current"
        / "content"
        / "bin"
        / "ocx"
    )
    assert candidate.exists(), (
        f"candidate binary must exist at {candidate}; "
        f"installer stdout:\n{result.stdout}"
    )
    assert os.access(candidate, os.X_OK), (
        f"candidate binary must be executable at {candidate}"
    )

    # The `ocx self setup` handoff writes the env shims to OCX_HOME.
    env_sh = ocx_home / "env.sh"
    assert env_sh.exists(), (
        f"env.sh must be written to OCX_HOME ({ocx_home}) by the `ocx self setup` handoff; "
        f"installer stdout:\n{result.stdout}"
    )

    env_ps1 = ocx_home / "env.ps1"
    assert env_ps1.exists(), (
        f"env.ps1 must be written to OCX_HOME ({ocx_home}) by the `ocx self setup` handoff; "
        f"installer stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# ASCII guard - the Windows-read surface must contain no non-ASCII bytes.
#
# Windows PowerShell 5.1 decodes a BOM-less file in the active console codepage
# (typically Windows-1252), not UTF-8. A single non-ASCII byte (em-dash, ellipsis,
# arrow) in install.ps1 - or in install.sh - decodes to mojibake; inside a
# double-quoted string the stray byte closes the string early and triggers a
# "Missing ')'/'}'" parse-error cascade. The gate harness is read by WinPS 5.1 too.
#
# The env.* shim bodies that install.sh used to embed are now Rust consts in
# `ocx_lib::setup::shims`; their ASCII-cleanliness is enforced by the generated
# `ocx self activate` / completion output guard (`test/tests/test_completion_ascii.py`)
# and the Rust `cli_help_text_is_ascii` walk. Keep these files pure ASCII: '-'
# for em-dash, '...' for ellipsis, '->' for arrow.
# ---------------------------------------------------------------------------

_REPO_ROOT = Path(__file__).resolve().parents[2]


@pytest.mark.parametrize(
    "rel_path",
    [
        "website/src/public/install.ps1",
        "website/src/public/install.sh",
        "test/manual/test-windows-activation.ps1",
    ],
)
def test_windows_read_surface_is_ascii(rel_path: str) -> None:
    """install.ps1, install.sh, and the activation gate harness must be ASCII-only.

    A non-ASCII byte in any of these is misread by Windows PowerShell 5.1 under
    its console codepage and reproduces the parse-error cascade this change fixes
    (the gate harness is itself executed by WinPS; install.sh and install.ps1 are
    streamed into ``sh`` / dot-sourced).
    """
    path = _REPO_ROOT / rel_path
    data = path.read_bytes()
    offenders = [(offset, byte) for offset, byte in enumerate(data) if byte > 0x7F]
    assert not offenders, (
        f"{rel_path} contains {len(offenders)} non-ASCII byte(s) "
        f"(first at offsets {[off for off, _ in offenders[:10]]}); "
        "Windows PowerShell 5.1 misreads these under the console codepage. "
        "Use ASCII: '-' for em-dash, '...' for ellipsis, '->' for arrow."
    )
