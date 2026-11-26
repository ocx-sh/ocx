# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx vendor` (project tier), written from the design contracts.

Every case traces to a contract (C-xxx), verification row (V-xx) or scenario
(S-xxx) of the vendor design. A check that reports "fine" on good input also
carries its control: the same check shown failing on a deliberately bad input.
"""

from __future__ import annotations

import json
import os
import posixpath
import shlex
import stat
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import Any

import pytest

from src.helpers import make_package, resolved_metadata_path
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo, current_platform
from src.vendor_fixtures import (
    VENDOR_TOKEN,
    bazel_names_package,
    cmake_with_ccache,
    env_template_package,
    layer_digests,
    links_package,
    no_names_package,
    partial_platform_pair,
    record_requests,
    setuid_package,
    windows_only_package,
)

pytestmark = pytest.mark.command("vendor")

EXIT_USAGE = 64
EXIT_DATA = 65
EXIT_CONFIG = 78
EXIT_NOT_FOUND = 79

HOST = ("-p", current_platform())
posix_only = pytest.mark.skipif(
    sys.platform == "win32", reason="POSIX modes, symlinks and umask"
)
# 1980-01-01T00:00:00Z, the fixed mtime of every vendored directory.
FIXED_MTIME = 315532800
PATH_ENTRY = {
    "key": "PATH",
    "type": "path",
    "value": "${installPath}/bin",
    "visibility": "public",
}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _project(ocx: OcxRunner, root: Path, tools: dict[str, PackageInfo]) -> Path:
    """Write an `ocx.toml` declaring `tools`, lock it, return the project directory."""
    root.mkdir(parents=True)
    body = "[tools]\n" + "".join(f'{name} = "{pkg.fq}"\n' for name, pkg in tools.items())
    (root / "ocx.toml").write_text(body, encoding="utf-8")
    _ocx(ocx, root, "lock", "--no-pull", env=None)
    return root


def _ocx(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    env: dict[str, str] | None,
    check: bool = True,
    umask: str | None = None,
) -> subprocess.CompletedProcess[str]:
    argv = [str(ocx.binary), "--format", "json", *args]
    if umask is not None:
        argv = ["sh", "-c", f'umask {umask} && exec "$@"', "sh", *argv]
    result = subprocess.run(
        argv,
        cwd=cwd,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **(env or {})},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            f"{shlex.join(args)} failed (rc={result.returncode})\nstderr: {result.stderr.strip()}"
        )
    return result


def _vendor(
    ocx: OcxRunner,
    project: Path,
    out: Path,
    *args: str,
    env: dict[str, str] | None = None,
    check: bool = True,
    umask: str | None = None,
) -> subprocess.CompletedProcess[str]:
    return _ocx(
        ocx, project, "vendor", "-o", str(out), *args, env=env, check=check, umask=umask
    )


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


def _env_values(manifest: dict[str, Any]) -> list[str]:
    return [
        item["value"] for env in manifest["environments"] for item in env["items"]
    ]


def _bazel_findings(root: Path) -> list[str]:
    """Bazel files or terms outside package content and launcher bytes.

    Scans file names everywhere but package `content/`, and the text of the
    manifest and every `.direct` sidecar (launchers are ocx's own blob). The
    manifest's `packages[].tree` lists the names inside package content, which
    the scan exempts, so it is dropped before the scan.
    """
    terms = ("bazel", "runfiles", "workspace", "BUILD")
    names = {"BUILD", "BUILD.bazel", "WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel"}
    findings: list[str] = []
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root)
        if "content" in rel.parts and rel.parts[0] == "packages":
            continue
        if path.name in names:
            findings.append(f"file {rel.as_posix()}")
        if path.is_file() and (path.name == "manifest.json" or path.suffix == ".direct"):
            text = path.read_text(encoding="utf-8")
            if path.name == "manifest.json" and rel.parent == Path("."):
                document = json.loads(text)
                for package in document.get("packages", []):
                    package.pop("tree", None)
                text = json.dumps(document)
            findings += [f"{rel.as_posix()} names {t!r}" for t in terms if t in text]
    return findings


def _publish_with_link_target(
    ocx: OcxRunner, repo: str, tmp_path: Path, target: str
) -> PackageInfo:
    """Publish a package whose `bin/alias` link points at `target`.

    `ocx package create` refuses an absolute or escaping link, so the bundle is
    created with a valid link and the archive is rewritten before the push: the
    shape is what another publisher's tool could have produced.
    """
    tmp_path.mkdir(parents=True)
    tree = tmp_path / "tree"
    (tree / "bin").mkdir(parents=True)
    tool = tree / "bin" / "tool"
    tool.write_text("#!/bin/sh\n", encoding="utf-8")
    tool.chmod(0o755)
    os.symlink("tool", tree / "bin" / "alias")
    metadata = tmp_path / "metadata.json"
    metadata.write_text(
        json.dumps({"type": "bundle", "version": 1, "env": [PATH_ENTRY]}), encoding="utf-8"
    )
    bundle = tmp_path / "bundle.tar.xz"
    plat = current_platform()
    ocx.plain("package", "create", "-m", str(metadata), "-o", str(bundle), "-p", plat, str(tree))
    rewritten = tmp_path / "rewritten.tar.xz"
    with tarfile.open(bundle, "r:xz") as src, tarfile.open(rewritten, "w:xz") as dst:
        patched = 0
        for member in src.getmembers():
            if member.issym() and member.name.endswith("bin/alias"):
                member.linkname = target
                patched += 1
            dst.addfile(member, src.extractfile(member) if member.isfile() else None)
    assert patched == 1, "the link to rewrite was not found in the archive"
    fq = f"{ocx.registry}/{repo}:1.0.0"
    ocx.plain(
        "package", "push", "-p", plat, "-m", str(resolved_metadata_path(bundle)),
        "--cascade", "-i", fq, str(rewritten),
    )
    ocx.plain("index", "update", repo)
    return PackageInfo(
        repo=repo, tag="1.0.0", short=f"{repo}:1.0.0", fq=fq,
        content_dir=tree, marker="", platform=plat,
    )


def _publish(ocx: OcxRunner, repo: str, tmp_path: Path, name: str, **kwargs: Any) -> PackageInfo:
    return make_package(ocx, repo, "1.0.0", tmp_path / name, **kwargs)


# ---------------------------------------------------------------------------
# Happy path (smoke)
# ---------------------------------------------------------------------------


@pytest.mark.smoke
def test_vendor_writes_manifest_content_and_launchers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg", bins=["hello"])
    project = _project(ocx, tmp_path / "proj", {"hello": pkg})
    out = tmp_path / "out"

    result = _vendor(ocx, project, out, *HOST)

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
# No `-o`
# ---------------------------------------------------------------------------


def test_vendor_without_output_exits_usage_and_writes_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    # The control (the same call with `-o` writes) is the smoke case above.
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    project = _project(ocx, tmp_path / "proj", {"hello": pkg})
    before = _snapshot(project)

    result = _ocx(ocx, project, "vendor", *HOST, env=None, check=False)

    assert result.returncode == EXIT_USAGE, result.stderr
    assert _snapshot(project) == before
    assert not (tmp_path / "out").exists()


# ---------------------------------------------------------------------------
# The target must be absent or empty
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("state", ["control_empty_dir", "non_empty_dir"])
def test_vendor_refuses_non_empty_target_before_any_request(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, state: str
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    project = _project(ocx, tmp_path / "proj", {"hello": pkg})
    out = tmp_path / "out"
    out.mkdir()
    if state == "non_empty_dir":
        (out / "keep.txt").write_text("user data", encoding="utf-8")
    before = _snapshot(out)

    with record_requests() as recorder:
        result = _vendor(ocx, project, out, *HOST, env=recorder.env, check=False)

    if state == "control_empty_dir":
        assert result.returncode == 0, result.stderr
        return
    assert result.returncode == EXIT_USAGE, result.stderr
    assert _detail(result) == "vendor_output_not_empty"
    assert _snapshot(out) == before
    assert recorder.paths() == [], "the refusal must precede any network request"


@posix_only
def test_vendor_refuses_symlink_as_target(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    project = _project(ocx, tmp_path / "proj", {"hello": pkg})
    real = tmp_path / "real"
    real.mkdir()
    link = tmp_path / "out-link"
    os.symlink(real, link)

    result = _vendor(ocx, project, link, *HOST, check=False)

    assert result.returncode == EXIT_USAGE, result.stderr
    assert _detail(result) == "vendor_output_not_empty"
    assert list(real.iterdir()) == []
    assert link.is_symlink()


@pytest.mark.parametrize("target_exists", [False, True], ids=["absent", "empty_dir"])
def test_failed_vendor_leaves_target_as_it_was_and_no_staging(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, target_exists: bool
) -> None:
    pkg = env_template_package(ocx, unique_repo, tmp_path / "pkg", literal_token=True)
    project = _project(ocx, tmp_path / "proj", {"tpl": pkg})
    parent = tmp_path / "parent"
    parent.mkdir()
    out = parent / "out"
    if target_exists:
        out.mkdir()

    result = _vendor(ocx, project, out, *HOST, check=False)

    assert result.returncode == EXIT_DATA, result.stderr
    assert out.is_dir() is target_exists
    assert list(out.iterdir()) == [] if target_exists else True
    assert sorted(p.name for p in parent.iterdir()) == (["out"] if target_exists else [])
    # Control: the retry of a good selection into the same path succeeds.
    good = _publish(ocx, f"{unique_repo}_ok", tmp_path, "good")
    _vendor(ocx, _project(ocx, tmp_path / "proj2", {"hello": good}), out, *HOST)
    assert (out / "manifest.json").is_file()
    assert [p.name for p in parent.iterdir()] == ["out"]


# ---------------------------------------------------------------------------
# --manifest-only
# ---------------------------------------------------------------------------


def test_manifest_only_fetches_no_layer_and_equals_full_run_without_content(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg", bins=["tool"])
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    layers = set(layer_digests(ocx.registry, unique_repo, "1.0.0", pkg.platform))
    assert layers

    with record_requests() as recorder:
        _vendor(ocx, project, tmp_path / "m", "--manifest-only", *HOST, env=recorder.env)
    assert not layers & set(recorder.blob_get_digests()), "manifest-only fetched a layer"
    assert sorted(p.name for p in (tmp_path / "m").iterdir()) == ["manifest.json"]
    manifest_only = _manifest(tmp_path / "m")
    assert manifest_only["content"] == "absent"
    assert all("tree" not in package for package in manifest_only["packages"])

    with record_requests() as recorder:
        _vendor(ocx, project, tmp_path / "f", *HOST, env=recorder.env)
    # Control: the full run's log does hold the layer GETs the check looks for.
    assert layers <= set(recorder.blob_get_digests())
    full = _manifest(tmp_path / "f")
    assert full["content"] == "present"
    assert _strip_content(full) == _strip_content(manifest_only)

    # Control: a renamed fixture tool breaks the equality.
    other = _publish(ocx, f"{unique_repo}_renamed", tmp_path, "other", bins=["renamed"])
    other_project = _project(ocx, tmp_path / "proj_other", {"tool": other})
    _vendor(ocx, other_project, tmp_path / "o", *HOST)
    assert _strip_content(_manifest(tmp_path / "o")) != _strip_content(manifest_only)


# ---------------------------------------------------------------------------
# --manifest-only runs the trust checks
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", [[], ["--manifest-only"]], ids=["full", "manifest_only"])
def test_trust_policy_refuses_in_both_modes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, mode: list[str]
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    project = _project(ocx, tmp_path / "proj", {"hello": pkg})
    config = ocx.ocx_home / "config.toml"
    config.write_text(
        f'[[trust.policy]]\nscope = "{ocx.registry}/{unique_repo}"\n'
        'signers = [{ kind = "keyless", identity = "nobody@example.com",'
        ' oidc_issuer = "https://issuer.invalid" }]\n',
        encoding="utf-8",
    )

    # Both modes fail with the code `ocx package install` gives for the same policy
    # (an unsigned package under a covering rule is `no_signatures_found`, 79).
    installed = _ocx(ocx, tmp_path, "package", "install", pkg.fq, env=None, check=False)
    assert installed.returncode == EXIT_NOT_FOUND, installed.stderr
    refused = _vendor(ocx, project, tmp_path / "refused", *mode, *HOST, check=False)
    assert refused.returncode == installed.returncode, refused.stderr
    assert not (tmp_path / "refused").exists()

    # Control: with the rule removed the same vendor succeeds.
    config.unlink()
    _vendor(ocx, project, tmp_path / "allowed", *mode, *HOST)


# ---------------------------------------------------------------------------
# No Bazel file or term in what ocx writes beside package content
# ---------------------------------------------------------------------------


def test_vendor_output_carries_no_bazel_file_or_term(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    # The test's own name holds "bazel", and the repository name reaches the manifest.
    pkg = bazel_names_package(ocx, unique_repo.replace("bazel", "neutral"), tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    out = tmp_path / "out"
    _vendor(ocx, project, out, *HOST)

    assert _bazel_findings(out) == []
    # The package's own BUILD files and the case variant land in content, untouched.
    paths = {f["path"] for p in _manifest(out)["packages"] for f in p["tree"]["files"]}
    for name in ("BUILD.bazel", "BUILD", "pkg/build.bazel"):
        assert any(p.endswith(f"/content/{name}") for p in paths), name

    # Control: a planted BUILD.bazel and a planted term are found.
    (out / "selections" / "BUILD.bazel").write_text("", encoding="utf-8")
    manifest = out / "manifest.json"
    planted = json.loads(manifest.read_text(encoding="utf-8"))
    planted["note"] = "runfiles"
    manifest.write_text(json.dumps(planted), encoding="utf-8")
    findings = _bazel_findings(out)
    assert "file selections/BUILD.bazel" in findings
    assert any("'runfiles'" in f for f in findings)


# ---------------------------------------------------------------------------
# A selection brings its closure
# ---------------------------------------------------------------------------


def test_selection_brings_its_dependency_closure_and_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    cmake, ccache = cmake_with_ccache(ocx, unique_repo, tmp_path)
    out = tmp_path / "out"
    _vendor(ocx, _project(ocx, tmp_path / "proj", {"cmake": cmake}), out, *HOST)

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
        v.startswith(f"{VENDOR_TOKEN}/{ccache_package['content_dir']}/") for v in path_values
    ), path_values

    # Control: selecting ccache alone does not drag cmake in (the closure is directional).
    alone = tmp_path / "alone"
    _vendor(ocx, _project(ocx, tmp_path / "proj2", {"ccache": ccache}), alone, *HOST)
    assert len(_manifest(alone)["packages"]) == 1


# ---------------------------------------------------------------------------
# Values are resolved; only the token is non-literal
# ---------------------------------------------------------------------------


def test_environment_values_hold_only_the_token(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = env_template_package(ocx, unique_repo, tmp_path / "pkg")
    out = tmp_path / "out"
    _vendor(ocx, _project(ocx, tmp_path / "proj", {"tpl": pkg}), out, *HOST)

    manifest = _manifest(out)
    values = _env_values(manifest)
    assert values
    assert not [v for v in values if "${" in v]
    content_dir = manifest["packages"][0]["content_dir"]
    items = {i["key"]: i["value"] for e in manifest["environments"] for i in e["items"]}
    assert items["TPL_HOME"] == f"{VENDOR_TOKEN}/{content_dir}"
    assert items["TPL_DATA"] == f"{VENDOR_TOKEN}/{content_dir}/share/data"
    texts = [(out / "manifest.json").read_text(encoding="utf-8")] + [
        p.read_text(encoding="utf-8") for p in out.rglob("*.direct")
    ]
    for text in texts:
        assert str(out) not in text
        assert str(ocx.ocx_home) not in text

    # Control: the scan reports a planted `${` value.
    planted = json.loads(json.dumps(manifest))
    planted["environments"][0]["items"][0]["value"] = "${installPath}/bin"
    assert [v for v in _env_values(planted) if "${" in v]


def test_value_already_holding_the_token_is_refused(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    bad = env_template_package(ocx, unique_repo, tmp_path / "bad", literal_token=True)
    result = _vendor(
        ocx, _project(ocx, tmp_path / "proj", {"tpl": bad}), tmp_path / "out", *HOST, check=False
    )
    assert result.returncode == EXIT_DATA, result.stderr
    assert _detail(result) == "vendor_value_unrepresentable"

    # Control: the same fixture without the literal vendors.
    good = env_template_package(ocx, f"{unique_repo}_ok", tmp_path / "good")
    _vendor(ocx, _project(ocx, tmp_path / "proj2", {"tpl": good}), tmp_path / "out2", *HOST)


@posix_only
def test_value_holding_the_real_home_is_refused_under_a_symlinked_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A value spelling the home survives token replacement under either spelling of the home."""
    real_home = ocx.ocx_home.resolve()
    literal_home = {
        "key": "HOME_COPY",
        "type": "constant",
        "value": str(real_home),
        "visibility": "public",
    }
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg", bins=["tool"], env=[PATH_ENTRY, literal_home])
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    home_link = tmp_path / "home-link"
    os.symlink(real_home, home_link)

    # The home is named through the link; the value spells the canonical path.
    refused = _vendor(
        ocx, project, tmp_path / "out", *HOST, env={"OCX_HOME": str(home_link)}, check=False
    )
    assert refused.returncode == EXIT_DATA, refused.stderr
    assert _detail(refused) == "vendor_value_unrepresentable"
    assert not (tmp_path / "out").exists()

    # Control: the same project with a value that names no host path vendors.
    clean = _publish(
        ocx,
        f"{unique_repo}_clean",
        tmp_path,
        "clean",
        bins=["tool"],
        env=[PATH_ENTRY, {**literal_home, "value": "/opt/elsewhere"}],
    )
    _vendor(
        ocx,
        _project(ocx, tmp_path / "proj_clean", {"tool": clean}),
        tmp_path / "ok",
        *HOST,
        env={"OCX_HOME": str(home_link)},
    )


# ---------------------------------------------------------------------------
# Modes and mtimes
# ---------------------------------------------------------------------------


@posix_only
def test_setuid_bit_is_cleared_and_modes_and_mtimes_are_normalised(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    setuid = setuid_package(ocx, unique_repo, tmp_path / "suid")
    normal = _publish(ocx, f"{unique_repo}_n", tmp_path, "normal", bins=["plain"])
    ocx.plain("package", "pull", *HOST, setuid.fq)
    out = tmp_path / "out"

    # `package create` and extraction both clear the bit, so set it on the installed copy:
    # without it in the source, clearing it in the output proves nothing.
    installed = [
        p
        for p in (ocx.ocx_home / "packages").rglob("tool")
        if p.parent.name == "bin" and not p.is_symlink()
    ]
    assert installed, "the setuid fixture was not installed"
    for path in installed:
        path.chmod(0o4755)
    assert all(stat.S_IMODE(p.stat().st_mode) & stat.S_ISUID for p in installed)

    _vendor(
        ocx, _project(ocx, tmp_path / "proj", {"suid": setuid, "plain": normal}), out, *HOST
    )

    files = {
        f["path"]: f for p in _manifest(out)["packages"] for f in p["tree"]["files"]
    }
    tool = out / next(p for p in files if p.endswith("/content/bin/tool"))
    plain = out / next(p for p in files if p.endswith("/content/bin/plain"))
    assert stat.S_IMODE(tool.stat().st_mode) & 0o7022 == 0
    assert tool.stat().st_mode & stat.S_IXUSR
    # Control: a normal file keeps the mode it was archived with.
    assert stat.S_IMODE(plain.stat().st_mode) == 0o755

    dirs = [p for p in out.rglob("*") if p.is_dir() and not p.is_symlink()] + [out]
    assert dirs
    assert {stat.S_IMODE(d.stat().st_mode) for d in dirs} == {0o755}
    assert {int(d.stat().st_mtime) for d in dirs if d != out} == {FIXED_MTIME}


# ---------------------------------------------------------------------------
# Link targets are relative and stay inside content
# ---------------------------------------------------------------------------


@posix_only
@pytest.mark.parametrize(
    "target",
    ["tool", "/etc/passwd", "../../x"],
    ids=["control_relative_inside", "absolute", "escaping"],
)
def test_link_targets_must_be_relative_and_inside_content(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, target: str
) -> None:
    pkg = _publish_with_link_target(ocx, unique_repo, tmp_path / "pkg", target)
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    out = tmp_path / "out"

    result = _vendor(ocx, project, out, *HOST, check=False)

    if target == "tool":
        assert result.returncode == 0, result.stderr
        links = [l for p in _manifest(out)["packages"] for l in p["tree"]["links"]]
        assert [l["target"] for l in links] == ["tool"]
        return
    # Install refuses such a link first, so only the exit code is the contract here.
    assert result.returncode == EXIT_DATA, result.stderr
    assert not out.exists()


def test_links_package_vendors_relative_links(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    if sys.platform == "win32":
        pytest.skip("links_package needs symlink privilege")
    pkg = links_package(ocx, unique_repo, tmp_path / "pkg")
    out = tmp_path / "out"
    _vendor(ocx, _project(ocx, tmp_path / "proj", {"tool": pkg}), out, *HOST)
    links = [
        (p["content_dir"], l)
        for p in _manifest(out)["packages"]
        for l in p["tree"]["links"]
    ]
    assert sorted(l["target"] for _, l in links) == ["../lib/real/data.txt", "real", "tool"]
    # Relative, and resolving inside `content_dir` (`..` is allowed while it stays inside).
    for content_dir, link in links:
        assert not link["target"].startswith("/")
        resolved = posixpath.normpath(
            posixpath.join(posixpath.dirname(link["path"]), link["target"])
        )
        assert resolved.startswith(f"{content_dir}/"), link


# ---------------------------------------------------------------------------
# A package declaring no names yields no tools
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("manifest_only", [False, True], ids=["full", "manifest_only"])
def test_package_without_declared_names_yields_no_tools(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, manifest_only: bool
) -> None:
    flags = ["--manifest-only"] if manifest_only else []
    nonames = no_names_package(ocx, unique_repo, tmp_path / "nn")
    out = tmp_path / "out"
    _vendor(ocx, _project(ocx, tmp_path / "proj", {"nn": nonames}), out, *flags, *HOST)

    manifest = _manifest(out)
    assert manifest["tools"] == []
    (package,) = manifest["packages"]
    assert package["names_declared"] is False
    assert "NONAMES_HOME" in {
        i["key"] for e in manifest["environments"] for i in e["items"]
    }

    # Control: a fixture declaring one binary yields one tool and `true`.
    declared = _publish(ocx, f"{unique_repo}_d", tmp_path, "decl", bins=["one"])
    out2 = tmp_path / "out2"
    _vendor(ocx, _project(ocx, tmp_path / "proj2", {"d": declared}), out2, *flags, *HOST)
    control = _manifest(out2)
    assert list(_tools(control)) == ["one"]
    assert control["packages"][0]["names_declared"] is True


# ---------------------------------------------------------------------------
# Available platforms
# ---------------------------------------------------------------------------


def test_available_platforms_is_the_intersection_over_the_selection(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    full, partial = partial_platform_pair(ocx, unique_repo, tmp_path)

    both = tmp_path / "both"
    _vendor(
        ocx,
        _project(ocx, tmp_path / "p_both", {"full": full[0], "partial": partial[0]}),
        both,
        "--manifest-only",
        *HOST,
    )
    available = set(_manifest(both)["available_platforms"])
    assert "linux/amd64" in available and "linux/arm64" in available
    assert "darwin/arm64" not in available

    # Control: the selection without the partial binding lists darwin/arm64.
    only_full = tmp_path / "only_full"
    _vendor(
        ocx,
        _project(ocx, tmp_path / "p_full", {"full": full[0]}),
        only_full,
        "--manifest-only",
        *HOST,
    )
    assert "darwin/arm64" in set(_manifest(only_full)["available_platforms"])


def test_platform_without_host_leaf_vendors_only_when_named(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    if current_platform() == "windows/amd64":
        pytest.fail("the fixture has only a windows/amd64 leaf; the host must differ")
    pkg = windows_only_package(ocx, unique_repo, tmp_path / "win")
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})

    host_default = _vendor(ocx, project, tmp_path / "host", "--manifest-only", check=False)
    assert host_default.returncode == EXIT_CONFIG, host_default.stderr

    named = tmp_path / "named"
    _vendor(ocx, project, named, "--manifest-only", "-p", "windows/amd64")
    manifest = _manifest(named)
    assert manifest["platform"].startswith("windows/amd64")
    assert manifest["available_platforms"] == ["windows/amd64"]


# ---------------------------------------------------------------------------
# Stale lock
# ---------------------------------------------------------------------------


def test_stale_lock_exits_data_error_until_relocked(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    first = _publish(ocx, unique_repo, tmp_path, "first", bins=["first"])
    second = _publish(ocx, f"{unique_repo}_2", tmp_path, "second", bins=["second"])
    project = _project(ocx, tmp_path / "proj", {"first": first})
    with (project / "ocx.toml").open("a", encoding="utf-8") as handle:
        handle.write(f'second = "{second.fq}"\n')

    stale = _vendor(ocx, project, tmp_path / "stale", *HOST, check=False)
    assert stale.returncode == EXIT_DATA, stale.stderr
    assert _detail(stale) == "lock_stale"
    assert not (tmp_path / "stale").exists()

    # Control: relocking makes the same call succeed.
    _ocx(ocx, project, "lock", "--no-pull", env=None)
    _vendor(ocx, project, tmp_path / "fresh", *HOST)
    assert set(_tools(_manifest(tmp_path / "fresh"))) == {"first", "second"}


# ---------------------------------------------------------------------------
# Manifest and sidecar bytes are host-independent
# ---------------------------------------------------------------------------


@posix_only
def _tree_listing(root: Path) -> list[tuple[str, str, int, int]]:
    """Path, type, mode and mtime of every entry, links not followed.

    Directories, links and the files ocx writes carry fixed modes and the normalised mtime;
    package content keeps the archive's mtime and masked mode, equal across hosts.
    """
    rows = []
    for path in sorted(root.rglob("*")):
        st = path.lstat()
        mode, mtime = stat.S_IMODE(st.st_mode), st.st_mtime_ns
        if stat.S_ISLNK(st.st_mode):
            kind = "link"
        elif stat.S_ISDIR(st.st_mode):
            kind = "dir"
        else:
            kind = "file"
        rows.append((path.relative_to(root).as_posix(), kind, mode, mtime))
    return rows


def test_manifest_and_sidecars_are_byte_identical_across_hosts(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = env_template_package(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"tpl": pkg})
    other_home = tmp_path / "other-home"
    other_home.mkdir()
    other_cwd = tmp_path / "elsewhere"
    other_cwd.mkdir()
    ocx.run("index", "update", unique_repo, env_overrides={"OCX_HOME": str(other_home)})

    first = tmp_path / "first"
    _vendor(ocx, project, first, *HOST, umask="022", env={"TZ": "UTC"})
    second = tmp_path / "second-out" / "nested"
    second.parent.mkdir()
    result = _ocx(
        ocx,
        other_cwd,
        "--project",
        str(project / "ocx.toml"),
        "vendor",
        "-o",
        str(second),
        *HOST,
        env={"OCX_HOME": str(other_home), "TZ": "Pacific/Auckland"},
        umask="077",
    )
    assert result.returncode == 0, result.stderr

    sidecars = sorted(p.relative_to(first) for p in first.rglob("*.direct"))
    assert sidecars, "no .direct sidecar was written"
    assert sorted(p.relative_to(second) for p in second.rglob("*.direct")) == sidecars
    for rel in [Path("manifest.json"), *sidecars]:
        assert (first / rel).read_bytes() == (second / rel).read_bytes(), rel
    assert _tree_listing(first) == _tree_listing(second)

    # Control: a different selection does not give equal bytes.
    other = _publish(ocx, f"{unique_repo}_other", tmp_path, "other", bins=["other"])
    third = tmp_path / "third"
    _vendor(ocx, _project(ocx, tmp_path / "proj3", {"o": other}), third, *HOST)
    assert (third / "manifest.json").read_bytes() != (first / "manifest.json").read_bytes()


def test_vendor_id_is_a_function_of_platform_and_digests(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish(ocx, unique_repo, tmp_path, "pkg")
    project = _project(ocx, tmp_path / "proj", {"hello": pkg})
    _vendor(ocx, project, tmp_path / "a", "--manifest-only", *HOST)
    _vendor(ocx, project, tmp_path / "b", *HOST)
    digest = fetch_platform_manifest_digest(
        ocx.registry, unique_repo, "1.0.0", platform=pkg.platform
    )
    a, b = _manifest(tmp_path / "a"), _manifest(tmp_path / "b")
    assert a["vendor_id"] == b["vendor_id"]
    assert [p["digest"] for p in a["packages"]] == [digest]
