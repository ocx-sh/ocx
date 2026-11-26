# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Parity of `ocx vendor` output with what `ocx` itself computes (design § 4.1, § 4.2).

A vendored directory is a second description of an install: its environments,
its file tree and its launchers must equal the ones `ocx env`, the installed
`content/` and `ocx exec` give for the same selection. Every equality carries
its control: the same comparison shown failing on one deliberately bad input.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any
from uuid import uuid4

import pytest
from jsonschema import Draft202012Validator

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo, current_platform, registry_dir
from src.vendor_fixtures import (
    VENDOR_TOKEN,
    empty_dir_package,
    entrypoint_package,
    links_package,
    publish_tree,
    windows_only_package,
)

pytestmark = pytest.mark.command("vendor")

HOST = ("-p", current_platform())
posix_only = pytest.mark.skipif(
    sys.platform == "win32", reason="POSIX shell probe, modes and symlinks"
)
PATH_ENTRY = {
    "key": "PATH",
    "type": "path",
    "value": "${installPath}/bin",
    "visibility": "public",
}
GOLDEN_DIR = Path(__file__).resolve().parents[1] / "fixtures" / "vendor"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _project(ocx: OcxRunner, root: Path, tools: dict[str, PackageInfo]) -> Path:
    """Write an `ocx.toml` declaring `tools`, lock it, return the project directory."""
    root.mkdir(parents=True)
    body = "[tools]\n" + "".join(
        f'{name} = "{pkg.fq}"\n' for name, pkg in tools.items()
    )
    (root / "ocx.toml").write_text(body, encoding="utf-8")
    _ocx(ocx, root, "lock", "--no-pull")
    return root


def _ocx(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    env: dict[str, str] | None = None,
    json_format: bool = True,
) -> subprocess.CompletedProcess[str]:
    argv = [str(ocx.binary), *(["--format", "json"] if json_format else []), *args]
    result = subprocess.run(
        argv,
        cwd=cwd,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **(env or {})},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    if result.returncode != 0:
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
) -> None:
    _ocx(ocx, project, "vendor", "-o", str(out), *args, env=env)


def _manifest(out: Path) -> dict[str, Any]:
    return json.loads((out / "manifest.json").read_text(encoding="utf-8"))


def _store_to_token(text: str, ocx: OcxRunner) -> str:
    """The manifest's rewrite of an installed package path: `<home>/packages/` -> `@VENDOR_DIR@/packages/`."""
    prefix = (ocx.ocx_home / "packages").as_posix() + "/"
    return text.replace(prefix, f"{VENDOR_TOKEN}/packages/")


def _expected_items(report: dict[str, Any], ocx: OcxRunner) -> list[dict[str, Any]]:
    """A `ocx env` / `package env` report's items, store paths replaced by the vendor token (C-005)."""
    return [
        {**item, "value": _store_to_token(item["value"], ocx)}
        for item in report["items"]
    ]


def _environment(manifest: dict[str, Any], env_id: str) -> list[dict[str, Any]]:
    matches = [env for env in manifest["environments"] if env["id"] == env_id]
    assert len(matches) == 1, (
        f"environment {env_id!r} not exactly once in {manifest['environments']}"
    )
    return matches[0]["items"]


def _publish_probe(ocx: OcxRunner, repo: str, tmp_path: Path) -> PackageInfo:
    """A tool that prints argv then its environment, with a path, a list and a constant entry (V-22)."""

    def build(root: Path) -> None:
        (root / "bin").mkdir()
        probe = root / "bin" / "probe"
        probe.write_text(
            '#!/bin/sh\nfor a in "$0" "$@"; do printf "arg=%s\\n" "$a"; done\necho ---\nenv\n',
            encoding="utf-8",
        )
        probe.chmod(0o755)

    return publish_tree(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        build,
        {
            "env": [
                PATH_ENTRY,
                {
                    "key": "PROBE_LIST",
                    "type": "list",
                    "separator": ":",
                    "value": "from-package",
                    "visibility": "public",
                },
                {
                    "key": "PROBE_CONST",
                    "type": "constant",
                    "value": "${installPath}/share",
                    "visibility": "public",
                },
            ]
        },
    )


def _parse_probe(stdout: str) -> tuple[list[str], dict[str, str]]:
    head, sep, tail = stdout.partition("---\n")
    assert sep, f"probe output has no separator: {stdout!r}"
    args = [line.removeprefix("arg=") for line in head.splitlines()]
    env = dict(line.split("=", 1) for line in tail.splitlines() if "=" in line)
    return args, env


