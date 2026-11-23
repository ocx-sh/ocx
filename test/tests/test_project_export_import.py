# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the project-level ``ocx export`` / ``ocx import`` verbs.

Root ``export`` writes every package the project lock pins to a directory; root ``import`` installs
them on a machine that cannot reach the registry and roots the project there, so the tools survive
``ocx clean``. ``--global`` renders ``$OCX_HOME``'s toolchain and registers no project.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src import registry_guard as registry_guard_mod
from src.export_dir import MARKER, blob_data_files, read_marker
from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("export", "import")

EXIT_SUCCESS = 0
EXIT_USAGE = 64
EXIT_CONFIG = 78


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        check=False,
    )


def _ledger_entries(ocx_home: Path) -> list[Path]:
    projects = ocx_home / "projects"
    if not projects.is_dir():
        return []
    return [p for p in projects.iterdir() if not p.name.startswith(".tmp-")]


def _locked_project(ocx: OcxRunner, tmp_path: Path, label: str) -> tuple[Path, str]:
    """Publish a package ``hello`` and write a project dir with ``ocx.toml`` and a committed ``ocx.lock``."""
    repo = f"t_{uuid4().hex[:8]}_pe_{label}"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False, bins=["hello"])
    project = tmp_path / "project"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n'
    )
    locked = _run(ocx, project, "lock")
    assert locked.returncode == EXIT_SUCCESS, (
        f"setup: ocx lock failed:\n{locked.stderr}"
    )
    return project, repo


def _clone(project: Path, tmp_path: Path, *extra: str) -> Path:
    """A project dir holding only the committed files of ``project`` (plus ``extra`` names)."""
    clone = tmp_path / "clone"
    clone.mkdir()
    for name in ("ocx.toml", "ocx.lock", *extra):
        shutil.copy(project / name, clone / name)
    return clone


def _export(
    ocx: OcxRunner, project: Path, out: Path, *args: str
) -> subprocess.CompletedProcess[str]:
    exported = _run(ocx, project, "export", "-o", str(out), *args)
    assert exported.returncode == EXIT_SUCCESS, (
        f"setup: ocx export failed:\n{exported.stderr}"
    )
    return exported


def test_import_then_project_exec_survives_clean(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    tmp_path: Path,
) -> None:
    """Import on an offline home, delete the export, run, ``ocx clean``, run again: the tools stay."""
    project, _ = _locked_project(ocx, tmp_path, "survive")
    export_dir = tmp_path / "export"
    _export(ocx, project, export_dir)
    clone = _clone(project, tmp_path)

    registry_guard.deny(second_home)
    imported = _run(second_home, clone, "import", str(export_dir))
    assert imported.returncode == EXIT_SUCCESS, (
        f"ocx import must succeed offline:\n{imported.stderr}"
    )
    shutil.rmtree(export_dir)

    first = _run(second_home, clone, "exec", "--", "hello")
    assert first.returncode == EXIT_SUCCESS, (
        f"exec after import must run the tool:\n{first.stderr}"
    )
    cleaned = _run(second_home, clone, "clean")
    assert cleaned.returncode == EXIT_SUCCESS, f"ocx clean failed:\n{cleaned.stderr}"
    second = _run(second_home, clone, "exec", "--", "hello")
    assert second.returncode == EXIT_SUCCESS, (
        f"exec after clean must still run offline; rc={second.returncode}\n{second.stderr}"
    )
    assert registry_guard.count == 0, (
        f"an imported project must need no registry; saw {registry_guard.requests}"
    )


def _write_patch_config(ocx: OcxRunner, registry: str) -> Path:
    config = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config.write_text(
        f'[patches]\nregistry = "{registry}/p{uuid4().hex[:12]}"\nrequired = false\n'
    )
    return config


def _env_items(ocx: OcxRunner, cwd: Path) -> list[dict]:
    result = _run(ocx, cwd, "--format", "json", "env")
    assert result.returncode == EXIT_SUCCESS, f"ocx env failed:\n{result.stderr}"
    # Home- and project-relative: two machines differ only in where `$OCX_HOME` and the project are.
    # Replaced in the decoded values: raw JSON text escapes each Windows backslash, so a path never matches there.
    items = []
    for item in json.loads(result.stdout)["items"]:
        if item["key"] == "__OCX_ENV_STATE":
            # A compressed record of those absolute paths, not part of the composed env.
            continue
        value = item["value"].replace(str(ocx.ocx_home), "<HOME>").replace(str(cwd), "<PROJECT>")
        items.append({**item, "value": value})
    return items


