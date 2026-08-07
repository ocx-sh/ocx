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
- an already-adopted seed reconciles on every run: newer content →
  ``refreshed``; unchanged content → ``already_adopted`` (now verified by a
  real fetch); a refresh that cannot reach the registry is best-effort
  (``refresh_unavailable``, exit 0, snapshot kept); ``--offline`` and a
  digest-pinned seed skip the refresh outright, without a warning
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
    drop_env: set[str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx --format json <args>` with the runner's isolated env."""
    env = dict(ocx.env)
    for key in drop_env or ():
        env.pop(key, None)
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


def _registry_probe(ocx: OcxRunner, **kwargs: object) -> str:
    """Runs `ocx package install <bogus>` with `OCX_DEFAULT_REGISTRY` dropped
    and returns combined stdout+stderr, so the resolved registry default (set
    by a merged `[registry] default = "<marker>"` managed payload) is
    observable in the failure message."""
    result = _run(
        ocx,
        "package",
        "install",
        "nonexistent_pkg_ocx_test:0",
        drop_env={"OCX_DEFAULT_REGISTRY"},
        **kwargs,  # type: ignore[arg-type]
    )
    return result.stdout + result.stderr


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


def test_rerun_refreshes_to_newer_payload(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """A re-run against the SAME ref reconciles the tier on every invocation:
    when the registry now serves newer content under the same tag, `config
    setup` refreshes in place — `status == "refreshed"`, `previous_digest`
    names the digest it replaced, and the merged config resolves the newly
    published marker. The fence itself is never rewritten by a refresh.

    Red check: restoring the pre-fix early return at `setup.rs:373-389`
    reports `already_adopted` with the ORIGINAL marker instead.
    """
    ref = _publish(registry, unique_repo, "config-setup-refresh-a.example")
    first = _run(ocx, "config", "setup", "--managed-config", ref)
    assert first.returncode == EXIT_SUCCESS, f"first adopt must succeed: {first.stderr}"
    first_digest = json.loads(_snapshot_path(ocx).read_text())["digest"]
    fence_before = (_home(ocx) / "config.toml").read_text()

    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "config-setup-refresh-b.example"\n')

    second = _run(ocx, "config", "setup", "--managed-config", ref)
    assert second.returncode == EXIT_SUCCESS, f"a refresh to newer content must succeed: {second.stderr}"
    assert _status(second) == "refreshed", second.stdout
    payload = json.loads(second.stdout)["managed_config"]
    assert payload["previous_digest"] == first_digest, payload
    assert payload["digest"] != first_digest, "a refresh to newer content must change the digest"

    assert json.loads(_snapshot_path(ocx).read_text())["digest"] == payload["digest"]
    assert (_home(ocx) / "config.toml").read_text() == fence_before, (
        "the fence itself must never be rewritten by a refresh"
    )

    combined = _registry_probe(ocx)
    assert "config-setup-refresh-b.example" in combined, (
        f"the refreshed snapshot must merge into subsequent commands: {combined!r}"
    )


def test_rerun_same_content_stays_already_adopted(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """Same ref + unchanged registry content → `already_adopted`, verified by
    a real re-fetch rather than assumed from the fence: `previous_digest` is
    absent (only `refreshed` carries it) and the snapshot's `fetched_at` does
    not move — guards against reporting `refreshed` on every run."""
    ref = _publish(registry, unique_repo, "config-setup-same-content.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    snapshot_before = json.loads(_snapshot_path(ocx).read_text())

    second = _run(ocx, "config", "setup", "--managed-config", ref)
    assert second.returncode == EXIT_SUCCESS
    assert _status(second) == "already_adopted"
    payload = json.loads(second.stdout)["managed_config"]
    assert payload["digest"] == snapshot_before["digest"]
    assert "previous_digest" not in payload, payload

    snapshot_after = json.loads(_snapshot_path(ocx).read_text())
    assert snapshot_after["fetched_at"] == snapshot_before["fetched_at"], (
        "unchanged content must not re-persist the snapshot"
    )


def test_refresh_failure_keeps_snapshot_and_exits_zero(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """Once a snapshot is adopted, a refresh that cannot be fetched is
    best-effort: the run still exits 0, the existing snapshot survives
    byte-for-byte, and the failure is surfaced as `refresh_unavailable` plus a
    stderr warning — never a silent `already_adopted`.

    The SAME tag is republished with a platform-only image index (no `any/any`
    entry): the ref text is unchanged (the fence classifies as Current and the
    snapshot's identity still matches), so only the fetch itself fails
    (`NoAnyPlatformEntry`) — a deterministic stand-in for the registry-down
    case that needs no live network fault. Dropping the best-effort arm
    (`Err(e) if adopted.is_some()`) turns this red.
    """
    ref = _publish(registry, unique_repo, "config-setup-outage.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    snapshot_before = _snapshot_path(ocx).read_text()

    push_raw_config_package(
        registry,
        unique_repo,
        "v1",
        b'[registry]\ndefault = "config-setup-outage-2.example"\n',
        platform=("linux", "amd64"),
    )

    result = _run(ocx, "config", "setup", "--managed-config", ref)
    assert result.returncode == EXIT_SUCCESS, (
        f"a failed refresh behind an existing snapshot must still exit 0: {result.stderr}"
    )
    assert _status(result) == "refresh_unavailable", result.stdout
    payload = json.loads(result.stdout)["managed_config"]
    # Both diagnostics must NAME the failure: the fetch error's own `Display`
    # is the bare "failed to fetch managed config", so the cause is reachable
    # only by flattening the source chain.
    assert "any/any" in payload["reason"], payload
    assert "could not refresh the managed-config snapshot" in result.stderr, result.stderr
    assert "any/any" in result.stderr, result.stderr

    assert _snapshot_path(ocx).read_text() == snapshot_before, (
        "a failed refresh must never overwrite the existing snapshot"
    )


def test_dry_run_reports_would_refresh_for_adopted_seed(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """`--dry-run` against an already-adopted seed reports `would_refresh`
    without fetching or writing anything, even when newer content exists."""
    ref = _publish(registry, unique_repo, "config-setup-dry-refresh.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    snapshot_before = _snapshot_path(ocx).read_text()

    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "config-setup-dry-refresh-2.example"\n')

    result = _run(ocx, "config", "setup", "--managed-config", ref, "--dry-run")
    assert result.returncode == EXIT_SUCCESS, f"dry-run refresh must succeed: {result.stderr}"
    assert _status(result) == "would_refresh"
    assert _snapshot_path(ocx).read_text() == snapshot_before, "dry-run must never fetch or persist"


def test_offline_rerun_reports_already_adopted_without_warn(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """A deliberate `--offline` re-run against an already-adopted seed skips
    the refresh WITHOUT a warning — a deliberate skip is not a failure, unlike
    a registry that could not be reached (`refresh_unavailable`)."""
    ref = _publish(registry, unique_repo, "config-setup-offline.example")
    assert _run(ocx, "config", "setup", "--managed-config", ref).returncode == EXIT_SUCCESS
    snapshot_before = _snapshot_path(ocx).read_text()

    result = _run(ocx, "--offline", "config", "setup", "--managed-config", ref)
    assert result.returncode == EXIT_SUCCESS, f"an offline re-run must still succeed: {result.stderr}"
    assert _status(result) == "already_adopted"
    assert _snapshot_path(ocx).read_text() == snapshot_before
    assert "could not refresh" not in result.stderr, (
        f"a deliberate --offline skip must not warn: {result.stderr!r}"
    )


def test_digest_pinned_seed_skips_refresh_without_warn(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """A digest-pinned seed is content-addressed and can never drift — a
    re-run against the SAME pinned ref skips the refresh fetch entirely,
    reporting `already_adopted` without a warning."""
    digest = push_raw_config_package(
        registry, unique_repo, "v1", b'[registry]\ndefault = "config-setup-pin.example"\n'
    )
    pinned_ref = f"{registry}/{unique_repo}@{digest}"

    assert _run(ocx, "config", "setup", "--managed-config", pinned_ref).returncode == EXIT_SUCCESS
    snapshot_before = _snapshot_path(ocx).read_text()

    result = _run(ocx, "config", "setup", "--managed-config", pinned_ref)
    assert result.returncode == EXIT_SUCCESS, f"a re-run against a digest-pinned seed must succeed: {result.stderr}"
    assert _status(result) == "already_adopted"
    assert _snapshot_path(ocx).read_text() == snapshot_before
    assert "could not refresh" not in result.stderr, (
        f"a digest-pinned skip must not warn: {result.stderr!r}"
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
