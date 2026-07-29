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


def _unattributable_binary(destination: Path) -> None:
    """A dynamically linked binary naming a loader OCX cannot attribute.

    Same `PT_INTERP` override trick as `_musl_binary`, pointed at a name no
    libc family claims. This is the check's *fail-closed* branch
    (`UnrecognizedInterpreter`) rather than its mismatch branch — a distinct
    refusal, and the shape a bug in the attribution table would produce.
    """
    _compile(
        destination,
        "int main(void) { return 0; }\n",
        "-Wl,--dynamic-linker=/lib/ld-newlibc.so.9",
    )


def _write_tree(tmp_path: Path, name: str) -> Path:
    """A content tree whose `bin/` is on the package's interface `PATH`."""
    bin_dir = tmp_path / f"pkg-{name}" / "bin"
    bin_dir.mkdir(parents=True)
    return bin_dir


def _write_metadata(tmp_path: Path, name: str, *, binaries: list[str] | None = None) -> Path:
    """Write a `-m` input sidecar with one interface PATH var.

    ``binaries=None`` omits the field (undeclared); any list declares it, which
    is what makes bin-scan's Auto mode skip scanning entirely.
    """
    metadata_obj: dict = {
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
    if binaries is not None:
        metadata_obj["binaries"] = binaries
    metadata_path = tmp_path / f"metadata-{name}.json"
    metadata_path.write_text(json.dumps(metadata_obj))
    return metadata_path


def _create_at(
    ocx: OcxRunner,
    tree: Path,
    metadata_path: Path,
    output: Path,
    platform_spec: str,
    *args: str,
):
    """Run `package create` over an explicit tree/sidecar/output triple."""
    return ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata_path),
        "-o",
        str(output),
        "-p",
        platform_spec,
        *args,
        str(tree),
        check=False,
    )


