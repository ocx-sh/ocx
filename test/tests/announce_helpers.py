# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Shared helpers for the `ocx package announce` acceptance suite
(`test_announce.py`, `test_announce_push_file.py`)."""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

from fake_forge import FakeForge
from src.runner import OcxRunner

# Recognizable, never-real test credential — token-leak assertions (X6) grep
# for this literal in stdout/stderr and every logged forge request path.
TOKEN = "ghp_test_forge_token_TOKEN_VALUE_1234567890"

INDEX_OWNER = "ocx-sh"
INDEX_REPO = "index"
INDEX_FULL = f"{INDEX_OWNER}/{INDEX_REPO}"

# Pins the "observed" timestamp for every announce run in this suite
# (`__OCX_TESTING_ANNOUNCE_CLOCK`, `announce/pipeline.rs::current_timestamp`)
# so byte-exactness assertions don't depend on wall-clock time.
FIXED_CLOCK = "2026-07-24T00:00:00Z"


def registry_host(registry: str) -> str:
    """The bare host of a `host[:port]` registry address."""
    return registry.split(":", 1)[0]


def configure_trusted_hosts(ocx: OcxRunner, registry: str, hosts: list[str]) -> None:
    """Writes `[registries."<registry>"] trusted_hosts = [...]` to `config.toml`
    (design register X2 — the sole SSRF escape hatch, config-only)."""
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    hosts_toml = ", ".join(f'"{host}"' for host in hosts)
    config_path.write_text(f'[registries."{registry}"]\ntrusted_hosts = [{hosts_toml}]\n')


def seed_empty_root(fake_forge: FakeForge, package: str, physical_repository: str) -> None:
    """Seeds an empty-tags committed root at `p/<package>.json` on the index
    repo's `main` — the "namespace already claimed, nothing curated yet"
    starting state every scenario announces against.

    `name` is carried because the index root schema requires it of every root:
    announce reads it (it is the fallback for a description with no title
    annotation), so a fixture omitting it exercises a root the index cannot
    hold."""
    fake_forge.seed_root(
        INDEX_OWNER,
        INDEX_REPO,
        f"p/{package}.json",
        {"name": f"ocx.sh/{package}", "repository": physical_repository, "tags": {}},
    )


def branch_name(package: str) -> str:
    """The deterministic per-package announce branch name (design register C9)."""
    return f"indexbot-announce-{package.replace('/', '-')}"


def announce(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    *args: str,
    token: str | None = TOKEN,
    check: bool = True,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx package announce`, pointed at `fake_forge` via the
    `__OCX_TESTING_FORGE_BASE_URL` test seam, with the `observed` timestamp
    pinned to `FIXED_CLOCK`. `token=None` omits `OCX_ANNOUNCE_TOKEN` entirely
    (for the missing-credential exercise)."""
    env_overrides = {
        "__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url,
        "__OCX_TESTING_ANNOUNCE_CLOCK": FIXED_CLOCK,
    }
    if token is not None:
        env_overrides["OCX_ANNOUNCE_TOKEN"] = token
    if extra_env:
        env_overrides.update(extra_env)
    return ocx.run("package", "announce", *args, format="json", check=check, env_overrides=env_overrides)


def announce_json(ocx: OcxRunner, fake_forge: FakeForge, *args: str, **kwargs: Any) -> dict:
    result = announce(ocx, fake_forge, *args, **kwargs)
    return json.loads(result.stdout)


def committed_root(fake_forge: FakeForge, package: str, *, owner: str = "forkuser", repo: str = "index") -> dict:
    """Reads + parses a fork branch's just-committed root (bypasses HTTP —
    `--out` mode never surfaces fork-committed state, and the real forge has
    no diff endpoint to poll)."""
    branch = branch_name(package)
    raw = fake_forge.read_file(owner, repo, f"p/{package}.json", branch=branch)
    assert raw is not None, f"no committed root found for {package} on {owner}/{repo}:{branch}"
    return json.loads(raw)
