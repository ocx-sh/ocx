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
  - `--show-patches` provenance bounded to the companion overlay region
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
from urllib.parse import parse_qs, urlsplit
from uuid import uuid4

import pytest

from src import static_index
from src.assertions import assert_not_exists, assert_symlink_exists
from src.helpers import (
    make_package,
    make_package_with_entrypoints,
    push_managed_config,
    resolved_metadata_path,
)
from src.registry import (
    delete_manifest,
    fetch_manifest_digest,
    fetch_platform_manifest_digest,
)
from src.runner import OcxRunner, PackageInfo, current_platform, registry_dir

# The global descriptor lives at a FIXED repository under each patch registry
# (`<patch-registry>/global:__ocx.patch`), so tests sharing one patch registry
# overwrite each other's global descriptor and observe a sibling's. They do not
# share one: `_write_config` points every test's `[patches]` tier at a registry
# path of its own, and every publish and install here resolves through that
# tier. The module therefore spreads across xdist workers. Two tests keep
# `xdist_group("patch_global_slot")`: `test_global_descriptor_publishes_at_the_bare_registry_root`
# writes the bare registry's slot on purpose, and `test_patch_publish_without_config_errors`
# has no tier to scope (a regression would publish there). The group's lint lives under
# `test/lint/`; the next line keeps the pre-move spelling for the diff guard:
# `tests/test_patch_global_slot.py` is what keeps its membership complete.
pytestmark = pytest.mark.command("patch*", "install", "env", "toolchain_env")


@pytest.fixture(scope="module")
def _empty_global_descriptor_slot_afterwards(
    ocx_binary: Path, registry: str, tmp_path_factory: pytest.TempPathFactory
):
    """Leave the bare registry's global descriptor slot empty when this module ends.

    `test_global_descriptor_publishes_at_the_bare_registry_root` publishes a
    `match: "*"` rule there and requests this. The slot outlives the session,
    so a rule left behind means every later ocx against this registry with a
    bare `[patches]` tier (a dogfooding shell, a manual rig) installs that
    test's throwaway companion. Every other test's tier is a registry path of
    its own (`_write_config`), so it is not autouse: only its requester's
    group worker, which is where it runs, writes the bare slot.
    """
    yield
    home = tmp_path_factory.mktemp("global_slot_reset")
    descriptor = home / "empty_descriptor.json"
    _write_descriptor(descriptor, rules=[])
    OcxRunner(ocx_binary, home, registry).run(
        "patch", "publish",
        "--descriptor", str(descriptor),
        "--global",
        "--registry", registry,
        format=None,
        check=False,
    )


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_config(ocx: OcxRunner, patch_registry: str, *, required: bool = True) -> Path:
    """Write $OCX_HOME/config.toml with a [patches] section.

    The tier names `<patch_registry>/p<a uuid4 kept on the runner>`, not `patch_registry`
    itself: the global descriptor is one fixed repository per patch registry, so a
    registry path per test is what keeps two tests from sharing it. The uuid is drawn
    once per runner (`vars(ocx)`), so every call in one test names one path, and anew
    every run — a hash of the home repeats when Bazel reuses the basetemp.
    """
    required_str = "true" if required else "false"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f"[patches]\n"
        f'registry = "{patch_registry}/p{vars(ocx).setdefault('patch_tier', uuid4().hex[:12])}"\n'
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
    visibility: str = "public",
) -> PackageInfo:
    """Publish an env-only companion with one env var of ``visibility``, ``public`` by default.

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
                "visibility": visibility,
            }
        ],
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


def _publish_descriptor_global(ocx: OcxRunner, descriptor_path: Path) -> None:
    """Publish descriptor to the reserved `global` repository."""
    result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert result.returncode == 0, (
        f"--global patch publish failed:\n{result.stderr}"
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base1 = make_package(ocx, base1_repo, "1.0.0", tmp_path, cascade=True)

    base2_repo = _unique_repo("global_base2")
    base2 = make_package(ocx, base2_repo, "1.0.0", tmp_path, cascade=True)

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


def test_global_companion_appears_once_when_it_matches_several_bases(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """A companion matched for several admitted bases is emitted ONCE.

    Regression guard: the projection cache deduped the compute but not the
    emission, so a `match: "*"` global rule over a multi-package
    `ocx package env a b` appended the companion's entries once per matched
    base — duplicate JSON entries, duplicate shell exports, N PATH prepends.

    Counted, not first-matched: `_entry_by_key` returns the first hit and would
    stay green against duplicates.
    """
    companion_repo = _unique_repo("dedup_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "DEDUP_CA", "one-copy")

    descriptor_path = tmp_path / "dedup_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [companion_fq], "required": True}],
    )
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert result.returncode == 0, f"--global publish failed:\n{result.stderr}"

    base1 = make_package(ocx, _unique_repo("dedup_base1"), "1.0.0", tmp_path, cascade=True)
    base2 = make_package(ocx, _unique_repo("dedup_base2"), "1.0.0", tmp_path, cascade=True)
    ocx.plain("package", "install", base1.short)
    ocx.plain("package", "install", base2.short)

    entries = ocx.json("package", "env", base1.short, base2.short)["entries"]
    dedup_count = sum(1 for e in entries if e["key"] == "DEDUP_CA")
    assert dedup_count == 1, (
        f"a companion matching both bases must contribute exactly one DEDUP_CA entry; "
        f"got {dedup_count}. entries: {entries}"
    )


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
    base_a = make_package(ocx, base_a_repo, "21.0.0", tmp_path, cascade=True)

    # ── Publish unrelated base ──
    base_b_repo = _unique_repo("cmake_base")
    base_b = make_package(ocx, base_b_repo, "3.28.0", tmp_path, cascade=True)

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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install must fail because companion is not in registry
    result = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert result.returncode != 0, (
        "Installing base with required=true missing companion must fail (fail-closed C7). "
        f"Got exit 0.\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )


def _install_then_unpin(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str, key: str, *, required: bool
) -> tuple[PackageInfo, Path]:
    """Install a base whose descriptor names one companion, then delete the companion's pin.

    Returns the base and the deleted pin's path; the companion stays installed, only unpinned.
    """
    companion_repo = _unique_repo(key.lower())
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, key, "live-resolved")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / f"{key.lower()}_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [f"{registry}/{companion_repo}:1.0.0"], "required": required}],
    )
    _write_config(ocx, registry, required=required)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)
    _companion_pin(ocx, registry, companion_repo)
    pin_path = ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{companion_repo}.json"
    pin_path.unlink()
    return base_pkg, pin_path


def _run_ocx(ocx: OcxRunner, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    """Run the binary with `args` verbatim, never raising on a non-zero exit."""
    return subprocess.run(
        [str(ocx.binary), *args], capture_output=True, text=True, env=env or ocx.env, check=False
    )


def _freeze_snapshot(ocx: OcxRunner) -> Path:
    """Run `ocx --global patch freeze` and return the snapshot path it wrote."""
    result = ocx.run("--global", "patch", "freeze", format="json", check=False)
    assert result.returncode == 0, f"patch freeze must succeed:\n{result.stderr}"
    return Path(json.loads(result.stdout)["path"])


def test_env_candidate_resolves_and_pins_an_unpinned_required_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A required companion with no pin (installed before the rule existed, or pin lost)
    resolves live at compose time and records the pin again.

    `--candidate` composes with no patch discovery at all, so only the compose-time live
    resolution can bring the companion back.
    """
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "UNPINNED_REQUIRED", required=True)

    result = _run_ocx(ocx, "--format", "json", "package", "env", "--candidate", base_pkg.short)
    assert result.returncode == 0, (
        f"env must resolve the unpinned required companion live; rc={result.returncode}\nstderr: {result.stderr}"
    )
    entry = _entry_by_key(json.loads(result.stdout)["entries"], "UNPINNED_REQUIRED")
    assert entry is not None and entry["value"] == "live-resolved", (
        f"the live-resolved companion's env var must be composed; got {entry}"
    )
    assert pin_path.exists(), "the live resolution must record the companion pin again"


def test_exec_skips_an_unpinned_optional_companion_without_resolving_it(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """An optional companion with no pin is not resolved at compose time: exec runs without
    it, and no pin appears until an install or `ocx patch sync` records one.
    """
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "UNPINNED_OPTIONAL", required=False)

    result = _run_ocx(ocx, "package", "exec", base_pkg.short, "--", "env")
    assert result.returncode == 0, f"exec must succeed without the optional companion:\n{result.stderr}"
    assert not any(line.startswith("UNPINNED_OPTIONAL=") for line in result.stdout.splitlines()), (
        f"an unpinned optional companion must not compose; env dump:\n{result.stdout}"
    )
    assert not pin_path.exists(), "compose must not resolve and pin an optional companion"


def test_snapshot_without_the_required_companion_fails_closed_without_resolving(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Under `OCX_PATCH_SNAPSHOT` the snapshot is the frozen answer: a required companion it
    does not pin fails the compose (exit 79) instead of resolving live, and writes no pin.
    """
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "SNAPSHOT_REQUIRED", required=True)
    snapshot_env = {**ocx.env, "OCX_PATCH_SNAPSHOT": str(_freeze_snapshot(ocx))}

    result = _run_ocx(ocx, "package", "env", "--candidate", base_pkg.short, env=snapshot_env)
    assert result.returncode == 79, (
        f"a required companion missing from the snapshot must fail closed with 79; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert not pin_path.exists(), "a snapshot-driven compose must not resolve and pin a companion"


def test_offline_unpinned_required_companion_fails_closed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Under `--offline` an unpinned required companion cannot resolve, so the compose fails (exit 79)."""
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "OFFLINE_REQUIRED", required=True)

    result = _run_ocx(ocx, "--offline", "package", "env", "--candidate", base_pkg.short)
    assert result.returncode == 79, (
        f"an unpinned required companion must fail closed offline with 79; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert not pin_path.exists(), "an offline compose must not pin a companion"


def test_snapshot_without_an_optional_companion_composes_without_it(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """An optional companion the snapshot does not pin is absent from the composed env."""
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "SNAPSHOT_OPTIONAL", required=False)
    snapshot_env = {**ocx.env, "OCX_PATCH_SNAPSHOT": str(_freeze_snapshot(ocx))}

    result = _run_ocx(ocx, "package", "exec", base_pkg.short, "--", "env", env=snapshot_env)
    assert result.returncode == 0, f"exec must succeed without the optional companion:\n{result.stderr}"
    assert not any(line.startswith("SNAPSHOT_OPTIONAL=") for line in result.stdout.splitlines()), (
        f"an optional companion missing from the snapshot must not compose; env dump:\n{result.stdout}"
    )
    assert not pin_path.exists(), "a snapshot-driven compose must not pin a companion"


@pytest.mark.parametrize("required", [False, True], ids=["optional", "required"])
def test_snapshot_omission_overrides_a_recorded_pin(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str, required: bool
) -> None:
    """A companion the snapshot omits stays out even though its recorded pin and install remain:
    optional → absent from the env, required → exit 79. The record is left byte-identical.
    """
    key = "OMITTED_REQUIRED" if required else "OMITTED_OPTIONAL"
    companion = _make_companion(ocx, _unique_repo(key.lower()), "1.0.0", tmp_path / "c", key, "recorded")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion.fq], "required": required}])
    _write_config(ocx, registry, required=required)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    snapshot_path = _freeze_snapshot(ocx)
    snapshot = json.loads(snapshot_path.read_text())
    assert snapshot["companions"].pop(companion.fq, None) is not None, (
        f"setup: the freeze must have pinned the companion; got {snapshot}"
    )
    snapshot_path.write_text(json.dumps(snapshot))
    pin_path = ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{companion.repo}.json"
    pin_before = pin_path.read_bytes()

    result = _run_ocx(
        ocx, "package", "exec", base_pkg.short, "--", "env", env={**ocx.env, "OCX_PATCH_SNAPSHOT": str(snapshot_path)}
    )
    if required:
        assert result.returncode == 79, (
            f"a required companion the snapshot omits must fail with 79 despite its recorded pin; "
            f"got {result.returncode}\nstderr: {result.stderr}"
        )
    else:
        assert result.returncode == 0, f"exec must succeed without the optional companion:\n{result.stderr}"
        assert not any(line.startswith(f"{key}=") for line in result.stdout.splitlines()), (
            f"an optional companion the snapshot omits must not compose from its recorded pin; env dump:\n{result.stdout}"
        )
    assert pin_path.read_bytes() == pin_before, "a snapshot-driven exec must leave the recorded pin untouched"


def test_exec_under_a_snapshot_never_resolves_an_omitted_required_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`package exec` under `OCX_PATCH_SNAPSHOT` never resolves a required companion the
    snapshot omits, in discovery or in compose: it fails closed (exit 79) and writes no pin.
    """
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "EXEC_SNAPSHOT", required=True)
    snapshot_env = {**ocx.env, "OCX_PATCH_SNAPSHOT": str(_freeze_snapshot(ocx))}

    result = _run_ocx(ocx, "package", "exec", base_pkg.short, "--", "true", env=snapshot_env)
    assert result.returncode == 79, (
        f"a required companion missing from the snapshot must fail exec with 79; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert not pin_path.exists(), "a snapshot-driven exec must not resolve and pin a companion"


def _fresh_runner_on_the_same_tier(ocx: OcxRunner, tmp_path: Path, registry: str) -> OcxRunner:
    """A second, empty `$OCX_HOME` whose `[patches]` tier names the same registry path as `ocx`'s."""
    home = tmp_path / "fresh_home"
    home.mkdir()
    fresh = OcxRunner(ocx.binary, home, registry)
    vars(fresh)["patch_tier"] = vars(ocx)["patch_tier"]
    return fresh


def _freeze_a_base_with_one_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str, key: str
) -> tuple[PackageInfo, Path]:
    """Install a base whose required per-base descriptor names one companion, then freeze.

    Returns the base and the snapshot path.
    """
    companion = _make_companion(ocx, _unique_repo(key.lower()), "1.0.0", tmp_path / "companion", key, "frozen")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion.fq]}])
    _write_config(ocx, registry, required=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)
    return base_pkg, _freeze_snapshot(ocx)


