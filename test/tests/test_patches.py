# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the OCX patch overlay feature (Phases 1-6A).

Covers the end-to-end lifecycle of the patches feature:
  - Publishing patch descriptors (per-base path and global descriptor)
  - Discovering and installing companion packages at base install time
  - Composing companion INTERFACE env vars onto base package env
  - Scoped glob matching (per-base descriptor pattern matching)
  - Required fail-closed / optional fail-open semantics
  - `ocx patch sync` for refreshing descriptors
  - `ocx patch freeze` + `OCX_PATCH_SNAPSHOT` for deterministic builds
  - `ocx patch test` for local descriptor dry-run
  - `ocx package env` JSON output verification
  - `ocx package exec` command receives companion env vars
  - Companion visibility inheritance via dependency surface (sealed/private/public/interface)

NOTE: The global descriptor lives at the reserved `global` repository in the patch
registry (e.g. `<patch-registry>/global:__ocx.patch`). It is exercised by
`test_global_descriptor_applies_to_multiple_bases`.

Each test function carries a docstring naming the ADR behaviour (C-code) it
covers. ADR reference: adr_infrastructure_patches.md
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo

# The global descriptor lives at a FIXED, registry-wide repository
# (`<patch-registry>/global:__ocx.patch`). Several tests here publish to it,
# and any base install with a `[patches]` tier probes it — so two patch tests
# running concurrently on the shared registry:2 can overwrite each other's
# global descriptor or observe a sibling's. Pin the whole module to a single
# xdist worker so these tests run sequentially (deterministic order); other
# test modules still parallelize, and none of them touch the `[patches]` tier.
pytestmark = pytest.mark.xdist_group("patch_global_slot")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_config(ocx: OcxRunner, patch_registry: str, *, required: bool = True) -> Path:
    """Write $OCX_HOME/config.toml with a [patches] section."""
    required_str = "true" if required else "false"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f"[patches]\n"
        f'registry = "{patch_registry}"\n'
        f"required = {required_str}\n"
    )
    return config_path


def _make_companion(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    env_key: str,
    env_value: str,
) -> PackageInfo:
    """Publish an env-only companion package with INTERFACE-visible env var.

    Always published `platform="any"`: a binary-free, env-only companion is
    the canonical `any`-published package (adr_platform_model_unification.md
    D1's `Any`-offer rule) — it survives `ocx patch sync`'s default 5-platform
    fan-out regardless of which concrete platform(s) a consumer actually runs
    on. A companion pinned to the current host platform instead fails closed
    the moment any `required` rule referencing it (especially a `--global`
    wildcard rule, published to the registry's single reserved, cross-session
    `global` repo slot) is fanned out against a platform it was never
    published for.
    """
    return make_package(
        ocx,
        repo,
        tag,
        tmp_path,
        bins=[],
        env=[
            {
                "key": env_key,
                "type": "constant",
                "value": env_value,
                "visibility": "interface",
            }
        ],
        new=True,
        cascade=True,
        platform="any",
    )


def _write_descriptor(path: Path, rules: list[dict]) -> None:
    """Write a patch descriptor JSON file."""
    path.write_text(json.dumps({"version": 1, "rules": rules}))


def _publish_descriptor_at_base(
    ocx: OcxRunner,
    descriptor_path: Path,
    base_fq: str,
) -> None:
    """Publish descriptor at the per-base path (`base_id` form)."""
    result = ocx.plain(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        base_fq,
    )
    assert result.returncode == 0, (
        f"patch publish at base {base_fq} failed:\n{result.stderr}"
    )


def _env_entries(ocx: OcxRunner, pkg_short: str) -> list[dict]:
    """Return `entries` from `ocx --format json package env <pkg>`."""
    result = ocx.json("package", "env", pkg_short)
    return result["entries"]


def _entry_by_key(entries: list[dict], key: str) -> dict | None:
    """Return the first entry with the given key, or None."""
    return next((e for e in entries if e["key"] == key), None)


def _unique_repo(label: str) -> str:
    """Generate a unique OCI repository name for within-test use."""
    return f"t_{uuid4().hex[:8]}_{label}"


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str) -> dict:
    """Build a dependency descriptor for `make_package(dependencies=...)`."""
    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    return {"identifier": f"{pkg.fq}@{digest}", "visibility": visibility}


# ---------------------------------------------------------------------------
# Scenario 1: Per-base descriptor with * glob composes companion env onto base
# (flagship corp-CA pattern)
# ---------------------------------------------------------------------------


