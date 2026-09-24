"""Acceptance tests for `ocx package receipt` (#459).

The build receipt is the `<stem>-receipt.json` sidecar `ocx package create`
writes beside a bundle; `push` and `package test` read it silently. This
command prints it, so an SDK reads the recorded `--platform`/`--identifier`
through the CLI contract rather than an undocumented file.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from src.helpers import resolved_receipt_path
from src.runner import OcxRunner, current_platform

pytestmark = pytest.mark.command("package_receipt")

EXIT_DATA_ERR = 65
EXIT_NOT_FOUND = 79


def _create(ocx: OcxRunner, tmp_path: Path, name: str, *create_args: str) -> Path:
    """`ocx package create` of a one-file directory; returns the bundle path."""
    pkg_dir = tmp_path / f"content-{name}"
    (pkg_dir / "bin").mkdir(parents=True)
    (pkg_dir / "bin" / "app").write_text("#!/bin/sh\necho app\n")
    out = tmp_path / f"{name}.tar.xz"
    ocx.plain("package", "create", "-o", str(out), *create_args, str(pkg_dir))
    return out


def test_receipt_prints_what_create_recorded(ocx: OcxRunner, tmp_path: Path) -> None:
    """Both fields recorded → both printed, as the canonical strings."""
    bundle = _create(
        ocx, tmp_path, "both", "-p", current_platform(), "-i", "example.com/acme/widget:1.0.0"
    )
    assert resolved_receipt_path(bundle).is_file(), "precondition: create wrote the sidecar"

    assert ocx.json("package", "receipt", str(bundle)) == {
        "platform": current_platform(),
        "identifier": "example.com/acme/widget:1.0.0",
    }


def test_receipt_omits_a_field_create_was_not_given(ocx: OcxRunner, tmp_path: Path) -> None:
    """Absent means "not recorded" — the key is missing, never `null`."""
    bundle = _create(ocx, tmp_path, "platform-only", "-p", current_platform())

    assert ocx.json("package", "receipt", str(bundle)) == {"platform": current_platform()}


def test_receipt_missing_exits_not_found(ocx: OcxRunner, tmp_path: Path) -> None:
    """No sidecar beside the bundle → 79, and the message names the path."""
    bundle = tmp_path / "bare.tar.xz"
    bundle.write_bytes(b"")

    result = ocx.run("package", "receipt", str(bundle), check=False)

    assert result.returncode == EXIT_NOT_FOUND, result.stderr
    assert str(resolved_receipt_path(bundle)) in result.stderr, result.stderr


def test_receipt_malformed_exits_data_error(ocx: OcxRunner, tmp_path: Path) -> None:
    """A sidecar that is not a receipt → 65, the same class `push` raises."""
    bundle = tmp_path / "broken.tar.xz"
    bundle.write_bytes(b"")
    resolved_receipt_path(bundle).write_text("{not json")

    result = ocx.run("package", "receipt", str(bundle), check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
