# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package push --build-timestamp`.

Covers:
- Primary tag receives `_<YYYYMMDDhhmmss>` suffix when `--build-timestamp=datetime` is used.
- A second push with `--build-timestamp datetime` against a tag that already carries build
  metadata is rejected with exit 65 (DataError) and an explanatory error message.
- Cascade push with `--build-timestamp=datetime --cascade --new` creates the timestamped
  primary tag and all rolling tags (`X.Y`, `X`, `latest`) pointing at the same digest.
"""
from __future__ import annotations

import json
import re
import stat
import sys
from pathlib import Path

from src import OcxRunner, current_platform
from src.registry import fetch_manifest_digest, fetch_manifest_from_registry, index_platforms


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

def _make_tiny_bundle(ocx: OcxRunner, tmp_path: Path, repo: str, tag: str) -> tuple[Path, Path]:
    """Create a minimal package bundle and metadata file.

    Returns ``(bundle_path, metadata_path)``.
    """
    pkg_dir = tmp_path / f"pkg-{repo}-{tag}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)

    hello = bin_dir / "hello"
    if sys.platform == "win32":
        hello = hello.with_suffix(".bat")
        hello.write_text("@echo hello\n")
    else:
        hello.write_text("#!/bin/sh\necho hello\n")
        hello.chmod(hello.stat().st_mode | stat.S_IEXEC)

    metadata_path = tmp_path / f"metadata-{repo}-{tag}.json"
    metadata_path.write_text(json.dumps({
        "type": "bundle",
        "version": 1,
        "env": [
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            },
        ],
    }))

    bundle = tmp_path / f"bundle-{repo}-{tag}.tar.xz"
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(pkg_dir))

    return bundle, metadata_path


# ---------------------------------------------------------------------------
# Test 1: --build-timestamp=datetime appends _<14-digit> suffix to tag
# ---------------------------------------------------------------------------

def test_package_push_with_build_timestamp_datetime(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Push with --build-timestamp=datetime publishes tag matching <version>_<14-digit-ts>.

    The primary tag must not be the bare version (e.g. 0.3.0) but the
    timestamped form (e.g. 0.3.0_20260514120000). Verified by updating the
    local index and listing tags — the tagged form must appear and match the
    expected regex.
    """
    tag = "0.3.0"
    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain(
        "package", "push",
        "--build-timestamp=datetime",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
    )

    # Sync local index so `index list` can report what was pushed.
    ocx.plain("index", "update", unique_repo)

    result = ocx.plain("index", "list", unique_repo)
    tags_output = result.stdout

    # The bare version must NOT appear as a standalone tag (we pushed with build
    # metadata only, so the registry should not have a plain `0.3.0` entry).
    # The `index list` output is a two-column table (Package | Tag); match the
    # tag column by looking for the version surrounded by whitespace or line
    # boundaries, so `0.3.0_<ts>` does not accidentally match this check.
    bare_tag_pattern = re.compile(rf"\s{re.escape(tag)}\s", re.MULTILINE)
    assert not bare_tag_pattern.search(tags_output), (
        f"bare tag '{tag}' should not appear in index; got:\n{tags_output}"
    )

    # At least one tag matching <version>_<14-digit-ts> must be present.
    # The table format embeds the tag as a column value, so search without
    # anchors for the versioned form.
    timestamped_pattern = re.compile(rf"{re.escape(tag)}_\d{{14}}")
    assert timestamped_pattern.search(tags_output), (
        f"expected a tag matching '{tag}_NNNNNNNNNNNNNN' in index output; got:\n{tags_output}"
    )


# ---------------------------------------------------------------------------
# Test 2: double-push with --build-timestamp datetime rejected (exit 65)
# ---------------------------------------------------------------------------

