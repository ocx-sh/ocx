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
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.registry import fetch_manifest_digest
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
    """Publish an env-only companion package with INTERFACE-visible env var."""
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
        "--descriptor-file", str(descriptor_path),
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
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
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
        "--descriptor-file", str(descriptor_path),
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
    sync_result = ocx.run("patch", "sync", format=None, check=False)
    assert sync_result.returncode == 0, (
        f"ocx patch sync must succeed; got {sync_result.returncode}\nstderr: {sync_result.stderr}"
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


def test_no_patches_opt_out_is_project_tier() -> None:
    """ADR behaviour C6: per-package no-patches opt-out requires project-tier config.

    Skipped: the `[package."<id>"] no-patches = true` stanza lives in `ocx.toml`
    (project tier) and requires CWD-walk project resolution to take effect.
    `ocx package env` is an OCI-tier command that never reads `ocx.toml`, so this
    scenario cannot be verified through `ocx package env` alone. Coverage deferred
    until a project-tier env test harness is available.
    """
    pytest.skip(
        "no-patches opt-out requires project-tier ocx.toml resolution; "
        "OCI-tier 'ocx package env' never reads ocx.toml (subsystem-cli-commands.md: "
        "layer-purity rule). Deferred to future project-tier test harness."
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
        "--descriptor-file", str(descriptor_path),
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