def _create(
    ocx: OcxRunner,
    tmp_path: Path,
    name: str,
    platform_spec: str,
    *args: str,
    binaries: list[str] | None = None,
):
    return _create_at(
        ocx,
        tmp_path / f"pkg-{name}",
        _write_metadata(tmp_path, name, binaries=binaries),
        tmp_path / f"{name}.tar.xz",
        platform_spec,
        *args,
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


# ---------------------------------------------------------------------------
# Independence from the `--bin-scan` gates
#
# The lint reads the same content tree as the interface-binaries scan, but it
# must NOT inherit that scan's mode table. `--bin-scan` governs the `binaries`
# claim; the lint governs the `os.features` claim. The two ways a publisher
# can legitimately switch the binaries scan off are exactly the shape of the
# reported bug — a mirror that hand-declares its binaries — so a lint that
# rode along on `resolve_binaries` would have missed it.
# ---------------------------------------------------------------------------


def test_lint_still_runs_under_no_bin_scan(ocx: OcxRunner, tmp_path: Path):
    """`--no-bin-scan` declines a binary-list fill, not a false libc claim."""
    _glibc_binary(_write_tree(tmp_path, "noscan") / "bazel")

    result = _create(ocx, tmp_path, "noscan", "linux/amd64", "--no-bin-scan")

    assert result.returncode == EXIT_DATA_ERR, (
        f"--no-bin-scan must not disable the libc check\n{result.stderr}"
    )
    assert "libc.glibc" in result.stderr


def test_lint_still_runs_when_binaries_are_hand_declared(ocx: OcxRunner, tmp_path: Path):
    """Auto mode skips scanning when `binaries` is declared — the lint does not.

    This is the reported bug's exact shape: a mirror that authors its own
    `binaries` list gets no scan from bin-scan's Auto default, so a lint hung
    off that traversal would never have seen the glibc-linked binary.
    """
    _glibc_binary(_write_tree(tmp_path, "declaredbins") / "bazel")

    result = _create(ocx, tmp_path, "declaredbins", "linux/amd64", binaries=["bazel"])

    assert result.returncode == EXIT_DATA_ERR, (
        f"a hand-declared binaries list must not disable the libc check\n{result.stderr}"
    )
    assert "libc.glibc" in result.stderr


# ---------------------------------------------------------------------------
# `--no-libc-lint` — the escape hatch
#
# A false refusal from the check would otherwise block every `ocx package
# create` for a Linux target with no way through, which is an availability
# failure: a bug in the lint must not be able to stop publishing. The flag is
# therefore a total bypass of the check and of nothing else.
# ---------------------------------------------------------------------------


def test_no_libc_lint_admits_exactly_what_the_check_refuses(ocx: OcxRunner, tmp_path: Path):
    """Both outcomes on one fixture: refused without the flag, admitted with it.

    A "with the flag it exits 0" assertion alone cannot tell the bypass from a
    check that never fired, so the same tree is run both ways here rather than
    across two tests that could drift apart.
    """
    _glibc_binary(_write_tree(tmp_path, "hatch") / "bazel")

    refused = _create(ocx, tmp_path, "hatch", "linux/amd64")
    assert refused.returncode == EXIT_DATA_ERR, (
        f"expected DataError ({EXIT_DATA_ERR}) without the flag, got {refused.returncode}\n{refused.stderr}"
    )
    assert not (tmp_path / "hatch.tar.xz").exists(), "a refused create must leave no bundle"

    admitted = _create(ocx, tmp_path, "hatch", "linux/amd64", "--no-libc-lint")
    assert admitted.returncode == EXIT_SUCCESS, (
        f"--no-libc-lint must be a way past the refusal\n{admitted.stderr}"
    )
    assert (tmp_path / "hatch.tar.xz").exists(), "the bypassed create must still produce the bundle"
    assert "--no-libc-lint" in admitted.stderr, "the suppression must be loud"
    assert "linux/amd64" in admitted.stderr, "the warning must name the declared platform"
    assert "os.features" in admitted.stderr, "the warning must say what went unverified"


def test_no_libc_lint_does_not_suppress_an_unrelated_create_failure(ocx: OcxRunner, tmp_path: Path):
    """The flag skips one check, not the rest of the command.

    The subject is the publish-time metadata validation that runs immediately
    after the libc check, so a bypass that short-circuited the whole sidecar
    arm would swallow it. The fixture would also trip the libc check, which
    makes the absent `libc.glibc` in stderr proof that the skip happened and
    the run continued anyway.
    """
    _glibc_binary(_write_tree(tmp_path, "unrelated") / "bazel")
    metadata_path = tmp_path / "metadata-unrelated.json"
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
                    },
                    # References a dependency this sidecar never declares.
                    {
                        "key": "TOOL_HOME",
                        "type": "constant",
                        "value": "${deps.missing.installPath}",
                        "visibility": "public",
                    },
                ],
            }
        )
    )

    result = _create_at(
        ocx,
        tmp_path / "pkg-unrelated",
        metadata_path,
        tmp_path / "unrelated.tar.xz",
        "linux/amd64",
        "--no-libc-lint",
    )

    assert result.returncode == EXIT_DATA_ERR, (
        f"invalid metadata must still be refused under --no-libc-lint\n{result.stderr}"
    )
    assert "unknown dependency 'missing'" in result.stderr
    assert "libc.glibc" not in result.stderr, (
        "the libc check was meant to be skipped, so its message must not be what failed the run"
    )
    assert not (tmp_path / "unrelated.tar.xz").exists(), "a refused create must leave no bundle"