def test_package_push_rejects_double_build_timestamp(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Pushing with --build-timestamp=datetime against a tag that already carries build metadata exits 65.

    The identifier tag (`0.3.0_20260514120000`) already has a build-metadata
    segment separated by `_`. Supplying `--build-timestamp=datetime` (equals
    form, not space-separated) on top of it is rejected by `Version::with_build`'s
    `AlreadyPresent` branch, which maps to `ExitCode::DataError` (65).
    """
    # The identifier tag already contains build metadata (underscore-separated segment).
    base_version = "0.3.0"
    build_seg = "20260514120000"
    tag_with_build = f"{base_version}_{build_seg}"

    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag_with_build)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag_with_build}"
    result = ocx.run(
        "package", "push",
        "--build-timestamp=datetime",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
        format=None,
        check=False,
    )

    assert result.returncode == 65, (
        f"expected exit 65 (DataError) for double build-timestamp, "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    assert "already has build metadata" in result.stderr, (
        f"expected 'already has build metadata' in stderr; got:\n{result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# Test 3: cascade with --build-timestamp=datetime creates rolling tags
# ---------------------------------------------------------------------------

def test_package_push_cascade_with_build_timestamp(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Cascade push with --build-timestamp=datetime creates the timestamped primary tag
    and all rolling tags (X.Y, X, latest) pointing at the same digest.

    The primary tag is the one carrying the `_<ts>` suffix (e.g. `0.3.0_<ts>`).
    The rolling tags `0.3`, `0`, and `latest` are derived from the version core
    (`0.3.0`) without the build segment, per cascade algebra. All four published
    tags must resolve to the same underlying manifest digest.
    """
    tag = "0.3.0"
    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain(
        "package", "push",
        "--build-timestamp=datetime",
        "--cascade",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
    )

    # Sync local index and retrieve all published tags.
    ocx.plain("index", "update", unique_repo)
    result = ocx.plain("index", "list", unique_repo)
    tags_output = result.stdout

    # Confirm the primary timestamped tag is present.
    # The `index list` output is a two-column table (Package | Tag); the tag
    # value appears as a column, not at line start/end, so search without anchors.
    timestamped_pattern = re.compile(rf"({re.escape(tag)}_\d{{14}})")
    match = timestamped_pattern.search(tags_output)
    assert match, (
        f"expected a tag matching '{tag}_NNNNNNNNNNNNNN' in index output; got:\n{tags_output}"
    )
    primary_tag = match.group(1).strip()

    # Confirm the rolling cascade tags are present.
    rolling_tags = ["0.3", "0", "latest"]
    for rolling in rolling_tags:
        assert rolling in tags_output, (
            f"expected rolling tag '{rolling}' in index output after cascade; got:\n{tags_output}"
        )

    # All tags must resolve to the same manifest digest, confirming cascade
    # created image-index pointers and not independent pushes.
    primary_digest = fetch_manifest_digest(ocx.registry, unique_repo, primary_tag)
    for rolling in rolling_tags:
        rolling_digest = fetch_manifest_digest(ocx.registry, unique_repo, rolling)
        assert rolling_digest == primary_digest, (
            f"rolling tag '{rolling}' digest {rolling_digest!r} does not match "
            f"primary tag '{primary_tag}' digest {primary_digest!r}"
        )

    # The platform must be reachable via all cascade tags.
    for rolling in rolling_tags:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, rolling)
        platforms = index_platforms(manifest)
        assert plat in platforms, (
            f"platform {plat!r} missing from cascade tag '{rolling}': {platforms}"
        )


# ---------------------------------------------------------------------------
# Test 4: --build-timestamp=date appends _<8-digit> suffix
# ---------------------------------------------------------------------------

