# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package create`'s libc lint.

Proves the CLI actually *invokes* the check — the unit tests in
`libc_lint.rs` prove the check is correct, and a correct check that nothing
calls is a green that never ran.

The bug: `os.features` subset matching reads an empty feature list as "this
artifact demands nothing of the host", so an omitted `libc.glibc` is a
positive claim of libc universality. A glibc-linked binary published that way
resolves on Alpine and then fails to execute with a bare `No such file or
directory` — the kernel reporting the missing ELF interpreter.

Fixtures are real ELF objects built by the host C toolchain, so the module is
Linux-only: `cc` on macOS emits Mach-O and on Windows PE, neither of which
carries a `PT_INTERP` for the lint to read. `cc` itself is asserted, never
skipped — linking any Rust binary on this host already required a C linker
driver, so its absence is unreachable wherever these tests run.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner

pytestmark = pytest.mark.skipif(
    sys.platform != "linux",
    reason="fixtures are real ELF objects from the host C toolchain; cc emits "
    "Mach-O on macOS and PE on Windows, neither of which carries a PT_INTERP",
)

EXIT_SUCCESS = 0
EXIT_DATA_ERR = 65  # DataError — LibcLintError::{UndeclaredLibc,AgnosticPlatformClaim}

MUSL_INTERPRETER = {
    "x86_64": "/lib/ld-musl-x86_64.so.1",
    "aarch64": "/lib/ld-musl-aarch64.so.1",
}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _compile(destination: Path, source: str, *args: str) -> None:
    """Build one ELF fixture at `destination` with the host C toolchain."""
    source_path = destination.parent / f"{destination.name}.c"
    source_path.write_text(source)
    result = subprocess.run(
        ["cc", "-o", str(destination), str(source_path), *args],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"cc failed to build fixture: {result.stderr}"
    source_path.unlink()


def _glibc_binary(destination: Path) -> None:
    """A dynamically linked glibc binary — the shape of the published `bazel`."""
    _compile(destination, "int main(void) { return 0; }\n")


def _musl_binary(destination: Path) -> None:
    """A dynamically linked binary naming the musl loader.

    Built by the glibc toolchain with an overridden `PT_INTERP`, so it will
    not run here — irrelevant: the lint reads the header, and this is exactly
    the cross-build shape it exists to check.
    """
    import platform as platform_module

    machine = platform_module.machine().lower()
    interpreter = MUSL_INTERPRETER.get(machine)
    assert interpreter is not None, f"no musl loader path known for arch {machine}"
    _compile(destination, "int main(void) { return 0; }\n", f"-Wl,--dynamic-linker={interpreter}")


def _static_binary(destination: Path) -> None:
    """A statically linked binary: no `PT_INTERP`, no libc requirement.

    `-nostdlib` avoids needing static glibc installed.
    """
    _compile(destination, "void _start(void) { __builtin_trap(); }\n", "-static", "-nostdlib")


def _write_tree(tmp_path: Path, name: str) -> Path:
    """A content tree whose `bin/` is on the package's interface `PATH`."""
    bin_dir = tmp_path / f"pkg-{name}" / "bin"
    bin_dir.mkdir(parents=True)
    return bin_dir


def _write_metadata(tmp_path: Path, name: str) -> Path:
    metadata_path = tmp_path / f"metadata-{name}.json"
    metadata_path.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": "${installPath}/bin",
                        "visibility": "public",
                    }
                ],
            }
        )
    )
    return metadata_path


def _create(ocx: OcxRunner, tmp_path: Path, name: str, platform_spec: str):
    return ocx.plain(
        "package",
        "create",
        "-m",
        str(_write_metadata(tmp_path, name)),
        "-o",
        str(tmp_path / f"{name}.tar.xz"),
        "-p",
        platform_spec,
        str(tmp_path / f"pkg-{name}"),
        check=False,
    )


# ---------------------------------------------------------------------------
# The reported bug
# ---------------------------------------------------------------------------


def test_glibc_binary_with_no_declared_libc_is_refused(ocx: OcxRunner, tmp_path: Path):
    """A glibc-linked tool published as libc-universal is refused at create."""
    _glibc_binary(_write_tree(tmp_path, "bazel") / "bazel")

    result = _create(ocx, tmp_path, "bazel", "linux/amd64")

    assert result.returncode == EXIT_DATA_ERR, (
        f"expected DataError ({EXIT_DATA_ERR}), got {result.returncode}\n{result.stderr}"
    )
    assert "libc.glibc" in result.stderr
    assert "linux/amd64+libc.glibc" in result.stderr, (
        "the message must hand back a paste-ready --platform value"
    )


def test_a_refused_create_leaves_no_bundle_on_disk(ocx: OcxRunner, tmp_path: Path):
    """The lint runs before archiving, so a refusal writes no orphan artifact."""
    _glibc_binary(_write_tree(tmp_path, "orphan") / "tool")

    result = _create(ocx, tmp_path, "orphan", "linux/amd64")

    assert result.returncode == EXIT_DATA_ERR
    assert not (tmp_path / "orphan.tar.xz").exists(), "a refused create must leave no bundle"


def test_declaring_the_requirement_admits_the_package(ocx: OcxRunner, tmp_path: Path):
    """The fix the error message names actually works, and is recorded."""
    _glibc_binary(_write_tree(tmp_path, "declared") / "bazel")

    result = _create(ocx, tmp_path, "declared", "linux/amd64+libc.glibc")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    sidecar = json.loads((tmp_path / "declared-metadata.json").read_text())
    assert sidecar["platform"] == "linux/amd64+libc.glibc"


def test_musl_binary_under_a_glibc_only_declaration_is_refused(ocx: OcxRunner, tmp_path: Path):
    """Matching is per family — "some libc is declared" is not enough."""
    _musl_binary(_write_tree(tmp_path, "musl") / "tool")

    result = _create(ocx, tmp_path, "musl", "linux/amd64+libc.glibc")

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "libc.musl" in result.stderr


def test_static_binary_needs_no_declaration(ocx: OcxRunner, tmp_path: Path):
    """The legitimate empty-`os.features` case: the universality claim is true."""
    _static_binary(_write_tree(tmp_path, "static") / "tool")

    result = _create(ocx, tmp_path, "static", "linux/amd64")

    assert result.returncode == EXIT_SUCCESS, result.stderr


def test_script_only_package_is_unaffected(ocx: OcxRunner, tmp_path: Path):
    """Shell scripts are not ELF objects and are never subjects of the lint.

    Locks in that the lint does not disturb the packages every other
    acceptance test builds.
    """
    script = _write_tree(tmp_path, "script") / "hello"
    script.write_text("#!/bin/sh\necho hi\n")
    script.chmod(0o755)

    result = _create(ocx, tmp_path, "script", "linux/amd64")

    assert result.returncode == EXIT_SUCCESS, result.stderr


def test_native_binary_declared_platform_agnostic_is_refused(ocx: OcxRunner, tmp_path: Path):
    """`any` satisfies every host requirement — a broader false claim still."""
    _glibc_binary(_write_tree(tmp_path, "agnostic") / "tool")

    result = _create(ocx, tmp_path, "agnostic", "any")

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "libc.glibc" in result.stderr