# Only `ocx exec` writes these; the launcher is specified not to (design § 4.2, V-22).
_EXEC_ONLY = {"__OCX_ENV_STATE", "OCX_BINARY_PIN"}
# Set by the probe's own shell, not by the launcher or `ocx exec`.
_SHELL_NOISE = {"_", "SHLVL", "OLDPWD"}


def _comparable(
    args: list[str], env: dict[str, str], root: str
) -> tuple[list[str], dict[str, str]]:
    """Argv and environment with the package root replaced by `@PKG@` and writer-specific keys dropped."""
    drop = _EXEC_ONLY | _SHELL_NOISE
    return (
        [a.replace(root, "@PKG@") for a in args],
        {k: v.replace(root, "@PKG@") for k, v in env.items() if k not in drop},
    )


def _tree(root: Path) -> dict[str, tuple[Any, ...]]:
    """Every node under `root`, symlinks not followed: kind, content or target, exec bit."""
    nodes: dict[str, tuple[Any, ...]] = {}
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        if path.is_symlink():
            nodes[rel] = ("link", os.readlink(path))
        elif path.is_dir():
            nodes[rel] = ("dir",)
        else:
            executable = (
                bool(path.stat().st_mode & stat.S_IXUSR)
                if sys.platform != "win32"
                else None
            )
            nodes[rel] = ("file", path.read_bytes(), executable)
    return nodes


def _tree_diff(installed: Path, vendored: Path, *, omit_links: bool) -> list[str]:
    expected = _tree(installed)
    if omit_links:
        expected = {rel: node for rel, node in expected.items() if node[0] != "link"}
    actual = _tree(vendored)
    problems = [f"missing {rel}" for rel in expected.keys() - actual.keys()]
    problems += [f"extra {rel}" for rel in actual.keys() - expected.keys()]
    problems += [
        f"differs {rel}"
        for rel in expected.keys() & actual.keys()
        if expected[rel] != actual[rel]
    ]
    return sorted(problems)


def _installed_content(ocx: OcxRunner, package: dict[str, Any]) -> Path:
    return ocx.ocx_home / package["content_dir"]


# ---------------------------------------------------------------------------
# V-20 / C-005: environments equal `ocx env`
# ---------------------------------------------------------------------------


def test_selection_environment_equals_ocx_env_report(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish_probe(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"probe": pkg})
    out = tmp_path / "out"

    _vendor(ocx, project, out, *HOST)

    report = json.loads(_ocx(ocx, project, "env", "--pinned").stdout)
    expected = _expected_items(report, ocx)
    actual = _environment(_manifest(out), "selection:default")
    assert len(expected) == 3, f"the probe declares three entries: {expected}"
    assert actual == expected

    mutated = json.loads(json.dumps(expected))
    mutated[0]["value"] += "/mutated"
    assert actual != mutated, "the comparison must see one changed entry"


def test_entrypoint_environment_equals_package_env_self(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = entrypoint_package(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"ep": pkg})
    out = tmp_path / "out"

    _vendor(ocx, project, out, *HOST)

    manifest = _manifest(out)
    entry_envs = [
        e for e in manifest["environments"] if e["scope"]["type"] == "entrypoint"
    ]
    assert {e["scope"]["name"] for e in entry_envs} == {"alpha", "gamma"}
    expected = _expected_items(
        json.loads(_ocx(ocx, project, "package", "env", "--self", pkg.fq).stdout), ocx
    )
    for env in entry_envs:
        assert env["items"] == expected, env["id"]


# ---------------------------------------------------------------------------
# V-20b / C-006: patches never reach the manifest
# ---------------------------------------------------------------------------


