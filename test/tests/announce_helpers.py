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
    repo's `main` — the "package already claimed, nothing curated yet"
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


def forge_args(forge: str | None) -> list[str]:
    """`--forge <kind>` when a forge is named, nothing otherwise.

    The default (no flag) is GitHub, so a GitHub scenario passes `None` and the
    identical scenario passes `"gitlab"` — the one-flag difference that lets both
    clients be held to one oracle."""
    return [] if forge is None else ["--forge", forge]


def announce(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    *args: str,
    token: str | None = TOKEN,
    check: bool = True,
    extra_env: dict[str, str] | None = None,
    forge: str | None = None,
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
    return ocx.run(
        "package", "announce", *forge_args(forge), *args, format="json", check=check, env_overrides=env_overrides
    )


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


def index_root_bytes(package: str, physical_repository: str, tags: dict[str, Any] | None = None) -> bytes:
    """The committed index root `seed_empty_root` writes, as bytes.

    Split out so the git-transport seeding route below writes byte-identical
    content: `git_seed_files` commits real bytes into a real bare repository,
    while `seed_root` inserts a value into the in-memory graph, and two
    renderings of "the same" root would make a byte-exact comparison across the
    two transports meaningless.
    """
    root = {"name": f"ocx.sh/{package}", "repository": physical_repository, "tags": tags or {}}
    return (json.dumps(root, indent=2) + "\n").encode()


def git_index_project(
    fake_forge: FakeForge,
    package: str,
    physical_repository: str,
    *,
    tags: dict[str, Any] | None = None,
) -> str:
    """Register the index repository as a **git-transport** project and seed its
    `main` with the claimed root at `p/<package>.json`. Returns the head's sha.

    The git twin of `seed_empty_root`, and a separate function rather than a flag
    on it: `seed_root` reaches `_reject_git_project_locked`, which raises
    `GitFixtureError` for any project registered as a git route, so the REST
    helper cannot be widened without breaking the REST callers that depend on it.
    `git_seed_files` is the only seeding route on a git project.
    """
    fake_forge.git_create_project(INDEX_FULL)
    return fake_forge.git_seed_files(
        INDEX_FULL, "main", {f"p/{package}.json": index_root_bytes(package, physical_repository, tags)}
    )


def git_transport_env(shim: Any, home: Any, **extra: str) -> dict[str, str]:
    """The per-invocation environment a `--transport git` run needs: the shim's
    `PATH` and the fixture `HOME`, plus whichever credential or CI variables the
    caller arms.

    One builder rather than a literal per row, because `GITLAB_CI` and
    `CI_JOB_TOKEN` are conjuncts of C-063's job-token rung: a row that spells the
    dict by hand and forgets `GITLAB_CI` silently measures push-ladder rung 2
    while reading as a job-token row, and `job_token_push_applies()` then reports
    every capability check as `skipped`.

    `shim` is a `git_shim.GitShim` and `home` a `git_http_fixture.FixtureHome`;
    they are untyped here so this module keeps importing neither — it is shared
    with four REST-only announce modules.
    """
    env = {"PATH": shim.child_path(), "HOME": str(home.path)}
    env.update(extra)
    return env
