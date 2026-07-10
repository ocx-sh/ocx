# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx config setup` (config-only managed-config adoption).

The automation/CI counterpart to `ocx self setup --managed-config`: adopts (or
clears) the `[managed]` tier without bootstrapping the ocx binary, writing env
shims, or touching shell profiles. Both entry points share the single lib
implementation (`ocx_lib::setup::apply_managed_config`), so this suite pins the
config-setup-specific contract:

- ``ocx config setup --managed-config <ref>`` → fence + snapshot, exit 0
- ``OCX_MANAGED_CONFIG=<ref> ocx config setup`` → adopts via the env override
- bare ``ocx config setup`` with an existing seed → re-adopts / self-heals
- bare ``ocx config setup`` with nothing configured → exit 64 (UsageError)
- ``--managed-config ""`` → clears fence + snapshot dir
- dirty fence → exit 82; ``--force`` overwrites; ``--dry-run`` writes nothing
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

from src.registry import push_raw_config_package
from src.runner import OcxRunner

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # UsageError (sysexits EX_USAGE)
EXIT_DIRTY = 82  # DirtyRcBlock

# The managed-config fence closer (distinct from the shell-activation fence).
_MANAGED_FENCE_CLOSER = "# <<< ocx managed <<<"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner,
    *args: str,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx --format json <args>` with the runner's isolated env."""
    env = dict(ocx.env)
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [str(ocx.binary), "--format", "json", *args],
        capture_output=True,
        text=True,
        env=env,
    )


def _home(ocx: OcxRunner) -> Path:
    return Path(ocx.env["OCX_HOME"])


def _snapshot_path(ocx: OcxRunner) -> Path:
    return _home(ocx) / "state" / "managed-config" / "snapshot.json"


def _status(result: subprocess.CompletedProcess[str]) -> str:
    return json.loads(result.stdout)["managed_config"]["status"]


def _publish(registry: str, repo: str, marker: str) -> str:
    """Pushes a managed-config package and returns its fully-qualified ref."""
    push_raw_config_package(registry, repo, "v1", f'[registry]\ndefault = "{marker}"\n'.encode())
    return f"{registry}/{repo}:v1"


# ---------------------------------------------------------------------------
# Adopt paths
# ---------------------------------------------------------------------------


def test_adopt_writes_fence_and_snapshot(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """`ocx config setup --managed-config <ref>` fetches the snapshot and
    writes the `[managed]` seed fence — no binary bootstrap, no profiles."""
    ref = _publish(registry, unique_repo, "config-setup-adopt.example")

    result = _run(ocx, "config", "setup", "--managed-config", ref)
    assert result.returncode == EXIT_SUCCESS, f"adopt must succeed: {result.stderr}"
    assert _status(result) == "adopted"

    config_text = (_home(ocx) / "config.toml").read_text()
    assert "[managed]" in config_text
    assert f'source = "{ref}"' in config_text
    assert _snapshot_path(ocx).exists(), "a synced snapshot must exist after adopt"


def test_adopt_via_env_override(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """With the flag omitted, `OCX_MANAGED_CONFIG` drives the adoption."""
    ref = _publish(registry, unique_repo, "config-setup-env.example")

    result = _run(ocx, "config", "setup", env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert result.returncode == EXIT_SUCCESS, f"env-driven adopt must succeed: {result.stderr}"
    assert _status(result) == "adopted"
    assert _snapshot_path(ocx).exists()


def test_bare_rerun_self_heals_wiped_snapshot(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """A bare `ocx config setup` re-adopts the existing seed: a wiped snapshot
    is re-fetched (self-heal) while the fence stays untouched."""
    ref = _publish(registry, unique_repo, "config-setup-heal.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    fence_before = (_home(ocx) / "config.toml").read_text()

    shutil.rmtree(_snapshot_path(ocx).parent)

    healed = _run(ocx, "config", "setup")
    assert healed.returncode == EXIT_SUCCESS, f"bare re-run must self-heal: {healed.stderr}"
    assert _status(healed) == "adopted"
    assert _snapshot_path(ocx).exists(), "the snapshot must be re-persisted"
    assert (_home(ocx) / "config.toml").read_text() == fence_before, (
        "the fence itself is never rewritten by the self-heal"
    )


def test_same_ref_rerun_is_already_adopted(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """Same ref + intact snapshot → `already_adopted`, no re-fetch."""
    ref = _publish(registry, unique_repo, "config-setup-noop.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    first_fetched_at = json.loads(_snapshot_path(ocx).read_text())["fetched_at"]

    second = _run(ocx, "config", "setup", "--managed-config", ref)
    assert second.returncode == EXIT_SUCCESS
    assert _status(second) == "already_adopted"
    assert json.loads(_snapshot_path(ocx).read_text())["fetched_at"] == first_fetched_at, (
        "same-ref re-run must not re-fetch (fence Current)"
    )


# ---------------------------------------------------------------------------
# Nothing to set up → usage error
# ---------------------------------------------------------------------------


def test_bare_with_nothing_configured_exits_64(ocx: OcxRunner) -> None:
    """No flag, no env var, no seed: `config setup` has nothing to do — a
    usage error (unlike `self setup`, which treats this as a no-op phase)."""
    result = _run(ocx, "config", "setup")
    assert result.returncode == EXIT_USAGE, (
        f"bare config setup with nothing configured must exit 64; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "nothing to set up" in result.stderr


# ---------------------------------------------------------------------------
# Clear path
# ---------------------------------------------------------------------------


def test_clear_removes_fence_and_snapshot(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """`--managed-config ""` clears the fence and deletes the snapshot dir."""
    ref = _publish(registry, unique_repo, "config-setup-clear.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS

    cleared = _run(ocx, "config", "setup", "--managed-config", "")
    assert cleared.returncode == EXIT_SUCCESS, f"clear must succeed: {cleared.stderr}"
    assert _status(cleared) == "cleared"
    assert "[managed]" not in (_home(ocx) / "config.toml").read_text()
    assert not _snapshot_path(ocx).parent.exists(), "the snapshot directory must be deleted"


# ---------------------------------------------------------------------------
# Dirty fence contract (exit 82 / --force / --dry-run)
# ---------------------------------------------------------------------------


def _tamper_fence(ocx: OcxRunner) -> None:
    config_path = _home(ocx) / "config.toml"
    text = config_path.read_text()
    config_path.write_text(text.replace(_MANAGED_FENCE_CLOSER, f"# tampered\n{_MANAGED_FENCE_CLOSER}"))


def test_dirty_fence_exits_82_force_overwrites(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """User edits inside the fence → exit 82 (dirty, untouched); `--force`
    rewrites the fence."""
    ref = _publish(registry, unique_repo, "config-setup-dirty.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    _tamper_fence(ocx)

    dirty = _run(ocx, "config", "setup", "--managed-config", ref)
    assert dirty.returncode == EXIT_DIRTY, (
        f"a tampered fence must exit 82; got {dirty.returncode}\nstderr:\n{dirty.stderr}"
    )
    assert _status(dirty) == "dirty"
    assert "# tampered" in (_home(ocx) / "config.toml").read_text(), "dirty fence left untouched"

    forced = _run(ocx, "config", "setup", "--managed-config", ref, "--force")
    assert forced.returncode == EXIT_SUCCESS, f"--force must overwrite: {forced.stderr}"
    assert "# tampered" not in (_home(ocx) / "config.toml").read_text()


def test_dry_run_reports_would_adopt_and_writes_nothing(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """`--dry-run` reports `would_adopt` without touching disk."""
    ref = _publish(registry, unique_repo, "config-setup-dry.example")

    result = _run(ocx, "config", "setup", "--managed-config", ref, "--dry-run")
    assert result.returncode == EXIT_SUCCESS, f"dry-run must succeed: {result.stderr}"
    assert _status(result) == "would_adopt"
    assert not (_home(ocx) / "config.toml").exists() or "[managed]" not in (_home(ocx) / "config.toml").read_text()
    assert not _snapshot_path(ocx).exists(), "dry-run must not fetch or persist"