def test_package_push_with_build_timestamp_date(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Push with --build-timestamp=date publishes tag matching <version>_<8-digit-date>.

    Unlike `datetime` (14-digit), `date` appends only `_YYYYMMDD` (8 digits).
    Verified by syncing the local index and checking that the tag column matches
    the expected regex.
    """
    tag = "0.3.0"
    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain(
        "package", "push",
        "--build-timestamp=date",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
    )

    ocx.plain("index", "update", unique_repo)

    result = ocx.plain("index", "list", unique_repo)
    tags_output = result.stdout

    # The date-only suffix is exactly 8 digits (YYYYMMDD).
    date_pattern = re.compile(rf"{re.escape(tag)}_\d{{8}}$", re.MULTILINE)
    assert date_pattern.search(tags_output), (
        f"expected a tag matching '{tag}_NNNNNNNN' (8-digit date) in index output; got:\n{tags_output}"
    )

    # Must not appear as a 14-digit datetime tag.
    datetime_pattern = re.compile(rf"{re.escape(tag)}_\d{{14}}")
    assert not datetime_pattern.search(tags_output), (
        f"unexpected 14-digit datetime tag found with --build-timestamp=date; got:\n{tags_output}"
    )


# ---------------------------------------------------------------------------
# Test 5: --build-timestamp=none is a no-op (bare version tag published)
# ---------------------------------------------------------------------------

def test_package_push_build_timestamp_none_is_noop(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Push with --build-timestamp=none publishes the bare version tag unchanged.

    `BuildTimestampFormat::None` short-circuits before `apply_build_meta`, so
    the tag stored in the registry is the bare version (e.g. `0.3.0`) with no
    `_<ts>` suffix appended.
    """
    tag = "0.3.0"
    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain(
        "package", "push",
        "--build-timestamp=none",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
    )

    ocx.plain("index", "update", unique_repo)

    result = ocx.plain("index", "list", unique_repo)
    tags_output = result.stdout

    # The bare version must appear as a standalone tag.
    # Match whole-word via word boundary on the right (or end-of-line) to avoid
    # `0.3.0_<ts>` accidentally matching this check.
    bare_pattern = re.compile(rf"\s{re.escape(tag)}\s", re.MULTILINE)
    assert bare_pattern.search(tags_output), (
        f"expected bare tag '{tag}' in index output with --build-timestamp=none; got:\n{tags_output}"
    )

    # No timestamped form should exist.
    timestamped_pattern = re.compile(rf"{re.escape(tag)}_\d+")
    assert not timestamped_pattern.search(tags_output), (
        f"unexpected timestamped tag found with --build-timestamp=none; got:\n{tags_output}"
    )


# ---------------------------------------------------------------------------
# Test 6: bare --build-timestamp (no =, no value) defaults to datetime
# ---------------------------------------------------------------------------

def test_package_push_bare_build_timestamp_defaults_to_datetime(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Bare --build-timestamp flag (no = and no value) defaults to datetime format.

    With `require_equals = true` and `default_missing_value = "datetime"`, clap
    accepts the bare flag and substitutes `datetime`. The published tag must
    therefore match the 14-digit datetime pattern, not the 8-digit date pattern.
    """
    tag = "0.3.0"
    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    # Bare flag — no "=" and no value following it.
    ocx.plain(
        "package", "push",
        "--build-timestamp",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
    )

    ocx.plain("index", "update", unique_repo)

    result = ocx.plain("index", "list", unique_repo)
    tags_output = result.stdout

    # Bare flag must produce the 14-digit datetime suffix.
    datetime_pattern = re.compile(rf"{re.escape(tag)}_\d{{14}}")
    assert datetime_pattern.search(tags_output), (
        f"expected a tag matching '{tag}_NNNNNNNNNNNNNN' (14-digit datetime) "
        f"from bare --build-timestamp; got:\n{tags_output}"
    )


# ---------------------------------------------------------------------------
# Test 7: identifier without patch segment (X.Y) rejected with exit 65
# ---------------------------------------------------------------------------

def test_package_push_build_timestamp_rejects_no_patch_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Push with --build-timestamp=datetime against a X.Y (no patch) tag exits 65 (DataError).

    `Version::with_build` requires a `X.Y.Z` core. An identifier whose tag is
    `1.2` (minor-only, no patch segment) triggers `BuildMetaError::NoPatch`,
    which maps to `ExitCode::DataError` (65). The error message must reference
    `X.Y.Z` to explain the required format.
    """
    # A two-component tag — no patch segment.
    tag = "1.2"
    bundle, metadata_path = _make_tiny_bundle(ocx, tmp_path, unique_repo, tag)
    plat = current_platform()

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    result = ocx.run(
        "package", "push",
        "--build-timestamp=datetime",
        "-n",
        "-p", plat,
        "-m", str(metadata_path),
        "-i", fq,
        str(bundle),
        format=None,
        check=False,
    )

    assert result.returncode == 65, (
        f"expected exit 65 (DataError) for X.Y tag with --build-timestamp, "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    assert "X.Y.Z" in result.stderr, (
        f"expected 'X.Y.Z' in stderr for NoPatch error; got:\n{result.stderr.strip()}"
    )