def test_a_snapshot_composes_on_a_fresh_machine_by_fetching_its_pinned_descriptor(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A snapshot frozen on one machine drives `package exec` on an empty `$OCX_HOME`: the
    descriptor it pins is fetched by its frozen digest, the companion pulled by its digest, and
    no descriptor state or companion pin is recorded.
    """
    base_pkg, snapshot = _freeze_a_base_with_one_companion(ocx, unique_repo, tmp_path, registry, "FRESH_SNAPSHOT")
    fresh = _fresh_runner_on_the_same_tier(ocx, tmp_path, registry)
    _write_config(fresh, registry, required=True)

    result = _run_ocx(
        fresh, "package", "exec", base_pkg.short, "--", "env",
        env={**fresh.env, "OCX_PATCH_SNAPSHOT": str(snapshot)},
    )
    assert result.returncode == 0, (
        f"exec on a fresh machine must fetch the snapshot's descriptor; rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert "FRESH_SNAPSHOT=frozen" in result.stdout.splitlines(), (
        f"the snapshot-pinned companion must compose on the fresh machine; env dump:\n{result.stdout}"
    )
    assert _patch_state(fresh) == {}, "a snapshot-driven exec must record no descriptor state and no companion pin"


def test_offline_a_snapshot_descriptor_missing_from_the_store_fails_closed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Offline on a machine whose store lacks the descriptor a snapshot pins, a required tier
    fails closed with not-found (exit 79), never an I/O error, and records nothing.
    """
    base_pkg, snapshot = _freeze_a_base_with_one_companion(ocx, unique_repo, tmp_path, registry, "OFFLINE_FRESH")
    fresh = _fresh_runner_on_the_same_tier(ocx, tmp_path, registry)
    # Installed before the tier exists, so no descriptor reaches this store.
    fresh.plain("package", "install", base_pkg.short)
    _write_config(fresh, registry, required=True)

    result = _run_ocx(
        fresh, "--offline", "package", "exec", base_pkg.short, "--", "true",
        env={**fresh.env, "OCX_PATCH_SNAPSHOT": str(snapshot)},
    )
    assert result.returncode == 79, (
        f"a snapshot descriptor missing offline must fail with 79; got {result.returncode}\nstderr: {result.stderr}"
    )
    assert "pinned by the patch snapshot" in result.stderr, f"the error must name the snapshot pin; stderr: {result.stderr}"
    assert _patch_state(fresh) == {}, "an offline exec must record no descriptor state and no companion pin"


def test_exec_skips_a_pinned_optional_companion_that_is_not_installed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A compose never pulls an optional companion: one that is pinned but no longer installed
    is left out of `package exec`, and stays uninstalled.
    """
    companion = _make_companion(ocx, _unique_repo("gone"), "1.0.0", tmp_path / "companion", "GONE_OPTIONAL", "pulled")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion.fq], "required": False}])
    _write_config(ocx, registry, required=False)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)
    companion_path = Path(ocx.json("package", "which", companion.short)[companion.short]["path"])
    # The pin keeps the companion rooted, so drop it for the clean and put it back after.
    pin_path = ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{companion.repo}.json"
    pin = pin_path.read_bytes()
    pin_path.unlink()
    clean = ocx.run("clean", "--force", format=None, check=False)
    assert clean.returncode == 0, f"setup: clean must succeed:\n{clean.stderr}"
    assert not companion_path.exists(), f"setup: clean must collect the unpinned companion: {companion_path}"
    pin_path.write_bytes(pin)

    result = _run_ocx(ocx, "package", "exec", base_pkg.short, "--", "env")
    assert result.returncode == 0, f"exec must succeed without the optional companion:\n{result.stderr}"
    assert not any(line.startswith("GONE_OPTIONAL=") for line in result.stdout.splitlines()), (
        f"an uninstalled optional companion must not compose; env dump:\n{result.stdout}"
    )
    assert not companion_path.exists(), "a compose must not pull an optional companion"


def test_env_no_pull_never_resolves_an_unpinned_required_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx env --no-pull` never reaches the registry: an unpinned required companion fails
    the compose with not-found (exit 79) and no pin is written.
    """
    companion_repo = _unique_repo("no_pull_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "NO_PULL_CA", "resolved-live")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "no_pull_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [f"{registry}/{companion_repo}:1.0.0"]}],
    )
    _write_config(ocx, registry, required=True)
    _publish_descriptor_global(ocx, descriptor_path)
    project = tmp_path / "no_pull_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    assert _run_in(ocx, project, "lock").returncode == 0, "setup: ocx lock must succeed"
    assert _run_in(ocx, project, "pull").returncode == 0, "setup: ocx pull must succeed"
    pin_path = ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{companion_repo}.json"
    pin_path.unlink()

    result = _run_in(ocx, project, "env", "--no-pull")
    assert result.returncode == 79, (
        f"--no-pull must fail closed on an unpinned required companion with 79; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert not pin_path.exists(), "--no-pull must not resolve and pin a companion"


def test_offline_exec_never_resolves_an_unpinned_required_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--offline package exec` fails closed (exit 79) on an unpinned required companion and writes no pin."""
    base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "EXEC_OFFLINE", required=True)

    result = _run_ocx(ocx, "--offline", "package", "exec", base_pkg.short, "--", "true")
    assert result.returncode == 79, (
        f"an unpinned required companion must fail offline exec with 79; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert not pin_path.exists(), "an offline exec must not pin a companion"


def _patch_state(ocx: OcxRunner) -> dict[str, bytes]:
    """Every recorded descriptor state and companion pin, by path under `$OCX_HOME/state`."""
    state = ocx.ocx_home / "state"
    return {
        str(path.relative_to(state)): path.read_bytes()
        for tier in ("patch-descriptors", "patch-companions")
        for path in sorted((state / tier).rglob("*"))
        if path.is_file()
    }


def test_install_under_a_snapshot_ignores_a_required_rule_added_after_the_freeze(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`package install` under `OCX_PATCH_SNAPSHOT` reads the frozen descriptor: a required rule
    published after the freeze is not seen, nothing is probed or re-recorded, and no pin is written.
    """
    frozen = _make_companion(ocx, _unique_repo("frozen"), "1.0.0", tmp_path / "a", "FROZEN_RULE", "frozen")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [frozen.fq]}])
    _write_config(ocx, registry, required=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)
    snapshot_env = {**ocx.env, "OCX_PATCH_SNAPSHOT": str(_freeze_snapshot(ocx))}

    late = _make_companion(ocx, _unique_repo("late"), "1.0.0", tmp_path / "b", "LATE_RULE", "late")
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [frozen.fq, late.fq]}])
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    before = _patch_state(ocx)
    assert before, "setup: the online install must have recorded patch state"

    result = _run_ocx(ocx, "package", "install", base_pkg.short, env=snapshot_env)
    assert result.returncode == 0, (
        f"install under a snapshot must ignore the post-freeze rule; rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert _patch_state(ocx) == before, (
        "install under a snapshot must neither re-record a descriptor nor write a companion pin"
    )


def test_lock_under_a_snapshot_ignores_a_post_freeze_rule(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx lock` under `OCX_PATCH_SNAPSHOT` reads the frozen descriptor: a required rule published
    after the freeze is not seen, nothing is probed or re-recorded, and no pin is written.
    """
    frozen = _make_companion(ocx, _unique_repo("lock_frozen"), "1.0.0", tmp_path / "a", "LOCK_FROZEN", "frozen")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [frozen.fq]}])
    _write_config(ocx, registry, required=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    project = tmp_path / "project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"setup: ocx lock must succeed:\n{lock.stderr}"
    snapshot_path = _freeze_snapshot(ocx)
    assert frozen.fq in json.loads(snapshot_path.read_text())["companions"], (
        "setup: the freeze must pin the companion the lock discovered"
    )

    late = _make_companion(ocx, _unique_repo("lock_late"), "1.0.0", tmp_path / "b", "LOCK_LATE", "late")
    _write_descriptor(
        descriptor_path, rules=[{"match": "*", "packages": [frozen.fq]}, {"match": "*", "packages": [late.fq], "required": True}]
    )
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    before = _patch_state(ocx)

    result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project,
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_PATCH_SNAPSHOT": str(snapshot_path)},
        check=False,
    )
    assert result.returncode == 0, (
        f"lock under a snapshot must ignore the post-freeze rule; rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert _patch_state(ocx) == before, (
        "lock under a snapshot must neither re-record a descriptor nor write a companion pin"
    )


def _cold_snapshot_runner(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str, key: str
) -> tuple[PackageInfo, str, Path, dict[str, str]]:
    """Freeze a base whose required companion composes `<key>=pulled`, then unpin and collect it.

    The descriptor names the rolling tag `:1`, so a later publish can move it. Returns the base,
    the companion repository, the collected install path and the snapshot env: a runner that
    has only the snapshot.
    """
    companion_repo = _unique_repo(key.lower())
    companion = _make_companion(ocx, companion_repo, "1.0.0", tmp_path / "c", key, "pulled")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [f"{registry}/{companion_repo}:1"]}])
    _write_config(ocx, registry, required=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)
    companion_path = Path(ocx.json("package", "which", companion.short)[companion.short]["path"])
    snapshot_env = {**ocx.env, "OCX_PATCH_SNAPSHOT": str(_freeze_snapshot(ocx))}

    _companion_pin(ocx, registry, companion_repo)
    (ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{companion_repo}.json").unlink()
    clean = ocx.run("clean", "--force", format=None, check=False)
    assert clean.returncode == 0, f"setup: clean must succeed:\n{clean.stderr}"
    assert not companion_path.exists(), f"setup: clean must collect the unrecorded companion: {companion_path}"
    return base_pkg, companion_repo, companion_path, snapshot_env


def test_cold_exec_under_a_snapshot_pulls_the_pinned_companion_by_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Online `package exec` under `OCX_PATCH_SNAPSHOT` pulls a snapshot-pinned companion that is
    not installed by its snapshot digest, even after its tag moved, composes it, and records no pin.
    """
    base_pkg, companion_repo, _, snapshot_env = _cold_snapshot_runner(
        ocx, unique_repo, tmp_path, registry, "COLD_SNAPSHOT"
    )
    # The tag moves after the freeze; `index=False` so only a live resolve could see it.
    make_package(
        ocx,
        companion_repo,
        "1.0.1",
        tmp_path / "moved",
        bins=[],
        env=[{"key": "COLD_SNAPSHOT", "type": "constant", "value": "moved", "visibility": "interface"}],
        cascade=True,
        platform="any",
        index=False,
    )

    result = _run_ocx(ocx, "package", "exec", base_pkg.short, "--", "env", env=snapshot_env)
    assert result.returncode == 0, (
        f"exec must pull the snapshot-pinned companion; rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert "COLD_SNAPSHOT=pulled" in result.stdout.splitlines(), (
        f"the companion must be pulled by its snapshot digest, not its moved tag; env dump:\n{result.stdout}"
    )
    pin_path = ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{companion_repo}.json"
    assert not pin_path.exists(), "a snapshot-driven exec must not record a companion pin"


def test_offline_exec_under_a_snapshot_never_pulls_a_pinned_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--offline package exec` under `OCX_PATCH_SNAPSHOT` does not pull a snapshot-pinned companion
    that is not installed: the required companion fails the run with 79 and stays uninstalled.
    """
    base_pkg, _, companion_path, snapshot_env = _cold_snapshot_runner(
        ocx, unique_repo, tmp_path, registry, "OFFLINE_COLD"
    )

    result = _run_ocx(ocx, "--offline", "package", "exec", base_pkg.short, "--", "true", env=snapshot_env)
    assert result.returncode == 79, (
        f"an offline exec must fail closed on the uninstalled pinned companion with 79; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert not companion_path.exists(), f"an offline exec must not install the companion: {companion_path}"


def test_patch_sync_under_a_snapshot_is_refused(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx patch sync` advances the pins a snapshot freezes, so under `OCX_PATCH_SNAPSHOT` it
    refuses up front with a config error (exit 78) naming the variable, and records nothing.
    """
    _base_pkg, pin_path = _install_then_unpin(ocx, unique_repo, tmp_path, registry, "SYNC_SNAPSHOT", required=True)
    snapshot_env = {**ocx.env, "OCX_PATCH_SNAPSHOT": str(_freeze_snapshot(ocx))}

    result = _run_ocx(ocx, "patch", "sync", env=snapshot_env)
    assert result.returncode == 78, (
        f"patch sync under a snapshot must be refused with 78; got {result.returncode}\nstderr: {result.stderr}"
    )
    assert "OCX_PATCH_SNAPSHOT" in result.stderr, f"the refusal must name the variable; stderr: {result.stderr}"
    assert not pin_path.exists(), "a refused sync must not record a companion pin"


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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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

    # ── The just-synced companion is a GC root ──
    #
    # GC reads the patch tier's own pin record to derive companion roots, so a
    # companion the sync pinned must survive `ocx clean`.
    clean_result = ocx.run("clean", "--force", format=None, check=False)
    assert clean_result.returncode == 0, (
        f"ocx clean --force must succeed; got {clean_result.returncode}\nstderr: {clean_result.stderr}"
    )
    ca_after_clean = _entry_by_key(_env_entries(ocx, base_pkg.short), "SYNC_CA")
    assert ca_after_clean is not None and ca_after_clean["value"] == "/certs/v2/ca.pem", (
        f"the synced companion must survive `ocx clean` — its pin is a GC root; got: {ca_after_clean}"
    )


def _companion_pin(ocx: OcxRunner, registry: str, repo: str) -> dict:
    """Return the patch-tier companion pin record (tag -> digest) for `repo`."""
    pin_path = ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{repo}.json"
    assert pin_path.exists(), (
        f"expected a companion pin record at {pin_path}; a discovered companion is pinned in "
        "patch state, never in the shared local index"
    )
    return json.loads(pin_path.read_text())


def assert_no_index_footprint(ocx: OcxRunner, registry: str, repo: str, why: str) -> None:
    """Assert `repo` owns **zero bytes** anywhere under ``$OCX_HOME/index``.

    A companion is named by a descriptor, not by the user, so nothing about it
    may enter the package tier's index — and the root document is only the
    visible half. A pinned (``tag@digest``) pull commits no tag but still
    persists the DISPATCH OBJECT into ``p/<repo>/o/<algo>/<hex>.json``, which
    is the same shared directory; asserting on the root document alone passes
    over it.
    """
    package_dir = ocx.ocx_home / "index" / registry_dir(registry) / "p" / repo
    root_document = package_dir.with_name(f"{package_dir.name}.json")
    leftovers = sorted(str(p.relative_to(ocx.ocx_home)) for p in package_dir.rglob("*") if p.is_file())
    assert not root_document.exists() and not leftovers, (
        f"{why}: {repo} must own nothing under the local index; found "
        f"{'the root document ' if root_document.exists() else ''}{leftovers}"
    )


def test_patch_sync_advances_a_same_tag_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx patch sync` re-resolves a companion tag that did not change name.

    The descriptor names a ROLLING tag (`:1`), so republishing the companion
    re-points that same tag at a new digest without touching the descriptor.
    Sync must notice: it re-resolves the companion live and advances the
    patch-tier pin. Answering from the already-recorded pin (or from the local
    index's stale package-tier pointer) leaves the base composing v1 forever,
    with no diagnostic — the silence the fix removes.

    The v2 publish deliberately skips `ocx index update`, so nothing but sync
    itself can move the binding.
    """
    companion_repo = _unique_repo("rolling_companion")
    rolling_fq = f"{registry}/{companion_repo}:1"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path / "v1", "ROLLING_CA", "/certs/v1/ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "rolling_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [rolling_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    before = _entry_by_key(_env_entries(ocx, base_pkg.short), "ROLLING_CA")
    assert before is not None, "ROLLING_CA must be composed after the initial install"
    assert before["value"] == "/certs/v1/ca.pem"
    pin_before = _companion_pin(ocx, registry, companion_repo)["1"]

    # v2 at the same rolling tag: the cascade re-points `:1` at the new digest.
    # `index=False` keeps the local package-tier index at v1, so only a live
    # re-resolve can see the move.
    make_package(
        ocx,
        companion_repo,
        "1.0.1",
        tmp_path / "v2",
        bins=[],
        env=[{"key": "ROLLING_CA", "type": "constant", "value": "/certs/v2/ca.pem", "visibility": "interface"}],
        cascade=True,
        platform="any",
        index=False,
    )

    sync_result = ocx.run("patch", "sync", format=None, check=False)
    assert sync_result.returncode == 0, (
        f"ocx patch sync must succeed; got {sync_result.returncode}\nstderr: {sync_result.stderr}"
    )

    pin_after = _companion_pin(ocx, registry, companion_repo)["1"]
    assert pin_after != pin_before, (
        f"patch sync must advance the companion pin for the rolling tag; still {pin_after}"
    )

    after = _entry_by_key(_env_entries(ocx, base_pkg.short), "ROLLING_CA")
    assert after is not None, "ROLLING_CA must still be composed after sync"
    assert after["value"] == "/certs/v2/ca.pem", (
        f"after sync the same-tag companion must compose v2; got: {after['value']}"
    )


def test_compose_does_not_advance_a_companion_between_syncs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A companion advances on sync, never as a side effect of an index refresh.

    `ocx index update <companion>` moves that repository's PACKAGE-tier pin in
    the local index — here with the `[patches]` tier switched off, so no
    patch-sync piggyback runs and the refresh is purely package-tier. The patch
    tier keeps its own pin, so compose must stay on the digest the last
    discovery recorded, and must not fail closed chasing a version nothing ever
    installed.
    """
    companion_repo = _unique_repo("determinism_companion")
    rolling_fq = f"{registry}/{companion_repo}:1"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path / "v1", "DETERMINISM_CA", "/certs/v1/ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "determinism_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [rolling_fq]}])
    config_path = _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)
    pinned = _companion_pin(ocx, registry, companion_repo)["1"]

    # v2 exists in the registry but is neither indexed nor installed.
    make_package(
        ocx,
        companion_repo,
        "1.0.1",
        tmp_path / "v2",
        bins=[],
        env=[{"key": "DETERMINISM_CA", "type": "constant", "value": "/certs/v2/ca.pem", "visibility": "interface"}],
        cascade=True,
        platform="any",
        index=False,
    )
    # Refresh the companion's package-tier pins with the patch tier switched
    # off, so `index update`'s patch-sync piggyback cannot run: the local index
    # now points `:1` at v2 while the patch pin still names v1.
    config_path.write_text("")
    ocx.plain("index", "update", companion_repo)
    _write_config(ocx, registry)

    result = ocx.run("--format", "json", "package", "env", base_pkg.short, format=None, check=False)
    assert result.returncode == 0, (
        "compose must not fail closed when the local index moved past the pinned companion; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    entry = _entry_by_key(json.loads(result.stdout)["entries"], "DETERMINISM_CA")
    assert entry is not None, "DETERMINISM_CA must still be composed"
    assert entry["value"] == "/certs/v1/ca.pem", (
        f"compose must stay on the pinned companion until the next sync; got: {entry['value']}"
    )
    assert _companion_pin(ocx, registry, companion_repo)["1"] == pinned, (
        "an index refresh must not rewrite the patch-tier companion pin"
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    result_frozen = subprocess.run(cmd, capture_output=True, text=True, env=env_frozen, check=False)
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
        env=ocx.env, check=False,
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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


# ---------------------------------------------------------------------------
# Scenario 7b: `--show-patches` provenance is bounded to the overlay region
# ---------------------------------------------------------------------------


def test_show_patches_attributes_the_overlay_and_not_the_project_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--show-patches` annotates the companion overlay region and nothing else.

    The composed entry vector is three regions in one flat list: the packages'
    own entries, then the companion overlay, then the project's declared
    stages (`[env]`, group `[env]`, `--env`). Only the middle region has
    provenance, and it is bounded on BOTH sides — an unbounded upper edge
    would attribute a project-declared entry to a companion the user never
    configured, and would index past the provenance vector to do it.

    Asserting the two keys together in ONE report is what discriminates:
    each alone passes under a mislabelling that the other catches.
    """
    companion_repo = _unique_repo("show_patches_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(
        ocx, companion_repo, "1.0.0", tmp_path, "SHOW_PATCHES_CA", "/etc/ssl/show-patches-ca.pem"
    )

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "show_patches_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install base -> companion auto-discovered and installed, local patch state recorded.
    ocx.plain("package", "install", base_pkg.short)

    project = tmp_path / "proj_show_patches"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\ntool = "{base_pkg.fq}"\n\n[env]\nSHOW_PATCHES_PROJECT = "project-value"\n'
    )
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    result = _run_in(ocx, project, "--format", "json", "env", "--show-patches")
    assert result.returncode == 0, (
        f"ocx env --show-patches must succeed; rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    entries = json.loads(result.stdout)["entries"]

    companion_entry = _entry_by_key(entries, "SHOW_PATCHES_CA")
    assert companion_entry is not None, (
        f"the companion's interface var must reach the project-tier composed env; "
        f"got keys: {[e['key'] for e in entries]}"
    )
    source = companion_entry.get("source")
    assert source is not None, (
        f"a companion overlay entry must carry provenance under --show-patches; "
        f"got: {companion_entry}"
    )
    assert source["kind"] == "patch", f"unexpected source kind; got: {source}"
    assert source["rule"] == "*", (
        f"provenance must name the descriptor rule that admitted the companion; got: {source}"
    )
    assert companion_repo in source["companion"], (
        f"provenance must name the companion identifier; got: {source}"
    )

    project_entry = _entry_by_key(entries, "SHOW_PATCHES_PROJECT")
    assert project_entry is not None, (
        f"the project's [env] declaration must reach the composed env; "
        f"got keys: {[e['key'] for e in entries]}"
    )
    assert project_entry.get("source") is None, (
        f"a project-declared entry sits past the overlay region and must render "
        f"unattributed; got: {project_entry}"
    )


# ---------------------------------------------------------------------------
# Scenario 7c: adr_package_integrations.md S-013 / C-017 — a companion
# contributes integrations exactly the way a package does
# ---------------------------------------------------------------------------


def test_patch_companion_contributes_integrations(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """S-013 / C-017: a companion's ``integrations`` reach ``ocx env``,
    attributed to the companion, exactly as a package's own do.

    Rationale (ADR C-017, reversed): patches are just packages loaded into the
    environment, so a companion gets no exceptional carrier rules. The earlier
    contract discarded them to avoid "policy injection", but that forbade the
    inert carrier while permitting the powerful one — a companion's ``env``
    already changes how every process in the shell behaves, while a
    integrations payload does nothing until a consumer reads its namespace.

    Three assertions, each load-bearing:

    * the companion's ``env`` overlay entry proves the companion mechanism
      engaged at all (not a blanket regression elsewhere);
    * the base package's OWN namespace is the positive control — it proves the
      array is populated, so the companion's row is a real contribution rather
      than an array that happens to contain everything;
    * the companion row's ``package`` names the companion, proving attribution
      is the companion's own identifier and not the base's.
    """
    companion_repo = _unique_repo("integrations_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    make_package(
        ocx, companion_repo, "1.0.0", tmp_path,
        bins=[],
        env=[
            {
                "key": "INTEGRATIONS_COMPANION_CA",
                "type": "constant",
                "value": "/etc/ssl/integrations-companion-ca.pem",
                "visibility": "public",
            }
        ],
        integrations={
            "com.example.companion": {"marker": "COMPANION_PAYLOAD"},
        },
        cascade=True,
        # Env-only, binary-free companions publish `any` (adr_platform_model_
        # unification.md D1) so they survive install regardless of host
        # platform — the same convention `_make_companion` follows.
        platform="any",
    )

    base_pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, cascade=True,
        integrations={"com.example.base": {"k": "v"}},
    )
    descriptor_path = tmp_path / "integrations_companion_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    # Install base -> companion auto-discovered and installed.
    ocx.plain("package", "install", base_pkg.short)

    result = ocx.json("package", "env", base_pkg.short)
    entries = result["entries"]
    assert any(e["key"] == "INTEGRATIONS_COMPANION_CA" for e in entries), (
        f"sanity: the companion's public env var must reach the composed "
        f"env, proving the companion mechanism actually engaged; got keys: "
        f"{[e['key'] for e in entries]}"
    )

    rows = result["integrations"]
    namespaces = {row["namespace"] for row in rows}
    assert namespaces == {"com.example.base", "com.example.companion"}, (
        f"the base package's OWN integration (positive control) and the "
        f"companion's (C-017, reversed) must both appear; got {namespaces}"
    )

    companion_row = next(row for row in rows if row["namespace"] == "com.example.companion")
    assert companion_repo in companion_row["package"], (
        f"the companion's contribution must be attributed to the COMPANION's "
        f"own identifier, not the base's; got {companion_row['package']!r}"
    )
    assert companion_row["payload"] == {"marker": "COMPANION_PAYLOAD"}, (
        f"the companion's payload must arrive intact; got {companion_row['payload']!r}"
    )

    # `--self` is the private surface; integrations are an interface-only
    # carrier, so NEITHER the base's own nor the companion's may appear. The
    # gate is one predicate applied to the whole composition — a companion
    # leaking here would mean it took a path the base did not.
    self_view = ocx.json("package", "env", base_pkg.short, "--self")
    assert self_view["integrations"] == [], (
        f"`--self` is the private surface and carries no integrations from "
        f"any contributor; got {self_view['integrations']}"
    )
    # Positive control for the empty assertion above: the companion's public
    # ENV still crosses `--self` (only its integrations carrier is gated) —
    # this is the other half of the C-017 coherence argument. Without this,
    # a future change that gated the whole companion overlay by `self_view`
    # would leave `integrations == []` green while deleting the premise
    # the design record rests on.
    self_entries = self_view["entries"]
    assert any(e["key"] == "INTEGRATIONS_COMPANION_CA" for e in self_entries), (
        f"the companion's env var must still reach `--self` even though its "
        f"integrations do not; got keys: {[e['key'] for e in self_entries]}"
    )


def test_patch_companion_integrations_appear_once_across_several_bases(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """A companion matched for several admitted bases contributes its
    integrations ONCE — the carrier sibling of
    ``test_global_companion_appears_once_when_it_matches_several_bases``.

    Counted, not membership-tested: a set of namespaces would stay green
    against duplicate rows, which is exactly the regression this guards.
    """
    companion_repo = _unique_repo("dedup_cust_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    make_package(
        ocx, companion_repo, "1.0.0", tmp_path,
        bins=[],
        env=[
            {
                "key": "DEDUP_CUST_CA",
                "type": "constant",
                "value": "/etc/ssl/dedup-cust-ca.pem",
                "visibility": "interface",
            }
        ],
        integrations={"com.example.dedup": {"marker": "once"}},
        cascade=True,
        platform="any",
    )

    descriptor_path = tmp_path / "dedup_cust_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [companion_fq], "required": True}],
    )
    _write_config(ocx, registry)

    publish = ocx.run(
        "patch", "publish", "--descriptor", str(descriptor_path), "--global",
        format=None, check=False,
    )
    assert publish.returncode == 0, f"--global publish failed:\n{publish.stderr}"

    base1 = make_package(ocx, _unique_repo("dedup_cust_base1"), "1.0.0", tmp_path, cascade=True)
    base2 = make_package(ocx, _unique_repo("dedup_cust_base2"), "1.0.0", tmp_path, cascade=True)
    ocx.plain("package", "install", base1.short)
    ocx.plain("package", "install", base2.short)

    rows = ocx.json("package", "env", base1.short, base2.short)["integrations"]
    dedup_count = sum(1 for row in rows if row["namespace"] == "com.example.dedup")
    assert dedup_count == 1, (
        f"a companion matching both bases must contribute exactly one "
        f"com.example.dedup row; got {dedup_count}. integrations: {rows}"
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
    result = _run_in(ocx, project, "exec", "--", "showenv")
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
    pkg_root = Path(which[base_pkg.short]["path"])
    # The real content digest the launcher's `install_info_from_package_root`
    # derives for this base — read verbatim from the on-disk sidecar so the test
    # proves string-form agreement instead of re-deriving it independently.
    base_digest = (pkg_root / "digest").read_text().strip()
    assert base_digest.startswith("sha256:"), f"unexpected digest sidecar content: {base_digest!r}"

    def _launcher_env_dump(*, system_required: bool) -> subprocess.CompletedProcess[str]:
        wire = json.dumps(
            {
                "registry": f"{registry}/p{vars(ocx).setdefault('patch_tier', uuid4().hex[:12])}",
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
            env=env, check=False,
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
    `execute` around the `EnvScope::Project { no_patches: ctx.config.no_patches_repositories(), .. }`
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    `EnvScope::Project { no_patches, .. }` line built from
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    ocx.plain("package", "install", base_pkg.short)

    # An UNRELATED opt-out key: a repository the base is not, so it is only
    # meaningful as leaked ambient state — never a legitimate opt-out here.
    unrelated_key = f"{registry}/unrelated-{uuid4().hex[:8]}"
    ambient_patches = json.dumps(
        {
            "registry": f"{registry}/p{vars(ocx).setdefault('patch_tier', uuid4().hex[:12])}",
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
        env=env, check=False,
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

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


def test_package_test_composes_matching_patch_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx package test` composes the tier's companions onto the bundle under test.

    The descriptor is published for the `-i` repository only, so the companion
    reaches the child env solely through per-package discovery on the locally
    built (never pushed) bundle, exactly as an install of that identifier would.
    The rule names `<registry>/<repo>:<tag>`, so it matches only when the bundle
    keeps the identifier's repository and tag.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("package_test_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PACKAGE_TEST_PATCH_VAR", "composed-value")

    descriptor_path = tmp_path / "package_test_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": base_pkg.fq, "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    pkg_dir = tmp_path / "package-test-bundle-src"
    (pkg_dir / "bin").mkdir(parents=True)
    (pkg_dir / "bin" / "hello").write_text("#!/bin/sh\necho hello\n")
    metadata_in = tmp_path / "package-test-bundle-input.json"
    metadata_in.write_text(json.dumps({"type": "bundle", "version": 1, "env": []}))
    bundle = tmp_path / "package-test-bundle.tar.xz"
    ocx.plain(
        "package", "create",
        "-m", str(metadata_in),
        "-o", str(bundle),
        "-p", current_platform(),
        str(pkg_dir),
    )

    result = ocx.plain(
        "package", "test",
        "-i", base_pkg.short,
        str(bundle),
        "--clean",
        "--",
        "sh", "-c", 'printf %s "$PACKAGE_TEST_PATCH_VAR"',
        check=False,
    )
    assert result.returncode == 0, (
        f"package test must succeed; got {result.returncode}\nstderr: {result.stderr}"
    )
    assert result.stdout == "composed-value", (
        "the companion's env var must reach the command under test; "
        f"stdout: {result.stdout!r}\nstderr: {result.stderr}"
    )


def test_package_test_composes_only_the_companion_of_a_matching_rule(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A rule naming another repository adds nothing to the bundle under test: the run
    succeeds and that companion's env var is absent. A second rule in the same descriptor
    matches the bundle, so the matched companion's presence proves discovery ran.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("package_test_unmatched_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PACKAGE_TEST_UNMATCHED_VAR", "must-not-appear")
    matched_repo = _unique_repo("package_test_matched_companion")
    _make_companion(ocx, matched_repo, "1.0.0", tmp_path, "PACKAGE_TEST_MATCHED_VAR", "matched")
    other_repo = _unique_repo("package_test_other_base")

    descriptor_path = tmp_path / "package_test_unmatched_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[
            {"match": f"{registry}/{other_repo}:1.0.0", "packages": [f"{registry}/{companion_repo}:1.0.0"]},
            {"match": base_pkg.fq, "packages": [f"{registry}/{matched_repo}:1.0.0"]},
        ],
    )
    _write_config(ocx, registry, required=True)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    pkg_dir = tmp_path / "package-test-bundle-src"
    (pkg_dir / "bin").mkdir(parents=True)
    (pkg_dir / "bin" / "hello").write_text("#!/bin/sh\necho hello\n")
    metadata_in = tmp_path / "package-test-bundle-input.json"
    metadata_in.write_text(json.dumps({"type": "bundle", "version": 1, "env": []}))
    bundle = tmp_path / "package-test-bundle.tar.xz"
    ocx.plain(
        "package", "create",
        "-m", str(metadata_in),
        "-o", str(bundle),
        "-p", current_platform(),
        str(pkg_dir),
    )

    result = ocx.plain(
        "package", "test",
        "-i", base_pkg.short,
        str(bundle),
        "--clean",
        "--",
        "sh", "-c",
        'printf "%s|%s" "${PACKAGE_TEST_MATCHED_VAR-absent}" "${PACKAGE_TEST_UNMATCHED_VAR-absent}"',
        check=False,
    )
    assert result.returncode == 0, (
        f"package test must succeed with an unmatched rule beside a matching one; got {result.returncode}\nstderr: {result.stderr}"
    )
    assert result.stdout == "matched|absent", (
        "the matching rule's companion must be composed and the other repository's rule must not "
        "contribute its companion; "
        f"stdout: {result.stdout!r}\nstderr: {result.stderr}"
    )


def _local_companion_archive(
    ocx: OcxRunner, tmp_path: Path, repo: str, tag: str, env_key: str, env_value: str
) -> Path:
    """Build — but never publish — a companion bundle for `--companion-archive`.

    `ocx package create` writes the canonicalized metadata sidecar next to the
    bundle; `patch test` reads the companion's identifier from that sibling, so
    the identifier is added there after `create` has run.
    """
    pkg_dir = tmp_path / f"local-{repo}"
    (pkg_dir / "certs").mkdir(parents=True)
    (pkg_dir / "certs" / "ca.pem").write_text("local-ca\n")
    metadata_in = tmp_path / f"local-{repo}-input.json"
    metadata_in.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {"key": env_key, "type": "constant", "value": env_value, "visibility": "interface"}
                ],
            }
        )
    )
    bundle = tmp_path / f"local-{repo}.tar.xz"
    ocx.plain(
        "package", "create",
        "-m", str(metadata_in),
        "-o", str(bundle),
        "-p", "any",
        str(pkg_dir),
    )
    sidecar = resolved_metadata_path(bundle)
    document = json.loads(sidecar.read_text())
    document["identifier"] = f"{ocx.registry}/{repo}:{tag}"
    sidecar.write_text(json.dumps(document))
    return bundle


def test_patch_test_companion_archive_still_resolves(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--companion-archive` previews a companion that was never published.

    The archive is materialized into the scratch store and its tag -> digest
    binding registered there, so the compose step resolves it with no registry
    round-trip. That registration is patch-tier state, so it has to travel with
    the scratch `FileStructure` the preview composes against.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("archive_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    archive = _local_companion_archive(
        ocx, tmp_path, companion_repo, "1.0.0", "ARCHIVE_CA", "/certs/archive/ca.pem"
    )

    descriptor_path = tmp_path / "archive_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--companion-archive", str(archive),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        "patch test must resolve an unpublished companion handed to it as an archive; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    entry = _entry_by_key(json.loads(result.stdout)["entries"], "ARCHIVE_CA")
    assert entry is not None, "the archive companion's INTERFACE var must be composed"
    assert entry["value"] == "/certs/archive/ca.pem", (
        f"ARCHIVE_CA must carry the archive companion's value; got: {entry['value']}"
    )


def test_frozen_patch_test_resolves_an_unindexed_companion_live(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--frozen` does not reach a companion resolve, in `patch test` either.

    `--frozen` scopes to the package tier; patches float by design. The two
    halves are deliberately asymmetric so the assertion can only be about the
    companion: the base IS indexed locally, so the frozen chain resolves it
    with no network; the companion is published but `index=False`, so it has
    no local tag pointer at all and only the mode-independent live view can
    find it.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("frozen_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path,
        bins=[],
        env=[{"key": "FROZEN_PATCH_VAR", "type": "constant", "value": "v", "visibility": "interface"}],
        cascade=True,
        platform="any",
        # The whole point: resolvable from the registry, absent from the local
        # index, so only a chain that still walks its sources can find it.
        index=False,
    )

    descriptor_path = tmp_path / "frozen_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    result = ocx.run(
        "--frozen",
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        "ocx --frozen patch test must resolve an unpinned, unindexed companion live; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    entry = _entry_by_key(json.loads(result.stdout)["entries"], "FROZEN_PATCH_VAR")
    assert entry is not None and entry["value"] == "v", (
        f"the live-resolved companion's INTERFACE var must compose; got: {entry}"
    )


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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
        env=ocx.env, check=False,
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


@pytest.mark.xdist_group("patch_global_slot")
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
    """ADR behaviour: `ocx patch test` with no [patches] tier and no
    `--registry` must exit 64 (UsageError), naming the three ways to supply a
    patch registry.

    The tier is resolved as step 0 of the command, before the descriptor file
    is read, so 64 here is the tier gate and nothing else. Control for
    `test_patch_test_registry_flag_composes_without_config`, which composes
    from this same tier-less state once `--registry` supplies one.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    assert result.returncode == 64, (
        "`ocx patch test` without a [patches] tier and without --registry must exit 64 "
        f"(UsageError — 'no patch registry configured'); got {result.returncode}\n"
        f"stderr: {result.stderr}"
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
    base_b = make_package(ocx, base_b_repo, "1.0.0", tmp_path, cascade=True)

    # base_other won't match rule B (but matches rule A via '*')
    base_other_repo = _unique_repo("multi_base_other")
    base_other = make_package(ocx, base_other_repo, "1.0.0", tmp_path, cascade=True)

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

    base_a = make_package(ocx, _unique_repo("par_base_a"), "1.0.0", tmp_path, cascade=True)
    base_b = make_package(ocx, _unique_repo("par_base_b"), "1.0.0", tmp_path, cascade=True)

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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
        cascade=True,
    )

    root_repo = _unique_repo("vis_sealed_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="sealed")],
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
    # Installing the dependency as a user-requested base is what runs discovery
    # against the rule that names it, so the companion arrives — and is pinned —
    # through the patch tier. A hand-installed companion package carries no
    # patch-tier pin and therefore composes nothing.
    ocx.plain("package", "install", dep.short)

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
        cascade=True,
    )

    root_repo = _unique_repo("vis_private_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="private")],
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
    # Installing the dependency as a user-requested base is what runs discovery
    # against the rule that names it, so the companion arrives — and is pinned —
    # through the patch tier. A hand-installed companion package carries no
    # patch-tier pin and therefore composes nothing.
    ocx.plain("package", "install", dep.short)

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


def test_private_companion_var_reaches_its_targets_launcher_and_self_only(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A companion composes as part of its target: its ``private`` var reaches the
    target's own entrypoint launcher and ``ocx package env --self``, and never the
    consumer view ``ocx package env``.

    The shape of a package whose entrypoint runs through a private runtime
    dependency, patched with a launcher-only option. The rule is a catch-all
    because the launcher re-derives against a synthetic base id only such a rule
    matches.
    """
    companion_repo = _unique_repo("part_of_target_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(
        ocx, companion_repo, "1.0.0", tmp_path, "JDK_JAVA_OPTIONS", "-Dpatched=1", visibility="private"
    )
    descriptor_path = tmp_path / "part_of_target_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry, required=False)
    publish = ocx.run(
        "patch", "publish", "--descriptor", str(descriptor_path), "--global",
        format=None, check=False,
    )
    assert publish.returncode == 0, f"global patch publish must succeed:\n{publish.stderr}"

    runtime = make_package(ocx, _unique_repo("part_of_target_runtime"), "1.0.0", tmp_path, cascade=True)
    base_pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        dependencies=[_dep_entry(ocx, runtime, visibility="private")],
    )
    ocx.plain("package", "install", base_pkg.short)

    self_entries = ocx.json("package", "env", "--self", base_pkg.short)["entries"]
    assert _entry_by_key(self_entries, "JDK_JAVA_OPTIONS") is not None, (
        "a private companion var must reach its target's `--self` surface; got keys: "
        f"{[e['key'] for e in self_entries]}"
    )

    launched = ocx.run("package", "exec", base_pkg.short, "--", "showenv", format=None, check=False)
    assert launched.returncode == 0, (
        f"the launcher must run; rc={launched.returncode}\nstderr: {launched.stderr}"
    )
    assert "JDK_JAVA_OPTIONS=-Dpatched=1" in launched.stdout.splitlines(), (
        "a private companion var must reach its target's own launcher; got env dump:\n"
        f"{launched.stdout}"
    )

    consumer_entries = _env_entries(ocx, base_pkg.short)
    assert _entry_by_key(consumer_entries, "JDK_JAVA_OPTIONS") is None, (
        "a private companion var must never reach the consumer view; got keys: "
        f"{[e['key'] for e in consumer_entries]}"
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
        cascade=True,
    )

    root_repo = _unique_repo("vis_public_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
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
    # Installing the dependency as a user-requested base is what runs discovery
    # against the rule that names it, so the companion arrives — and is pinned —
    # through the patch tier. A hand-installed companion package carries no
    # patch-tier pin and therefore composes nothing.
    ocx.plain("package", "install", dep.short)

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
        cascade=True,
    )

    root_repo = _unique_repo("vis_interface_root")
    root = make_package(
        ocx,
        root_repo,
        "1.0.0",
        tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="interface")],
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
    # Installing the dependency as a user-requested base is what runs discovery
    # against the rule that names it, so the companion arrives — and is pinned —
    # through the patch tier. A hand-installed companion package carries no
    # patch-tier pin and therefore composes nothing.
    ocx.plain("package", "install", dep.short)

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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
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
        env={**ocx.env, "OCX_HOME": str(original_home)}, check=False,
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
        env=relocated_env, check=False,
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

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "gc_collect_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    ocx.plain("package", "install", base_pkg.short)

    # Capture the companion package directory while the base is installed
    # (companion present). `package which` does not auto-install and maps each
    # identifier to its package-root path string.
    which = ocx.json("package", "which", companion.short)
    companion_path = Path(which[companion.short]["path"])
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
    base_pkg = make_package(ocx, _unique_repo("override_base"), "1.0.0", tmp_path, cascade=True)
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
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

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


# ---------------------------------------------------------------------------
# Provenance: why a companion repository shows up in the local index after an
# `ocx index update` that never named it
# ---------------------------------------------------------------------------


def test_index_update_does_not_pin_a_global_companion_in_the_local_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A companion is pinned in patch state, never in the shared local index.

    `ocx index update <base>` piggybacks `sync_patches`, which re-checks the
    reserved, registry-wide `<patch-registry>/global:__ocx.patch` descriptor
    for every installed base and installs newly-referenced companions. That
    companion is a repository the user never typed, so its tag -> digest
    binding must land in `state/patch-companions/`, leaving the local index's
    package-tier pins untouched (`subsystem-oci`: a pin moves only when named).

    Previously the companion install committed a tag pointer into the local
    index — which leaked this suite's throwaway companions into any
    dogfooding shell pointed at a git-tracked `OCX_INDEX`, and made a
    same-tag companion unable to ever advance (the stale pin answered first).

    The contract is zero bytes, not just no root document: a `tag@digest` pull
    writes no tag pointer but did keep persisting the dispatch object into the
    companion's own `o/` directory inside that same index.
    """
    companion_repo = _unique_repo("index_update_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "INDEX_UPDATE_PROBE", "on")

    _write_config(ocx, registry)

    descriptor_path = tmp_path / "index_update_global_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    published = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert published.returncode == 0, (
        f"global patch publish must succeed; got {published.returncode}\nstderr: {published.stderr}"
    )

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    ocx.plain("package", "install", base_pkg.short)

    # The discovery that ran with the base install recorded the companion in
    # patch state, keyed by the tag the descriptor named.
    pin = _companion_pin(ocx, registry, companion_repo)
    assert pin.get("1.0.0", "").startswith("sha256:"), (
        f"the companion pin must record tag 1.0.0 -> digest; got: {pin}"
    )

    # Drop everything the user-named `index update` inside the publish helper
    # legitimately wrote for the companion — the root document AND its dispatch
    # objects — so the assertion below can only pass if no patch-tier write
    # recreated any of it.
    index_home = ocx.ocx_home / "index" / registry_dir(registry) / "p"
    companion_root = index_home / f"{companion_repo}.json"
    assert companion_root.exists(), (
        "setup: the publish helper's own `ocx index update` must have written the companion's "
        "root document, or the deletion below removes nothing and the assertion is vacuous"
    )
    companion_root.unlink()
    shutil.rmtree(index_home / companion_repo, ignore_errors=True)

    ocx.plain("index", "update", base_pkg.short)

    assert_no_index_footprint(
        ocx,
        registry,
        companion_repo,
        f"`ocx index update {base_pkg.short}` names only the base, so the patch-sync piggyback",
    )
    assert _companion_pin(ocx, registry, companion_repo) == pin, (
        "the companion pin must survive the sync piggyback unchanged (same digest, same tag)"
    )


def test_patch_sync_pins_companions_of_a_rule_matching_an_index_name(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A rule written against an index-routed name gets its companion pinned by `patch sync`.

    The package is installed as `corp.example/<repo>/pkg`, an index name whose
    root document points at the physical registry. The sync must enumerate the
    installed base under that name (what `exec` composes under), not under
    `<physical host>/<index path>`, or no rule written against the name matches.
    """
    namespace = "corp.example"
    companion_repo = _unique_repo("index_name_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "INDEX_NAME_PROBE", "on")

    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(registry, base.repo, base.tag)
    os_name, arch_name = base.platform.split("/")
    index_repository = f"{unique_repo}/pkg"
    index_name = f"{namespace}/{index_repository}:1.0.0"

    site_root = tmp_path / "static_index_root"
    site_root.mkdir()
    with static_index.running(site_root) as server:
        static_index.write_config(server.root)
        static_index.write_package(
            server.root,
            repository=index_repository,
            tag="1.0.0",
            physical_repository=f"oci://{registry}/{base.repo}",
            platform_digest=leaf_digest,
            os=os_name,
            architecture=arch_name,
        )
        config_path = _write_config(ocx, registry)
        registry_host = registry.split(":", 1)[0]
        with config_path.open("a") as config:
            config.write(
                f'\n[registries."{namespace}"]\nindex = "{server.base_url}"\n'
                f'trusted_hosts = ["{registry_host}"]\n'
            )
        ocx.env["OCX_INSECURE_REGISTRIES"] = f"{registry},{server.host}"

        ocx.plain("index", "update", index_name)
        ocx.plain("package", "install", index_name)

        descriptor_path = tmp_path / "index_name_descriptor.json"
        _write_descriptor(
            descriptor_path,
            rules=[
                {
                    "match": f"{namespace}/{index_repository}:*",
                    "packages": [f"{registry}/{companion_repo}:1.0.0"],
                    "required": True,
                }
            ],
        )
        _publish_descriptor_global(ocx, descriptor_path)

        sync = ocx.run("patch", "sync", format=None, check=False)
        assert sync.returncode == 0, f"patch sync must succeed; got {sync.returncode}\nstderr: {sync.stderr}"

        pin = _companion_pin(ocx, registry, companion_repo)
        assert pin.get("1.0.0", "").startswith("sha256:"), (
            f"the sync must pin the companion of a rule matching the index name; got: {pin}"
        )

        executed = ocx.plain("package", "exec", index_name, "--", "env")
        assert "INDEX_NAME_PROBE=on" in executed.stdout, (
            f"exec must compose the companion env under the index name; stdout: {executed.stdout}"
        )


def test_patch_sync_restores_a_port_host_base_from_a_redirected_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`patch sync` restores an installed base's real registry host from the redirected index.

    The store slugs `localhost:5000` to `localhost_5000`; only the root document in
    the index the run is pointed at (`OCX_INDEX`) restores the port. A sync that reads
    the machine-local `$OCX_HOME/index` instead finds nothing there, keeps the slug, and
    a rule written against `localhost:5000/<repo>` matches nothing, so nothing is pinned.
    """
    companion_repo = _unique_repo("redirected_index_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "REDIRECTED_INDEX_PROBE", "on")

    redirected_index = tmp_path / "redirected_index"
    ocx.env["OCX_INDEX"] = str(redirected_index)
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True, index=False)
    _write_config(ocx, registry)
    ocx.plain("index", "update", base.short)
    ocx.plain("package", "install", base.short)

    root_document = Path(registry_dir(registry)) / "p" / f"{unique_repo}.json"
    assert (redirected_index / root_document).exists(), (
        f"setup: `index update` must have written the base's root document into {redirected_index}"
    )
    assert not (ocx.ocx_home / "index" / root_document).exists(), (
        "setup: the machine-local index must not hold the base's root document, or the "
        "redirect is not what the sync reads"
    )

    descriptor_path = tmp_path / "redirected_index_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[
            {
                "match": f"{registry}/{unique_repo}:*",
                "packages": [f"{registry}/{companion_repo}:1.0.0"],
                "required": True,
            }
        ],
    )
    _publish_descriptor_global(ocx, descriptor_path)

    sync = ocx.run("patch", "sync", format=None, check=False)
    assert sync.returncode == 0, f"patch sync must succeed; got {sync.returncode}\nstderr: {sync.stderr}"

    pin = _companion_pin(ocx, registry, companion_repo)
    assert pin.get("1.0.0", "").startswith("sha256:"), (
        f"the sync must pin the companion of a rule naming the port host; got: {pin}"
    )


# ---------------------------------------------------------------------------
# `ocx patch test --env` — per-invocation override
# ---------------------------------------------------------------------------


def _dumped_value(dump: str, key: str) -> str | None:
    """Return the value of a ``KEY=value`` line in an ``env``-dumped block."""
    prefix = f"{key}="
    for line in dump.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    return None


def _entrypoint_base_declaring_probe(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> PackageInfo:
    """Publish a base that declares an entrypoint AND its own ``LAUNCHER_PROBE``.

    The entrypoint makes the base resolve through a generated launcher; the
    declared constant is what the launcher re-applies at that hop.
    """
    return make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
            },
            {
                "key": "LAUNCHER_PROBE",
                "type": "constant",
                "value": "package-value",
            },
        ],
    )


def test_patch_test_runs_generated_entrypoint(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A trailing command that names a base's entrypoint runs.

    The composed env puts the base's ``entrypoints/`` on ``PATH``, so naming an
    entrypoint as the trailing command is the ordinary way to preview a patched
    tool. The entrypoint resolves to a generated launcher that re-enters ``ocx
    launcher exec`` with the materialized package root — which for this command
    lives in the scratch store under ``$OCX_HOME/temp/patch-test/``, not under
    ``$OCX_HOME/packages/`` and not under the ``$OCX_HOME/temp/test/`` root
    ``ocx package test`` is allowed. No ``--env`` here: this pins the launcher
    hop itself, so a failure separates from the forwarding assertion in the
    sibling test below.
    """
    base_pkg = _entrypoint_base_declaring_probe(ocx, unique_repo, tmp_path)

    descriptor_path = tmp_path / "entrypoint_descriptor.json"
    _write_descriptor(descriptor_path, rules=[])
    _write_config(ocx, registry)

    result = ocx.plain(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        "--",
        "showenv",  # the generated entrypoint launcher, not a bin/ script
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 running the generated entrypoint; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    assert _dumped_value(result.stdout, "LAUNCHER_PROBE") == "package-value", (
        f"the base's own declared value must reach the entrypoint; "
        f"stdout:\n{result.stdout}"
    )


def test_patch_test_env_flag_survives_generated_entrypoint_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A ``--env`` override reaches a tool invoked THROUGH a generated launcher.

    Sibling of
    ``test_env.py::test_package_exec_env_flag_survives_generated_entrypoint_launcher``
    — same property, the other command that composes AND spawns. A base that
    declares entrypoints resolves through its launcher, which re-enters ``ocx
    launcher exec``: a process with no project context that rebuilds its env
    from scratch and re-applies the package's own entries on top. Without the
    override being forwarded across that hop the package-declared value is
    silently restored.

    The override MUST target a key the base itself declares. A key the base
    does not declare survives the hop by plain inheritance whether or not it
    was forwarded, so a test using one passes either way and proves nothing.

    Depends on ``test_patch_test_runs_generated_entrypoint`` above: while the
    launcher hop is rejected outright, this test cannot reach the forwarding
    assertion at all. Forwarding the override is necessary here but not
    sufficient on its own.
    """
    base_pkg = _entrypoint_base_declaring_probe(ocx, unique_repo, tmp_path)

    # A zero-rule descriptor keeps the composition to base entries + the
    # override: the launcher hop is what is under test, not the overlay.
    descriptor_path = tmp_path / "launcher_probe_descriptor.json"
    _write_descriptor(descriptor_path, rules=[])
    _write_config(ocx, registry)

    result = ocx.plain(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--env", "LAUNCHER_PROBE=flag-value",
        base_pkg.short,
        "--",
        "showenv",  # the generated entrypoint launcher, not a bin/ script
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 running the generated entrypoint; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    assert _dumped_value(result.stdout, "LAUNCHER_PROBE") == "flag-value", (
        f"the override must survive the launcher re-entry — without forwarding "
        f"the launcher re-applies the base's own 'package-value' on top; "
        f"stdout:\n{result.stdout}"
    )


def test_patch_test_report_lists_env_override_without_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Report mode (no trailing command) survives ``--env`` when no companion
    contributes anything.

    ``--env`` entries compose AFTER the companion overlay, so an override sits
    at an index past the overlay region and carries no provenance. With an
    empty overlay every override index is out of range for the provenance
    vector — the report must attribute the entry to nothing, not index past
    the end of the vector.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    descriptor_path = tmp_path / "no_companion_descriptor.json"
    _write_descriptor(descriptor_path, rules=[])
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--env", "REPORT_PROBE=from-flag",
        base_pkg.short,
        format="json",
        check=False,
    )

    assert result.returncode == 0, (
        f"`ocx patch test --env` in report mode must exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    override = _entry_by_key(report["entries"], "REPORT_PROBE")
    assert override is not None, (
        f"the override must appear in the report; got: {[e['key'] for e in report['entries']]}"
    )
    assert override["value"] == "from-flag", override
    assert override.get("source") is None, (
        f"an `--env` override is nobody's companion contribution — it must be "
        f"reported unattributed; got: {override}"
    )


def test_patch_test_report_lists_env_override_alongside_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Report mode (no trailing command) survives ``--env`` when a companion
    DOES contribute entries.

    The overlay region is non-empty here, so the override lands past its end.
    Both attributions must be right in the same report: the companion entry
    names its rule + companion, the override names nothing.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("report_probe_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "COMPANION_PROBE", "companion-value")

    descriptor_path = tmp_path / "companion_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--env", "REPORT_PROBE=from-flag",
        base_pkg.short,
        format="json",
        check=False,
    )

    assert result.returncode == 0, (
        f"`ocx patch test --env` in report mode must exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    companion_entry = _entry_by_key(report["entries"], "COMPANION_PROBE")
    assert companion_entry is not None, (
        f"the companion var must appear in the report; "
        f"got: {[e['key'] for e in report['entries']]}"
    )
    assert companion_entry.get("source") is not None, (
        f"a companion overlay entry must keep its provenance; got: {companion_entry}"
    )

    override = _entry_by_key(report["entries"], "REPORT_PROBE")
    assert override is not None, (
        f"the override must appear in the report; got: {[e['key'] for e in report['entries']]}"
    )
    assert override["value"] == "from-flag", override
    assert override.get("source") is None, (
        f"an `--env` override is nobody's companion contribution — it must be "
        f"reported unattributed; got: {override}"
    )


# ---------------------------------------------------------------------------
# Regression tests for github issue #286 ("issues testing patches")
#
# The real-world session in the issue chains four distinct bugs. Each test
# below pins one:
#   - test_patch_test_with_path_prefixed_registry_composes:
#       a path-prefixed `[patches]` registry (the common corporate shape,
#       e.g. `registry.corp.example/ocx-patches`) makes the seeded local
#       descriptor unreadable back ("manifest blob not found in CAS").
#   - test_patch_test_companion_archive_composes_unpublished_companion /
#     test_patch_test_companion_archive_tag_mismatch_is_loud:
#       first-ever e2e coverage for `--companion-archive`.
#   - test_patch_test_resolves_registry_from_managed_config:
#       the exact managed-config flow the issue's session used.
#   - test_patch_test_optional_tier_with_path_prefixed_registry_composes:
#       `required = false` must not turn an unreadable seed into a silent,
#       zero-companion success.
# ---------------------------------------------------------------------------


def test_patch_test_with_path_prefixed_registry_composes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A `[patches]` registry carrying a path prefix after the host authority
    (the common corporate-registry shape, e.g.
    `registry.corp.example/ocx-patches`) must compose exactly like a bare-host
    registry.

    Same shape as `test_patch_test_composes_env_locally_without_publishing`,
    but `_write_config` is given a path-prefixed registry value. The
    descriptor is a LOCAL, unpublished file — `ocx patch test` seeds it into
    a scratch store and must read the very same bytes back to compose the
    companion overlay.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("prefixed_registry_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PREFIXED_REGISTRY_VAR", "prefixed-value")

    descriptor_path = tmp_path / "prefixed_registry_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    # The path-prefixed form: a host authority followed by `/`-separated path
    # components under it, deeper than the one-segment path `_write_config`
    # gives every other test in this module.
    _write_config(ocx, f"{registry}/extra/prefix")

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"ocx patch test with a path-prefixed [patches] registry must succeed; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    prefixed_var = _entry_by_key(entries, "PREFIXED_REGISTRY_VAR")
    assert prefixed_var is not None, (
        f"PREFIXED_REGISTRY_VAR must appear in patch test entries under a "
        f"path-prefixed [patches] registry; got: {[e['key'] for e in entries]}"
    )
    assert prefixed_var["value"] == "prefixed-value"
    assert len(report["companions"]) >= 1, "patch test report must list at least one companion"


def test_patch_test_companion_archive_composes_unpublished_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """First-ever e2e coverage for `--companion-archive`: a companion built
    and bundled locally, never pushed to the registry, materializes via
    `--companion-archive` and its INTERFACE var composes onto the base.

    The sidecar convention (`conventions::infer_metadata_file` /
    `resolved_metadata_path`) names the metadata file `<archive-stem>-metadata.json`
    next to the archive; `ocx patch test` additionally requires an
    `"identifier"` key in that sidecar (`patch_test.rs::metadata_identifier_or_error`)
    naming the companion — there is no `-i` flag on `patch test`.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    # Build the companion archive locally via `ocx package create` — never
    # pushed to the registry, proving `--companion-archive` needs no
    # registry round-trip for it.
    companion_repo = _unique_repo("archive_companion")
    companion_tag = "1.0.0"
    companion_identifier = f"{registry}/{companion_repo}:{companion_tag}"
    companion_dir = tmp_path / "archive_companion_content"
    companion_dir.mkdir()
    (companion_dir / "README.txt").write_text("archive companion fixture\n")
    companion_metadata_path = tmp_path / "archive_companion_metadata.json"
    companion_metadata_path.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {
                        "key": "ARCHIVE_COMPANION_VAR",
                        "type": "constant",
                        "value": "archive-value",
                        "visibility": "interface",
                    }
                ],
            }
        )
    )
    companion_bundle = tmp_path / "archive_companion_bundle.tar.xz"
    ocx.plain(
        "package", "create",
        "-m", str(companion_metadata_path),
        "-o", str(companion_bundle),
        "-p", "any",
        str(companion_dir),
    )
    # `create` writes the resolved sidecar next to `-o`, not back to `-m`
    # (see `resolved_metadata_path` doc). `patch test --companion-archive`
    # reads THAT sidecar and additionally requires an `"identifier"` key —
    # inject it here, matching the descriptor's companion entry exactly.
    resolved_metadata_path = companion_bundle.parent / "archive_companion_bundle-metadata.json"
    assert resolved_metadata_path.exists(), (
        f"ocx package create must write the resolved sidecar next to -o; "
        f"expected {resolved_metadata_path}, found: {sorted(p.name for p in companion_bundle.parent.iterdir())}"
    )
    resolved_metadata = json.loads(resolved_metadata_path.read_text())
    resolved_metadata["identifier"] = companion_identifier
    resolved_metadata_path.write_text(json.dumps(resolved_metadata))

    descriptor_path = tmp_path / "archive_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_identifier]}])
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--companion-archive", str(companion_bundle),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"ocx patch test --companion-archive with a matching sidecar identifier "
        f"must succeed; got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    archive_var = _entry_by_key(entries, "ARCHIVE_COMPANION_VAR")
    assert archive_var is not None, (
        f"ARCHIVE_COMPANION_VAR must appear in patch test entries after "
        f"--companion-archive materialization; got: {[e['key'] for e in entries]}"
    )
    assert archive_var["value"] == "archive-value"


def test_patch_test_companion_archive_tag_mismatch_is_loud(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A `--companion-archive` whose sidecar `"identifier"` differs from the
    descriptor's companion entry ONLY in tag must fail loudly, naming the
    supplied archive so a maintainer can see the near-miss — not a generic
    "companion not found" that never mentions what was actually supplied.

    The CLI dedup keys the archive against descriptor entries on
    registry+repo only, but compose-time resolution needs registry+repo+tag:
    an archive tagged `2.0.0` is skipped as "already materialized" for a
    descriptor entry naming `1.0.0`, so the supplied archive is silently
    discarded and never actually satisfies the required companion.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("tag_mismatch_companion")
    descriptor_tag = "1.0.0"
    archive_tag = "2.0.0"
    descriptor_identifier = f"{registry}/{companion_repo}:{descriptor_tag}"
    archive_identifier = f"{registry}/{companion_repo}:{archive_tag}"

    companion_dir = tmp_path / "mismatch_companion_content"
    companion_dir.mkdir()
    (companion_dir / "README.txt").write_text("mismatch companion fixture\n")
    companion_metadata_path = tmp_path / "mismatch_companion_metadata.json"
    companion_metadata_path.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {
                        "key": "TAG_MISMATCH_VAR",
                        "type": "constant",
                        "value": "mismatch-value",
                        "visibility": "interface",
                    }
                ],
            }
        )
    )
    companion_bundle = tmp_path / "mismatch_companion_bundle.tar.xz"
    ocx.plain(
        "package", "create",
        "-m", str(companion_metadata_path),
        "-o", str(companion_bundle),
        "-p", "any",
        str(companion_dir),
    )
    resolved_metadata_path = companion_bundle.parent / "mismatch_companion_bundle-metadata.json"
    assert resolved_metadata_path.exists(), (
        f"ocx package create must write the resolved sidecar next to -o; "
        f"expected {resolved_metadata_path}, found: {sorted(p.name for p in companion_bundle.parent.iterdir())}"
    )
    resolved_metadata = json.loads(resolved_metadata_path.read_text())
    # The archive's own identifier — deliberately a DIFFERENT tag than what
    # the descriptor below names.
    resolved_metadata["identifier"] = archive_identifier
    resolved_metadata_path.write_text(json.dumps(resolved_metadata))

    descriptor_path = tmp_path / "tag_mismatch_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [descriptor_identifier]}])
    _write_config(ocx, registry, required=True)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--companion-archive", str(companion_bundle),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode != 0, (
        f"a --companion-archive whose tag differs from the descriptor's companion "
        f"entry must not silently succeed; got exit 0, stdout: {result.stdout}"
    )
    assert archive_identifier in result.stderr, (
        f"the error must name the SUPPLIED archive identifier ({archive_identifier!r}) "
        f"so the maintainer sees the near-miss against the descriptor's expected "
        f"{descriptor_identifier!r}; got stderr:\n{result.stderr}"
    )


def test_patch_test_resolves_registry_from_managed_config(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """The exact flow from github issue #286: a fleet-managed `[patches]`
    section, adopted via `ocx config setup --managed-config <ref>`, must be
    honoured by `ocx patch test` run WITHOUT `--registry` — the command reads
    the merged config, not just `$OCX_HOME/config.toml` written directly.

    The managed payload's `[patches].registry` is path-prefixed (the real
    session's registry was `pes-dcp-oci-ocx-patch.example.corp/managed/user`),
    so this also exercises the path-prefix seed/read mismatch end to end
    through the managed-config resolution path.
    """
    managed_repo = _unique_repo("managed_patches_config")
    managed_patch_registry = f"{registry}/extra/managed_prefix"
    config_toml = f'[patches]\nregistry = "{managed_patch_registry}"\nrequired = true\n'
    ref = f"{registry}/{managed_repo}:v1"
    push_managed_config(ocx, managed_repo, "v1", config_toml, tmp_path)

    setup_result = ocx.run(
        "config", "setup",
        "--managed-config", ref,
        format="json",
        check=False,
    )
    assert setup_result.returncode == 0, (
        f"ocx config setup --managed-config must adopt the pushed payload; "
        f"got {setup_result.returncode}\nstderr: {setup_result.stderr}"
    )

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("managed_config_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "MANAGED_CONFIG_VAR", "managed-value")

    descriptor_path = tmp_path / "managed_config_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    # No _write_config call and no --registry flag — the [patches] tier must
    # come from the adopted managed config alone.

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"ocx patch test without --registry must resolve [patches] from the "
        f"adopted managed config; got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    managed_var = _entry_by_key(entries, "MANAGED_CONFIG_VAR")
    assert managed_var is not None, (
        f"MANAGED_CONFIG_VAR must appear in patch test entries when the [patches] "
        f"tier comes from managed config; got: {[e['key'] for e in entries]}"
    )
    assert managed_var["value"] == "managed-value"


def test_patch_test_optional_tier_with_path_prefixed_registry_composes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """An optional (`required = false`) patch tier on a path-prefixed
    `[patches]` registry composes the descriptor's companion, exactly like the
    required tier does.

    `required = false` is the posture that used to hide the seed/read
    registry-key mismatch: the compose step downgrades an unreadable global
    descriptor to a warning, so `ocx patch test` exited 0 with none of the
    descriptor's companions applied — indistinguishable from a genuinely empty
    descriptor. Now the key agrees and the seed is read back before compose, so
    the only honest outcome is a successful compose carrying the companion var.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("optional_prefixed_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "OPTIONAL_PREFIXED_VAR", "optional-value")

    descriptor_path = tmp_path / "optional_prefixed_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, f"{registry}/extra/optional_prefix", required=False)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format="json",
        check=False,
    )

    assert result.returncode == 0, (
        f"an optional patch tier on a path-prefixed registry must compose; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    report = json.loads(result.stdout)
    entries = report["entries"]
    optional_var = _entry_by_key(entries, "OPTIONAL_PREFIXED_VAR")
    assert optional_var is not None, (
        "the seeded descriptor names a companion, so its var must be composed — "
        "a zero-companion exit 0 is the silent failure this pins; "
        f"got entries: {[e['key'] for e in entries]}"
    )
    assert optional_var["value"] == "optional-value"


# ---------------------------------------------------------------------------
# `ocx patch test` — the published contract, one test per documented line
#
# Sources: website/src/docs/user-guide/patches.md "Test locally without
# publishing" + the bootstrap tip, and the `patch test` exit-code table in
# website/src/docs/reference/command-line.md.
#
# Every assertion names the ONE exit code the contract names. A `!= 0` band
# cannot tell "the binary rejected my input" from "the flag was never
# implemented", so it is never used here.
# ---------------------------------------------------------------------------


def test_patch_test_registry_flag_composes_without_config(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--registry` stands in for a missing `[patches]` tier (bootstrap path).

    The documented bootstrap tip: a maintainer seeding a brand-new patch
    registry has no `[patches]` config block yet, and `--registry <HOST/PATH>`
    must be enough on its own to preview a descriptor. No config file is
    written here at all.

    Discriminating against the same run without the flag:
    `test_patch_test_without_config_errors` above is the control — the very
    same tier-less state exits 64. So a green here can only come from
    `--registry` actually constructing the tier, and the pair shows both
    outcomes of the same gate.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("bootstrap_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "BOOTSTRAP_VAR", "bootstrap-value")

    descriptor_path = tmp_path / "bootstrap_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    # Deliberately no `_write_config`: the [patches] tier does not exist.

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--registry", f"{registry}/ocx-patches",
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        "`ocx patch test --registry` must compose with no [patches] config block at all; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    assert entries, "the composed report must carry entries, not an empty env"
    bootstrap_var = _entry_by_key(entries, "BOOTSTRAP_VAR")
    assert bootstrap_var is not None, (
        f"BOOTSTRAP_VAR must appear in patch test entries under the ad-hoc --registry tier; "
        f"got: {[e['key'] for e in entries]}"
    )
    assert bootstrap_var["value"] == "bootstrap-value"
    assert len(report["companions"]) >= 1, "patch test report must list the matched companion"


def test_patch_test_registry_flag_wins_over_configured_tier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--registry` retargets the tier when a `[patches]` block is ALSO present.

    What this pins is the seed/read key agreement on the override path. The
    local descriptor is seeded into the scratch CAS under a key derived from
    the effective patch registry, and compose reads it back under the key its
    own view of that tier produces. The two prefixes here differ
    (`configured_prefix` vs `override_prefix`), so an override honoured by one
    half and not the other writes and reads two different CAS directories and
    composes zero companions — the exact failure shape github issue #286 had
    for a path-prefixed configured registry, on the ad-hoc override path.

    Note what it cannot see: a `patch test` that ignored `--registry`
    *entirely* would use the configured tier consistently on both halves and
    still compose, because nothing in this command contacts the patch registry
    over the network (the descriptor is local, and `build_site_patch_set` is a
    local read). The tier's `required` posture is preserved by the override, so
    it cannot serve as a second discriminator either.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("override_registry_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "OVERRIDE_REGISTRY_VAR", "override-value")

    descriptor_path = tmp_path / "override_registry_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    # A configured tier the override must displace — nothing is ever published
    # under this prefix, and no run may key its seed by it.
    _write_config(ocx, f"{registry}/configured_prefix")

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--registry", f"{registry}/override_prefix",
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        "`ocx patch test --registry` must compose when a [patches] tier is also configured; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    assert entries, "the composed report must carry entries, not an empty env"
    override_var = _entry_by_key(entries, "OVERRIDE_REGISTRY_VAR")
    assert override_var is not None, (
        "OVERRIDE_REGISTRY_VAR must appear once --registry retargets the configured tier — "
        "a zero-companion exit 0 is the seed/read key mismatch this pins; "
        f"got: {[e['key'] for e in entries]}"
    )
    assert override_var["value"] == "override-value"
    assert len(report["companions"]) >= 1, "patch test report must list the matched companion"


def test_patch_test_platform_flag_composes_for_named_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`-p` selects the platform the base and its companion are composed for.

    Both packages are published for exactly ONE platform, chosen to be foreign
    to whatever host runs this (the fixture picks `linux/arm64` unless that IS
    the host, in which case `linux/amd64`) — the same host-agnostic trick
    `test_cross_platform_materialize.py` uses. That makes `-p` load-bearing on
    both hops: with the flag dropped, `patch test` resolves the host platform
    and neither the base pull nor the required-companion pull finds a leaf, so
    the run cannot reach a composed env at all.
    """
    # Foreign by construction, on every CI host (linux/amd64, linux/arm64,
    # darwin/*, windows/amd64).
    foreign_platform = "linux/arm64" if current_platform() != "linux/arm64" else "linux/amd64"

    base_pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, cascade=True, platform=foreign_platform
    )

    companion_repo = _unique_repo("platform_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    # Not `_make_companion`: that publishes `platform="any"`, which satisfies
    # every request and so would prove nothing about `-p` on the companion hop.
    make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path,
        bins=[],
        env=[
            {
                "key": "PLATFORM_COMPANION_VAR",
                "type": "constant",
                "value": "foreign-value",
                "visibility": "interface",
            }
        ],
        cascade=True,
        platform=foreign_platform,
    )

    descriptor_path = tmp_path / "platform_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "-p", foreign_platform,
        base_pkg.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"`ocx patch test -p {foreign_platform}` must compose for the named platform; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    assert entries, "the composed report must carry entries, not an empty env"
    companion_var = _entry_by_key(entries, "PLATFORM_COMPANION_VAR")
    assert companion_var is not None, (
        f"PLATFORM_COMPANION_VAR must appear when both packages ship only {foreign_platform}; "
        f"got: {[e['key'] for e in entries]}"
    )
    assert companion_var["value"] == "foreign-value"


def test_patch_test_invalid_descriptor_json_exits_65(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Bytes that are not JSON at all exit 65 (DataError), naming the file.

    `PatchError::InvalidDescriptorJson` classifies as `DataError`, which the
    documented `patch test` exit-code table spells "descriptor JSON is
    malformed or the version is unsupported".

    The base is published even though descriptor validation runs before the
    base is pulled: with a valid descriptor this exact fixture composes (see
    `test_patch_test_composes_env_locally_without_publishing`), so a 65 here is
    attributable to the descriptor and not to an unrelated missing package.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    descriptor_path = tmp_path / "invalid_descriptor.json"
    descriptor_path.write_text("not json {{{")
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format=None,
        check=False,
    )

    assert result.returncode == 65, (
        "a descriptor file that is not JSON must exit 65 (DataError); "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    assert str(descriptor_path) in result.stderr, (
        "the error must name the descriptor file being validated, so the exit code is "
        f"attributable to it; got stderr:\n{result.stderr}"
    )


def test_patch_test_unsupported_descriptor_version_exits_65(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A structurally valid descriptor at an unknown `version` exits 65.

    Sibling of `test_patch_test_invalid_descriptor_json_exits_65`: the bytes
    parse as JSON and carry the right shape, so only the version check can
    reject them (`PatchError::UnsupportedVersion` -> DataError). Forward
    compatibility is deliberately fail-closed — a descriptor a newer ocx
    published is refused, never silently composed as v1.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    descriptor_path = tmp_path / "future_version_descriptor.json"
    # Not `_write_descriptor`, which always writes `"version": 1`.
    descriptor_path.write_text(json.dumps({"version": 2, "rules": []}))
    _write_config(ocx, registry)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format=None,
        check=False,
    )

    assert result.returncode == 65, (
        "a descriptor declaring an unsupported version must exit 65 (DataError); "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    assert str(descriptor_path) in result.stderr, (
        "the error must name the descriptor file being validated, so the exit code is "
        f"attributable to it; got stderr:\n{result.stderr}"
    )


def test_patch_test_forwards_child_exit_code(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A trailing command's exit code is forwarded verbatim.

    The documented row: "with a trailing command, the child's exit code is
    forwarded unchanged — a command that exits 7 makes `patch test` exit 7".
    7 is chosen because no ocx exit code is 7, so a forwarded code cannot be
    confused with one ocx produced itself.

    A zero-rule descriptor keeps the run to the child hop — the same isolation
    `test_patch_test_runs_generated_entrypoint` uses, which is also the exit-0
    half of this contract (that test asserts a successful trailing command
    exits 0, this one that a failing one does not collapse to 0 or 1).
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    descriptor_path = tmp_path / "exit_code_descriptor.json"
    _write_descriptor(descriptor_path, rules=[])
    _write_config(ocx, registry)

    result = ocx.plain(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        "--",
        "sh", "-c", "exit 7",
        check=False,
    )

    assert result.returncode == 7, (
        "the trailing command's exit code must be forwarded verbatim; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_patch_test_optional_missing_companion_warns_and_skips(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """An unresolvable OPTIONAL companion is warned-and-skipped, exit 0.

    The documented fail-open half: "an optional companion that cannot be
    resolved is warned-and-skipped, matching the production fail-open path".
    The companion here is named by a repository that was never pushed, so the
    registry 404s it.

    Two assertions, because exit 0 alone cannot tell a fail-open from a run
    that quietly composed nothing: the base's own env must still be present in
    the report, and stderr must name the companion that was skipped. A silent
    skip is the failure mode — the maintainer has to be able to see which
    companion the preview did without.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    # Never published: `_unique_repo` mints a name nothing pushed to.
    missing_companion_fq = f"{registry}/{_unique_repo('unpublished_companion')}:1.0.0"

    descriptor_path = tmp_path / "optional_missing_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [missing_companion_fq]}])
    _write_config(ocx, registry, required=False)

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        base_pkg.short,
        format="json",
        check=False,
    )

    assert result.returncode == 0, (
        "an unresolvable OPTIONAL companion must fail open (exit 0), not abort the preview; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    assert entries, (
        "the base's own env must still be composed after the optional companion is skipped; "
        "an empty report is the silent-failure shape this pins"
    )
    path_entry = _entry_by_key(entries, "PATH")
    assert path_entry is not None, (
        f"the base's own PATH entry must survive the skip; got: {[e['key'] for e in entries]}"
    )
    assert missing_companion_fq in result.stderr, (
        f"stderr must name the skipped companion ({missing_companion_fq!r}) — a skip the "
        f"maintainer cannot see is indistinguishable from a descriptor that matched "
        f"nothing; got stderr:\n{result.stderr}"
    )


def test_patch_test_script_failing_assertion_exits_1(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`--script` with a failing `expect` assertion exits 1.

    Per the scripted-tests contract an assertion failure is `Failure` (1),
    distinct from a script-level fault (64/65/74). The companion IS published
    and the composition succeeds, so the only thing that can fail is the
    assertion — which is what makes 1 the right code rather than a resolution
    error's.

    Red/green pair with `test_patch_test_script_asserts_composed_env` above:
    same fixture shape, same script shape, only the expected value differs, and
    that one exits 0.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = _unique_repo("scriptfail_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "SCRIPT_FAIL_VAR", "composed-value")

    descriptor_path = tmp_path / "scriptfail_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)

    # The var IS composed, and carries "composed-value"; the script demands a
    # different one, so the assertion is the only thing that can fail.
    script_path = tmp_path / "failing_assert_env.star"
    script_path.write_text(
        'val = ocx.env("SCRIPT_FAIL_VAR")\n'
        'expect.eq(val, "not-the-composed-value", msg="deliberate mismatch: pins the '
        'assertion-failure exit code")\n'
    )

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor_path),
        "--script", str(script_path),
        base_pkg.short,
        format=None,
        check=False,
    )

    assert result.returncode == 1, (
        "`ocx patch test --script` with a failing expect.eq must exit 1 (assertion failure), "
        "not a resolution or script-level code; "
        f"got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 1c: a global descriptor at the bare registry root
# ---------------------------------------------------------------------------


@pytest.mark.xdist_group("patch_global_slot")
def test_global_descriptor_publishes_at_the_bare_registry_root(
    ocx: OcxRunner, tmp_path: Path, registry: str, _empty_global_descriptor_slot_afterwards: None
) -> None:
    """ADR behaviour: with a `[patches]` tier that is the bare registry host,
    `--global` publishes to `<host>/global:__ocx.patch` and a `*` rule there is
    applied to every installed base.

    Regression guard: this is the shape registry:2 once rejected — the global
    descriptor sat at the empty-repository root before it moved to the reserved
    single-segment `global` repository. `test_global_descriptor_applies_to_multiple_bases`
    runs under a path-scoped tier, where `global` is a nested repository, so it
    no longer covers this one.

    The one writer of the bare host's `global` slot here, so it carries the
    group and requests the module fixture that empties the slot again.
    """
    companion_repo = _unique_repo("bare_global_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "BARE_GLOBAL_CA", "bare-corp-ca")

    descriptor_path = tmp_path / "bare_global_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": "*", "packages": [companion_fq], "required": True}],
    )
    (ocx.ocx_home / "config.toml").write_text(
        f'[patches]\nregistry = "{registry}"\nrequired = true\n'
    )

    result = ocx.run(
        "patch", "publish",
        "--descriptor", str(descriptor_path),
        "--global",
        format=None,
        check=False,
    )
    assert result.returncode == 0, (
        f"--global publish at the bare registry root must succeed on registry:2.\n"
        f"stderr: {result.stderr}"
    )

    bases = [
        make_package(ocx, _unique_repo(f"bare_global_base{n}"), "1.0.0", tmp_path, cascade=True)
        for n in (1, 2)
    ]
    for base in bases:
        ocx.plain("package", "install", base.short)

    for base in bases:
        entries = _env_entries(ocx, base.short)
        entry = _entry_by_key(entries, "BARE_GLOBAL_CA")
        assert entry is not None, (
            f"BARE_GLOBAL_CA must appear in env of {base.short} (the bare host's global "
            f"descriptor applies to all);\ngot keys: {[e['key'] for e in entries]}"
        )
        assert entry["value"] == "bare-corp-ca"


# Scenario 8: tag-scoped rules for tools resolved from ocx.lock (advisory tag).
# ocx.lock stores the bare digest with no tag, so a tag-scoped rule needs the
# advisory tag rejoined from ocx.toml before it matches.


def test_tag_scoped_rule_matches_project_tool_from_lock(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a tag-scoped rule (`<registry>/<repo>:*`) matches a project tool
    declared `<registry>/<repo>:<tag>` in `ocx.toml`, once resolved through
    `ocx.lock` -- both `ocx exec -- env` and `ocx env` must carry the
    companion. The rule is published in the global descriptor; no install of the
    base precedes `ocx lock`, which must pin the companion itself.
    """
    companion_repo = _unique_repo("lock_tag_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "LOCK_TAG_CA", "/etc/ssl/lock-tag-ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "lock_tag_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "s001_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    assert _companion_pin(ocx, registry, companion_repo).get("1.0.0", "").startswith("sha256:"), (
        "ocx lock must pin the companion itself, with no install of the base before it"
    )
    pull = _run_in(ocx, project, "pull")
    assert pull.returncode == 0, f"ocx pull must succeed:\n{pull.stderr}"

    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "LOCK_TAG_CA=/etc/ssl/lock-tag-ca.pem" in exec_result.stdout, (
        "a tag-scoped rule must match the lock-resolved tool's declared tag "
        f"(advisory tag rejoin); got env dump:\n{exec_result.stdout}"
    )

    env_result = _run_in(ocx, project, "--format", "json", "env")
    assert env_result.returncode == 0, (
        f"ocx env must succeed; rc={env_result.returncode}\nstderr: {env_result.stderr}"
    )
    env_entries = json.loads(env_result.stdout)["entries"]
    assert _entry_by_key(env_entries, "LOCK_TAG_CA") is not None, (
        "the tag-scoped companion must also appear in `ocx env`'s composed "
        f"entries; got keys: {[e['key'] for e in env_entries]}"
    )


def test_tag_scoped_rule_required_missing_companion_fails_closed_from_lock(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a `required=true` tag-scoped rule whose companion is unavailable
    fails closed for a project tool resolved from `ocx.lock`, mirroring
    the OCI-tier `test_required_true_missing_companion_fails_closed`. The
    seed install runs under `required=false` so the missing companion does
    not abort the SEED; it matches but stays unpinned. The tier then flips to
    `required=true`: `ocx lock` and `ocx exec` both exit 79.
    """
    nonexistent_companion = f"{registry}/nonexistent-companion-{uuid4().hex[:8]}:latest"
    descriptor_path = tmp_path / "lock_tag_required_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [nonexistent_companion]}],
    )
    # make_package's index update records "no descriptor" once [patches] is
    # configured, so the package must exist before the config is written.
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    _write_config(ocx, registry, required=False)

    _publish_descriptor_global(ocx, descriptor_path)

    # Seed install under required=false: the rule matches but the companion
    # doesn't exist, so discovery warns and skips rather than failing the install.
    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"ocx package install must succeed:\n{install.stderr}"

    # Flip the tier to required=true (same patch registry path -- `_write_config`
    # reuses the uuid suffix already recorded on `ocx`) before the project reads it.
    _write_config(ocx, registry, required=True)

    project = tmp_path / "s002_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    companion_name = nonexistent_companion.split("/", 1)[1].split(":", 1)[0]
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 79, f"ocx lock must fail closed (79); rc={lock.returncode}\n{lock.stderr}"
    assert companion_name in lock.stderr, (
        f"ocx lock must name the missing companion {companion_name}; stderr: {lock.stderr}"
    )
    assert "required companion" in lock.stderr, (
        f"the error must say a required companion failed; stderr: {lock.stderr}"
    )

    result = _run_in(ocx, project, "exec", "--", "env")
    assert result.returncode == 79, (
        "a required tag-scoped rule whose companion is matched but unpinned "
        "must fail closed for a lock-resolved tool (C7) with the NotFound "
        f"exit the missing companion itself classifies as; got {result.returncode}.\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert companion_name in result.stderr, (
        f"ocx exec must name the missing companion {companion_name}; stderr: {result.stderr}"
    )
    assert "required companion" in result.stderr, (
        f"the error must say a required companion failed; stderr: {result.stderr}"
    )


def test_lock_alone_pins_companion_for_offline_exec(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx lock` pins the companion of its tools itself: an offline `ocx exec` right
    after it composes the companion, so no online exec has to discover it first.
    """
    companion_repo = _unique_repo("lock_only_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "LOCK_ONLY_CA", "lock-only-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "lock_only_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [f"{registry}/{companion_repo}:1.0.0"]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "lock_only_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    assert _companion_pin(ocx, registry, companion_repo), "ocx lock itself must pin the companion"

    exec_result = _run_in(ocx, project, "--offline", "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"offline ocx exec must succeed after lock; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "LOCK_ONLY_CA=lock-only-value" in exec_result.stdout.splitlines(), (
        f"ocx lock must have pinned and installed the companion; got env dump:\n{exec_result.stdout}"
    )


def test_pull_pins_companion_for_offline_exec(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx pull` pins the companion of a tool locked before any `[patches]` tier existed:
    an offline `ocx exec` right after it composes the companion.
    """
    companion_repo = _unique_repo("pull_only_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PULL_ONLY_CA", "pull-only-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    project = tmp_path / "pull_only_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    # No tier yet, so the lock pins no companion.
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    descriptor_path = tmp_path / "pull_only_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [f"{registry}/{companion_repo}:1.0.0"]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    pull = _run_in(ocx, project, "pull")
    assert pull.returncode == 0, f"ocx pull must succeed:\n{pull.stderr}"

    exec_result = _run_in(ocx, project, "--offline", "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"offline ocx exec must succeed after pull; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PULL_ONLY_CA=pull-only-value" in exec_result.stdout.splitlines(), (
        f"ocx pull must have pinned and installed the companion; got env dump:\n{exec_result.stdout}"
    )


def test_exec_discovers_required_companion_for_tool_locked_before_patch_tier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A tool locked and pulled before a `[patches]` tier existed gets the tier's
    required companion from the first online `ocx exec`, with no lock or pull in between.
    """
    companion_repo = _unique_repo("exec_late_tier_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "LATE_TIER_CA", "late-tier-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    project = tmp_path / "late_tier_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    descriptor_path = tmp_path / "late_tier_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[
            {
                "match": f"{registry}/{unique_repo}:*",
                "packages": [f"{registry}/{companion_repo}:1.0.0"],
                "required": True,
            }
        ],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "LATE_TIER_CA=late-tier-value" in exec_result.stdout.splitlines(), (
        f"exec must discover the required companion of a tier added after the lock; got env dump:\n{exec_result.stdout}"
    )


def test_install_rechecks_a_cached_no_descriptor_state(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ocx-sh/ocx#551: `index update` records "looked, no descriptor" for the
    global descriptor. A descriptor published afterwards must take effect on the
    next `package install`, not wait for someone to delete the state: the required
    rule's companion is missing, so install and exec both fail closed.
    """
    nonexistent_companion = f"{registry}/nonexistent-companion-{uuid4().hex[:8]}:latest"
    descriptor_path = tmp_path / "late_required_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [nonexistent_companion], "required": True}],
    )
    _write_config(ocx, registry, required=False)
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    index = ocx.plain("index", "update", base_pkg.short)
    assert index.returncode == 0, f"ocx index update must succeed:\n{index.stderr}"

    # The precondition the recheck exists for: the file is present and the descriptor key absent.
    tier_repo_dir = ocx.ocx_home / "state" / "patch-descriptors" / registry_dir(registry) / f"p{vars(ocx)['patch_tier']}"
    global_state = tier_repo_dir / "global.json"
    assert global_state.exists(), (
        f"setup: `index update` must record the global descriptor as looked at {global_state}"
    )
    assert "__ocx.patch" not in json.loads(global_state.read_text()), (
        f"setup: the recorded state must be `no descriptor`; got: {global_state.read_text()}"
    )

    _publish_descriptor_global(ocx, descriptor_path)

    companion_name = nonexistent_companion.split("/", 1)[1].split(":", 1)[0]
    install = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert install.returncode == 79, (
        "a descriptor published after the last check must be picked up by install and fail "
        f"closed on its missing required companion; got {install.returncode}.\nstderr: {install.stderr}"
    )
    assert companion_name in install.stderr, (
        f"install must name the missing companion {companion_name}; stderr: {install.stderr}"
    )
    assert "required companion" in install.stderr, (
        f"the error must say a required companion failed; stderr: {install.stderr}"
    )
    result = ocx.run("package", "exec", base_pkg.short, "--", "true", format=None, check=False)
    assert result.returncode == 79, (
        f"exec must fail closed on the required rule; got {result.returncode}.\nstderr: {result.stderr}"
    )
    assert companion_name in result.stderr, (
        f"exec must name the missing companion {companion_name}; stderr: {result.stderr}"
    )
    assert "required companion" in result.stderr, (
        f"the error must say a required companion failed; stderr: {result.stderr}"
    )


def test_install_rechecks_a_cached_descriptor_republished_with_a_new_rule(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """ocx-sh/ocx#551: a found descriptor that is re-published with a further rule is
    picked up by the next `package install`, which refetches it on the digest drift.
    """
    first_repo = _unique_repo("recheck_first")
    _make_companion(ocx, first_repo, "1.0.0", tmp_path, "RECHECK_FIRST", "first")
    second_repo = _unique_repo("recheck_second")
    _make_companion(ocx, second_repo, "1.0.0", tmp_path, "RECHECK_SECOND", "second")
    match = f"{registry}/{unique_repo}:*"
    first_rule = {"match": match, "packages": [f"{registry}/{first_repo}:1.0.0"]}
    second_rule = {"match": match, "packages": [f"{registry}/{second_repo}:1.0.0"]}

    _write_config(ocx, registry, required=False)
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    descriptor_path = tmp_path / "recheck_descriptor.json"
    _write_descriptor(descriptor_path, rules=[first_rule])
    _publish_descriptor_global(ocx, descriptor_path)
    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"first install must succeed:\n{install.stderr}"
    assert _entry_by_key(_env_entries(ocx, base_pkg.short), "RECHECK_FIRST") is not None

    _write_descriptor(descriptor_path, rules=[first_rule, second_rule])
    _publish_descriptor_global(ocx, descriptor_path)
    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"second install must succeed:\n{install.stderr}"
    entries = _env_entries(ocx, base_pkg.short)
    assert _entry_by_key(entries, "RECHECK_SECOND") is not None, (
        f"the re-published rule must apply after the next install; got keys: {[e['key'] for e in entries]}"
    )


def test_install_fails_closed_on_a_required_descriptor_deleted_from_the_registry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Under a required tier a descriptor that vanishes from the registry after install fails the
    next `package install` (exit 79) and keeps the recorded state, so compose still carries the
    companion the last good descriptor named.
    """
    companion_repo = _unique_repo("vanish_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "VANISH_COMPANION", "kept")
    _write_config(ocx, registry, required=True)
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "vanish_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [f"{registry}/{companion_repo}:1.0.0"]}])
    _publish_descriptor_global(ocx, descriptor_path)
    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"first install must succeed:\n{install.stderr}"

    global_repo = f"p{vars(ocx)['patch_tier']}/global"
    delete_manifest(registry, global_repo, fetch_manifest_digest(registry, global_repo, "__ocx.patch"))
    before = _patch_state(ocx)

    install = ocx.run("package", "install", base_pkg.short, format=None, check=False)
    assert install.returncode == 79, (
        f"a required descriptor gone from the registry must fail install with 79; got {install.returncode}\n"
        f"stderr: {install.stderr}"
    )
    assert _patch_state(ocx) == before, "the failed install must leave every descriptor state and pin byte-identical"
    entry = _entry_by_key(_env_entries(ocx, base_pkg.short), "VANISH_COMPANION")
    assert entry is not None and entry["value"] == "kept", (
        f"the failed install must leave the recorded descriptor in effect; got {entry}"
    )


def test_patch_sync_records_a_vanished_required_descriptor_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """Under a required tier `ocx patch sync` is the command that records a vanished descriptor as
    absent: afterwards the global descriptor state no longer names `__ocx.patch`.
    """
    companion_repo = _unique_repo("sync_vanish_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "SYNC_VANISH", "kept")
    _write_config(ocx, registry, required=True)
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "sync_vanish_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [f"{registry}/{companion_repo}:1.0.0"]}])
    _publish_descriptor_global(ocx, descriptor_path)
    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"first install must succeed:\n{install.stderr}"
    global_state = (
        ocx.ocx_home / "state" / "patch-descriptors" / registry_dir(registry) / f"p{vars(ocx)['patch_tier']}" / "global.json"
    )
    assert "__ocx.patch" in json.loads(global_state.read_text()), (
        f"setup: the install must record the global descriptor; got: {global_state.read_text()}"
    )

    global_repo = f"p{vars(ocx)['patch_tier']}/global"
    delete_manifest(registry, global_repo, fetch_manifest_digest(registry, global_repo, "__ocx.patch"))

    sync = ocx.run("patch", "sync", format=None, check=False)
    assert sync.returncode == 0, f"patch sync must succeed:\n{sync.stderr}"
    assert "__ocx.patch" not in json.loads(global_state.read_text()), (
        f"patch sync must record the vanished descriptor as absent; got: {global_state.read_text()}"
    )


def test_package_pull_pins_a_matching_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx package pull` runs patch discovery, so a companion the descriptor names is pinned
    and installed: an offline env right after it composes the companion.
    """
    companion_repo = _unique_repo("pull_companion")
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PULL_COMPANION", "pulled")
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "pull_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [f"{registry}/{companion_repo}:1.0.0"]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    pull = ocx.plain("package", "pull", base_pkg.short)
    assert pull.returncode == 0, f"package pull must succeed:\n{pull.stderr}"
    env = _run_ocx(ocx, "--offline", "--format", "json", "package", "env", base_pkg.short)
    assert env.returncode == 0, f"offline package env must succeed after the pull; rc={env.returncode}\n{env.stderr}"
    entry = _entry_by_key(json.loads(env.stdout)["entries"], "PULL_COMPANION")
    assert entry is not None and entry["value"] == "pulled", (
        f"package pull must have installed the companion for an offline compose; got {entry}"
    )
    assert _companion_pin(ocx, registry, companion_repo), "package pull must pin the matching companion"


def test_package_pull_revalidates_a_republished_descriptor(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx package pull` re-checks a descriptor it already recorded: a rule published between
    two pulls has its companion pinned by the second.
    """
    first_repo = _unique_repo("pull_first")
    _make_companion(ocx, first_repo, "1.0.0", tmp_path, "PULL_FIRST", "first")
    second_repo = _unique_repo("pull_second")
    _make_companion(ocx, second_repo, "1.0.0", tmp_path, "PULL_SECOND", "second")
    first_rule = {"match": "*", "packages": [f"{registry}/{first_repo}:1.0.0"]}
    second_rule = {"match": "*", "packages": [f"{registry}/{second_repo}:1.0.0"]}
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "pull_revalidate_descriptor.json"
    _write_descriptor(descriptor_path, rules=[first_rule])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    pull = ocx.plain("package", "pull", base_pkg.short)
    assert pull.returncode == 0, f"first package pull must succeed:\n{pull.stderr}"
    assert _companion_pin(ocx, registry, first_repo), "setup: the first pull must pin the first companion"

    _write_descriptor(descriptor_path, rules=[first_rule, second_rule])
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)
    pull = ocx.plain("package", "pull", base_pkg.short)
    assert pull.returncode == 0, f"second package pull must succeed:\n{pull.stderr}"
    assert _companion_pin(ocx, registry, second_repo), "the second pull must pin the republished rule's companion"


def test_lock_and_exec_install_per_package_companion_for_digest_only_declaration(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a per-package descriptor's companion reaches a digest-only `ocx.toml` tool
    through `ocx lock` + `ocx exec` alone: project commands run patch discovery, so
    no `ocx package install` of the base is needed first.
    """
    companion_repo = _unique_repo("proj_digest_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_DIGEST_CA", "proj-digest-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    digest = fetch_platform_manifest_digest(ocx.registry, base_pkg.repo, base_pkg.tag)
    descriptor_path = tmp_path / "proj_digest_descriptor.json"
    _write_descriptor(descriptor_path, rules=[{"match": "*", "packages": [companion_fq]}])
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    project = tmp_path / "p550a_project"
    project.mkdir()
    _write_project_toml(project, f"{registry}/{unique_repo}@{digest}", opt_out=False)

    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_DIGEST_CA=proj-digest-value" in exec_result.stdout


def test_lock_and_exec_install_per_package_companion_for_tagged_declaration(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """as above for a tagged declaration, with a tag-scoped rule in the per-package descriptor."""
    companion_repo = _unique_repo("proj_tagged_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_TAGGED_CA", "proj-tagged-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "proj_tagged_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_at_base(ocx, descriptor_path, base_pkg.fq)

    project = tmp_path / "p550b_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)

    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_TAGGED_CA=proj-tagged-value" in exec_result.stdout


def test_lock_and_exec_install_global_rule_companion_for_digest_only_declaration(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a global descriptor's `<registry>/<repo>*` rule installs its companion for a
    digest-only `ocx.toml` tool through `ocx lock` + `ocx exec` alone.
    """
    companion_repo = _unique_repo("proj_global_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_GLOBAL_CA", "proj-global-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    digest = fetch_platform_manifest_digest(ocx.registry, base_pkg.repo, base_pkg.tag)
    descriptor_path = tmp_path / "proj_global_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "p550c_project"
    project.mkdir()
    _write_project_toml(project, f"{registry}/{unique_repo}@{digest}", opt_out=False)

    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_GLOBAL_CA=proj-global-value" in exec_result.stdout


def test_clean_keeps_companion_of_lock_only_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx clean` keeps the companion of a tool that only `ocx.toml` + `ocx.lock` hold:
    the project lock is a base for the patch GC roots, so a later offline `ocx exec`
    still composes the companion.
    """
    companion_repo = _unique_repo("proj_clean_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_CLEAN_CA", "proj-clean-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "proj_clean_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "p550d_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_CLEAN_CA=proj-clean-value" in exec_result.stdout

    clean = _run_in(ocx, project, "clean")
    assert clean.returncode == 0, f"ocx clean must succeed:\n{clean.stderr}"

    # Offline, or `exec` would re-install a collected companion and mask the collection.
    exec_result = _run_in(ocx, project, "--offline", "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"offline ocx exec must succeed after clean; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_CLEAN_CA=proj-clean-value" in exec_result.stdout, (
        f"ocx clean must keep the companion of a lock-only tool; got env dump:\n{exec_result.stdout}"
    )


def test_clean_keeps_companion_of_digest_glob_rule_for_lock_only_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx clean` roots a lock-only tool's companion under the digest-bearing identity
    `ocx lock` discovered it with, so a required `<repo>@*` rule's companion survives
    and a later offline `ocx exec` composes it instead of exiting 79.
    """
    companion_repo = _unique_repo("proj_clean_digest_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_DIGEST_GLOB_CA", "proj-digest-glob-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "proj_clean_digest_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}@*", "packages": [companion_fq], "required": True}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "p550e_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    assert _companion_pin(ocx, registry, companion_repo), "ocx lock must pin the digest-rule companion"

    clean = _run_in(ocx, project, "clean")
    assert clean.returncode == 0, f"ocx clean must succeed:\n{clean.stderr}"

    # Offline, or `exec` would re-install a collected companion and mask the collection.
    exec_result = _run_in(ocx, project, "--offline", "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"offline ocx exec must succeed after clean; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_DIGEST_GLOB_CA=proj-digest-glob-value" in exec_result.stdout, (
        f"ocx clean must keep a digest-rule companion of a lock-only tool; got env dump:\n{exec_result.stdout}"
    )


def test_patch_sync_pins_digest_glob_rule_companion_for_lock_only_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx patch sync` discovers a lock-only tool's companions under the digest-bearing
    identity `ocx lock` uses, so a required `<repo>@*` rule published after the lock is
    pinned and installed, and a later offline `ocx exec` composes it.
    """
    companion_repo = _unique_repo("proj_sync_digest_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_SYNC_DIGEST_CA", "proj-sync-digest-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    project = tmp_path / "p550g_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    # Locked before any patch tier exists, so only `patch sync` can pin the companion.
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    descriptor_path = tmp_path / "proj_sync_digest_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}@*", "packages": [companion_fq], "required": True}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    sync = _run_in(ocx, project, "patch", "sync")
    assert sync.returncode == 0, f"ocx patch sync must succeed:\n{sync.stderr}"
    assert _companion_pin(ocx, registry, companion_repo), "patch sync must pin the digest-rule companion"

    exec_result = _run_in(ocx, project, "--offline", "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"offline ocx exec must succeed after patch sync; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "PROJ_SYNC_DIGEST_CA=proj-sync-digest-value" in exec_result.stdout, (
        f"patch sync must install a digest-rule companion of a lock-only tool; got env dump:\n{exec_result.stdout}"
    )


def test_clean_force_drops_project_lock_companion_roots(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx clean --force` ignores project locks, so the companion only a lock-only tool
    holds loses its root and is collected with it.
    """
    companion_repo = _unique_repo("proj_force_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "PROJ_FORCE_CA", "proj-force-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "proj_force_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "p550f_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    algorithm, _, hex_digest = fetch_platform_manifest_digest(ocx.registry, companion_repo, "1.0.0").partition(":")
    companion_dir = (
        ocx.ocx_home / "packages" / registry_dir(registry) / algorithm / hex_digest[:2] / hex_digest[2:32]
    )
    assert companion_dir.is_dir(), f"precondition: ocx lock must install the companion at {companion_dir}"

    clean = _run_in(ocx, project, "clean")
    assert clean.returncode == 0, f"ocx clean must succeed:\n{clean.stderr}"
    assert companion_dir.is_dir(), "precondition: plain ocx clean keeps the project-lock companion"

    clean = _run_in(ocx, project, "clean", "--force")
    assert clean.returncode == 0, f"ocx clean --force must succeed:\n{clean.stderr}"
    assert not companion_dir.exists(), (
        f"ocx clean --force must collect a companion only a project lock holds: {companion_dir}"
    )


def test_digest_only_declaration_tag_anchor_skips_repo_wildcard_matches(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a digest-only declaration (`<registry>/<repo>@sha256:<digest>`) resolved
    through `ocx.lock` is untagged, so a tag-anchored rule (`<repo>:*`) skips it
    while a bare repository rule (`<repo>`) matches every tag and digest of it.
    A TAGGED install pins both companions, so the tag-anchored one is absent only
    because the matcher skips the untagged declaration; both rules publish to the
    global descriptor (ocx-sh/ocx#550).
    """
    tag_companion_repo = _unique_repo("digest_tag_companion")
    tag_companion_fq = f"{registry}/{tag_companion_repo}:1.0.0"
    _make_companion(ocx, tag_companion_repo, "1.0.0", tmp_path, "DIGEST_TAG_CA", "tag-anchor-value")

    repo_companion_repo = _unique_repo("digest_repo_companion")
    repo_companion_fq = f"{registry}/{repo_companion_repo}:1.0.0"
    _make_companion(ocx, repo_companion_repo, "1.0.0", tmp_path, "DIGEST_REPO_CA", "repo-prefix-value")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    digest = fetch_platform_manifest_digest(ocx.registry, base_pkg.repo, base_pkg.tag)

    descriptor_path = tmp_path / "digest_only_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[
            {"match": f"{registry}/{unique_repo}:*", "packages": [tag_companion_fq]},
            {"match": f"{registry}/{unique_repo}", "packages": [repo_companion_fq]},
        ],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    digest_only_ref = f"{registry}/{unique_repo}@{digest}"

    # The tagged install pins both companions in patch state.
    for ref in (base_pkg.short, digest_only_ref):
        install = ocx.plain("package", "install", ref)
        assert install.returncode == 0, f"ocx package install {ref} must succeed:\n{install.stderr}"

    project = tmp_path / "s003_project"
    project.mkdir()
    _write_project_toml(project, digest_only_ref, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "DIGEST_TAG_CA=tag-anchor-value" not in exec_result.stdout, (
        "a tag-anchored rule must NOT match a digest-only (untagged) "
        f"declaration resolved from ocx.lock; got env dump:\n{exec_result.stdout}"
    )
    assert "DIGEST_REPO_CA=repo-prefix-value" in exec_result.stdout, (
        "a bare repository rule must match a "
        f"digest-only declaration resolved from ocx.lock; got env dump:\n{exec_result.stdout}"
    )

    env_result = _run_in(ocx, project, "--format", "json", "env")
    assert env_result.returncode == 0, (
        f"ocx env must succeed; rc={env_result.returncode}\nstderr: {env_result.stderr}"
    )
    env_entries = json.loads(env_result.stdout)["entries"]
    assert _entry_by_key(env_entries, "DIGEST_TAG_CA") is None, (
        "the tag-anchored rule's companion must also be absent from `ocx env`'s "
        f"composed entries; got keys: {[e['key'] for e in env_entries]}"
    )
    assert _entry_by_key(env_entries, "DIGEST_REPO_CA") is not None, (
        "the bare repository rule's companion must also appear in `ocx env`'s "
        f"composed entries; got keys: {[e['key'] for e in env_entries]}"
    )


def test_bare_repo_rule_matches_a_tagged_declaration(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a tagged declaration (`<repo>:<tag>`) resolved through `ocx.lock` carries
    the tag and the resolved digest, so a bare repository rule (`<repo>`) matches
    it while a `:latest` rule does not.
    """
    companions = {}
    for label, var in (("bare", "TAGGED_BARE_CA"), ("latest", "TAGGED_LATEST_CA")):
        repo = _unique_repo(f"tagged_{label}_companion")
        _make_companion(ocx, repo, "1.0.0", tmp_path, var, f"{label}-value")
        companions[label] = f"{registry}/{repo}:1.0.0"

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    descriptor_path = tmp_path / "tagged_declaration_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[
            {"match": f"{registry}/{unique_repo}", "packages": [companions["bare"]]},
            {"match": f"{registry}/{unique_repo}:latest", "packages": [companions["latest"]]},
        ],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    project = tmp_path / "tagged_declaration_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    exec_result = _run_in(ocx, project, "exec", "--", "env")
    assert exec_result.returncode == 0, (
        f"ocx exec -- env must succeed; rc={exec_result.returncode}\nstderr: {exec_result.stderr}"
    )
    assert "TAGGED_BARE_CA=bare-value" in exec_result.stdout, (
        f"a bare repository rule must match a tagged declaration; got env dump:\n{exec_result.stdout}"
    )
    assert "TAGGED_LATEST_CA" not in exec_result.stdout, (
        f"a `<repo>:latest` rule must not match a `:1.0.0` declaration; got env dump:\n{exec_result.stdout}"
    )


def test_fresh_home_pull_writes_no_tag_entry_for_base_repo(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """the base package and lock are prepared in the `ocx` fixture's own
    home, but `ocx pull` runs from a SECOND, genuinely fresh `$OCX_HOME`
    (pattern: `test_index_ocx_sh.py`'s `clean_home` runners). In the clean
    home, `ocx pull` succeeds and the base repository's index carries no
    `1.0.0` tag entry, while the seeding home's root proves the path read --
    a digest-pinned lock resolve grows no tag pointer (`chained_index.rs`
    only grows the tag store for a tag with no digest). A genuinely fresh `$OCX_HOME` has never run `ocx package
    install`, so no companion has ever been pinned there and asserting on
    one would be vacuous -- this test carries no companion assertion.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    project = tmp_path / "c006_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    clean_home = tmp_path / "c006_clean_home"
    clean_home.mkdir()
    clean_home_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)

    pull = _run_in(clean_home_runner, project, "pull")
    assert pull.returncode == 0, f"ocx pull must succeed:\n{pull.stderr}"

    def candidate(home: Path) -> Path:
        return home / "symlinks" / registry_dir(registry) / unique_repo / "candidates" / "1.0.0"

    assert_not_exists(candidate(clean_home_runner.ocx_home))
    # Path control: an install in the seeding home creates the candidate at this exact path.
    ocx.plain("package", "install", base_pkg.short)
    assert_symlink_exists(candidate(ocx.ocx_home))

    def index_tags(home: Path) -> dict[str, object] | None:
        root = home / "index" / registry_dir(registry) / "p" / f"{unique_repo}.json"
        return json.loads(root.read_text(encoding="utf-8")).get("tags", {}) if root.is_file() else None

    # Path and shape control: the seeding home's tag resolve wrote `1.0.0` at this exact path.
    seeded_tags = index_tags(ocx.ocx_home)
    assert seeded_tags is not None and "1.0.0" in seeded_tags, f"seeding home tags: {seeded_tags}"
    # The clean home may write no root at all; absent reads as no tag entry.
    clean_tags = index_tags(clean_home_runner.ocx_home) or {}
    assert "1.0.0" not in clean_tags, (
        "a lock-resolved (digest-pinned) pull must never commit a tag "
        f"pointer for the base repository; got tags: {sorted(clean_tags)}"
    )


def test_tag_scoped_rule_matches_global_toolchain_tool_from_lock(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """a tag-scoped rule matches a global-toolchain tool
    (`ocx --global add <registry>/<repo>:<tag>`) resolved from the global
    lock -- `ocx --global env` must carry the companion. The rule is
    published in the global descriptor because project commands don't fetch
    a per-package descriptor yet (ocx-sh/ocx#550). The companion reaches the
    global toolchain only because a prior `ocx package install` of the
    TAGGED ref ran discovery and pinned it -- `ocx --global add`/`env` never
    run discovery themselves (ocx-sh/ocx#550).
    """
    companion_repo = _unique_repo("global_lock_tag_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(
        ocx, companion_repo, "1.0.0", tmp_path, "GLOBAL_LOCK_TAG_CA", "/etc/ssl/global-lock-tag-ca.pem"
    )

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "global_lock_tag_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    # Install the TAGGED ref -> discovery matches the tag-scoped rule and pins
    # the companion in patch state before `--global add` ever runs.
    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"ocx package install must succeed:\n{install.stderr}"

    add = ocx.run("--global", "add", base_pkg.fq, format=None, check=False)
    assert add.returncode == 0, (
        f"ocx --global add must succeed; rc={add.returncode}\nstderr: {add.stderr}"
    )

    result = ocx.run("--global", "env", format="json", check=False)
    assert result.returncode == 0, (
        f"ocx --global env must succeed; rc={result.returncode}\nstderr: {result.stderr}"
    )
    entries = json.loads(result.stdout)["entries"]
    assert _entry_by_key(entries, "GLOBAL_LOCK_TAG_CA") is not None, (
        "a tag-scoped rule must match a global-toolchain tool resolved from "
        f"the global lock; got keys: {[e['key'] for e in entries]}"
    )


def test_stale_project_lock_tag_edit_exec_exits_65(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """editing a locked project tool's TAG in `ocx.toml` (to a
    second, already-published tag) mismatches the lock's declaration_hash
    exactly like any other declaration edit -- `ocx exec` exits 65. Existing
    staleness behaviour, untouched by the lock advisory-tag fix.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=True)

    project = tmp_path / "s006a_project"
    project.mkdir()
    tag1_fq = f"{registry}/{unique_repo}:1.0.0"
    tag2_fq = f"{registry}/{unique_repo}:2.0.0"
    _write_project_toml(project, tag1_fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    # A valid declaration (the tag is published), but one that mismatches
    # the recorded declaration_hash.
    _write_project_toml(project, tag2_fq, opt_out=False)

    result = _run_in(ocx, project, "exec", "--", "env")
    assert result.returncode == 65, (
        f"ocx exec must exit 65 on a stale lock after a tag edit; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_stale_project_lock_tag_edit_direnv_export_no_tag_scoped_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """the same stale-tag-edit lock as
    `test_stale_project_lock_tag_edit_exec_exits_65`, read through the LENIENT `ocx
    direnv export` path -- exits 0, and its output does NOT carry the
    tag-scoped companion var (a stale lock falls back to a tagless digest,
    which no tag-anchored rule matches). Existing staleness behaviour,
    untouched by the lock advisory-tag fix. The rule publishes to the
    global descriptor (ocx-sh/ocx#550); the companion is pinned by a prior
    `ocx package install` of the TAGGED ref, so the absence below is the
    staleness fallback suppressing a real, reachable companion.
    """
    companion_repo = _unique_repo("s006b_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "S006B_CA", "/etc/ssl/s006b-ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "s006b_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"ocx package install must succeed:\n{install.stderr}"

    project = tmp_path / "s006b_project"
    project.mkdir()
    tag1_fq = f"{registry}/{unique_repo}:1.0.0"
    tag2_fq = f"{registry}/{unique_repo}:2.0.0"
    _write_project_toml(project, tag1_fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"
    pull = _run_in(ocx, project, "pull")
    assert pull.returncode == 0, f"ocx pull must succeed:\n{pull.stderr}"

    baseline = _run_in(ocx, project, "direnv", "export")
    assert baseline.returncode == 0, (
        f"ocx direnv export must succeed against the current lock:\n{baseline.stderr}"
    )
    assert "S006B_CA" in baseline.stdout, (
        "the tag-scoped companion must be present via direnv export while the "
        f"lock is current; got:\n{baseline.stdout}"
    )

    _write_project_toml(project, tag2_fq, opt_out=False)

    result = _run_in(ocx, project, "direnv", "export")
    assert result.returncode == 0, (
        f"ocx direnv export must not fail on a stale lock; "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert "S006B_CA" not in result.stdout, (
        "a stale-lock lenient read must never surface a tag-scoped "
        f"companion; got:\n{result.stdout}"
    )
    # Positive control: the export is not empty -- the base's own tagless
    # fallback env still composes, so the absence above is the companion
    # being suppressed, not `direnv export` emitting nothing at all.
    home_key = unique_repo.upper().replace("-", "_").replace("/", "_") + "_HOME"
    assert home_key in result.stdout, (
        "the base's own env must still be exported through the tagless "
        f"fallback despite the stale lock; got:\n{result.stdout}"
    )


def test_stale_global_lock_tag_edit_env_tolerant_no_tag_scoped_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """the global toolchain has no staleness gate on reads -- a
    hand-drifted `$OCX_HOME/ocx.toml` tag edit still emits the tool's env
    (exit 0), and the tag-scoped companion is absent because a drifted
    global lock falls back to a tagless digest. Existing staleness
    behaviour, untouched by the lock advisory-tag fix. The rule publishes
    to the global descriptor (ocx-sh/ocx#550); the companion is pinned by
    a prior `ocx package install` of the TAGGED ref, so the absence below
    is the staleness fallback suppressing a real, reachable companion.
    """
    companion_repo = _unique_repo("s006c_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "S006C_CA", "/etc/ssl/s006c-ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "s006c_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"ocx package install must succeed:\n{install.stderr}"

    tag1_fq = f"{registry}/{unique_repo}:1.0.0"
    tag2_fq = f"{registry}/{unique_repo}:2.0.0"
    add = ocx.run("--global", "add", tag1_fq, format=None, check=False)
    assert add.returncode == 0, (
        f"ocx --global add must succeed; rc={add.returncode}\nstderr: {add.stderr}"
    )

    baseline = ocx.run("--global", "env", format="json", check=False)
    assert baseline.returncode == 0, (
        f"ocx --global env must succeed while the lock is current:\n{baseline.stderr}"
    )
    baseline_entries = json.loads(baseline.stdout)["entries"]
    assert _entry_by_key(baseline_entries, "S006C_CA") is not None, (
        "the tag-scoped companion must be present via `ocx --global env` while "
        f"the declared tag matches the lock; got keys: {[e['key'] for e in baseline_entries]}"
    )

    # Hand-drift the global ocx.toml's declared tag WITHOUT relocking.
    global_toml = ocx.ocx_home / "ocx.toml"
    original = global_toml.read_text()
    drifted = original.replace(tag1_fq, tag2_fq)
    assert drifted != original, "the tag edit must actually change the file"
    global_toml.write_text(drifted)

    result = ocx.run("--global", "env", format="json", check=False)
    assert result.returncode == 0, (
        "the global toolchain env exporter has no staleness gate on reads; "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )
    entries = json.loads(result.stdout)["entries"]
    assert _entry_by_key(entries, "S006C_CA") is None, (
        "a drifted global lock must never surface a tag-scoped companion; "
        f"got keys: {[e['key'] for e in entries]}"
    )
    # Positive control: the locked tool's own env still composes -- the
    # absence above is the companion being suppressed, not the exporter
    # emitting nothing under the drifted config.
    home_key = unique_repo.upper().replace("-", "_").replace("/", "_") + "_HOME"
    assert _entry_by_key(entries, home_key) is not None, (
        "the global toolchain's locked tool must still export its own env "
        f"despite the drifted declared tag; got keys: {[e['key'] for e in entries]}"
    )


def test_stale_global_lock_unparseable_ocx_toml_env_tolerant_no_tag_scoped_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """the global toolchain also has no staleness gate when `ocx.toml` is
    outright unparseable -- with a lock still present, `ocx --global env`
    still emits the locked tool's own env (exit 0, lenient fallback), and
    the tag-scoped companion is absent: no declaration survives an
    unparseable config, so resolution falls back to the tagless digest
    (matcher.rs's own `*:*` caveat only lets a digest or a registry port's
    colon satisfy it, not a bare-repo `:*` anchor). The rule publishes to
    the global descriptor (ocx-sh/ocx#550); the companion, pinned by a
    prior tagged install, is absent below via that same fallback.
    """
    companion_repo = _unique_repo("s006c2_companion")
    companion_fq = f"{registry}/{companion_repo}:1.0.0"
    _make_companion(ocx, companion_repo, "1.0.0", tmp_path, "S006C2_CA", "/etc/ssl/s006c2-ca.pem")

    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor_path = tmp_path / "s006c2_descriptor.json"
    _write_descriptor(
        descriptor_path,
        rules=[{"match": f"{registry}/{unique_repo}:*", "packages": [companion_fq]}],
    )
    _write_config(ocx, registry)
    _publish_descriptor_global(ocx, descriptor_path)

    install = ocx.plain("package", "install", base_pkg.short)
    assert install.returncode == 0, f"ocx package install must succeed:\n{install.stderr}"

    tag1_fq = f"{registry}/{unique_repo}:1.0.0"
    add = ocx.run("--global", "add", tag1_fq, format=None, check=False)
    assert add.returncode == 0, (
        f"ocx --global add must succeed; rc={add.returncode}\nstderr: {add.stderr}"
    )

    baseline = ocx.run("--global", "env", format="json", check=False)
    assert baseline.returncode == 0, (
        f"ocx --global env must succeed while ocx.toml is parseable:\n{baseline.stderr}"
    )
    baseline_entries = json.loads(baseline.stdout)["entries"]
    assert _entry_by_key(baseline_entries, "S006C2_CA") is not None, (
        "the tag-scoped companion must be present via `ocx --global env` before "
        f"ocx.toml is corrupted; got keys: {[e['key'] for e in baseline_entries]}"
    )

    # Corrupt the global ocx.toml into unparseable TOML WITHOUT touching the
    # lock `--global add` just wrote.
    global_toml = ocx.ocx_home / "ocx.toml"
    global_toml.write_text("this is not [ valid toml at all\n")

    result = ocx.run("--global", "env", format="json", check=False)
    assert result.returncode == 0, (
        "the global toolchain env exporter has no staleness gate on an "
        f"unparseable ocx.toml; rc={result.returncode}\nstderr: {result.stderr}"
    )
    entries = json.loads(result.stdout)["entries"]
    assert _entry_by_key(entries, "S006C2_CA") is None, (
        "an unparseable global ocx.toml must never surface a tag-scoped "
        f"companion; got keys: {[e['key'] for e in entries]}"
    )
    home_key = unique_repo.upper().replace("-", "_").replace("/", "_") + "_HOME"
    assert _entry_by_key(entries, home_key) is not None, (
        "the locked tool's own env must still export despite the "
        f"unparseable global ocx.toml; got keys: {[e['key'] for e in entries]}"
    )


def test_execution_record_for_lock_resolved_tool_has_no_tag_annotation_or_qualifier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """an `ocx exec` execution record for a tool resolved from
    `ocx.lock` carries NO `sh.ocx.resolved-from` annotation and NO purl
    `tag` qualifier on its root entry -- the advisory tag exists only to
    rejoin patch-match identifiers and must never leak into execution-record
    identity. Existing record shape, untouched by the lock advisory-tag fix.
    """
    base_pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    project = tmp_path / "s007_project"
    project.mkdir()
    _write_project_toml(project, base_pkg.fq, opt_out=False)
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == 0, f"ocx lock must succeed:\n{lock.stderr}"

    # `ocx lock` materializes eagerly into its own home, so an exec there
    # installs nothing and `autoInstalled` below would be empty; a fresh home
    # makes this exec the one that pulls the tool.
    clean_home = tmp_path / "s007_clean_home"
    clean_home.mkdir()
    clean_home_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)

    sink = tmp_path / "s007_records"
    sink.mkdir()
    result = _run_in(clean_home_runner, project, "exec", "--records-dir", str(sink), "--", "env")
    assert result.returncode == 0, (
        f"ocx exec must succeed; rc={result.returncode}\nstderr: {result.stderr}"
    )

    record_files = sorted(
        path for path in sink.iterdir() if path.is_file() and not path.name.startswith(".tmp")
    )
    assert len(record_files) == 1, f"expected exactly one execution record; got {record_files}"
    record = json.loads(record_files[0].read_text().strip())

    root_entries = [
        entry
        for entry in record["packages"]
        if entry.get("annotations", {}).get("sh.ocx.role") == "root"
    ]
    assert len(root_entries) == 1, f"expected exactly one root package entry: {record['packages']}"
    root_entry = root_entries[0]

    assert "sh.ocx.resolved-from" not in root_entry.get("annotations", {}), (
        "a lock-resolved tool's execution record must carry NO "
        f"sh.ocx.resolved-from annotation; got {root_entry.get('annotations')}"
    )
    uri = root_entry.get("uri", "")
    assert uri.startswith("pkg:"), f"root entry uri must be a purl; got uri={uri!r}"
    qualifiers = parse_qs(urlsplit(uri).query)
    assert "tag" not in qualifiers, (
        "a lock-resolved tool's execution record purl must carry no `tag` "
        f"qualifier; got uri={uri!r}"
    )

    # `resolution.autoInstalled` is filled from the same pull-request
    # identifiers, so a lock-origin tool's entry there must also be tagless.
    auto_installed = record.get("resolution", {}).get("autoInstalled") or []
    matching = [entry for entry in auto_installed if entry.startswith(f"{registry}/{unique_repo}")]
    assert matching, (
        "the lock-resolved tool must appear in resolution.autoInstalled (it was "
        f"materialized by this exec, not previously pulled); got {auto_installed}"
    )
    for entry in matching:
        before_digest = entry[len(f"{registry}/") :].split("@", 1)[0]
        assert ":" not in before_digest, (
            "a lock-resolved tool's resolution.autoInstalled identifier must be "
            f"tagless (repo@digest, no repo:tag@digest); got {entry!r}"
        )
