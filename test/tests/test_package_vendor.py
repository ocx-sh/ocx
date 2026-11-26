# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package vendor` (identifier tier), written from the design contracts.

Counterparts of `test_vendor.py` for a selection given as package identifiers
instead of a project. Every case traces to a contract (C-xxx) or verification
row (V-xx) of the vendor design, and a check that reports "fine" carries its
control: the same check shown failing on a deliberately bad input.
"""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo, current_platform
from src.vendor_fixtures import (
    VENDOR_TOKEN,
    cmake_with_ccache,
    env_template_package,
    host_bins,
    layer_digests,
    links_package,
    partial_platform_pair,
    record_requests,
    wasm_file_package,
    windows_only_package,
)

pytestmark = pytest.mark.command("package_vendor")

EXIT_USAGE = 64
EXIT_DATA = 65
EXIT_NOT_FOUND = 79

HOST = ("-p", current_platform())
posix_only = pytest.mark.skipif(sys.platform == "win32", reason="POSIX symlinks")


def _pvendor(
    ocx: OcxRunner,
    out: Path | None,
    *args: str,
    env: dict[str, str] | None = None,
    check: bool = True,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx package vendor [-o out] <args>`; `args` carry flags and identifiers."""
    output = ["-o", str(out)] if out is not None else []
    argv = [str(ocx.binary), "--format", "json", "package", "vendor", *output, *args]
    result = subprocess.run(
        argv,
        cwd=cwd or ocx.ocx_home,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **(env or {})},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            f"{shlex.join(argv[3:])} failed (rc={result.returncode})\nstderr: {result.stderr.strip()}"
        )
    return result


def _manifest(out: Path) -> dict[str, Any]:
    return json.loads((out / "manifest.json").read_text(encoding="utf-8"))


def _detail(result: subprocess.CompletedProcess[str]) -> str:
    return json.loads(result.stdout)["error"]["detail"]


def _snapshot(root: Path) -> dict[str, bytes | None]:
    """Every node under `root` (symlinks not followed): path -> bytes, `None` for a directory."""
    nodes: dict[str, bytes | None] = {}
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        if path.is_symlink():
            nodes[rel] = b"-> " + os.readlink(path).encode()
        else:
            nodes[rel] = None if path.is_dir() else path.read_bytes()
    return nodes


def _strip_content(manifest: dict[str, Any]) -> dict[str, Any]:
    """The manifest minus `content` and every `packages[].tree`."""
    stripped = json.loads(json.dumps(manifest))
    stripped.pop("content", None)
    for package in stripped["packages"]:
        package.pop("tree", None)
    return stripped