def test_matching_patch_rule_leaves_manifest_byte_identical(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    companion = make_package(
        ocx,
        f"{unique_repo}_companion",
        "1.0.0",
        tmp_path / "companion",
        bins=[],
        env=[
            {
                "key": "PATCH_COMPANION",
                "type": "constant",
                "value": "present",
                "visibility": "public",
            }
        ],
        platform="any",
    )
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path / "base", bins=["tool"])
    project = _project(ocx, tmp_path / "proj", {"tool": base})
    plain = tmp_path / "plain"
    _vendor(ocx, project, plain, *HOST)

    (ocx.ocx_home / "config.toml").write_text(
        f'[patches]\nregistry = "{registry}/p{uuid4().hex[:12]}"\nrequired = true\n',
        encoding="utf-8",
    )
    descriptor = tmp_path / "descriptor.json"
    descriptor.write_text(
        json.dumps(
            {"version": 1, "rules": [{"match": "*", "packages": [companion.fq]}]}
        ),
        encoding="utf-8",
    )
    ocx.plain("patch", "publish", "--descriptor", str(descriptor), base.fq)

    # Control: the rule is live, so `ocx env` of the same project carries the companion variable.
    env_keys = {
        i["key"]
        for i in json.loads(_ocx(ocx, project, "env", "--pinned").stdout)["items"]
    }
    assert "PATCH_COMPANION" in env_keys, (
        "the patch rule must take effect under `ocx env`"
    )

    patched = tmp_path / "patched"
    _vendor(ocx, project, patched, *HOST)

    assert (patched / "manifest.json").read_bytes() == (
        plain / "manifest.json"
    ).read_bytes()


# ---------------------------------------------------------------------------
# V-21 / C-007, C-011: files and links equal the installed content
# ---------------------------------------------------------------------------


