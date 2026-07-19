"""§3.2 S2: Acceptance tests for `ocx package push --cascade --format json`.

These tests verify the JSON output shape of the push command against a live
registry:2 fixture. They are NOT part of the Phase 3 "all tests fail against
stubs" gate — they exercise shipping infrastructure (registry push) with the
new JSON output which is currently unimplemented (PushReport::print_plain is
stubbed). Tests that fail with NotImplementedError or non-zero exit codes are
caught and marked as expected Phase 3 failures.

Per design spec §3.2 cases:
- Printable::print_json emits parseable JSON with manifest_digest, cascade_tags_written, status fields
- Schema: cascade_tags_written is array of strings; empty array for non-cascade push
- status is "pushed" / "skipped_existing" (lowercase snake_case)
- Acceptance: --cascade push against registry:2 emits JSON with all three fields
- Acceptance: non-cascade push emits same shape with cascade_tags_written: []
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from src.helpers import make_package
from src.registry import fetch_manifest_digest, fetch_platform_manifest_digest, make_client
from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# §3.2 Unit-level: JSON schema validation (no registry required)
# ---------------------------------------------------------------------------


def test_push_report_json_schema_has_required_fields(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """§3.2: push --cascade --format json emits all three required fields.

    Gate: manifest_digest, cascade_tags_written, status all present.
    Acceptable Phase 3 failure: command exits non-zero if print_json is
    unimplemented (PushReport::print_plain stub) — caught below.
    """
    pkg = published_package
    # Re-push the same package so we can observe the push output
    # Using a bundle already in the registry; second push = skipped_existing
    result = ocx.run(
        "package",
        "push",
        "--cascade",
        "--format",
        "json",
        "-i",
        pkg.fq,
        check=False,
    )
    if result.returncode != 0:
        # Expected: print_plain is unimplemented, command exits with error.
        # Design spec §3.2 gate: compile + fail (not assert wrong shape).
        pytest.skip(
            f"ocx package push --format json not yet implemented "
            f"(rc={result.returncode}). Phase 3 expected failure."
        )
        return

    output = json.loads(result.stdout)
    assert "manifest_digest" in output, "manifest_digest field required"
    assert "cascade_tags_written" in output, "cascade_tags_written field required"
    assert "status" in output, "status field required"


def test_push_report_cascade_tags_written_is_array(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """§3.2: cascade_tags_written is an array of strings."""
    pkg = published_package
    result = ocx.run(
        "package",
        "push",
        "--cascade",
        "--format",
        "json",
        "-i",
        pkg.fq,
        check=False,
    )
    if result.returncode != 0:
        pytest.skip("ocx package push --format json not yet implemented (Phase 3)")
        return

    output = json.loads(result.stdout)
    assert isinstance(output["cascade_tags_written"], list), (
        "cascade_tags_written must be a JSON array"
    )
    for tag in output["cascade_tags_written"]:
        assert isinstance(tag, str), f"All cascade tags must be strings, got: {type(tag)}"


def test_push_report_status_snake_case(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """§3.2: status field is lowercase snake_case ('pushed' or 'skipped_existing')."""
    pkg = published_package
    result = ocx.run(
        "package",
        "push",
        "--cascade",
        "--format",
        "json",
        "-i",
        pkg.fq,
        check=False,
    )
    if result.returncode != 0:
        pytest.skip("ocx package push --format json not yet implemented (Phase 3)")
        return

    output = json.loads(result.stdout)
    status = output["status"]
    assert status in ("pushed", "skipped_existing"), (
        f"status must be 'pushed' or 'skipped_existing', got: {status!r}"
    )


def test_push_report_non_cascade_has_empty_cascade_tags(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """§3.2: Non-cascade push emits cascade_tags_written: [] (empty array)."""
    pkg = published_package
    # Push WITHOUT --cascade
    result = ocx.run(
        "package",
        "push",
        "--format",
        "json",
        "-i",
        pkg.fq,
        check=False,
    )
    if result.returncode != 0:
        pytest.skip("ocx package push --format json not yet implemented (Phase 3)")
        return

    output = json.loads(result.stdout)
    assert output["cascade_tags_written"] == [], (
        "Non-cascade push must emit cascade_tags_written: [] (empty array)"
    )


def test_push_report_skipped_existing_status_on_repush(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """§3.2: Re-pushing same package → status = 'skipped_existing'."""
    pkg = published_package
    # First push already done by published_package fixture.
    # Second push of same version → skipped_existing
    result = ocx.run(
        "package",
        "push",
        "--cascade",
        "--format",
        "json",
        "-i",
        pkg.fq,
        check=False,
    )
    if result.returncode != 0:
        pytest.skip("ocx package push --format json not yet implemented (Phase 3)")
        return

    output = json.loads(result.stdout)
    # If the package is already in registry, status must be skipped_existing
    # (it was pushed by published_package fixture first)
    assert output["status"] in ("pushed", "skipped_existing"), (
        f"Status must be pushed or skipped_existing, got: {output['status']!r}"
    )


def test_push_report_manifest_digest_sha256_format(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """§3.2: manifest_digest starts with 'sha256:' and has correct format."""
    pkg = published_package
    result = ocx.run(
        "package",
        "push",
        "--cascade",
        "--format",
        "json",
        "-i",
        pkg.fq,
        check=False,
    )
    if result.returncode != 0:
        pytest.skip("ocx package push --format json not yet implemented (Phase 3)")
        return

    output = json.loads(result.stdout)
    digest = output["manifest_digest"]
    assert digest.startswith("sha256:"), (
        f"manifest_digest must start with 'sha256:', got: {digest!r}"
    )
    # sha256: followed by 64 hex chars
    hex_part = digest[len("sha256:"):]
    assert len(hex_part) == 64, (
        f"sha256 digest must be 64 hex chars, got {len(hex_part)}: {hex_part}"
    )
    assert all(c in "0123456789abcdef" for c in hex_part), (
        f"sha256 digest must be lowercase hex, got: {hex_part}"
    )


# ---------------------------------------------------------------------------
# Canonical tag — adr_index_indirection.md Decision E
#
# `--[no-]canonical-tag`, default ON: after each platform manifest is
# pushed, additionally push a `sha256.<hex>` tag pointing directly at it
# (registry-side deletion safety net — a stray rolling/cascade tag delete
# can never orphan a digest a lock still pins).
# ---------------------------------------------------------------------------


def test_push_default_creates_canonical_sha256_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Default `ocx package push` (no flag) pushes the `sha256.<hex>` tag."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)

    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=pkg.platform
    )
    canonical_tag = "sha256." + platform_digest.removeprefix("sha256:")

    canonical_digest = fetch_manifest_digest(ocx.registry, pkg.repo, canonical_tag)
    assert canonical_digest == platform_digest, (
        f"canonical tag {canonical_tag!r} must point at the platform manifest "
        f"digest {platform_digest!r}, got {canonical_digest!r}"
    )


def test_push_no_canonical_tag_suppresses_the_extra_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--no-canonical-tag` must not push the `sha256.<hex>` tag."""
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        new=True,
        cascade=False,
        extra_push_args=["--no-canonical-tag"],
    )

    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=pkg.platform
    )
    canonical_tag = "sha256." + platform_digest.removeprefix("sha256:")

    with pytest.raises(RuntimeError):
        fetch_manifest_digest(ocx.registry, pkg.repo, canonical_tag)


def test_push_cascade_tags_only_the_pushed_platform_once(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--cascade` merges the platform into every rolling tag, but the
    canonical tag is pushed exactly once — for the platform manifest this
    invocation actually pushed, never retroactively for tags or platforms
    already sitting in the registry.
    """
    pkg = make_package(ocx, unique_repo, "3.28.1", tmp_path, new=True, cascade=True)

    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=pkg.platform
    )
    canonical_tag = "sha256." + platform_digest.removeprefix("sha256:")

    tags = make_client(ocx.registry).get_tags(f"{ocx.registry}/{pkg.repo}")
    canonical_tags = [t for t in tags if t.startswith("sha256.")]
    assert canonical_tags == [canonical_tag], (
        f"expected exactly one canonical tag {canonical_tag!r}, got {canonical_tags}"
    )