def test_no_libc_lint_admits_a_fail_closed_refusal_too(ocx: OcxRunner, tmp_path: Path):
    """The bypass is total, not scoped to the libc *mismatch* refusal.

    The flag exists because a bug in the check must not be able to stop
    publishing — and a bug shows up as a misclassification, not as a correct
    `UndeclaredLibc`. `UnrecognizedInterpreter` is that shape: OCX refuses
    because it cannot attribute the loader, which is exactly what an
    attribution table missing a real libc would do. A bypass that opened only
    for `UndeclaredLibc` would leave the availability hole the flag was added
    to close, and every other test here would still pass.
    """
    _unattributable_binary(_write_tree(tmp_path, "exotic") / "tool")

    refused = _create(ocx, tmp_path, "exotic", "linux/amd64")
    assert refused.returncode == EXIT_DATA_ERR, (
        f"an unattributable loader must be refused without the flag, got {refused.returncode}\n{refused.stderr}"
    )
    assert "ld-newlibc.so.9" in refused.stderr, (
        "the refusal must be the unattributable-loader one, not a libc mismatch"
    )

    admitted = _create(ocx, tmp_path, "exotic", "linux/amd64", "--no-libc-lint")
    assert admitted.returncode == EXIT_SUCCESS, (
        f"--no-libc-lint must also be a way past a fail-closed refusal\n{admitted.stderr}"
    )
    assert (tmp_path / "exotic.tar.xz").exists(), "the bypassed create must still produce the bundle"


def test_no_libc_lint_is_silent_on_a_platform_the_check_never_inspects(
    ocx: OcxRunner, tmp_path: Path
):
    """The warning fires where something was actually skipped, and nowhere else.

    `check_declared_libc` returns `Ok` for every non-Linux concrete target, so
    on `darwin/*` the flag suppresses nothing and a warning would name a
    verification that was never going to happen. Both outcomes on one fixture:
    the same tree warns for `linux/amd64` and stays silent for `darwin/arm64`,
    so a gate that answered a constant fails one of the two.
    """
    _glibc_binary(_write_tree(tmp_path, "scoped") / "tool")
    metadata_path = _write_metadata(tmp_path, "scoped")
    tree = tmp_path / "pkg-scoped"

    linux = _create_at(
        ocx, tree, metadata_path, tmp_path / "scoped-linux.tar.xz", "linux/amd64", "--no-libc-lint"
    )
    assert linux.returncode == EXIT_SUCCESS, linux.stderr
    assert "--no-libc-lint" in linux.stderr, (
        "a real bypass on a checked platform must stay loud"
    )

    darwin = _create_at(
        ocx, tree, metadata_path, tmp_path / "scoped-darwin.tar.xz", "darwin/arm64", "--no-libc-lint"
    )
    assert darwin.returncode == EXIT_SUCCESS, darwin.stderr
    assert "--no-libc-lint" not in darwin.stderr, (
        f"nothing was skipped for darwin/arm64, so nothing must be claimed skipped:\n{darwin.stderr}"
    )
    assert "os.features" not in darwin.stderr, (
        f"the os.features of an unchecked platform are not 'unverified' by this flag:\n{darwin.stderr}"
    )


def test_no_libc_lint_changes_nothing_about_what_is_published(ocx: OcxRunner, tmp_path: Path):
    """Same sidecar bytes either way, on a tile that passes the check regardless.

    The flag suppresses a check; it is not an authoring switch. A static
    binary needs no libc, so both runs are clean publishes and any byte
    difference between their sidecars would be the flag leaking into output.
    """
    tree = tmp_path / "pkg-same"
    _static_binary(_write_tree(tmp_path, "same") / "tool")
    metadata_path = _write_metadata(tmp_path, "same")

    checked = _create_at(ocx, tree, metadata_path, tmp_path / "checked.tar.xz", "linux/amd64")
    bypassed = _create_at(
        ocx, tree, metadata_path, tmp_path / "bypassed.tar.xz", "linux/amd64", "--no-libc-lint"
    )

    assert checked.returncode == EXIT_SUCCESS, checked.stderr
    assert bypassed.returncode == EXIT_SUCCESS, bypassed.stderr

    checked_sidecar = (tmp_path / "checked-metadata.json").read_bytes()
    bypassed_sidecar = (tmp_path / "bypassed-metadata.json").read_bytes()
    assert b'"platform"' in checked_sidecar, (
        "guard against comparing two sidecars that both failed to be written"
    )
    assert checked_sidecar == bypassed_sidecar, (
        "--no-libc-lint must skip a check, never change the published metadata"
    )
