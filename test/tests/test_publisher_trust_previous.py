# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The previous ocx release keeps installing packages whose index root carries `publishers`.

A root's `publishers` list is an additive field: a release that predates it
ignores it, so an unrecognised entry (or any entry) must never stop it from
installing. Such an install is unverified by design; verification is a property
of the new client.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from src import previous_release, static_index
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, current_platform


def test_s013_previous_release_installs_a_package_whose_root_carries_publishers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path,
) -> None:
    """a usable entry beside an unrecognised one; the old client installs, unverified."""
    old = previous_release.resolve()
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")
    root = tmp_path / "static_index_root"
    root.mkdir()
    static_index.write_config(root)
    publishers = [
        {"kind": "keyless", "identity": "release@example.com", "oidc_issuer": "https://issuer.example.com"},
        {"kind": "future", "anything": 1},
    ]
    with static_index.running(root) as server:
        entry = static_index.write_package(
            root, repository=f"{unique_repo}/pkg", tag="1.0.0",
            physical_repository=f"oci://{ocx.registry}/{pkg.repo}", platform_digest=leaf,
            os=os_name, architecture=arch_name, publishers=publishers,
        )
        home = tmp_path / "old_home"
        home.mkdir()
        host = ocx.registry.split(":", 1)[0]
        (home / "config.toml").write_text(
            f'[registries."ocx.sh"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{host}"]\n'
        )
        env = {**ocx.env, "OCX_HOME": str(home), "OCX_INSECURE_REGISTRIES": f"{ocx.registry},{server.host}"}
        result = subprocess.run(
            [str(old), "package", "install", "-p", current_platform(), entry.logical_id],
            capture_output=True, text=True, env=env, stdin=subprocess.DEVNULL, check=False, timeout=120,
        )
    assert result.returncode == 0, f"previous release refused a root with publishers: {result.stderr}"
    assert list(home.rglob("metadata.json")), "the previous release left no installed package"