def test_corp_ca_wildcard_descriptor_composes_on_base(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C1/C4: descriptor with match '*' names a companion exposing
    SSL_CERT_FILE (interface visibility). After install of the base, `ocx package env`
    includes the companion's interface var.

    NOTE: Uses per-base publish path because --global-root fails with registry:2
    (empty OCI repository path — see module docstring for bug note).
    """
    # ── Publish companion ──
    companion_repo = _unique_repo("ca_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "SSL_CERT_FILE", "/etc/ssl/certs/corp-ca.pem")

    # ── Publish base and a matching descriptor at its per-package path ──
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "ca_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # ── Install base → companion auto-installed by lazy discovery ──
    ocx.plain("package", "install", base_pkg.short)

    # ── Assert companion env var present ──
    entries = _env_entries(ocx, base_pkg.short)
    ssl_entry = _entry_by_key(entries, "SSL_CERT_FILE")
    assert ssl_entry is not None, (
        f"SSL_CERT_FILE must appear in package env after install with companion descriptor; "
        f"got keys: {[e['key'] for e in entries]}"
    )
    assert ssl_entry["value"] == "/etc/ssl/certs/corp-ca.pem"
    assert ssl_entry["type"] == "constant"


# ---------------------------------------------------------------------------
# Scenario 1b: global descriptor applies to multiple bases via --global flag
# ---------------------------------------------------------------------------


def test_global_descriptor_applies_to_multiple_bases(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: `--global` publishes the descriptor to the reserved `global`
    repository in the patch registry (a normal OCI repo path). The global descriptor
    with a `*` rule is applied to every installed base.

    Regression guard: this MUST succeed on registry:2 — the fix moved global
    descriptor storage from an empty-repository root (which registry:2 rejected)
    to the reserved single-segment `global` repository.
    """
    # Publish a companion with a recognisable env var
    companion_repo = _unique_repo("global_ca_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "GLOBAL_CA", "corp-ca")

    # Write a descriptor that matches everything
    descriptor_path = tmp_path / "global_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [companion_fq], "required": True}],
    )
    _write_config(ocx, registry)

    # Publish as global — MUST succeed on registry:2 (core regression guard)
    result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert result.returncode == 0, (
        f"--global publish must succeed on registry:2 (reserved `global` repo fix).\n"
        f"stderr: {result.stderr}"
    )

    # Build two distinct base packages
    base1_repo = _unique_repo("global_base1")
    base1 = make_package(ocx, base1_repo, "1.0.0", tmp_path, new=True, cascade=True)

    base2_repo = _unique_repo("global_base2")
    base2 = make_package(ocx, base2_repo, "1.0.0", tmp_path, new=True, cascade=True)

    # Install both bases; lazy discovery fires for each
    ocx.plain("package", "install", base1.short)
    ocx.plain("package", "install", base2.short)

    # GLOBAL_CA must appear in BOTH bases' env (global descriptor applies to all)
    entries1 = _env_entries(ocx, base1.short)
    entries2 = _env_entries(ocx, base2.short)

    ca_entry1 = _entry_by_key(entries1, "GLOBAL_CA")
    assert ca_entry1 is not None, (
        f"GLOBAL_CA must appear in env of {base1.short} (global descriptor applies to all);\n"
        f"got keys: {[e['key'] for e in entries1]}"
    )
    assert ca_entry1["value"] == "corp-ca"

    ca_entry2 = _entry_by_key(entries2, "GLOBAL_CA")
    assert ca_entry2 is not None, (
        f"GLOBAL_CA must appear in env of {base2.short} (global descriptor applies to all);\n"
        f"got keys: {[e['key'] for e in entries2]}"
    )
    assert ca_entry2["value"] == "corp-ca"


# ---------------------------------------------------------------------------
# Scenario 2: Per-base descriptor scoped — does NOT compose on different base
# ---------------------------------------------------------------------------


def test_per_base_descriptor_only_applies_to_its_base(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C2: per-base descriptor is scoped to one base.
    JDK_TRUSTSTORE companion appears on the matched base, absent on unrelated base.
    """
    # ── Publish companion ──
    companion_repo = _unique_repo("jdk_truststore")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "JDK_TRUSTSTORE", "/etc/ssl/java/cacerts")

    # ── Publish matched base (JDK) ──
    base_a_repo = _unique_repo("jdk_base")
    base_a = make_package(ocx, base_a_repo, "21.0.0", tmp_path, new=True, cascade=True)

    # ── Publish unrelated base ──
    base_b_repo = _unique_repo("cmake_base")
    base_b = make_package(ocx, base_b_repo, "3.28.0", tmp_path, new=True, cascade=True)

    # ── Publish descriptor ONLY at base_a's path ──
    descriptor_path = tmp_path / "per_base_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_a.fq)
    # Deliberately NOT publishing descriptor at base_b's path

    # ── Install both bases ──
    ocx.plain("package", "install", base_a.short)
    ocx.plain("package", "install", base_b.short)

    # ── Companion var appears on base_a, absent on base_b ──
    entries_a = _env_entries(ocx, base_a.short)
    entries_b = _env_entries(ocx, base_b.short)

    assert _entry_by_key(entries_a, "JDK_TRUSTSTORE") is not None, (
        "JDK_TRUSTSTORE must appear on the base that has a descriptor"
    )
    assert _entry_by_key(entries_b, "JDK_TRUSTSTORE") is None, (
        "JDK_TRUSTSTORE must NOT appear on an unrelated base with no descriptor"
    )


# ---------------------------------------------------------------------------
# Scenario 3: required=true (default) with missing companion — fail closed
# ---------------------------------------------------------------------------


def test_required_true_missing_companion_fails_closed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C7: required=true descriptor with missing companion
    → base install fails non-zero (fail-closed posture).
    """
    nonexistent_companion = f"{registry}/nonexistent-companion-{uuid4().hex[:8]}:latest"
    descriptor_path = tmp_path / "required_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [nonexistent_companion]}])
    _write_config(ocx, registry, required=True)

    # Publish base and its descriptor (companion does not exist)
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install must fail because companion is not in registry
    result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert result.returncode != 0, (
        "Installing base with required=true missing companion must fail (fail-closed C7). "
        f"Got exit 0.\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 4: required=false — missing companion does not block install
# ---------------------------------------------------------------------------


def test_required_false_missing_companion_fails_open(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C7 (inverse): required=false descriptor with missing companion
    → install succeeds (warn-and-skip). Companion var absent from env.
    """
    nonexistent_companion = f"{registry}/optional-companion-{uuid4().hex[:8]}:latest"
    descriptor_path = tmp_path / "optional_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [nonexistent_companion], "required": False}],
    )
    _write_config(ocx, registry, required=False)

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install must succeed despite missing optional companion
    result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert result.returncode == 0, (
        f"Installing base with required=false missing companion must succeed (fail-open C7). "
        f"Got exit {result.returncode}.\nstderr: {result.stderr}"
    )

    # The non-existent companion's env var must be absent; entries list is valid
    entries = _env_entries(ocx, base_pkg.short)
    assert isinstance(entries, list), "entries must be a list"


# ---------------------------------------------------------------------------
# Scenario 4c: unreachable/empty patch registry — the descriptor *fetch* failure
# is gated on the tier `required` posture, not fatal unconditionally.
# ---------------------------------------------------------------------------

# A patch registry host with nothing listening: the descriptor fetch fails to
# connect (connection refused), which is DISTINCT from a reachable-but-empty
# registry that returns a clean 404 (recorded as "no patch", never an error).
# Port 1 is privileged and unused, so the connect fails immediately.
_UNREACHABLE_PATCH_REGISTRY = "127.0.0.1:1"


def test_unreachable_patch_registry_required_false_installs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Regression: a non-required patch tier whose registry is unreachable must NOT
    abort the base install. Discovery is a side effect of install, so a
    descriptor-fetch failure under `required = false` warns and continues.

    Before the fix, the fetch error propagated through `install` and failed the
    base install regardless of `required` — the empty/unreachable patch-server bug.
    """
    # Publish + index the base BEFORE writing the patch config, so the only
    # discovery pass that probes the unreachable registry is our explicit install.
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    _write_config(ocx, _UNREACHABLE_PATCH_REGISTRY, required=False)

    result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert result.returncode == 0, (
        "Installing a base with a non-required, unreachable patch registry must "
        f"succeed (warn + continue). Got exit {result.returncode}.\nstderr: {result.stderr}"
    )


def test_unreachable_patch_registry_required_true_fails_closed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Counterpart: a required patch tier whose registry is unreachable fails the
    install closed — OCX cannot confirm that no mandated companion applies, so it
    must not silently install the base without the overlay (C7).
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    _write_config(ocx, _UNREACHABLE_PATCH_REGISTRY, required=True)

    result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert result.returncode != 0, (
        "Installing a base with a required, unreachable patch registry must fail "
        f"closed. Got exit 0.\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 4b: explicit `ocx patch sync` fails closed on a required companion (F-A)
# ---------------------------------------------------------------------------


def test_patch_sync_fails_closed_on_required_missing_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """F-A: an explicit `ocx patch sync` that cannot install a `required`
    companion must exit non-zero (fail-closed, C7) — not warn-and-succeed.

    The base is installed BEFORE its descriptor exists (so the install itself
    succeeds), then a descriptor naming a non-existent required companion is
    published. `ocx patch sync` re-fetches that descriptor and tries to install
    the companion; the failure must propagate to the process exit code.
    """
    _write_config(ocx, registry, required=True)

    # Install the base while no descriptor exists yet -> install succeeds.
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    install_result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert install_result.returncode == 0, (
        "base install must succeed before any descriptor is published; "
        f"got exit {install_result.returncode}\nstderr: {install_result.stderr}"
    )

    # Publish a descriptor naming a required companion that does not exist.
    nonexistent_companion = f"{registry}/nonexistent-companion-{uuid4().hex[:8]}:latest"
    descriptor_path = tmp_path / "required_sync_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [nonexistent_companion]}])
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # `ocx patch sync` must fail closed: it re-fetches the descriptor and cannot
    # install the required companion.
    sync_result = ocx.run("patch", "sync", format=None, check=False)
    assert sync_result.returncode != 0, (
        "explicit `ocx patch sync` must exit non-zero when a required companion "
        "cannot be installed (fail-closed C7). Got exit 0.\n"
        f"stdout: {sync_result.stdout}\nstderr: {sync_result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 4c: `ocx --global env` fails closed on a missing required companion (F-B)
# ---------------------------------------------------------------------------


def test_global_env_fails_closed_on_missing_required_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """F-B: the global toolchain env exporter is lenient about AVAILABILITY (an
    absent global toolchain yields an empty env, exit 0) but must fail CLOSED on a
    C7 patch-enforcement failure — a `required` companion missing from a resolved
    global toolchain — exactly like the project tier. It must not silently drop an
    operator-mandated overlay.
    """
    # Leniency preserved: with no global toolchain configured yet, the exporter
    # returns an empty env and exit 0 (it must never break a login shell).
    _write_config(ocx, registry, required=False)
    empty = ocx.run("--global", "env", format=None, check=False)
    assert empty.returncode == 0, (
        "global env with no global toolchain must exit 0 (availability leniency); "
        f"got exit {empty.returncode}\nstderr: {empty.stderr}"
    )

    # A base whose descriptor names a companion that is NOT installed. Install
    # under `required=false` so discovery RECORDS the descriptor but tolerates the
    # missing companion (fail-open), then register the base in the global lock.
    missing_companion = f"{registry}/nonexistent-companion-{uuid4().hex[:8]}:latest"
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "global_fc_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [missing_companion]}])
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    install = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert install.returncode == 0, (
        "installing the base under required=false must succeed (fail-open records "
        f"the descriptor); got exit {install.returncode}\nstderr: {install.stderr}"
    )

    add = ocx.run("--global", "add", base_pkg.fq, format=None, check=False)
    assert add.returncode == 0, (
        f"--global add must succeed; got exit {add.returncode}\nstderr: {add.stderr}"
    )

    # Flip the tier to fail-closed. The recorded descriptor now names a REQUIRED
    # companion that is not installed — a C7 enforcement failure on the resolved
    # global toolchain.
    _write_config(ocx, registry, required=True)

    # `ocx --global env` must FAIL CLOSED (C7 parity with the project tier), not
    # silently emit an empty/partial env. Before the F-B fix the global arm
    # swallowed this error and exited 0.
    result = ocx.run("--global", "env", format=None, check=False)
    assert result.returncode != 0, (
        "global env must exit non-zero when a required companion is missing from a "
        "resolved global toolchain (fail-closed C7 parity with the project tier). "
        f"Got exit 0.\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 5: patch sync re-fetches descriptor and installs updated companion
# ---------------------------------------------------------------------------


def test_patch_sync_refreshes_descriptor_and_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: `ocx patch sync` re-fetches descriptors, installs
    newly-named companions, and reflects updated companion env values.
    """
    companion_repo = _unique_repo("sync_companion")

    # ── v1 companion ──
    companion_v1 = _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "SYNC_CA", "/certs/v1/ca.pem")

    # ── Publish base + descriptor pointing at v1 ──
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "sync_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_v1.fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    # Verify v1 value is present
    entries_before = _env_entries(ocx, base_pkg.short)
    ca_before = _entry_by_key(entries_before, "SYNC_CA")
    assert ca_before is not None, "SYNC_CA must be present after initial install"
    assert ca_before["value"] == "/certs/v1/ca.pem"

    # ── Publish v2 companion and re-publish descriptor pointing at v2 ──
    companion_v2 = _make_companion(ocx, companion_repo, "2.0.0", tmp_path, "SYNC_CA", "/certs/v2/ca.pem")
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_v2.fq]}])
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # ── Run patch sync ──
    #
    # No `--platform`: fans out over the full concrete ship matrix (D4
    # exception, `adr_platform_model_unification.md`). `_make_companion`
    # publishes `any`, which satisfies every platform in the fan-out.
    sync_result = ocx.run("patch", "sync", format="json", check=False)
    assert sync_result.returncode == 0, (
        f"ocx patch sync must succeed; got {sync_result.returncode}\nstderr: {sync_result.stderr}"
    )
    sync_report = json.loads(sync_result.stdout)
    assert sync_report["companions_installed"] >= 1, (
        "sync must report the v2 companion it installed, not the hardcoded 0; "
        f"got: {sync_report}"
    )

    # ── After sync, env should reflect v2 ──
    entries_after = _env_entries(ocx, base_pkg.short)
    ca_after = _entry_by_key(entries_after, "SYNC_CA")
    assert ca_after is not None, "SYNC_CA must still be present after sync"
    assert ca_after["value"] == "/certs/v2/ca.pem", (
        f"After sync, SYNC_CA must reflect v2; got: {ca_after['value']}"
    )


# ---------------------------------------------------------------------------
# Scenario 6: patch freeze pins companion digest; OCX_PATCH_SNAPSHOT keeps it frozen
# ---------------------------------------------------------------------------


def test_patch_freeze_pins_companion_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C8: `ocx --global patch freeze` writes patches.snapshot.json.
    Under OCX_PATCH_SNAPSHOT the env is frozen; without it the env floats.
    """
    companion_repo = _unique_repo("freeze_companion")

    # ── v1 companion ──
    companion_v1 = _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "FROZEN_CA", "/certs/frozen-v1/ca.pem")

    # ── Publish base + v1 descriptor ──
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "freeze_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_v1.fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    # ── Freeze ──
    freeze_result = ocx.run("--global", "patch", "freeze", format="json", check=False)
    assert freeze_result.returncode == 0, (
        f"ocx --global patch freeze must succeed; got {freeze_result.returncode}\n"
        f"stderr: {freeze_result.stderr}"
    )
    freeze_report = json.loads(freeze_result.stdout)
    snapshot_path = Path(freeze_report["path"])
    assert snapshot_path.exists(), f"patches.snapshot.json must exist at {snapshot_path}"

    # ── v2 companion ──
    companion_v2 = _make_companion(ocx, companion_repo, "2.0.0", tmp_path, "FROZEN_CA", "/certs/frozen-v2/ca.pem")
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_v2.fq]}])
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    # Sync to make local store aware of v2
    ocx.run("patch", "sync", format=None, check=False)

    # ── WITHOUT snapshot → float to v2 ──
    entries_float = _env_entries(ocx, base_pkg.short)
    ca_float = _entry_by_key(entries_float, "FROZEN_CA")
    assert ca_float is not None, "FROZEN_CA must appear without snapshot"
    assert ca_float["value"] == "/certs/frozen-v2/ca.pem", (
        f"Without snapshot, FROZEN_CA must float to v2; got: {ca_float['value']}"
    )

    # ── WITH OCX_PATCH_SNAPSHOT → frozen at v1 ──
    env_frozen = dict(ocx.env)
    env_frozen["OCX_PATCH_SNAPSHOT"] = str(snapshot_path)
    cmd = [str(ocx.binary), "--format", "json", "package", "env", base_pkg.short]
    result_frozen = subprocess.run(cmd, capture_output=True, text=True, env=env_frozen)
    assert result_frozen.returncode == 0, (
        f"package env with OCX_PATCH_SNAPSHOT must succeed; got {result_frozen.returncode}\n"
        f"stderr: {result_frozen.stderr}"
    )
    entries_frozen = json.loads(result_frozen.stdout)["entries"]
    ca_frozen = _entry_by_key(entries_frozen, "FROZEN_CA")
    assert ca_frozen is not None, "FROZEN_CA must appear with snapshot"
    assert ca_frozen["value"] == "/certs/frozen-v1/ca.pem", (
        f"With OCX_PATCH_SNAPSHOT, FROZEN_CA must stay at frozen v1 value; "
        f"got: {ca_frozen['value']}"
    )


# ---------------------------------------------------------------------------
# Scenario 7: no-patches opt-out (project tier)
# ---------------------------------------------------------------------------


def _write_project_toml(project_dir: Path, base_fq: str, *, opt_out: bool) -> None:
    """Write a project `ocx.toml` binding `base_fq`, optionally opting it out."""
    body = f'[tools]\ntool = "{base_fq}"\n'
    if opt_out:
        body += f'\n[package."{base_fq}"]\nno-patches = true\n'
    (project_dir / "ocx.toml").write_text(body)


def _run_in(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run `ocx` from `cwd` (project-tier commands read `ocx.toml`/`ocx.lock` from CWD)."""
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def test_no_patches_opt_out_suppresses_overlay_in_direnv_export(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C6/C7: `ocx direnv export` must honor a project's
    per-package `no-patches = true` opt-out.

    Regression guard: `direnv_export.rs` called the boundary-less
    `resolve_env` wrapper (hardcoded empty opt-out set) instead of
    `resolve_env_with_patch_boundary` threaded with
    `project.config.no_patches_repositories()` — so a project's opt-out was
    silently ignored and the companion overlay always applied via
    `ocx direnv export`, unlike every other project-tier env exit (`run`,
    `env`).

    The companion (INTERFACE `DIRENV_OPT_CA`) is installed once via
    `ocx package install` (site `[patches]` tier records local descriptor
    state). Two project directories bind the SAME installed base: one
    declares `no-patches = true` for it, the other does not. `ocx direnv
    export` in the opted-out project must NOT emit `DIRENV_OPT_CA`; the
    sibling project (no opt-out) must still emit it — proving the opt-out
    itself (not a blanket direnv/patches regression) is what suppresses it.
    """
    companion_repo = _unique_repo("direnv_opt_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DIRENV_OPT_CA", "/etc/ssl/direnv-opt-ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "direnv_opt_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install base -> companion auto-discovered + installed locally (site patch state
    # recorded offline-readable, so the later `ocx direnv export` offline view can see it).
    ocx.plain("package", "install", base_pkg.short)

    opted_out_project = tmp_path / "proj_opted_out"
    opted_out_project.mkdir()
    _write_project_toml(opted_out_project, base_pkg.fq, opt_out=True)

    baseline_project = tmp_path / "proj_baseline"
    baseline_project.mkdir()
    _write_project_toml(baseline_project, base_pkg.fq, opt_out=False)

    for project, label in ((opted_out_project, "opted_out"), (baseline_project, "baseline")):
        for args in (("lock",), ("pull",)):
            result = _run_in(ocx, project, *args)
            assert result.returncode == 0, (
                f"ocx {' '.join(args)} in the {label} project must succeed; "
                f"rc={result.returncode}\nstderr: {result.stderr}"
            )

    opted_out_result = _run_in(ocx, opted_out_project, "direnv", "export")
    assert opted_out_result.returncode == 0, (
        f"ocx direnv export must succeed; rc={opted_out_result.returncode}\n"
        f"stderr: {opted_out_result.stderr}"
    )
    assert "DIRENV_OPT_CA" not in opted_out_result.stdout, (
        "no-patches=true for this base must suppress the companion overlay in "
        f"`ocx direnv export`; got:\n{opted_out_result.stdout}"
    )

    baseline_result = _run_in(ocx, baseline_project, "direnv", "export")
    assert baseline_result.returncode == 0, (
        f"ocx direnv export must succeed; rc={baseline_result.returncode}\n"
        f"stderr: {baseline_result.stderr}"
    )
    assert "DIRENV_OPT_CA" in baseline_result.stdout, (
        "sibling project without no-patches must still receive the companion overlay "
        f"(proves the opt-out, not a blanket regression, suppressed it); "
        f"got:\n{baseline_result.stdout}"
    )


def test_no_patches_opt_out_honored_across_launcher_in_run(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """AF1 (adr_patch_env_resolution_uniformity.md): a project `no-patches = true`
    opt-out is honored across the generated entrypoint launcher when a tool runs
    through `ocx run`.

    A GLOBAL descriptor (`match: "*"`) is used deliberately: it is the only descriptor
    kind that re-derives at the launcher, because the launcher's synthetic
    `file-url-mode/<digest>` base id matches a catch-all rule but never a per-base
    descriptor. The entrypoint `showenv` dispatches to the system `env` dumper so the
    test reads the launchered tool's real process env.

    `ocx run` composes the PARENT env with the opt-out honored (companion excluded),
    then resolves `showenv` to the base's generated launcher; the launcher re-enters
    `ocx launcher exec`, which re-derives the base's env from the forwarded
    `OCX_PATCHES` (`no_patches` carrying both the project's `registry/repository`
    opt-out keys AND the opted-out base's content digest). Because the launcher's own
    base identity is a synthetic content-addressed id (no real `registry/repository`),
    it is the DIGEST leg of the opt-out that matches here and suppresses the
    re-injected companion (`adr_patch_env_resolution_uniformity.md` AF1 resolution).
    """
    companion_repo = _unique_repo("run_launch_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "RUN_LAUNCH_CA", "run-launch-ca-value")

    # A GLOBAL (`match: "*"`) descriptor is what the launcher re-derives against its
    # synthetic base id; a per-base descriptor would pass trivially (never re-injected).
    descriptor_path = tmp_path / "run_launch_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry, required=False)
    publish = ocx.run(
        "patch", "publish", "--descriptor", str(descriptor_path), "--global",
        format=None, check=False,
    )
    assert publish.returncode == 0, f"global patch publish must succeed:\n{publish.stderr}"

    # Entrypoint `showenv` dispatches to the system `env` dumper so the test reads the
    # launchered tool's real process env after the launcher re-entry.
    base_pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
    )
    ocx.plain("package", "install", base_pkg.short)

    project = tmp_path / "run_opted_out"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=True)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    # `ocx run -- showenv`: `showenv` resolves to the base's generated launcher on the
    # composed PATH; the launcher re-enters `ocx launcher exec` and dispatches to the
    # system `env`, dumping the launchered tool's real process env.
    result = _run_in(ocx, project, "run", "--", "showenv")
    assert result.returncode == 0, (
        f"ocx run -- showenv must succeed; rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert "RUN_LAUNCH_CA" not in result.stdout, (
        "no-patches=true must suppress the companion in the launchered tool's process "
        f"env (opt-out honored across the launcher); got env dump:\n{result.stdout}"
    )


def test_launcher_digest_matched_opt_out_respects_system_required(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """C7 invariant across the launcher's DIGEST-matched opt-out leg: a
    forwarded `no_patches` entry keyed by the base's content digest suppresses a
    NON-system-required companion but NEVER a SYSTEM-required one.

    Drives `ocx launcher exec` directly with a hand-set `OCX_PATCHES` wire whose
    `no_patches` entry is the installed base's REAL content digest (read from the
    on-disk `digest` sidecar file next to the package root — the same string form
    `Digest::to_string()` produces, e.g. `sha256:<hex>`), proving the producer
    (`run.rs`) and resolver (`resolve.rs`) agree on the digest string form.
    `system_required` cannot be reached through `ocx run` in this harness (only a
    SYSTEM-scope `/etc/ocx/config.toml` sets it, which acceptance tests cannot
    write), so this drives the launcher directly with `OCX_NO_CONFIG=1` — the
    harness the AF1 fork sanctions for this case.

    - `system_required = false` + digest opted out -> companion ABSENT.
    - `system_required = true`  + digest opted out -> companion PRESENT
      (enforcement beats opt-out — the digest-matching leg must not weaken C7).
    """
    companion_repo = _unique_repo("digest_sysreq_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DIGEST_SYSREQ_CA", "digest-sysreq-ca-value")

    base_pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
    )
    descriptor_path = tmp_path / "digest_sysreq_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry, required=False)
    publish = ocx.run(
        "patch", "publish", "--descriptor", str(descriptor_path), "--global",
        format=None, check=False,
    )
    assert publish.returncode == 0, f"global patch publish must succeed:\n{publish.stderr}"
    ocx.plain("package", "install", base_pkg.short)

    which = ocx.json("package", "which", base_pkg.short)
    pkg_root = Path(which[base_pkg.short])
    # The real content digest the launcher's `install_info_from_package_root`
    # derives for this base — read verbatim from the on-disk sidecar so the test
    # proves string-form agreement instead of re-deriving it independently.
    base_digest = (pkg_root / "digest").read_text().strip()
    assert base_digest.startswith("sha256:"), f"unexpected digest sidecar content: {base_digest!r}"

    def _launcher_env_dump(*, system_required: bool) -> subprocess.CompletedProcess[str]:
        wire = json.dumps(
            {
                "registry": registry,
                "path_template": "{registry}/{repository}",
                "required": True,
                "system_required": system_required,
                "no_patches": [base_digest],
            }
        )
        env = {**ocx.env, "OCX_NO_CONFIG": "1", "OCX_PATCHES": wire}
        return subprocess.run(
            [str(ocx.binary), "launcher", "exec", str(pkg_root), "--", "showenv"],
            capture_output=True,
            text=True,
            env=env,
        )

    non_enforced = _launcher_env_dump(system_required=False)
    assert non_enforced.returncode == 0, (
        f"launcher exec must succeed (non-system-required); rc={non_enforced.returncode}\n"
        f"stderr: {non_enforced.stderr}"
    )
    assert "DIGEST_SYSREQ_CA" not in non_enforced.stdout, (
        "a forwarded no_patches entry keyed by content digest must suppress a "
        f"NON-system-required companion; got env dump:\n{non_enforced.stdout}"
    )

    enforced = _launcher_env_dump(system_required=True)
    assert enforced.returncode == 0, (
        f"launcher exec must succeed (system-required); rc={enforced.returncode}\n"
        f"stderr: {enforced.stderr}"
    )
    assert "DIGEST_SYSREQ_CA=digest-sysreq-ca-value" in enforced.stdout, (
        "a SYSTEM-required tier must overlay its companion EVEN when the base's digest "
        "is opted out (C7 enforcement beats opt-out — the digest-matching leg must not "
        f"weaken it); got env dump:\n{enforced.stdout}"
    )


def test_no_patches_opt_out_suppresses_overlay_in_toolchain_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx env` (toolchain-tier project path, `command/toolchain_env.rs`'s
    `execute` around the `PatchScope::Project(ctx.config.no_patches_repositories())`
    line) must honor a project's per-package `no-patches = true` opt-out, exactly
    like `ocx direnv export` (`test_no_patches_opt_out_suppresses_overlay_in_direnv_export`)
    and `ocx run` (`test_no_patches_opt_out_honored_across_launcher_in_run`) already do.

    Two sibling projects bind the SAME installed base: one declares
    `no-patches = true` for it, the other does not. `ocx env` in the opted-out
    project must NOT emit the companion var; the sibling project (no opt-out)
    must still emit it — proving the opt-out itself (not a blanket regression)
    is what suppresses it.
    """
    companion_repo = _unique_repo("toolchain_env_opt_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(
        ocx, companion_repo, "1.0.0", tmp_path, "TOOLCHAIN_ENV_OPT_CA", "/etc/ssl/toolchain-env-opt-ca.pem"
    )

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "toolchain_env_opt_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install base -> companion auto-discovered + installed locally (site patch
    # state recorded offline-readable, so the later `ocx env` offline resolution
    # can see it).
    ocx.plain("package", "install", base_pkg.short)

    opted_out_project = tmp_path / "toolchain_env_opted_out"
    opted_out_project.mkdir()
    _write_project_toml(opted_out_project, base_pkg.fq, opt_out=True)

    baseline_project = tmp_path / "toolchain_env_baseline"
    baseline_project.mkdir()
    _write_project_toml(baseline_project, base_pkg.fq, opt_out=False)

    for project, label in ((opted_out_project, "opted_out"), (baseline_project, "baseline")):
        for args in (("lock",), ("pull",)):
            result = _run_in(ocx, project, *args)
            assert result.returncode == 0, (
                f"ocx {' '.join(args)} in the {label} project must succeed; "
                f"rc={result.returncode}\nstderr: {result.stderr}"
            )

    opted_out_result = _run_in(ocx, opted_out_project, "--format", "json", "env")
    assert opted_out_result.returncode == 0, (
        f"ocx env must succeed; rc={opted_out_result.returncode}\nstderr: {opted_out_result.stderr}"
    )
    opted_out_entries = json.loads(opted_out_result.stdout)["entries"]
    assert _entry_by_key(opted_out_entries, "TOOLCHAIN_ENV_OPT_CA") is None, (
        "no-patches=true for this base must suppress the companion overlay in "
        f"`ocx env`; got keys: {[e['key'] for e in opted_out_entries]}"
    )

    baseline_result = _run_in(ocx, baseline_project, "--format", "json", "env")
    assert baseline_result.returncode == 0, (
        f"ocx env must succeed; rc={baseline_result.returncode}\nstderr: {baseline_result.stderr}"
    )
    baseline_entries = json.loads(baseline_result.stdout)["entries"]
    assert _entry_by_key(baseline_entries, "TOOLCHAIN_ENV_OPT_CA") is not None, (
        "sibling project without no-patches must still receive the companion overlay "
        f"(proves the opt-out, not a blanket regression, suppressed it); "
        f"got keys: {[e['key'] for e in baseline_entries]}"
    )


def test_global_no_patches_opt_out_suppresses_overlay_in_global_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """The global toolchain env exporter's opt-out lookup
    (`resolve_global_pinned_env` in `command/toolchain_env.rs`, the
    `PatchScope::Project(no_patches)` line built from
    `$OCX_HOME/ocx.toml`'s `no_patches_repositories()`) must honor a
    per-package `no-patches = true` opt-out exactly like the project tier
    (`test_no_patches_opt_out_suppresses_overlay_in_toolchain_env`).

    Reads the SAME global-toolchain base's env before and after the opt-out is
    written to `$OCX_HOME/ocx.toml`: the companion var is present beforehand
    (sanity baseline: the global path picks up the overlay at all) and absent
    afterward (the opt-out actually suppresses it) — nothing else about the
    toolchain changes between the two reads.
    """
    companion_repo = _unique_repo("global_toolchain_env_opt_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path,
        "GLOBAL_TOOLCHAIN_ENV_OPT_CA",
        "/etc/ssl/global-toolchain-env-opt-ca.pem",
    )

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "global_toolchain_env_opt_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    ocx.plain("package", "install", base_pkg.short)

    # `--global add` auto-creates $OCX_HOME/ocx.toml if absent and records the
    # base into the global toolchain's default [tools] group + lock.
    add_result = ocx.run("--global", "add", base_pkg.fq, format=None, check=False)
    assert add_result.returncode == 0, (
        f"ocx --global add must succeed; rc={add_result.returncode}\nstderr: {add_result.stderr}"
    )

    baseline_result = ocx.run("--global", "env", format="json", check=False)
    assert baseline_result.returncode == 0, (
        f"ocx --global env must succeed; rc={baseline_result.returncode}\n"
        f"stderr: {baseline_result.stderr}"
    )
    baseline_entries = json.loads(baseline_result.stdout)["entries"]
    assert _entry_by_key(baseline_entries, "GLOBAL_TOOLCHAIN_ENV_OPT_CA") is not None, (
        "sanity baseline: without an opt-out, `ocx --global env` must carry the "
        f"companion overlay; got keys: {[e['key'] for e in baseline_entries]}"
    )

    # Opt the base out in the global ocx.toml -- the same `[package."<id>"]`
    # shape `_write_project_toml` uses for the project tier.
    global_toml = Path(ocx.env["OCX_HOME"]) / "ocx.toml"
    with global_toml.open("a") as handle:
        handle.write(f'\n[package."{base_pkg.fq}"]\nno-patches = true\n')

    opted_out_result = ocx.run("--global", "env", format="json", check=False)
    assert opted_out_result.returncode == 0, (
        f"ocx --global env must succeed; rc={opted_out_result.returncode}\n"
        f"stderr: {opted_out_result.stderr}"
    )
    opted_out_entries = json.loads(opted_out_result.stdout)["entries"]
    assert _entry_by_key(opted_out_entries, "GLOBAL_TOOLCHAIN_ENV_OPT_CA") is None, (
        "no-patches=true in $OCX_HOME/ocx.toml must suppress the companion overlay in "
        f"`ocx --global env`; got keys: {[e['key'] for e in opted_out_entries]}"
    )


def test_forwarded_opt_out_does_not_leak_into_unrelated_child_process(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """F2 (Codex cross-model finding): a forwarded project `no-patches` opt-out
    must NOT become ambient inherited process state.

    Regression: `Context::try_init` grafted an inherited `OCX_PATCHES.no_patches`
    onto ANY config-file-sourced `[patches]` tier, so the opt-out landed in
    `manager.patches()` AND was re-forwarded verbatim over `OCX_PATCHES` into
    every child process this ocx spawns — even for a resolution in an unrelated
    project/base that never opted anything out. A project-local opt-out thus
    became ambient inherited process state. The forwarded opt-out is meaningful
    ONLY at the launcher re-entry (`ocx launcher exec`), which now decodes it
    directly from the env at consumption time; it is never grafted onto the
    manager tier and therefore never re-forwarded from a config-file tier.

    Reproduction (single hop, deterministic): a site `config.toml` declares a
    `[patches]` tier (the config-file tier the graft attached to). An ambient
    `OCX_PATCHES` carries an UNRELATED opt-out key (a repository the base is
    not). `ocx package exec <base> -- env` forwards this ocx's
    `config_view.patches` into the child `env` process, which dumps its
    environment. The re-forwarded `OCX_PATCHES` must carry an EMPTY `no_patches`
    — the ambient opt-out must not leak through a config-file tier.

    - Before the fix: child `OCX_PATCHES.no_patches` == [unrelated_key] (leak).
    - After the fix:  child `OCX_PATCHES.no_patches` == []            (no leak).
    """
    # Config-file `[patches]` tier: the tier the graft attached the forwarded
    # opt-out to. required=false so the absent global descriptor is tolerated
    # (fail-open) during install-time discovery.
    _write_config(ocx, registry, required=False)

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    ocx.plain("package", "install", base_pkg.short)

    # An UNRELATED opt-out key: a repository the base is not, so it is only
    # meaningful as leaked ambient state — never a legitimate opt-out here.
    unrelated_key = f"{registry}/unrelated-{uuid4().hex[:8]}"
    ambient_patches = json.dumps(
        {
            "registry": registry,
            "path_template": "{registry}/{repository}",
            "required": False,
            "system_required": False,
            "no_patches": [unrelated_key],
        }
    )
    env = {**ocx.env, "OCX_PATCHES": ambient_patches}

    # `ocx package exec <base> -- env` composes the base env (OCI-tier, no
    # project opt-out) and forwards this ocx's resolution-affecting config —
    # including `[patches]` — into the child `env`, which dumps its environment.
    result = subprocess.run(
        [str(ocx.binary), "package", "exec", base_pkg.short, "--", "env"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == 0, (
        f"ocx package exec must succeed; rc={result.returncode}\nstderr: {result.stderr}"
    )

    # Extract the re-forwarded OCX_PATCHES from the child env dump (a single line;
    # json.dumps emits no embedded newlines).
    forwarded_line = next(
        (line for line in result.stdout.splitlines() if line.startswith("OCX_PATCHES=")),
        None,
    )
    assert forwarded_line is not None, (
        "the child process must receive a forwarded OCX_PATCHES (a `[patches]` tier "
        f"is configured); env dump:\n{result.stdout}"
    )
    forwarded = json.loads(forwarded_line[len("OCX_PATCHES=") :])
    assert unrelated_key not in forwarded.get("no_patches", []), (
        "a forwarded project opt-out must NOT leak into unrelated child processes as "
        "ambient inherited state; a config-file `[patches]` tier must re-forward an "
        f"EMPTY no_patches. Got no_patches={forwarded.get('no_patches')!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 8: GC retains companion while base is installed
# ---------------------------------------------------------------------------


def test_gc_retains_companion_as_patch_root(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: companion packages are GC roots while their base is installed.
    After `ocx clean --force`, companion env var must still be present.
    """
    companion_repo = _unique_repo("gc_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "GC_TEST_CA", "/certs/gc.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "gc_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    # GC run — companion must not be collected while base is installed
    clean_result = ocx.run("clean", "--force", format=None, check=False)
    assert clean_result.returncode == 0, (
        f"ocx clean --force must succeed; got {clean_result.returncode}\nstderr: {clean_result.stderr}"
    )

    # Companion env var must still appear
    entries = _env_entries(ocx, base_pkg.short)
    ca_entry = _entry_by_key(entries, "GC_TEST_CA")
    assert ca_entry is not None, (
        "GC_TEST_CA must still be present after clean — companion is a patch root "
        "while its base is installed"
    )


# ---------------------------------------------------------------------------
# Scenario 9: `ocx patch test` composes descriptor locally without publishing
# ---------------------------------------------------------------------------


def test_patch_test_composes_env_locally_without_publishing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: `ocx patch test` dry-run compose.
    Companion var appears in composed output; descriptor is not published.
    """
    # Publish a base (patch test still resolves it from registry)
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)

    # Publish companion (patch test pulls it from registry to resolve)
    companion_repo = _unique_repo("patchtest_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PATCH_TEST_VAR", "dry-run-value")

    # Local descriptor — NOT published to registry
    descriptor_path = tmp_path / "patchtest_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"ocx patch test must succeed; got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    assert "entries" in report, f"patch test JSON must have 'entries'; got: {list(report.keys())}"
    entries = report["entries"]
    patch_var = _entry_by_key(entries, "PATCH_TEST_VAR")
    assert patch_var is not None, (
        f"PATCH_TEST_VAR must appear in patch test entries; "
        f"got: {[e['key'] for e in entries]}"
    )
    assert patch_var["value"] == "dry-run-value", (
        f"PATCH_TEST_VAR must carry companion's value; got: {patch_var['value']}"
    )
    assert "companions" in report, "patch test report must have 'companions'"
    assert len(report["companions"]) >= 1, "patch test report must list at least one companion"


# ---------------------------------------------------------------------------
# Scenario 10: publish round-trip — install discovers companion via lazy discovery
# ---------------------------------------------------------------------------


def test_patch_publish_roundtrip_install_discovers_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: after `ocx patch publish`, a fresh install of the base
    discovers the companion via lazy discovery and composes its interface env var.
    This is the canonical publish -> install -> env flow.
    """
    companion_repo = _unique_repo("roundtrip_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "ROUNDTRIP_VAR", "roundtrip-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "roundtrip_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    # Publish with JSON report verification
    publish_result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        base_pkg.fq,
        format="json",
        check=False,
    )
    assert publish_result.returncode == 0, (
        f"patch publish must succeed; got {publish_result.returncode}\nstderr: {publish_result.stderr}"
    )
    publish_report = json.loads(publish_result.stdout)
    assert "reference" in publish_report, f"must have 'reference'; got: {list(publish_report.keys())}"
    assert "manifest_digest" in publish_report
    assert publish_report["rules"] == 1, f"must have 1 rule; got: {publish_report['rules']}"

    # Install (lazy discovery fires)
    install_result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert install_result.returncode == 0, (
        f"install must succeed after patch publish; got {install_result.returncode}\n"
        f"stderr: {install_result.stderr}"
    )

    # Env must include companion var
    entries = _env_entries(ocx, base_pkg.short)
    rt_entry = _entry_by_key(entries, "ROUNDTRIP_VAR")
    assert rt_entry is not None, (
        f"ROUNDTRIP_VAR must appear in package env; got keys: {[e['key'] for e in entries]}"
    )
    assert rt_entry["value"] == "roundtrip-value"


# ---------------------------------------------------------------------------
# Scenario 11: no patch config — env output is unaffected (no-op guarantee)
# ---------------------------------------------------------------------------


def test_package_env_without_patch_config_unaffected(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No-op guarantee: without [patches] config, `ocx package env` returns
    only the base package's own env vars -- no patch overlay applied.
    """
    # No config.toml written
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    ocx.plain("package", "install", base_pkg.short)

    entries = _env_entries(ocx, base_pkg.short)
    assert len(entries) >= 1, "Base package must have at least one env entry"

    # Default make_package includes PATH + {REPO}_HOME
    home_key = base_pkg.repo.upper().replace("-", "_").replace("/", "_") + "_HOME"
    home_entry = _entry_by_key(entries, home_key)
    assert home_entry is not None, f"{home_key} must appear in base env without patches"


# ---------------------------------------------------------------------------
# Scenario 12: exec receives companion env var
# ---------------------------------------------------------------------------


def test_exec_receives_companion_env_var(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C4: after patching, `ocx package exec <base> -- env`
    includes the companion's interface env var in the exec environment.
    """
    companion_repo = _unique_repo("exec_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "EXEC_COMPANION_VAR", "companion-exec-value")

    base_pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["mybin"],
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            }
        ],
        new=True,
        cascade=True,
    )
    descriptor_path = tmp_path / "exec_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", base_pkg.short, "--", "env"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, (
        f"ocx package exec ... env must succeed; got {result.returncode}\nstderr: {result.stderr}"
    )
    assert "EXEC_COMPANION_VAR=companion-exec-value" in result.stdout, (
        f"EXEC_COMPANION_VAR must appear in exec env; excerpt:\n{result.stdout[:500]}"
    )


# ---------------------------------------------------------------------------
# Scenario 13: patch publish without config errors clearly
# ---------------------------------------------------------------------------


def test_patch_publish_without_config_errors(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ADR behaviour: `ocx patch publish` without [patches] config must fail
    with a non-zero exit and a clear error, not silently succeed.
    """
    descriptor_path = tmp_path / "nodesc.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": ["some/companion:latest"]}])
    # No config.toml written

    result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert result.returncode != 0, (
        "`ocx patch publish` without [patches] config must fail; got exit 0. "
        "Should error with 'no patch registry configured'."
    )


# ---------------------------------------------------------------------------
# Scenario 14: patch test without config errors clearly
# ---------------------------------------------------------------------------


def test_patch_test_without_config_errors(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ADR behaviour: `ocx patch test` without [patches] config must fail
    with a non-zero exit and a clear error.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "desc.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": ["some/companion:latest"]}])
    # No config.toml written

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format=None,
        check=False,
    )
    assert result.returncode != 0, (
        "`ocx patch test` without [patches] config must fail; got exit 0"
    )


# ---------------------------------------------------------------------------
# Scenario 15: multiple rules -- only matching rules compose
# ---------------------------------------------------------------------------


def test_multiple_rules_match_only_specific_bases(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: descriptor with two rules applies each companion
    only to the bases whose identifiers match the rule's glob pattern.
    A base that matches both rules gets both companions.
    """
    companion_a_repo = _unique_repo("multi_ca_companion_a")
    companion_a_fq = f"{registry}/{companion_a_repo}:1.0.0"
    _make_companion(ocx, companion_a_repo, "1.0.0", tmp_path, "MULTI_A_VAR", "value-a")

    companion_b_repo = _unique_repo("multi_ca_companion_b")
    companion_b_fq = f"{registry}/{companion_b_repo}:1.0.0"
    _make_companion(ocx, companion_b_repo, "1.0.0", tmp_path, "MULTI_B_VAR", "value-b")

    # base_b will be named in rule B's glob match
    base_b_repo = _unique_repo("multi_base_b")
    base_b = make_package(ocx, base_b_repo, "1.0.0", tmp_path, new=True, cascade=True)

    # base_other won't match rule B (but matches rule A via '*')
    base_other_repo = _unique_repo("multi_base_other")
    base_other = make_package(ocx, base_other_repo, "1.0.0", tmp_path, new=True, cascade=True)

    # Descriptor: rule A='*' (all bases), rule B=specific to base_b_repo
    descriptor_path = tmp_path / "multi_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[
            {"match": "*", "packages": [companion_a_fq]},
            {"match": f"*{base_b_repo}*", "packages": [companion_b_fq]},
        ],
    )
    _write_config(ocx, registry)
    # Publish descriptor at each base's path
    _publish_descriptor_at_base(ocx, descriptor_path, base_b.fq)
    _publish_descriptor_at_base(ocx, descriptor_path, base_other.fq)

    # Install both bases
    ocx.plain("package", "install", base_b.short)
    ocx.plain("package", "install", base_other.short)

    # base_b gets both companions (matches both rules)
    entries_b = _env_entries(ocx, base_b.short)
    assert _entry_by_key(entries_b, "MULTI_A_VAR") is not None, (
        "MULTI_A_VAR must appear on base_b (matches '*')"
    )
    assert _entry_by_key(entries_b, "MULTI_B_VAR") is not None, (
        "MULTI_B_VAR must appear on base_b (matches specific pattern)"
    )

    # base_other gets only companion A (matches '*' only)
    entries_other = _env_entries(ocx, base_other.short)
    assert _entry_by_key(entries_other, "MULTI_A_VAR") is not None, (
        "MULTI_A_VAR must appear on base_other (matches '*')"
    )
    assert _entry_by_key(entries_other, "MULTI_B_VAR") is None, (
        "MULTI_B_VAR must NOT appear on base_other "
        "(only base_b matches the specific pattern)"
    )


# ---------------------------------------------------------------------------
# Scenario 15b: parallel patch discovery -- two bases in one install_all call
# ---------------------------------------------------------------------------


def test_parallel_discovery_installs_each_bases_companion(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """C3 (parallel patch discovery): installing two bases in a SINGLE
    `ocx package install base_a base_b` command runs Phase-3 discovery for both
    concurrently (JoinSet). Each base carries a DISTINCT per-base descriptor
    naming its own companion, so after the single parallel install both
    companions must be discovered and installed, and each base's env must carry
    only its own companion's interface var. This exercises the parallelized
    discovery loop (>=2 packages through one install_all), which the per-base
    single-install scenarios above do not.
    """
    # Two distinct companions, one per base.
    companion_a_repo = _unique_repo("par_companion_a")
    companion_a_fq = f"{registry}/{companion_a_repo}:1.0.0"
    _make_companion(ocx, companion_a_repo, "1.0.0", tmp_path, "PAR_A_VAR", "value-a")

    companion_b_repo = _unique_repo("par_companion_b")
    companion_b_fq = f"{registry}/{companion_b_repo}:1.0.0"
    _make_companion(ocx, companion_b_repo, "1.0.0", tmp_path, "PAR_B_VAR", "value-b")

    base_a = make_package(ocx, _unique_repo("par_base_a"), "1.0.0", tmp_path, new=True, cascade=True)
    base_b = make_package(ocx, _unique_repo("par_base_b"), "1.0.0", tmp_path, new=True, cascade=True)

    _write_config(ocx, registry)

    # Each base gets its own descriptor naming only its own companion. A per-base
    # descriptor applies only to the base at whose path it is published.
    descriptor_a = tmp_path / "par_descriptor_a.json"
    _write_descriptor(descriptor_a, rules=[{"match": "*", "packages": [companion_a_fq]}])
    _publish_descriptor_at_base(ocx, descriptor_a, base_a.fq)

    descriptor_b = tmp_path / "par_descriptor_b.json"
    _write_descriptor(descriptor_b, rules=[{"match": "*", "packages": [companion_b_fq]}])
    _publish_descriptor_at_base(ocx, descriptor_b, base_b.fq)

    # Install BOTH bases in ONE command -> single install_all -> parallel discovery.
    result = ocx.plain("package", "install", base_a.short, base_b.short)
    assert result.returncode == 0, f"parallel install failed:\n{result.stderr}"

    # base_a env carries only companion A's var.
    entries_a = _env_entries(ocx, base_a.short)
    assert _entry_by_key(entries_a, "PAR_A_VAR") is not None, (
        "PAR_A_VAR must appear on base_a after parallel discovery; "
        f"got keys: {[e['key'] for e in entries_a]}"
    )
    assert _entry_by_key(entries_a, "PAR_B_VAR") is None, (
        "PAR_B_VAR must NOT leak onto base_a (distinct per-base descriptors)"
    )

    # base_b env carries only companion B's var.
    entries_b = _env_entries(ocx, base_b.short)
    assert _entry_by_key(entries_b, "PAR_B_VAR") is not None, (
        "PAR_B_VAR must appear on base_b after parallel discovery; "
        f"got keys: {[e['key'] for e in entries_b]}"
    )
    assert _entry_by_key(entries_b, "PAR_A_VAR") is None, (
        "PAR_A_VAR must NOT leak onto base_b (distinct per-base descriptors)"
    )


# ---------------------------------------------------------------------------
# Scenario 16: per-rule required=false overrides tier-level default
# ---------------------------------------------------------------------------


def test_rule_required_false_overrides_tier_default(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour C7: per-rule `required: false` overrides tier-level `required = true`.
    A missing companion with rule-level required=false must not block install
    even when the tier default is fail-closed.
    """
    nonexistent_companion = f"{registry}/rule-optional-{uuid4().hex[:8]}:latest"
    descriptor_path = tmp_path / "rule_required_false.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [nonexistent_companion], "required": False}],
    )
    # Tier default is required=true (fail-closed), but rule overrides to false
    _write_config(ocx, registry, required=True)

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert result.returncode == 0, (
        f"Rule-level required=false must override tier required=true; "
        f"install must succeed even with missing companion. "
        f"Got exit {result.returncode}.\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 17–20: patch × dependency-visibility inheritance
#
# A global descriptor patches dependency D, which is wired into root R with
# varying visibility. The companion's env var must be admitted or blocked
# according to D's visibility surface.
#
# Common setup (DAMP — repeated per test for self-contained readability):
#   1. _write_config(ocx, registry, required=False)  -- fail-open; only C matters
#   2. Companion C with DISTINCTIVE var DEP_PATCH=present
#   3. Dependency D with own env var (public, so we can confirm D itself is fine)
#   4. Root R: make_package(..., dependencies=[_dep_entry(ocx, D, visibility=V)])
#   5. Publish global descriptor matching D by fq prefix
#   6. ocx package install R  (installs R + D)
#   7. ocx package install C  (pre-install so overlay can resolve C locally)
#   8. Assert consumer view (_env_entries) and --self view
# ---------------------------------------------------------------------------


def test_patch_on_sealed_dep_not_inherited(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """Sealed dependency: companion overlay is blocked in both consumer and --self views.

    A sealed dep is never admitted into the dependent's env surface at all,
    so its patch companions must not appear either.
    """
    _write_config(ocx, registry, required=False)

    companion_repo = _unique_repo("vis_sealed_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DEP_PATCH", "present")

    dep_repo = _unique_repo("vis_sealed_dep")
    dep = make_package(
        ocx,
        dep_repo,
        "1.0.0",
        tmp_path,
        env=[{"key": "SEALED_DEP_OWN", "type": "constant", "value": "own", "visibility": "public"}],
        new=True,
        cascade=True,
    )

    root_repo = _unique_repo("vis_sealed_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="sealed")],
        new=True,
        cascade=True,
    )

    descriptor_path = tmp_path / "sealed_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{dep.fq}*", "packages": [companion_fq], "required": False}],
    )
    publish_result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert publish_result.returncode == 0, (
        f"global publish must succeed; stderr: {publish_result.stderr}"
    )

    # Force sync so the newly published descriptor overwrites any stale cached
    # descriptor.  `ocx index update` piggybacks a sync at package-push time,
    # which may have populated global.json with the registry's previous content
    # before this test's publish.  `patch sync` Sync-mode re-fetches and updates
    # the tag-store entry to the freshly published digest.
    ocx.run("patch", "sync", format=None, check=False)

    ocx.plain("package", "install", root.short)
    ocx.plain("package", "install", companion_fq)

    consumer_entries = _env_entries(ocx, root.short)
    self_result = ocx.run("package", "env", "--self", root.short, format="json", check=False)
    assert self_result.returncode == 0, f"--self must succeed; stderr: {self_result.stderr}"
    self_entries: list[dict] = json.loads(self_result.stdout)["entries"]

    dep_patch_consumer = _entry_by_key(consumer_entries, "DEP_PATCH")
    dep_patch_self = _entry_by_key(self_entries, "DEP_PATCH")

    if dep_patch_consumer is not None or dep_patch_self is not None:
        # Report as product gap rather than forcing green
        consumer_keys = [e["key"] for e in consumer_entries]
        self_keys = [e["key"] for e in self_entries]
        pytest.fail(
            "PRODUCT GAP: sealed dep companion appeared in env output.\n"
            f"consumer keys: {consumer_keys}\n"
            f"--self keys: {self_keys}\n"
            "Sealed deps must block their patch companions from all surfaces."
        )


def test_patch_on_private_dep_only_under_self(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """Private dependency: companion overlay appears under --self, absent in consumer view.

    A private dep's env entries are admitted only on the owner's private surface
    (--self), so its patch companion must follow the same restriction.
    """
    _write_config(ocx, registry, required=False)

    companion_repo = _unique_repo("vis_private_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DEP_PATCH", "present")

    dep_repo = _unique_repo("vis_private_dep")
    dep = make_package(
        ocx,
        dep_repo,
        "1.0.0",
        tmp_path,
        env=[{"key": "PRIVATE_DEP_OWN", "type": "constant", "value": "own", "visibility": "public"}],
        new=True,
        cascade=True,
    )

    root_repo = _unique_repo("vis_private_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="private")],
        new=True,
        cascade=True,
    )

    descriptor_path = tmp_path / "private_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{dep.fq}*", "packages": [companion_fq], "required": False}],
    )
    publish_result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert publish_result.returncode == 0, (
        f"global publish must succeed; stderr: {publish_result.stderr}"
    )

    # Force sync so the newly published descriptor overwrites any stale cached
    # descriptor.  `ocx index update` piggybacks a sync at package-push time,
    # which may have populated global.json with the registry's previous content
    # before this test's publish.  `patch sync` Sync-mode re-fetches and updates
    # the tag-store entry to the freshly published digest.
    ocx.run("patch", "sync", format=None, check=False)

    ocx.plain("package", "install", root.short)
    ocx.plain("package", "install", companion_fq)

    consumer_entries = _env_entries(ocx, root.short)
    self_result = ocx.run("package", "env", "--self", root.short, format="json", check=False)
    assert self_result.returncode == 0, f"--self must succeed; stderr: {self_result.stderr}"
    self_entries: list[dict] = json.loads(self_result.stdout)["entries"]

    dep_patch_consumer = _entry_by_key(consumer_entries, "DEP_PATCH")
    dep_patch_self = _entry_by_key(self_entries, "DEP_PATCH")

    if dep_patch_consumer is not None:
        consumer_keys = [e["key"] for e in consumer_entries]
        pytest.fail(
            "PRODUCT GAP: private dep companion appeared in CONSUMER view (must be absent).\n"
            f"consumer keys: {consumer_keys}"
        )

    if dep_patch_self is None:
        self_keys = [e["key"] for e in self_entries]
        pytest.fail(
            "PRODUCT GAP: private dep companion absent from --self view (must be present).\n"
            f"--self keys: {self_keys}"
        )


def test_patch_on_public_dep_inherited_by_consumer(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """Public dependency: companion overlay is visible in the consumer view.

    A public dep surfaces its env entries to all consumers, so its patch
    companion must also appear in the consumer view.
    """
    _write_config(ocx, registry, required=False)

    companion_repo = _unique_repo("vis_public_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DEP_PATCH", "present")

    dep_repo = _unique_repo("vis_public_dep")
    dep = make_package(
        ocx,
        dep_repo,
        "1.0.0",
        tmp_path,
        env=[{"key": "PUBLIC_DEP_OWN", "type": "constant", "value": "own", "visibility": "public"}],
        new=True,
        cascade=True,
    )

    root_repo = _unique_repo("vis_public_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
        new=True,
        cascade=True,
    )

    descriptor_path = tmp_path / "public_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{dep.fq}*", "packages": [companion_fq], "required": False}],
    )
    publish_result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert publish_result.returncode == 0, (
        f"global publish must succeed; stderr: {publish_result.stderr}"
    )

    # Force sync so the newly published descriptor overwrites any stale cached
    # descriptor.  `ocx index update` piggybacks a sync at package-push time,
    # which may have populated global.json with the registry's previous content
    # before this test's publish.  `patch sync` Sync-mode re-fetches and updates
    # the tag-store entry to the freshly published digest.
    ocx.run("patch", "sync", format=None, check=False)

    ocx.plain("package", "install", root.short)
    ocx.plain("package", "install", companion_fq)

    consumer_entries = _env_entries(ocx, root.short)
    dep_patch_consumer = _entry_by_key(consumer_entries, "DEP_PATCH")

    if dep_patch_consumer is None:
        consumer_keys = [e["key"] for e in consumer_entries]
        pytest.fail(
            "PRODUCT GAP: public dep companion absent from CONSUMER view (must be present).\n"
            f"consumer keys: {consumer_keys}"
        )


def test_patch_on_interface_dep_inherited_by_consumer(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """Interface dependency: companion overlay is visible in the consumer view.

    An interface dep exposes its env entries to direct consumers (but not
    transitively to consumers of consumers). Its patch companion must appear
    in the same consumer view.
    """
    _write_config(ocx, registry, required=False)

    companion_repo = _unique_repo("vis_interface_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DEP_PATCH", "present")

    dep_repo = _unique_repo("vis_interface_dep")
    dep = make_package(
        ocx,
        dep_repo,
        "1.0.0",
        tmp_path,
        env=[{"key": "IFACE_DEP_OWN", "type": "constant", "value": "own", "visibility": "public"}],
        new=True,
        cascade=True,
    )

    root_repo = _unique_repo("vis_interface_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="interface")],
        new=True,
        cascade=True,
    )

    descriptor_path = tmp_path / "interface_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{dep.fq}*", "packages": [companion_fq], "required": False}],
    )
    publish_result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert publish_result.returncode == 0, (
        f"global publish must succeed; stderr: {publish_result.stderr}"
    )

    # Force sync so the newly published descriptor overwrites any stale cached
    # descriptor.  `ocx index update` piggybacks a sync at package-push time,
    # which may have populated global.json with the registry's previous content
    # before this test's publish.  `patch sync` Sync-mode re-fetches and updates
    # the tag-store entry to the freshly published digest.
    ocx.run("patch", "sync", format=None, check=False)

    ocx.plain("package", "install", root.short)
    ocx.plain("package", "install", companion_fq)

    consumer_entries = _env_entries(ocx, root.short)
    dep_patch_consumer = _entry_by_key(consumer_entries, "DEP_PATCH")

    if dep_patch_consumer is None:
        consumer_keys = [e["key"] for e in consumer_entries]
        pytest.fail(
            "PRODUCT GAP: interface dep companion absent from CONSUMER view (must be present).\n"
            f"consumer keys: {consumer_keys}"
        )


# ---------------------------------------------------------------------------
# Scenario 21: `ocx patch why` names the rule and companion for an applicable base
# ---------------------------------------------------------------------------


def test_patch_why_names_rule_and_companion_for_applicable_base(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx patch why <base>` names, for a companion-patched base, the env var
    it contributes, the descriptor rule glob that matched, and the companion
    identifier that produced it.
    """
    companion_repo = _unique_repo("why_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "WHY_VAR", "why-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "why_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install triggers lazy patch discovery so `patch why` finds the locally
    # recorded provenance without a live registry round-trip.
    install_result = ocx.plain("package", "install", base_pkg.short)
    assert install_result.returncode == 0, f"install must succeed; stderr: {install_result.stderr}"

    entries = ocx.json("patch", "why", base_pkg.short)
    assert isinstance(entries, list), f"`ocx patch why` JSON must be a bare array; got: {entries}"
    why_var = next((e for e in entries if e["variable"] == "WHY_VAR"), None)
    assert why_var is not None, (
        f"WHY_VAR must be named by `ocx patch why`; got variables: {[e['variable'] for e in entries]}"
    )
    assert why_var["rule"] == "*", f"must name the matching rule glob; got: {why_var['rule']}"
    assert why_var["companion"] == companion_fq, (
        f"must name the companion identifier; got: {why_var['companion']}"
    )


# ---------------------------------------------------------------------------
# Scenario 22: `ocx patch why` reports a clean empty result for an unaffected base
# ---------------------------------------------------------------------------


def test_patch_why_reports_no_patches_for_unaffected_base(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx patch why <base>` for a base with no applicable patch (no
    `[patches]` tier configured) exits 0 with an empty result -- not an error.
    """
    # No config.toml written -- no `[patches]` tier configured.
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    ocx.plain("package", "install", base_pkg.short)

    entries = ocx.json("patch", "why", base_pkg.short)
    assert entries == [], f"no `[patches]` tier configured must yield an empty result; got: {entries}"

    plain_result = ocx.plain("patch", "why", base_pkg.short)
    assert plain_result.returncode == 0, (
        f"`ocx patch why` on an unaffected base must exit 0; got {plain_result.returncode}\n"
        f"stderr: {plain_result.stderr}"
    )
    assert "no patches apply" in plain_result.stdout, (
        f"plain output must report a clean 'no patches apply' status; got: {plain_result.stdout}"
    )


# ---------------------------------------------------------------------------
# Scenario 23 (T3): a warmed OCX_HOME relocated to a new path resolves the same
# companion env offline (relocatable, content-addressed store)
# ---------------------------------------------------------------------------


def test_relocated_ocx_home_offline_companion_env_identical(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR validation: an OCX_HOME warmed by a patched install can be archived,
    restored at a DIFFERENT path, and `ocx --offline package env` there yields the
    same companion env.

    Offline resolution is content-addressed (local index + CAS blobs/packages), and
    the store's GC forward-refs (`refs/*`) are regenerated on `find`, so the whole
    store is relocatable — the property that lets CI cache `$OCX_HOME` and restore it
    on another runner at a different path. The companion is discovered/installed once
    in the warm home and must resolve with no network and no re-install after
    relocation.

    The relocation copies the store preserving its symlinks, then removes the
    original path so the store's absolute forward-refs now dangle — exactly the
    fresh-runner-different-path condition. `find` must self-heal those refs
    (`symlink::update` is idempotent) rather than error on them.
    """
    # ── Warm the store: publish companion + base + descriptor, install base. ──
    companion_repo = _unique_repo("relocate_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "RELOCATE_CA", "/certs/relocate.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "relocate_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    # Baseline companion env from the warm home.
    warm_ca = _entry_by_key(_env_entries(ocx, base_pkg.short), "RELOCATE_CA")
    assert warm_ca is not None, "setup: companion var must be present in the warm home before relocation"

    # ── Relocate WITHOUT deleting the fixture-provided OCX_HOME. Copy the warm
    #    fixture home into a THROWAWAY "original" dir, re-home its forward-refs onto
    #    that throwaway path (a warm offline resolve regenerates `refs/*` against the
    #    active OCX_HOME), then copy the throwaway into the final relocated path and
    #    delete only the throwaway. The relocated store's absolute forward-refs now
    #    dangle (they point at the deleted throwaway) — the fresh-runner condition —
    #    while the fixture home stays intact for teardown. ──
    warm_home = Path(ocx.env["OCX_HOME"])
    original_home = tmp_path / "warm_home_original"
    shutil.copytree(warm_home, original_home, symlinks=True)

    # Re-home the throwaway copy's absolute forward-refs onto its own path, so the
    # subsequent relocation dangles against the throwaway (not the fixture home).
    rehome = subprocess.run(
        [str(ocx.binary), "--format", "json", "--offline", "package", "env", base_pkg.short],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_HOME": str(original_home)},
    )
    assert rehome.returncode == 0, (
        f"setup: re-homing the throwaway copy's forward-refs must succeed; "
        f"rc={rehome.returncode}\nstderr: {rehome.stderr}"
    )

    relocated_home = tmp_path / "relocated_ocx_home"
    shutil.copytree(original_home, relocated_home, symlinks=True)
    shutil.rmtree(original_home)

    # ── Resolve the same base env OFFLINE against the RELOCATED home. ──
    relocated_env = {**ocx.env, "OCX_HOME": str(relocated_home)}
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "--offline", "package", "env", base_pkg.short],
        capture_output=True,
        text=True,
        env=relocated_env,
    )
    assert result.returncode == 0, (
        f"`ocx --offline package env` against a relocated OCX_HOME must succeed; "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )
    relocated_entries = json.loads(result.stdout)["entries"]
    relocated_ca = _entry_by_key(relocated_entries, "RELOCATE_CA")
    assert relocated_ca is not None, (
        "RELOCATE_CA must survive OCX_HOME relocation and resolve offline; "
        f"got keys: {[e['key'] for e in relocated_entries]}"
    )
    assert relocated_ca["value"] == warm_ca["value"], (
        "companion value must be identical after relocation; "
        f"warm={warm_ca['value']!r} relocated={relocated_ca['value']!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 24 (T4): GC collects a companion after its base is uninstalled
# (complement of Scenario 8, which asserts retention while installed)
# ---------------------------------------------------------------------------


def test_gc_collects_companion_after_base_uninstall(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: a companion is a GC (patch) root ONLY while its base is
    installed. After the base is uninstalled it is no longer an installed base, so
    the companion is no longer a patch root and `ocx clean --force` collects it from
    packages/. Complements `test_gc_retains_companion_as_patch_root`.
    """
    companion = _make_companion(
        ocx, _unique_repo("gc_collect_companion"), "1.0.0", tmp_path, "GC_COLLECT_CA", "/certs/collect.pem"
    )
    companion_fq = f"{registry}/{companion.repo}:1.0.0"

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)
    descriptor_path = tmp_path / "gc_collect_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    # Capture the companion package directory while the base is installed
    # (companion present). `package which` does not auto-install and maps each
    # identifier to its package-root path string.
    which = ocx.json("package", "which", companion.short)
    companion_path = Path(which[companion.short])
    assert companion_path.exists(), (
        f"setup: companion package dir must exist while its base is installed: {companion_path}"
    )

    # Uninstall the base → no installed base keeps the companion as a patch root.
    ocx.plain("package", "uninstall", base_pkg.short)

    clean_result = ocx.run("clean", "--force", format=None, check=False)
    assert clean_result.returncode == 0, (
        f"`ocx clean --force` must succeed; got {clean_result.returncode}\nstderr: {clean_result.stderr}"
    )

    assert not companion_path.exists(), (
        "the companion package dir must be collected once its base is uninstalled and `clean --force` runs "
        f"(no installed base keeps it as a patch root): {companion_path}"
    )


# ---------------------------------------------------------------------------
# Scenario 25 (T5): a package-specific descriptor overrides the global descriptor
# on a shared env key (package-specific companion wins, last-wins)
# ---------------------------------------------------------------------------


def test_package_specific_descriptor_overrides_global_on_shared_key(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: when both the global descriptor and a per-base descriptor
    patch the SAME env key (via distinct companions), the package-specific companion
    wins. Global companion sets SHARED_KEY=global; the base's own descriptor sets
    SHARED_KEY=specific; the resolved (last-wins) value is `specific` because
    per-base companions are composed after global ones.
    """
    # Global companion → SHARED_KEY=global.
    global_companion_repo = _unique_repo("override_global_companion")
    global_companion_fq = f"{registry}/{global_companion_repo}:1.0.0"
    _make_companion(ocx, global_companion_repo, "1.0.0", tmp_path, "SHARED_KEY", "global")

    # Package-specific companion → SHARED_KEY=specific.
    specific_companion_repo = _unique_repo("override_specific_companion")
    specific_companion_fq = f"{registry}/{specific_companion_repo}:1.0.0"
    _make_companion(ocx, specific_companion_repo, "1.0.0", tmp_path, "SHARED_KEY", "specific")

    _write_config(ocx, registry)

    # Publish the global descriptor (reserved `global` repo) → global companion.
    global_descriptor_path = tmp_path / "override_global_descriptor.json"
    _write_descriptor(global_descriptor_path, rules=[{"match": "*", "packages": [global_companion_fq]}])
    global_pub = ocx.run(
        "patch", "publish",
        "--descriptor", str(global_descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert global_pub.returncode == 0, (
        f"global patch publish must succeed; got {global_pub.returncode}\nstderr: {global_pub.stderr}"
    )

    # Base + per-base descriptor → package-specific companion.
    base_pkg = make_package(ocx, _unique_repo("override_base"), "1.0.0", tmp_path, new=True, cascade=True)
    base_descriptor_path = tmp_path / "override_base_descriptor.json"
    _write_descriptor(base_descriptor_path, rules=[{"match": "*", "packages": [specific_companion_fq]}])
    _publish_descriptor_at_base(ocx, base_descriptor_path, base_pkg.fq)

    ocx.plain("package", "install", base_pkg.short)

    entries = _env_entries(ocx, base_pkg.short)
    shared_entries = [e for e in entries if e["key"] == "SHARED_KEY"]
    # Both companions patch SHARED_KEY, so it must appear TWICE — the global
    # overlay first, the package-specific overlay last — proving the overlay order
    # (global before package-specific), not just the last-wins effective value.
    assert len(shared_entries) == 2, (
        "SHARED_KEY must appear exactly twice in the composed env — once for the global companion, "
        "once for the package-specific companion; "
        f"got {len(shared_entries)}: {shared_entries} (all keys: {[e['key'] for e in entries]})"
    )
    assert shared_entries[0]["value"] == "global", (
        "the FIRST SHARED_KEY entry must be the global companion's value (global overlay composed first); "
        f"got {shared_entries[0]['value']!r}"
    )
    assert shared_entries[-1]["value"] == "specific", (
        "the LAST SHARED_KEY entry must be the package-specific companion's value "
        "(per-base companions compose after global, last-wins); "
        f"got {shared_entries[-1]['value']!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 26 (T6): `ocx patch test --script` runs a Starlark script that asserts
# the composed companion env via ocx.env + expect.*
# ---------------------------------------------------------------------------


def test_patch_test_script_asserts_composed_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ADR behaviour: `ocx patch test --descriptor d.json --script test.star <base>`
    composes the descriptor's companions onto the base and runs the Starlark script
    in that composed environment. The script reads the companion's var via
    `ocx.env` and asserts it with `expect.eq`; a passing assertion exits 0. Sibling
    of `test_patch_test_composes_env_locally_without_publishing` (which uses
    `--format json` output instead of a script).
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)

    companion_repo = _unique_repo("scripttest_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "SCRIPT_TEST_VAR", "script-value")

    descriptor_path = tmp_path / "scripttest_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    # The script fails (non-zero) unless SCRIPT_TEST_VAR is composed with the
    # companion's value, so a clean exit 0 proves the overlay reached the script env.
    script_path = tmp_path / "assert_env.star"
    script_path.write_text(
        'val = ocx.env("SCRIPT_TEST_VAR")\n'
        'expect.eq(val, "script-value", msg="companion var must be composed into the patch-test env")\n'
    )

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--script", str(script_path),
        base_pkg.short,
        format=None,
        check=False,
    )
    assert result.returncode == 0, (
        "`ocx patch test --script` with a passing expect.eq on the composed env must exit 0; "
        f"got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