def test_root_import_patch_snapshot_round_trip(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    tmp_path: Path,
    registry: str,
) -> None:
    """A project export carries the patch snapshot's companions: the offline composed env equals the connected one."""
    companion_repo = f"t_{uuid4().hex[:8]}_pe_companion"
    companion = make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path / "c",
        bins=[],
        env=[
            {
                "key": "ROUND_TRIP_CA",
                "type": "constant",
                "value": "/certs/round-trip.pem",
                "visibility": "public",
            }
        ],
        cascade=True,
        platform="any",
    )
    (tmp_path / "c").mkdir(exist_ok=True)
    project, repo = _locked_project(ocx, tmp_path, "patch")
    config = _write_patch_config(ocx, registry)
    descriptor = tmp_path / "descriptor.json"
    descriptor.write_text(
        json.dumps(
            {"version": 1, "rules": [{"match": "*", "packages": [companion.fq]}]}
        )
    )
    base_fq = f"{registry}/{repo}:1.0.0"
    published = _run(
        ocx, project, "patch", "publish", "--descriptor", str(descriptor), base_fq
    )
    assert published.returncode == EXIT_SUCCESS, (
        f"setup: patch publish failed:\n{published.stderr}"
    )
    pulled = _run(ocx, project, "pull")
    assert pulled.returncode == EXIT_SUCCESS, (
        f"setup: ocx pull failed:\n{pulled.stderr}"
    )
    frozen = _run(ocx, project, "patch", "freeze")
    assert frozen.returncode == EXIT_SUCCESS, (
        f"setup: patch freeze failed:\n{frozen.stderr}"
    )
    assert (project / "patches.snapshot.json").is_file(), (
        "setup: the freeze must write the snapshot beside the lock"
    )

    # The snapshot is active only when `OCX_PATCH_SNAPSHOT` selects it.
    ocx.env["OCX_PATCH_SNAPSHOT"] = str(project / "patches.snapshot.json")
    connected = _env_items(ocx, project)
    assert any(e["key"] == "ROUND_TRIP_CA" for e in connected), (
        f"setup: the connected env lacks the companion: {connected}"
    )

    export_dir = tmp_path / "export"
    _export(ocx, project, export_dir)
    clone = _clone(project, tmp_path, "patches.snapshot.json")
    shutil.copy(config, Path(second_home.env["OCX_HOME"]) / "config.toml")

    second_home.env["OCX_PATCH_SNAPSHOT"] = str(clone / "patches.snapshot.json")
    registry_guard.deny(second_home)
    imported = _run(second_home, clone, "import", str(export_dir))
    assert imported.returncode == EXIT_SUCCESS, (
        f"ocx import must succeed offline:\n{imported.stderr}"
    )
    shutil.rmtree(export_dir)

    assert _env_items(second_home, clone) == connected
    assert registry_guard.count == 0, (
        f"the offline env must need no registry; saw {registry_guard.requests}"
    )


def test_global_export_import_registers_nothing(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    tmp_path: Path,
) -> None:
    """``--global`` export then import: the tool runs offline and the ``projects/`` ledger gains no entry."""
    repo = f"t_{uuid4().hex[:8]}_pe_global"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False, bins=["hello"])
    added = _run(
        ocx, tmp_path, "--global", "add", "--no-pull", f"{ocx.registry}/{repo}:1.0.0"
    )
    assert added.returncode == EXIT_SUCCESS, (
        f"setup: --global add failed:\n{added.stderr}"
    )
    export_dir = tmp_path / "export"
    exported = _run(ocx, tmp_path, "--global", "export", "-o", str(export_dir))
    assert exported.returncode == EXIT_SUCCESS, (
        f"--global export failed:\n{exported.stderr}"
    )

    target_home = Path(second_home.env["OCX_HOME"])
    for name in ("ocx.toml", "ocx.lock"):
        shutil.copy(ocx.ocx_home / name, target_home / name)

    registry_guard.deny(second_home)
    imported = _run(second_home, tmp_path, "--global", "import", str(export_dir))
    assert imported.returncode == EXIT_SUCCESS, (
        f"--global import must succeed offline:\n{imported.stderr}"
    )
    shutil.rmtree(export_dir)
    ran = _run(second_home, tmp_path, "--global", "exec", "--", "hello")
    assert ran.returncode == EXIT_SUCCESS, (
        f"the imported global tool must run offline:\n{ran.stderr}"
    )

    assert _ledger_entries(target_home) == [], (
        "--global import must register no project"
    )
    assert registry_guard.count == 0, f"saw {registry_guard.requests}"


def test_root_export_grammar_errors(ocx: OcxRunner, tmp_path: Path) -> None:
    """No project: usage error (64). ``ocx.toml`` without ``ocx.lock``: config error (78)."""
    nowhere = tmp_path / "nowhere"
    nowhere.mkdir()
    outside = _run(ocx, nowhere, "export", "-o", str(tmp_path / "out-a"))
    assert outside.returncode == EXIT_USAGE, (
        f"export outside a project must be 64; got {outside.returncode}\n{outside.stderr}"
    )

    unlocked = tmp_path / "unlocked"
    unlocked.mkdir()
    (unlocked / "ocx.toml").write_text(
        f'[tools]\nhello = "{ocx.registry}/hello:1.0.0"\n'
    )
    missing = _run(ocx, unlocked, "export", "-o", str(tmp_path / "out-b"))
    assert missing.returncode == EXIT_CONFIG, (
        f"export without ocx.lock must be 78; got {missing.returncode}\n{missing.stderr}"
    )


@pytest.mark.smoke
def test_root_export_smoke(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx export -o DIR`` in a locked project writes a marked export directory holding blobs."""
    project, _ = _locked_project(ocx, tmp_path, "smoke_export")
    out = tmp_path / "export"
    _export(ocx, project, out)
    assert (out / MARKER).is_file()
    assert read_marker(out)
    assert blob_data_files(out), "the export must hold the project's blobs"


@pytest.mark.smoke
def test_root_import_smoke(
    ocx: OcxRunner, second_home: OcxRunner, tmp_path: Path
) -> None:
    """``ocx import DIR`` installs the locked tools; ``ocx exec`` runs one."""
    project, _ = _locked_project(ocx, tmp_path, "smoke_import")
    out = tmp_path / "export"
    _export(ocx, project, out)
    clone = _clone(project, tmp_path)
    imported = _run(second_home, clone, "import", str(out))
    assert imported.returncode == EXIT_SUCCESS, f"ocx import failed:\n{imported.stderr}"
    ran = _run(second_home, clone, "exec", "--", "hello")
    assert ran.returncode == EXIT_SUCCESS, f"the imported tool must run:\n{ran.stderr}"