def _tools(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {tool["name"]: tool for tool in manifest["tools"]}


def _publish(
    ocx: OcxRunner, repo: str, tmp_path: Path, name: str, **kwargs: Any
) -> PackageInfo:
    kwargs["bins"] = host_bins(kwargs.get("bins"))
    return make_package(ocx, repo, "1.0.0", tmp_path / name, **kwargs)


# ---------------------------------------------------------------------------
# Happy path (smoke)
# ---------------------------------------------------------------------------


@pytest.mark.smoke
def test_package_vendor_writes_manifest_content_and_launchers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg", bins=["hello"])
    out = tmp_path / "out"

    result = _pvendor(ocx, out, *HOST, pkg.fq)

    report = json.loads(result.stdout)
    assert report["output_dir"] == str(out)
    assert report["manifest_path"] == str(out / "manifest.json")
    assert report["content"] == "present"
    assert report["tool_count"] == 1
    manifest = _manifest(out)
    assert manifest["schema_version"] == 1
    assert manifest["content"] == "present"
    assert manifest["dir_token"] == VENDOR_TOKEN
    tool = _tools(manifest)["hello"]
    assert (out / tool["launcher_path"]).is_file()
    assert (out / tool["target_path"]).is_file()


# ---------------------------------------------------------------------------
# No `-o`, no identifier
# ---------------------------------------------------------------------------


def test_package_vendor_without_output_exits_usage_and_writes_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    # The control (the same call with `-o` writes) is the smoke case above.
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    workdir = tmp_path / "work"
    workdir.mkdir()

    result = _pvendor(ocx, None, *HOST, pkg.fq, check=False, cwd=workdir)

    assert result.returncode == EXIT_USAGE, result.stderr
    assert _snapshot(workdir) == {}
    assert not (tmp_path / "out").exists()


def test_package_vendor_without_identifier_exits_usage_and_writes_nothing(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    out = tmp_path / "out"

    result = _pvendor(ocx, out, *HOST, check=False)

    assert result.returncode == EXIT_USAGE, result.stderr
    assert not out.exists()


# ---------------------------------------------------------------------------
# The target must be absent or empty
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("state", ["control_empty_dir", "non_empty_dir"])
def test_package_vendor_refuses_non_empty_target_before_any_request(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, state: str
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    out = tmp_path / "out"
    out.mkdir()
    if state == "non_empty_dir":
        (out / "keep.txt").write_text("user data", encoding="utf-8")
    before = _snapshot(out)

    with record_requests() as recorder:
        result = _pvendor(ocx, out, *HOST, pkg.fq, env=recorder.env, check=False)

    if state == "control_empty_dir":
        assert result.returncode == 0, result.stderr
        return
    assert result.returncode == EXIT_USAGE, result.stderr
    assert _detail(result) == "vendor_output_not_empty"
    assert _snapshot(out) == before
    assert recorder.paths() == [], "the refusal must precede any network request"


# ---------------------------------------------------------------------------
# --manifest-only
# ---------------------------------------------------------------------------


def test_package_manifest_only_fetches_no_layer_and_equals_full_run_without_content(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg", bins=["tool"])
    layers = set(layer_digests(ocx.registry, unique_repo, "1.0.0", pkg.platform))
    assert layers

    with record_requests() as recorder:
        _pvendor(
            ocx, tmp_path / "m", "--manifest-only", *HOST, pkg.fq, env=recorder.env
        )
    assert not layers & set(recorder.blob_get_digests()), (
        "manifest-only fetched a layer"
    )
    assert sorted(p.name for p in (tmp_path / "m").iterdir()) == ["manifest.json"]
    manifest_only = _manifest(tmp_path / "m")
    assert manifest_only["content"] == "absent"
    assert all("tree" not in package for package in manifest_only["packages"])

    with record_requests() as recorder:
        _pvendor(ocx, tmp_path / "f", *HOST, pkg.fq, env=recorder.env)
    # Control: the full run's log does hold the layer GETs the check looks for.
    assert layers <= set(recorder.blob_get_digests())
    full = _manifest(tmp_path / "f")
    assert full["content"] == "present"
    assert _strip_content(full) == _strip_content(manifest_only)

    # Control: a package with a renamed tool breaks the equality.
    other = _publish(ocx, f"{unique_repo}_renamed", tmp_path, "other", bins=["renamed"])
    _pvendor(ocx, tmp_path / "o", "--manifest-only", *HOST, other.fq)
    assert _strip_content(_manifest(tmp_path / "o")) != _strip_content(manifest_only)


# ---------------------------------------------------------------------------
# An identifier brings its closure
# ---------------------------------------------------------------------------


def test_package_vendor_brings_dependency_closure_and_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    cmake, ccache = cmake_with_ccache(ocx, unique_repo, tmp_path)
    out = tmp_path / "out"
    _pvendor(ocx, out, *HOST, cmake.fq)

    manifest = _manifest(out)
    by_repo = {p["identifier"].split("@")[0]: p for p in manifest["packages"]}
    assert len(by_repo) == 2
    ccache_package = next(p for ident, p in by_repo.items() if ccache.repo in ident)
    path_values = [
        item["value"]
        for env in manifest["environments"]
        if env["scope"]["type"] == "selection"
        for item in env["items"]
        if item["key"] == "PATH"
    ]
    assert any(
        v.startswith(f"{VENDOR_TOKEN}/{ccache_package['content_dir']}/")
        for v in path_values
    ), path_values

    # Control: naming ccache alone does not drag cmake in (the closure is directional).
    alone = tmp_path / "alone"
    _pvendor(ocx, alone, *HOST, ccache.fq)
    assert len(_manifest(alone)["packages"]) == 1


# ---------------------------------------------------------------------------
# Bad values are refused
# ---------------------------------------------------------------------------


def test_package_vendor_refuses_value_already_holding_the_token(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    bad = env_template_package(ocx, unique_repo, tmp_path / "bad", literal_token=True)
    refused = tmp_path / "refused"

    result = _pvendor(ocx, refused, *HOST, bad.fq, check=False)

    assert result.returncode == EXIT_DATA, result.stderr
    assert _detail(result) == "vendor_value_unrepresentable"
    assert not refused.exists()

    # Control: the same fixture without the literal vendors.
    good = env_template_package(ocx, f"{unique_repo}_ok", tmp_path / "good")
    _pvendor(ocx, tmp_path / "ok", *HOST, good.fq)
    assert (tmp_path / "ok" / "manifest.json").is_file()


# ---------------------------------------------------------------------------
# --omit-links
# ---------------------------------------------------------------------------


@posix_only
@pytest.mark.parametrize(
    "omit", [False, True], ids=["control_links_written", "omit_links"]
)
def test_package_vendor_omit_links_writes_no_symlink_and_keeps_listed_links(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, omit: bool
) -> None:
    pkg = links_package(ocx, unique_repo, tmp_path / "pkg")
    out = tmp_path / "out"

    _pvendor(ocx, out, *(["--omit-links"] if omit else []), *HOST, pkg.fq)

    links = [link for p in _manifest(out)["packages"] for link in p["tree"]["links"]]
    assert sorted(link["target"] for link in links) == [
        "../lib/real/data.txt",
        "real",
        "tool",
    ]
    on_disk = [p for p in out.rglob("*") if p.is_symlink()]
    if omit:
        assert on_disk == []
        return
    for link in links:
        assert os.readlink(out / link["path"]) == link["target"]
    assert len(on_disk) == len(links)


# ---------------------------------------------------------------------------
# Available platforms and `-p`
# ---------------------------------------------------------------------------


def test_package_vendor_available_platforms_is_the_intersection_over_the_identifiers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    full, partial = partial_platform_pair(ocx, unique_repo, tmp_path)

    both = tmp_path / "both"
    _pvendor(ocx, both, "--manifest-only", *HOST, full[0].fq, partial[0].fq)
    available = set(_manifest(both)["available_platforms"])
    assert "linux/amd64" in available and "linux/arm64" in available
    assert "darwin/arm64" not in available

    # Control: the identifier without the partial package lists darwin/arm64.
    only_full = tmp_path / "only_full"
    _pvendor(ocx, only_full, "--manifest-only", *HOST, full[0].fq)
    assert "darwin/arm64" in set(_manifest(only_full)["available_platforms"])


def test_package_vendor_platform_without_host_leaf_vendors_only_when_named(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    if current_platform() == "windows/amd64":
        pytest.fail("the fixture has only a windows/amd64 leaf; the host must differ")
    pkg = windows_only_package(ocx, unique_repo, tmp_path / "win")

    host_default = _pvendor(
        ocx, tmp_path / "host", "--manifest-only", pkg.fq, check=False
    )
    # The package tier resolves the identifier itself, so a missing leaf is "package not found";
    # exit 78 is the project tier's answer for a binding (design § 5.2).
    assert host_default.returncode == EXIT_NOT_FOUND, host_default.stderr
    # The same answer `ocx package pull` gives for the host platform.
    pulled = ocx.run("package", "pull", pkg.fq, check=False)
    assert pulled.returncode == host_default.returncode, pulled.stderr

    named = tmp_path / "named"
    _pvendor(ocx, named, "--manifest-only", "-p", "windows/amd64", pkg.fq)
    manifest = _manifest(named)
    assert manifest["platform"].startswith("windows/amd64")
    assert manifest["available_platforms"] == ["windows/amd64"]


# ---------------------------------------------------------------------------
# A platform with no launcher blob vendors without one
# ---------------------------------------------------------------------------


def test_package_vendor_wasm_package_has_files_and_no_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    wasm_file = wasm_file_package(ocx, unique_repo, tmp_path / "file")
    wasm_tool = _publish(
        ocx,
        f"{unique_repo}_tool",
        tmp_path,
        "tool",
        bins=["tool"],
        binaries=["tool"],
        platform="wasip1/wasm",
    )
    out = tmp_path / "out"

    _pvendor(ocx, out, "-p", "wasip1/wasm", wasm_file.fq, wasm_tool.fq)

    manifest = _manifest(out)
    assert manifest["platform"].startswith("wasip1/wasm")
    files = {f["path"] for p in manifest["packages"] for f in p["tree"]["files"]}
    wasm_path = next(p for p in sorted(files) if p.endswith("/content/share/tool.wasm"))
    assert (out / wasm_path).read_bytes() == b"\x00asm\x01\x00\x00\x00"
    tool = _tools(manifest)["tool"]
    assert "launcher_path" not in tool
    assert (out / tool["target_path"]).is_file()

    # Control: the same tool on the host platform does get a launcher.
    host_tool = _publish(ocx, f"{unique_repo}_host", tmp_path, "host", bins=["tool"])
    host_out = tmp_path / "host_out"
    _pvendor(ocx, host_out, *HOST, host_tool.fq)
    host_manifest_tool = _tools(_manifest(host_out))["tool"]
    assert (host_out / host_manifest_tool["launcher_path"]).is_file()