def test_vendored_tree_equals_installed_content_with_empty_dir(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = empty_dir_package(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    out = tmp_path / "out"

    _vendor(ocx, project, out, *HOST)

    (package,) = _manifest(out)["packages"]
    installed = _installed_content(ocx, package)
    vendored = out / package["content_dir"]
    assert (vendored / "share" / "empty").is_dir()
    assert list((vendored / "share" / "empty").iterdir()) == []
    assert len(_tree(installed)) >= 3, (
        "the installed tree must hold bin, tool, share and share/empty"
    )
    assert _tree_diff(installed, vendored, omit_links=False) == []

    tool = vendored / "bin" / "tool"
    tool.chmod(0o755)
    tool.write_bytes(tool.read_bytes()[:-1] + b"X")
    assert _tree_diff(installed, vendored, omit_links=False) == ["differs bin/tool"], (
        "one flipped byte must be reported"
    )


@posix_only
@pytest.mark.parametrize("omit_links", [False, True], ids=["links", "omit-links"])
def test_vendored_links_follow_omit_links_flag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, omit_links: bool
) -> None:
    pkg = links_package(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    out = tmp_path / "out"

    _vendor(ocx, project, out, *HOST, *(["--omit-links"] if omit_links else []))

    manifest = _manifest(out)
    (package,) = manifest["packages"]
    installed = _installed_content(ocx, package)
    vendored = out / package["content_dir"]
    installed_links = {
        rel for rel, node in _tree(installed).items() if node[0] == "link"
    }
    assert len(installed_links) == 3, (
        f"the fixture publishes three links: {installed_links}"
    )
    assert _tree_diff(installed, vendored, omit_links=omit_links) == []
    on_disk = {rel for rel, node in _tree(vendored).items() if node[0] == "link"}
    assert on_disk == (set() if omit_links else installed_links)
    # C-011: the manifest still lists every link either way.
    listed = {
        link["path"].removeprefix(package["content_dir"] + "/")
        for link in package["tree"]["links"]
    }
    assert listed == installed_links


# ---------------------------------------------------------------------------
# V-22 / C-014, C-016: a vendored launcher yields the child `ocx exec` yields
# ---------------------------------------------------------------------------


@posix_only
def test_launcher_child_matches_ocx_exec_child(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = _publish_probe(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"probe": pkg})
    out = tmp_path / "out"
    _vendor(ocx, project, out, *HOST)

    fake_package_bin = tmp_path / "caller-package" / "bin"
    fake_package_bin.mkdir(parents=True)
    caller = {
        "PATH": f"{fake_package_bin}{os.pathsep}{ocx.env['PATH']}",
        "PROBE_LIST": "caller-a:caller-b",
        "OCX_IDENTITY_TOKEN": "credential-value",
        "OCX_ANNOUNCE_TOKEN": "secret-input-value",
    }
    argv = ["a b", "", "--flag=x"]

    via_exec = subprocess.run(
        [str(ocx.binary), "exec", "--", "probe", *argv],
        cwd=project,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **caller},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    assert via_exec.returncode == 0, via_exec.stderr
    launcher = out / _manifest(out)["tools"][0]["launcher_path"]
    via_launcher = subprocess.run(
        [str(launcher), *argv],
        cwd=project,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **caller},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    assert via_launcher.returncode == 0, via_launcher.stderr

    package = _manifest(out)["packages"][0]
    exec_args, exec_env = _comparable(
        *_parse_probe(via_exec.stdout),
        str(project / ".ocx" / "toolchain" / "links" / "default" / "probe"),
    )
    launch_args, launch_env = _comparable(
        *_parse_probe(via_launcher.stdout), str(out / package["package_dir"])
    )
    assert launch_args == exec_args
    assert launch_env == exec_env
    assert launch_env["PROBE_LIST"] == "caller-a:caller-b:from-package"
    assert launch_env["PATH"].split(os.pathsep)[1] == str(fake_package_bin)
    assert "OCX_IDENTITY_TOKEN" not in launch_env, (
        "a Credential never reaches the child"
    )
    assert launch_env["OCX_ANNOUNCE_TOKEN"] == "secret-input-value", (
        "a SecretInput passes through"
    )

    # Control 1: the probe does see the credential when nothing strips it.
    direct = subprocess.run(
        [str(out / package["content_dir"] / "bin" / "probe")],
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **caller},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    assert "OCX_IDENTITY_TOKEN" in _parse_probe(direct.stdout)[1]

    # Control 2: a sidecar with `unset` emptied lets the credential through.
    sidecar = launcher.with_name(launcher.name + ".direct")
    document = json.loads(sidecar.read_text(encoding="utf-8"))
    assert "OCX_IDENTITY_TOKEN" in document["unset"]
    document["unset"] = []
    sidecar.write_text(json.dumps(document), encoding="utf-8")
    leaked = subprocess.run(
        [str(launcher)],
        cwd=project,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **caller},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    assert "OCX_IDENTITY_TOKEN" in _parse_probe(leaked.stdout)[1]


# ---------------------------------------------------------------------------
# V-40 / C-021: the manifest validates against the generated schema
# ---------------------------------------------------------------------------


def _manifest_schema() -> dict[str, Any]:
    binary = os.environ.get("OCX_SCHEMA_BINARY")
    if binary and Path(binary).exists():
        result = subprocess.run(
            [binary, "vendor-manifest"],
            capture_output=True,
            encoding="utf-8",
            timeout=60,
            check=False,
        )
        assert result.returncode == 0, (
            f"ocx_schema vendor-manifest failed: {result.stderr}"
        )
        return json.loads(result.stdout)
    pytest.skip("no vendor-manifest schema: OCX_SCHEMA_BINARY is unset or absent (Bazel names it)")


def test_real_manifest_validates_against_published_schema(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    schema = _manifest_schema()
    pkg = _publish_probe(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"probe": pkg})
    out = tmp_path / "out"
    _vendor(ocx, project, out, *HOST)
    manifest = _manifest(out)

    validator = Draft202012Validator(schema)
    assert list(validator.iter_errors(manifest)) == []

    broken = json.loads(json.dumps(manifest))
    del broken["schema_version"]
    assert list(validator.iter_errors(broken)), (
        "a manifest without schema_version must be rejected"
    )


# ---------------------------------------------------------------------------
# V-46 / C-008: a `windows/amd64` manifest written here equals the golden
# ---------------------------------------------------------------------------

_HEX64 = re.compile(r"[0-9a-f]{64}")
_STORE_SPLIT = re.compile(r"/[0-9a-f]{2}/[0-9a-f]{30}")


def _host_free(text: str, registry: str) -> str:
    """The manifest text with the run's registry address and digests masked."""
    text = text.replace(registry, "@REGISTRY@").replace(
        registry_dir(registry), "@REGISTRY@"
    )
    return _STORE_SPLIT.sub("/@XX@/@DIGEST30@", _HEX64.sub("@DIGEST64@", text))


def test_windows_amd64_manifest_equals_golden(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    golden = GOLDEN_DIR / "windows_amd64.manifest.json"
    if not golden.exists():
        pytest.skip(
            f"golden {golden} is absent (written by `ocx vendor -p windows/amd64`)"
        )
    pkg = windows_only_package(ocx, unique_repo, tmp_path / "pkg")
    project = _project(ocx, tmp_path / "proj", {"tool": pkg})
    out = tmp_path / "out"

    _vendor(ocx, project, out, "-p", "windows/amd64")

    actual = _host_free((out / "manifest.json").read_text(encoding="utf-8"), registry)
    expected = _host_free(golden.read_text(encoding="utf-8"), registry)
    assert json.loads(actual) == json.loads(expected)
    assert json.loads(actual)["platform"].startswith("windows/amd64")
