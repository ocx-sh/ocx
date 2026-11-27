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

import contextlib
import json
import os
import sys
import urllib.parse
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from src import forward_proxy
from src.helpers import (
    make_package,
    make_package_with_entrypoints,
    resolved_metadata_path,
)
from src.registry import fetch_platform_manifest_digest, get_manifest
from src.runner import OcxRunner, PackageInfo, current_platform

VENDOR_FIXTURE_DIR = Path(__file__).resolve().parent.parent / "fixtures" / "vendor"

# The literal a package author must never smuggle in.
VENDOR_TOKEN = "@VENDOR_DIR@"


def host_bins(names: list[str] | None = None) -> list[str]:
    """`make_package` bin names (default `hello`), spelled `<name>.exe` on a Windows host.

    Vendoring takes a Windows binary to be `<name>.exe`; a batch program must be an entry point.
    No vendor test launches one of these files on Windows, so their batch body is never run.
    """
    names = names or ["hello"]
    return [f"{name}.exe" for name in names] if sys.platform == "win32" else names


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


def hex16(digest: str) -> str:
    """The first 16 hex characters of a digest's hex part, without the algorithm (`p/<HEX16>`)."""
    algorithm, _, hex_part = digest.partition(":")
    assert algorithm and len(hex_part) >= 16, f"not a digest: {digest!r}"
    return hex_part[:16]


def _is_token_path(value: str, prefixes: list[str]) -> bool:
    return any(value == p or value.startswith(f"{p}/") for p in prefixes)


def assert_short_layout(out: Path, manifest: dict) -> None:
    """The vendored tree is `manifest.json`, `l/` and `p/<HEX16>/{c,e}` only; every name comes from the manifest.

    Reads the manifest the run wrote and the files on disk; derives each HEX16 from a
    package digest, never from a hard-coded value. A run with `content: absent` holds
    `manifest.json` alone, and the checks on files are skipped there.
    """
    present = manifest["content"] == "present"
    exe = ".exe" if manifest["platform"].startswith("windows/") else ""
    package_dirs: dict[str, str] = {}

    for package in manifest["packages"]:
        short = f"p/{hex16(package['digest'])}"
        package_dirs[package["digest"]] = short
        assert package["package_dir"] == short, package["package_dir"]
        assert package["content_dir"] == f"{short}/c", package["content_dir"]
        if present:
            assert (out / package["content_dir"]).is_dir(), package["content_dir"]
            for entry in package["tree"]["files"]:
                assert entry["path"].startswith(f"{short}/c/"), entry["path"]
            for entry in package["tree"]["links"]:
                assert entry["path"].startswith(f"{short}/c/"), entry["path"]
        for entry in package["entrypoints"]:
            _assert_launcher(
                out,
                manifest,
                entry,
                f"{short}/e/{entry['name']}{exe}",
                f"{short}/e/{entry['name']}.launch",
                present,
            )

    for tool in manifest["tools"]:
        if "launcher_path" not in tool:
            assert "record_path" not in tool, tool
            continue
        _assert_launcher(
            out,
            manifest,
            tool,
            f"l/{tool['name']}{exe}",
            f"l/{tool['name']}.launch",
            present,
        )

    for selection in manifest["selections"]:
        assert selection["launcher_dir"] == "l", selection["launcher_dir"]

    # The selection environment names the same `p/<HEX16>/c` paths as `packages[]`.
    token = manifest["dir_token"]
    allowed = [
        f"{token}/{prefix}"
        for short in package_dirs.values()
        for prefix in (f"{short}/c", f"{short}/e")
    ]
    for env in manifest["environments"]:
        for item in env["items"]:
            if item["value"].startswith(f"{token}/"):
                assert _is_token_path(item["value"], allowed), item["value"]

    if not present:
        return
    shorts = set(package_dirs.values())
    on_disk = sorted(p.relative_to(out).as_posix() for p in out.rglob("*"))
    assert "manifest.json" in on_disk
    for rel in on_disk:
        parts = rel.split("/")
        if parts == ["manifest.json"]:
            continue
        assert parts[0] in ("l", "p"), f"unexpected top-level entry {rel}"
        if parts[0] == "l":
            assert len(parts) <= 2, f"nested entry under l/: {rel}"
        elif len(parts) >= 2:
            assert f"p/{parts[1]}" in shorts, f"unknown package directory {rel}"
            if len(parts) >= 3:
                assert parts[2] in ("c", "e"), f"unexpected entry {rel}"
            if len(parts) >= 5 and parts[2] == "e":
                raise AssertionError(f"nested entry under e/: {rel}")


