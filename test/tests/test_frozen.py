"""Acceptance tests for ``--frozen`` (issue #155): freeze tag resolution to the
local index.

``--frozen`` lets a tag already in the local index (and any digest-pinned
reference) resolve, but refuses to fetch + commit an unknown (un-indexed) tag —
that errors with exit 81 (``PolicyBlocked``) so CI can ``case $?`` on it.
Distinct from ``--offline``, which forbids all network: frozen still pulls
pinned digests over the network.

Modeled on ``test_offline.py`` / ``test_pinned_offline.py``.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from src import OcxRunner
from src.helpers import make_package
from src.registry import fetch_manifest_digest


def _write_ocx_toml(project: Path, body: str) -> Path:
    path = project / "ocx.toml"
    path.write_text(body)
    return path


def _run_in_project(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ocx with the given args from ``cwd``, no exit check."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


# Exit codes (stable; see crates/ocx_lib/src/cli/exit_code.rs).
POLICY_BLOCKED = 81
USAGE_ERROR = 64


def _run(
    ocx: OcxRunner, *args: str, extra_env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Run ocx with the instance env (plus optional overrides), no exit check."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args], capture_output=True, text=True, env=env
    )


def _index_root(ocx: OcxRunner) -> Path:
    """The local index collection home: ``$OCX_HOME/index``.

    Holds every source's copy of the index wire grammar — per-package root
    documents (``<source>/p/<pkg>.json``) plus the dispatch-object CAS
    (``o/sha256/<hex>``) — under one root. Wiping it simulates a fresh machine
    with nothing locally indexed, so a tag can no longer resolve from the local
    index.
    """
    return Path(ocx.env["OCX_HOME"]) / "index"


def test_frozen_known_tag_resolves_from_local_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A fully-cached tag resolves under ``--frozen`` without walking the source.

    An online install caches the full manifest chain (tag pointer + manifest
    blobs) into the local store — the state a frozen resolve needs. The frozen
    re-resolve then hits that cache and never walks the source chain.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    # Online install caches the full chain (tag pointer + blobs) locally.
    ocx.json("package", "install", pkg.short)

    result = ocx.run("--frozen", "package", "install", "--select", pkg.short, check=False)
    assert result.returncode == 0, (
        f"--frozen install of a fully-cached tag must succeed; rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_frozen_unknown_tag_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An unpinned tag missing from the local index errors with exit 81.

    The tag exists on the registry, but ``--frozen`` refuses to walk the source
    chain to fetch + commit an un-indexed reference — the deliberate policy.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    # Drop the local index so the tag is no longer locally known.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    result = _run(ocx, "--frozen", "package", "install", pkg.short)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen install of an un-indexed tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_digest_pinned_succeeds(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A digest-pinned reference still fetches content under ``--frozen``.

    Wiping the local index first proves frozen fetched the pinned content from
    the registry rather than relying on a cached tag pointer — the digest axis
    is what distinguishes frozen from offline.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    pinned = f"{pkg.fq}@{digest}"

    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    result = _run(ocx, "--frozen", "package", "install", pinned)
    assert result.returncode == 0, (
        f"--frozen install of a digest-pinned ref must succeed; "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_with_remote_flag_exits_64(ocx: OcxRunner) -> None:
    """``--frozen --remote`` is a contradiction → clap usage error (exit 64)."""
    result = _run(ocx, "--frozen", "--remote", "package", "install", "whatever:1")
    assert result.returncode == USAGE_ERROR, (
        f"--frozen --remote must be a usage error (exit 64); rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_frozen_bare_repo_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A bare repo reference (no tag) under ``--frozen`` normalises to ``latest``
    and exits 81 when that tag is not in the local index.

    Bare identifiers normalise to ``:latest`` inside ``Identifier::tag_or_latest()``.
    The resulting unpinned tag is absent from the local index (the package was
    pushed but never installed, so no tag pointer was committed).  The frozen
    policy gate fires on the tag-only path (``identifier.digest().is_none()``),
    not on ``latest`` specifically — so this test also exercises the bare-
    identifier normalisation branch of ``ChainedIndex::walk_chain``.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    # Wipe the local index so ``latest`` is not indexed.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    # Bare identifier: no tag component — normalises to ``:latest`` internally.
    bare = f"{ocx.registry}/{unique_repo}"
    result = _run(ocx, "--frozen", "package", "install", bare)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen install of a bare (unindexed) repo must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_lock_blocks_unindexed_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--frozen lock`` exits 81 when an ``ocx.toml`` tool references a tag
    that is not in the local index.

    Exercises the project-tier resolve path (``project/resolve.rs`` →
    ``retry_fetch`` → ``policy_block_label`` → ``ProjectErrorKind::PolicyBlocked``)
    which has unit coverage but was previously untested end-to-end via
    acceptance tests.

    The package exists on the registry but was never installed (no tag pointer
    committed to the local index), so ``--frozen lock`` must refuse to walk the
    source chain and exit 81.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    # Ensure the local index is empty so the tag pointer is absent.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\n{unique_repo} = "{pkg.fq}"\n',
    )

    result = _run_in_project(ocx, project, "--frozen", "lock")
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen lock with an unindexed tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_allows_direct_registry_query(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--frozen`` does not block commands that query the registry directly.

    ``ocx package info`` calls ``Publisher::pull_description`` via
    ``context.remote_client()`` — the raw OCI client — bypassing the Index
    facade entirely.  The frozen policy only gates tag resolution through
    ``ChainedIndex``; it has no effect on code paths that never call
    ``default_index()``.

    Setup: push a package so the repo exists in the registry, then wipe the
    local index so the tag is absent.  ``--frozen package info`` must still
    exit 0 because it does not perform tag resolution through the index.

    Routing evidence:
      ``crates/ocx_cli/src/command/package_info.rs``:35 —
        ``Publisher::new(context.remote_client()?.clone())``
      ``crates/ocx_cli/src/app/context.rs``:249 —
        ``remote_client()`` only errors on ``OfflineMode``, not ``FrozenMode``
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    # Wipe the local index so the tag is absent — frozen would block install.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    # ``package info`` queries the ``__ocx.desc`` tag via remote_client(),
    # not the Index.  No description has been pushed, so it returns empty
    # results — but the command exits 0, not 81.
    result = _run(ocx, "--frozen", "package", "info", pkg.short)
    assert result.returncode == 0, (
        f"--frozen package info must succeed (direct registry query bypasses index "
        f"resolution); rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_add_blocks_unindexed_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--frozen add <repo:tag>`` exits 81 when the tag is not in the local index.

    ``ocx add`` resolves the tag through ``context.default_index()``, which
    carries ``ChainMode::Frozen`` when ``--frozen`` is set.  The frozen chain
    refuses to walk the source chain for an unpinned tag absent from the local
    index, routing through ``project/resolve.rs`` →
    ``policy_block_label`` → ``ProjectErrorKind::PolicyBlocked`` → exit 81.

    This mirrors ``test_frozen_lock_blocks_unindexed_tag`` for the ``add``
    command, confirming that the policy gate fires on every project-tier
    command that calls ``resolve_lock`` rather than only on ``lock``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    # Ensure the local index is empty so the tag pointer is absent.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    project = tmp_path / "proj_add"
    project.mkdir()
    _write_ocx_toml(project, "")  # minimal valid ocx.toml (no tools yet)

    result = _run_in_project(ocx, project, "--frozen", "add", pkg.fq)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen add with an unindexed tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_remote_env_conflict_exits_64(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``OCX_FROZEN=1`` + ``OCX_REMOTE=1`` (env, no flags) → runtime check, exit 64.

    clap's ``conflicts_with`` cannot see env-sourced defaults; the runtime
    ``check_frozen_remote_exclusivity`` guard closes that gap.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = _run(
        ocx,
        "package",
        "install",
        pkg.short,
        extra_env={"OCX_FROZEN": "1", "OCX_REMOTE": "1"},
    )
    assert result.returncode == USAGE_ERROR, (
        f"OCX_FROZEN + OCX_REMOTE must hit the runtime exclusivity check (exit 64); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )
