# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance test for `ocx package copy` reading its source from an export directory."""

from __future__ import annotations

from pathlib import Path

import pytest

from src import registry_guard as registry_guard_mod
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner

pytestmark = pytest.mark.command("package_copy", "package_export")


def test_copy_from_export_by_tag_with_source_down(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry: str,
    target_registry: str,
    registry_guard: registry_guard_mod.RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """The tag resolves from the export's index and the bytes from its blobs, with the source registry refusing everything."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    export = tmp_path / "export"
    exported = ocx.run("package", "export", pkg.short, "-o", str(export), check=False)
    assert exported.returncode == 0, exported.stderr

    # The target is reached as 127.0.0.1 so it can bypass the refusing proxy that
    # `localhost` (the source) goes through.
    target = target_registry.replace("localhost", "127.0.0.1")
    registry_guard.deny(second_home)
    second_home.env["NO_PROXY"] = "127.0.0.1"
    second_home.env["no_proxy"] = "127.0.0.1"
    second_home.env["OCX_INSECURE_REGISTRIES"] = f"{registry},{target}"
    registry_guard.reset()

    result = second_home.run(
        "--index",
        str(export / "index"),
        "--blob-dir",
        str(export / "blobs"),
        "package",
        "copy",
        f"{registry}/{pkg.short}",
        "-i",
        f"{target}/{pkg.short}",
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stderr.lower().count("sidecar") == 1, result.stderr
    copied = fetch_platform_manifest_digest(target_registry, pkg.repo, pkg.tag)
    assert copied == fetch_platform_manifest_digest(registry, pkg.repo, pkg.tag)
