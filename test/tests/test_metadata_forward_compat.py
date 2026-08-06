# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the unknown-env-modifier-type forward-compat remedy.

An env var whose modifier `type` this ocx does not know must fail closed with
an actionable "upgrade ocx" message naming the offending key/type, on every
surface that reads a modifier type — package metadata (install/exec), the
project `[env]` table (`ocx.toml`), the shared `--env` CLI grammar — with one
deliberate exception: `ocx package deps`, which treats it as "this package
needs a newer ocx" and skips the offending package instead of aborting.

`frobnicate` is the unknown-type token used throughout. It is deliberately
NOT `list`: this branch's own binary knows `list` (the W release lands here
too), so only a token that stays unknown forever proves the forward-compat
reader path rather than a feature this same binary will grow.

See ``plan_env_list_type.md`` contracts R-4 (metadata → 65), R-6 (`FromStr`
remedy → 78 toml / 64 flag), and the `deps.rs` lenient-site UX row.
"""

from __future__ import annotations

import io
import json
import subprocess
import tarfile
from pathlib import Path

from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest, push_raw_package
from src.runner import OcxRunner, current_platform

EXIT_USAGE = 64  # UsageError (sysexits EX_USAGE)
EXIT_DATA_ERR = 65  # DataError (sysexits EX_DATAERR)
EXIT_CONFIG_ERROR = 78  # ConfigError (sysexits EX_CONFIG)
EXIT_SUCCESS = 0

UNKNOWN_TYPE = "frobnicate"

# The metadata-surface remedy (`package/error.rs::Error::UnknownEnvModifier`),
# naming the declaring key and the unrecognized type.
REMEDY_METADATA = f"declares unknown type '{UNKNOWN_TYPE}'; upgrade ocx to use this package"

# The shared `FromStr` remedy (`modifier.rs::ParseModifierKindError`), reused
# verbatim by both the `ocx.toml` (78) and `--env` (64) surfaces.
REMEDY_PARSE = f"unknown modifier type '{UNKNOWN_TYPE}'; expected `path` or `constant` (a newer ocx may support it)"

# The lenient `ocx package deps` read-site warning (`command/deps.rs`).
DEPS_SKIP_WARNING = "skipping package: it requires a newer ocx than this one"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _push_unknown_type_package(ocx: OcxRunner, repo: str, tag: str, key: str) -> None:
    """Publishes an ocx PACKAGE directly via the registry HTTP API whose
    metadata declares `key` with an env modifier `type` this ocx never
    resolves (`frobnicate`).

    `ocx package create`/`push` both run `ValidMetadata::try_from` themselves
    (`package_create.rs`, `package_push.rs`) and would refuse this metadata
    with the very error under test before it ever reached the registry — so
    the fixture bypasses them and writes the wire shape directly, the same
    house pattern `test_dependencies.py::test_install_with_missing_dependency`
    and `test_package_push_gate.py` use for metadata a live create/push gate
    would otherwise reject (`src.registry.push_raw_package`). This is what
    simulates a package published against a newer ocx that DOES know
    `frobnicate` — the read path is exactly what R exists to harden.
    """
    layer_buffer = io.BytesIO()
    with tarfile.open(fileobj=layer_buffer, mode="w:xz") as tar:
        body = b"#!/bin/sh\necho app\n"
        info = tarfile.TarInfo(name="bin/app")
        info.size = len(body)
        info.mode = 0o755
        tar.addfile(info, io.BytesIO(body))

    metadata = {
        "type": "bundle",
        "version": 1,
        "env": [{"key": key, "type": UNKNOWN_TYPE, "value": "x"}],
    }
    os_name, arch = current_platform().split("/")
    push_raw_package(
        ocx.registry, repo, tag, metadata, layer_buffer.getvalue(), platform=(os_name, arch)
    )
    ocx.plain("index", "update", f"{repo}:{tag}")


def _run_in(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Runs ocx with the process `cwd` set to a project directory.

    `OcxRunner.run()` has no `cwd` parameter — every OCI-tier test passes
    identifiers, never a project directory. Project-tier tests reach for the
    raw subprocess call directly instead, the same as `test_status.py::_run`.
    """
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True, env=ocx.env, check=False
    )


# ---------------------------------------------------------------------------
# Scenario 1 — metadata surface: install / exec of a published unknown-type
# package fails closed with the "upgrade ocx" remedy (R-4).
# ---------------------------------------------------------------------------


def test_install_unknown_env_modifier_type_exits_data_error(ocx: OcxRunner, unique_repo: str):
    """A fresh `ocx package install` of a package whose metadata declares an
    env var with an unknown modifier type fails closed at ingestion
    (`load_config_metadata` -> `ValidMetadata::try_from`, before anything is
    written to local disk): exit 65, naming the key, the type, and the
    remedy — never a raw serde blob.
    """
    _push_unknown_type_package(ocx, unique_repo, "1.0.0", key="SOMEVAR")

    result = ocx.run("package", "install", f"{unique_repo}:1.0.0", check=False, format=None)

    assert result.returncode == EXIT_DATA_ERR, (
        f"expected exit {EXIT_DATA_ERR} (DataError); got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert f"env var 'SOMEVAR' {REMEDY_METADATA}" in result.stderr, (
        f"stderr must contain the exact upgrade remedy naming the key; stderr={result.stderr!r}"
    )


