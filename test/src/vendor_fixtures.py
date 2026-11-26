# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Fixture packages for the `ocx vendor` acceptance modules (design § 8, § 10).

Each builder is a plain function (ocx, repo, tmp_path) -> PackageInfo, so a
module can call it with its own repo name; the pytest fixtures at the bottom
wrap them for the common case. Builders reuse `make_package` /
`make_package_with_entrypoints`; only a tree they cannot express (symlinks)
goes through `publish_tree`, which runs the same `package create` + `push`.
"""

from __future__ import annotations

import json
import os
from collections.abc import Callable
from pathlib import Path

import pytest

from src.helpers import (
    make_package,
    make_package_with_entrypoints,
    resolved_metadata_path,
)
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo, current_platform

VENDOR_FIXTURE_DIR = Path(__file__).resolve().parent.parent / "fixtures" / "vendor"

# The literal a package author must never smuggle in (C-010, V-42).
VENDOR_TOKEN = "@VENDOR_DIR@"


def publish_tree(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    build: Callable[[Path], None],
    metadata: dict,
    *,
    platform: str | None = None,
) -> PackageInfo:
    """Publish the tree `build` writes into a fresh dir, with `metadata` as the sidecar."""
    plat = platform or current_platform()
    slug = repo.replace("/", "_")
    pkg_dir = tmp_path / f"tree-{slug}-{tag}"
    pkg_dir.mkdir(parents=True)
    build(pkg_dir)
    metadata_path = tmp_path / f"tree-metadata-{slug}-{tag}.json"
    metadata_path.write_text(json.dumps({"type": "bundle", "version": 1, **metadata}))
    bundle = tmp_path / f"tree-bundle-{slug}-{tag}.tar.xz"
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata_path),
        "-o",
        str(bundle),
        "-p",
        plat,
        str(pkg_dir),
    )
    fq = f"{ocx.registry}/{repo}:{tag}"
    ocx.plain(
        "package",
        "push",
        "-p",
        plat,
        "-m",
        str(resolved_metadata_path(bundle)),
        "--cascade",
        "-i",
        fq,
        str(bundle),
    )
    ocx.plain("index", "update", repo)
    return PackageInfo(
        repo=repo,
        tag=tag,
        short=f"{repo}:{tag}",
        fq=fq,
        content_dir=pkg_dir,
        marker="",
        platform=plat,
    )


def multi_platform_package(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    platforms: tuple[str, ...] = ("linux/amd64", "linux/arm64", "darwin/arm64"),
) -> list[PackageInfo]:
    """One tag published for every platform: a multi-entry image index (V-54, V-57)."""
    return [
        make_package(
            ocx, repo, "1.0.0", tmp_path / plat.replace("/", "_"), platform=plat
        )
        for plat in platforms
    ]


def cmake_with_ccache(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    """`cmake` depending publicly on `ccache`: the closure fixture (C-022, S-003, V-41).

    Returns (cmake, ccache). ccache's PATH is public, so it reaches the consumer view.
    """
    ccache_repo = f"{repo}_ccache"
    ccache = make_package(
        ocx, ccache_repo, "1.0.0", tmp_path / "ccache", bins=["ccache"]
    )
    digest = fetch_platform_manifest_digest(ocx.registry, ccache_repo, "1.0.0")
    cmake = make_package(
        ocx,
        repo,
        "1.0.0",
        tmp_path / "cmake",
        bins=["cmake"],
        dependencies=[{"identifier": f"{ccache.fq}@{digest}", "visibility": "public"}],
    )
    return cmake, ccache


def links_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Relative symlinks in `content/`: file link, directory link, nested link (C-007, C-011, V-21, V-69).

    POSIX only: Windows hosts cannot create the links without a privilege.
    """

    def build(root: Path) -> None:
        (root / "bin").mkdir()
        tool = root / "bin" / "tool"
        tool.write_text("#!/bin/sh\necho linked\n")
        tool.chmod(0o755)
        (root / "lib" / "real").mkdir(parents=True)
        (root / "lib" / "real" / "data.txt").write_text("data\n")
        os.symlink("tool", root / "bin" / "tool-alias")
        os.symlink("real", root / "lib" / "current")
        os.symlink("../lib/real/data.txt", root / "bin" / "data-link")

    return publish_tree(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        build,
        {
            "env": [
                {
                    "key": "PATH",
                    "type": "path",
                    "value": "${installPath}/bin",
                    "visibility": "public",
                }
            ]
        },
    )


def env_template_package(
    ocx: OcxRunner, repo: str, tmp_path: Path, *, literal_token: bool = False
) -> PackageInfo:
    """`${installPath}` templates in constant, path and list-ish values (C-010, V-25).

    With `literal_token`, one value already holds `@VENDOR_DIR@`: vendoring must exit 65 (V-42).
    """
    env = [
        {
            "key": "PATH",
            "type": "path",
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": "TPL_HOME",
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
        {
            "key": "TPL_DATA",
            "type": "constant",
            "value": "${installPath}/share/data",
            "visibility": "public",
        },
    ]
    if literal_token:
        env.append(
            {
                "key": "TPL_BAD",
                "type": "constant",
                "value": VENDOR_TOKEN,
                "visibility": "public",
            }
        )
    return make_package(ocx, repo, "1.0.0", tmp_path, env=env)


def entrypoint_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Entry points `alpha` and `gamma`; `gamma` also ships `bin/gamma.cmd` for the Windows launcher (C-017, V-58)."""
    return make_package_with_entrypoints(
        ocx,
        repo,
        tmp_path,
        ["alpha", "gamma"],
        bins=["alpha", "gamma"],
        extra_files={"bin/gamma.cmd": "@echo gamma-cmd %*\r\n"},
        env=[
            {
                "key": "PATH",
                "type": "path",
                "value": "${installPath}/bin",
                "visibility": "public",
            }
        ],
    )


# --- Step 2 adds below: BUILD.bazel + case variant, empty dir, wasip1 file package,
# --- windows-only, no names, setuid, the forward-proxy request recorder.


@pytest.fixture()
def vendor_multi_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> list[PackageInfo]:
    return multi_platform_package(ocx, unique_repo, tmp_path)


@pytest.fixture()
def vendor_cmake(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    return cmake_with_ccache(ocx, unique_repo, tmp_path)


@pytest.fixture()
def vendor_links(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> PackageInfo:
    return links_package(ocx, unique_repo, tmp_path)


@pytest.fixture()
def vendor_env_template(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> PackageInfo:
    return env_template_package(ocx, unique_repo, tmp_path)


@pytest.fixture()
def vendor_entrypoints(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> PackageInfo:
    return entrypoint_package(ocx, unique_repo, tmp_path)