def _assert_launcher(
    out: Path,
    manifest: dict,
    entry: dict,
    launcher_path: str,
    record_path: str,
    present: bool,
) -> None:
    assert entry.get("launcher_path") == launcher_path, entry
    assert entry.get("record_path") == record_path, entry
    if not present:
        return
    assert (out / launcher_path).is_file(), launcher_path
    record_file = out / record_path
    assert record_file.is_file(), record_path
    record = json.loads(record_file.read_text(encoding="utf-8"))
    assert record["kind"] == "direct", record
    assert record["schema_version"] == 1, record
    assert record["vendor_id"] == manifest["vendor_id"], record
    assert record["self"] == launcher_path.removesuffix(".exe"), record


def multi_platform_package(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    platforms: tuple[str, ...] = ("linux/amd64", "linux/arm64", "darwin/arm64"),
) -> list[PackageInfo]:
    """One tag published for every platform: a multi-entry image index."""
    return [
        make_package(
            ocx, repo, "1.0.0", tmp_path / plat.replace("/", "_"), platform=plat
        )
        for plat in platforms
    ]


def cmake_with_ccache(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    """`cmake` depending publicly on `ccache`: the closure fixture.

    Returns (cmake, ccache). ccache's PATH is public, so it reaches the consumer view.
    """
    ccache_repo = f"{repo}_ccache"
    ccache = make_package(
        ocx, ccache_repo, "1.0.0", tmp_path / "ccache", bins=host_bins(["ccache"])
    )
    digest = fetch_platform_manifest_digest(ocx.registry, ccache_repo, "1.0.0")
    cmake = make_package(
        ocx,
        repo,
        "1.0.0",
        tmp_path / "cmake",
        bins=host_bins(["cmake"]),
        dependencies=[{"identifier": f"{ccache.fq}@{digest}", "visibility": "public"}],
    )
    return cmake, ccache


def links_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Relative symlinks in `content/`: file link, directory link, nested link.

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
    """`${installPath}` templates in constant, path and list-ish values.

    With `literal_token`, one value already holds `@VENDOR_DIR@`: vendoring must exit 65.
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
    return make_package(ocx, repo, "1.0.0", tmp_path, bins=host_bins(), env=env)


def entrypoint_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Entry points `alpha` and `gamma`; `gamma` also ships `bin/gamma.cmd` for the Windows launcher."""
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


_PATH_ENV = {
    "key": "PATH",
    "type": "path",
    "value": "${installPath}/bin",
    "visibility": "public",
}


def _tool(root: Path, name: str = "tool") -> None:
    (root / "bin").mkdir(exist_ok=True)
    script = root / "bin" / name
    script.write_text("#!/bin/sh\necho vendored\n")
    script.chmod(0o755)


def bazel_names_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Content holding `BUILD.bazel`, `BUILD` and a lower-case variant in a subdir.

    The variant sits in `pkg/` so a case-insensitive host does not fold it into
    `BUILD.bazel` while the tree is written.
    """

    def build(root: Path) -> None:
        _tool(root)
        (root / "BUILD.bazel").write_text("# package-authored\n")
        (root / "BUILD").write_text("# package-authored\n")
        (root / "pkg").mkdir()
        (root / "pkg" / "build.bazel").write_text("# case variant\n")

    return publish_tree(ocx, repo, "1.0.0", tmp_path, build, {"env": [_PATH_ENV]})


def empty_dir_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Content with an empty directory `share/empty` next to one tool."""

    def build(root: Path) -> None:
        _tool(root)
        (root / "share" / "empty").mkdir(parents=True)

    return publish_tree(ocx, repo, "1.0.0", tmp_path, build, {"env": [_PATH_ENV]})


def wasm_file_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """A `wasip1/wasm` package holding one `.wasm` file and no launcher."""

    def build(root: Path) -> None:
        (root / "share").mkdir()
        (root / "share" / "tool.wasm").write_bytes(b"\x00asm\x01\x00\x00\x00")

    return publish_tree(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        build,
        {
            "env": [
                {
                    "key": "TOOL_WASM",
                    "type": "constant",
                    "value": "${installPath}/share/tool.wasm",
                    "visibility": "public",
                }
            ]
        },
        platform="wasip1/wasm",
    )


def windows_only_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """A package with a `windows/amd64` leaf only: no Linux leaf; its binary is `bin/tool.exe`."""
    return make_package(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        bins=["tool.exe"],
        binaries=["tool"],
        platform="windows/amd64",
    )


def no_names_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """Declares no binaries and no entry points; only an env constant."""
    return make_package(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        bins=[],
        binaries=[],
        env=[
            {
                "key": "NONAMES_HOME",
                "type": "constant",
                "value": "${installPath}",
                "visibility": "public",
            }
        ],
    )


def setuid_package(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """`bin/tool` archived with mode 04755; vendoring must clear the bit. POSIX only."""

    def build(root: Path) -> None:
        _tool(root)
        (root / "bin" / "tool").chmod(0o4755)

    return publish_tree(ocx, repo, "1.0.0", tmp_path, build, {"env": [_PATH_ENV]})


def reserved_segment_package(
    ocx: OcxRunner, repo: str, tmp_path: Path, segment: str
) -> PackageInfo:
    """One tool plus a file at `<segment>/data`; vendoring refuses `_ocx_reserved` in any ASCII case."""

    def build(root: Path) -> None:
        _tool(root)
        (root / segment).mkdir()
        (root / segment / "data").write_text("payload\n", encoding="utf-8")

    return publish_tree(ocx, repo, "1.0.0", tmp_path, build, {"env": [_PATH_ENV]})


def launch_named_package(
    ocx: OcxRunner, repo: str, tmp_path: Path, name: str
) -> PackageInfo:
    """A package exposing `name` as a binary (an entry-point name cannot hold a dot, so only a binary can end in `.launch`)."""
    return make_package(
        ocx, repo, "1.0.0", tmp_path, bins=host_bins([name]), binaries=[name]
    )


def partial_platform_pair(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[PackageInfo], list[PackageInfo]]:
    """Two repos: `full` has darwin/arm64, `partial` lacks it.

    Selecting both must leave `darwin/arm64` out of `available_platforms`.
    """
    full = multi_platform_package(ocx, f"{repo}_full", tmp_path / "full")
    partial = multi_platform_package(
        ocx,
        f"{repo}_partial",
        tmp_path / "partial",
        platforms=("linux/amd64", "linux/arm64"),
    )
    return full, partial


class RequestRecorder:
    """A forward proxy a test hands to `ocx` as `HTTP_PROXY`/`HTTPS_PROXY`.

    Pass `env` as `env_overrides` of the `ocx` call, then read what was fetched.
    Never `registry_guard.py`: it answers 503 instead of forwarding.
    """

    def __init__(self, proxy: forward_proxy.ForwardProxy) -> None:
        self._proxy = proxy
        self.env = {"HTTP_PROXY": proxy.url, "HTTPS_PROXY": proxy.url}

    def blob_get_digests(self) -> list[str]:
        """Digests of every blob GET seen: config and layers alike; compare to the manifest's layers."""
        digests = []
        for request in self._proxy.requests:
            path = urllib.parse.urlsplit(request.absolute_uri).path
            if request.method == "GET" and "/blobs/" in path:
                digests.append(path.rsplit("/", 1)[-1])
        return digests

    def paths(self) -> list[str]:
        return [
            urllib.parse.urlsplit(request.absolute_uri).path
            for request in self._proxy.requests
        ]


@contextlib.contextmanager
def record_requests() -> Iterator[RequestRecorder]:
    with forward_proxy.running({}) as proxy:
        yield RequestRecorder(proxy)


def layer_digests(registry: str, repo: str, tag: str, platform: str) -> list[str]:
    """Layer blob digests of the platform manifest `tag` resolves to."""
    leaf = fetch_platform_manifest_digest(registry, repo, tag, platform=platform)
    manifest = get_manifest(registry, repo, leaf)
    return [layer["digest"] for layer in manifest["layers"]]


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


@pytest.fixture()
def vendor_request_recorder() -> Iterator[RequestRecorder]:
    with record_requests() as recorder:
        yield recorder