def test_exec_unknown_env_modifier_type_exits_data_error(ocx: OcxRunner, unique_repo: str):
    """`ocx package exec` on a never-locally-installed package with an
    unknown env modifier type fails closed the same way `install` does:
    `find_or_install_all` falls through to install on a miss, hitting the
    identical fresh-pull validation — same exit code, same remedy text.
    """
    _push_unknown_type_package(ocx, unique_repo, "1.0.0", key="SOMEVAR")

    result = ocx.run(
        "package", "exec", f"{unique_repo}:1.0.0", "--", "echo", "hi",
        check=False, format=None,
    )

    assert result.returncode == EXIT_DATA_ERR, (
        f"expected exit {EXIT_DATA_ERR} (DataError); got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert f"env var 'SOMEVAR' {REMEDY_METADATA}" in result.stderr, (
        f"stderr must contain the exact upgrade remedy naming the key; stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 2 — `ocx.toml` `[env]` surface: an unknown modifier type is a
# config-shape fault (R-6 / ProjectErrorKind::EnvUnknownModifier).
# ---------------------------------------------------------------------------


def test_project_toml_unknown_env_modifier_type_exits_config_error(
    ocx: OcxRunner, tmp_path: Path
):
    """A project `ocx.toml` whose `[env]` table declares an unknown modifier
    `type` fails closed at load time — before any tool resolution — with the
    same shared remedy the `--env` flag carries: exit 78, naming the
    unrecognized type and pointing at the version gap, not a typo.
    """
    project = tmp_path / "project"
    project.mkdir()
    (project / "ocx.toml").write_text(
        '[env]\nSOMEVAR = { type = "frobnicate", value = "x" }\n', encoding="utf-8"
    )

    result = _run_in(ocx, project, "status")

    assert result.returncode == EXIT_CONFIG_ERROR, (
        f"expected exit {EXIT_CONFIG_ERROR} (ConfigError); got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert REMEDY_PARSE in result.stderr, (
        f"stderr must contain the shared unknown-modifier remedy; stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 3 — `--env KEY:TYPE=VALUE` surface: same shared grammar, usage
# error rather than a config-shape fault (R-6).
# ---------------------------------------------------------------------------


def test_env_flag_unknown_modifier_type_exits_usage_error(ocx: OcxRunner, unique_repo: str):
    """`--env K:frobnicate=v` fails the same shared `ModifierKind::FromStr`
    grammar `ocx.toml` uses, but as a CLI-misuse fault: exit 64, carrying the
    identical remedy text. The flag is parsed before package resolution, so
    no published package is needed to exercise it.
    """
    result = ocx.run(
        "package", "env", f"{unique_repo}:1.0.0", "--env", "K:frobnicate=v",
        check=False, format=None,
    )

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} (UsageError); got {result.returncode}\nstderr: {result.stderr}"
    )
    assert REMEDY_PARSE in result.stderr, (
        f"stderr must contain the shared unknown-modifier remedy; stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 4 — `ocx package deps` lenient read site: an installed
# dependency with an unknown modifier type is skipped, not fatal.
# ---------------------------------------------------------------------------


def test_deps_skips_installed_dependency_with_unknown_env_modifier_type(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx package deps` walks each root's declared dependencies through the
    lenient `load_install_info` read site (`command/deps.rs`), which treats
    an unknown modifier type as "this package requires a newer ocx" rather
    than a corrupted install: the dependency is skipped with a warning, the
    command still exits 0, and the root's tree comes back with that
    dependency absent from its children instead of the whole command
    aborting.

    The fixture installs a normal root+dependency pair through the product
    path (`ocx package create`/`push` would reject `frobnicate` at publish
    time, so the on-disk corruption has to happen after a valid install),
    then hand-corrupts the already-installed dependency's `metadata.json` —
    the same technique `test_deps_interpolation.py::
    test_launcher_exec_validates_metadata_via_validmetadata` uses to reach
    consumption-time `ValidMetadata` validation.
    """
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = make_package(ocx, leaf_repo, "1.0.0", tmp_path)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, leaf.repo, leaf.tag)
    app = make_package(
        ocx, app_repo, "1.0.0", tmp_path,
        dependencies=[{"identifier": f"{leaf.fq}@{leaf_digest}"}],
    )

    ocx.json("package", "install", "--select", app.short)

    which_result = ocx.json("package", "which", leaf.short)
    leaf_root = Path(which_result[leaf.short])
    leaf_metadata_path = leaf_root / "metadata.json"
    assert leaf_metadata_path.exists(), f"leaf metadata.json must exist at {leaf_metadata_path}"

    leaf_metadata_path.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [{"key": "LEAFVAR", "type": UNKNOWN_TYPE, "value": "x"}],
            }
        )
    )

    result = ocx.run("package", "deps", app.short, check=False, format=None)

    assert result.returncode == EXIT_SUCCESS, (
        f"expected exit {EXIT_SUCCESS}; a package requiring a newer ocx must not abort "
        f"the whole `deps` command: got {result.returncode}\nstderr: {result.stderr}"
    )
    assert DEPS_SKIP_WARNING in result.stderr, (
        f"stderr must warn that the dependency needs a newer ocx, not report it as "
        f"corrupted; stderr={result.stderr!r}"
    )

    tree = ocx.json("package", "deps", app.short)
    assert len(tree["roots"]) == 1
    assert tree["roots"][0]["dependencies"] == [], (
        "the unknown-modifier dependency must be skipped from the tree, not fatal to it: "
        f"{tree['roots'][0]}"
    )
