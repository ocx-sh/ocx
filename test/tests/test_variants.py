# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Acceptance tests for variant install, select, and discovery workflows."""

from pathlib import Path

from src import OcxRunner, assert_symlink_exists, registry_dir
from src.helpers import make_package


def test_install_variant_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Installing a variant-prefixed package creates the correct candidate symlink."""
    make_package(
        ocx, unique_repo, "debug-1.0.0", tmp_path,
        platform="linux/amd64", new=True,
    )

    ocx.plain("install", f"{unique_repo}:debug-1.0.0")
    candidate = (
        Path(ocx.ocx_home)
        / "installs"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "debug-1.0.0"
    )
    assert_symlink_exists(candidate)


def test_install_variant_rolling_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Installing via a variant rolling tag resolves correctly."""
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path,
        platform="linux/amd64", new=True,
    )

    # debug-1 is a rolling tag created by cascade
    ocx.plain("install", f"{unique_repo}:debug-1")
    candidate = (
        Path(ocx.ocx_home)
        / "installs"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "debug-1"
    )
    assert_symlink_exists(candidate)


def test_select_variant_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Selecting a variant package sets the current symlink."""
    make_package(
        ocx, unique_repo, "debug-1.0.0", tmp_path,
        platform="linux/amd64", new=True,
    )

    ocx.plain("install", "--select", f"{unique_repo}:debug-1.0.0")
    current = (
        Path(ocx.ocx_home)
        / "installs"
        / registry_dir(ocx.registry)
        / unique_repo
        / "current"
    )
    assert_symlink_exists(current)


def test_variant_and_default_coexist(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Variant and default (unadorned) packages can be installed side by side."""
    make_package(
        ocx, unique_repo, "debug-1.0.0", tmp_path / "debug",
        platform="linux/amd64", new=True,
    )
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "default",
        platform="linux/amd64", new=False, cascade=False,
    )

    ocx.plain("install", f"{unique_repo}:debug-1.0.0")
    ocx.plain("install", f"{unique_repo}:1.0.0")

    installs = (
        Path(ocx.ocx_home)
        / "installs"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
    )
    assert_symlink_exists(installs / "debug-1.0.0")
    assert_symlink_exists(installs / "1.0.0")


def test_index_list_variants(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """The --variants flag extracts unique variant names from tags."""
    make_package(
        ocx, unique_repo, "debug-1.0.0", tmp_path / "debug",
        platform="linux/amd64", new=True,
    )
    make_package(
        ocx, unique_repo, "pgo-2.0.0", tmp_path / "pgo",
        platform="linux/amd64", new=False,
    )
    # Push an unadorned version to represent the default variant
    make_package(
        ocx, unique_repo, "3.0.0", tmp_path / "default",
        platform="linux/amd64", new=False,
    )

    result = ocx.json("index", "list", "--variants", unique_repo)
    variants = result[unique_repo]
    assert "" in variants, f"Expected empty string (default variant) in variants: {variants}"
    assert "debug" in variants, f"Expected 'debug' in variants: {variants}"
    assert "pgo" in variants, f"Expected 'pgo' in variants: {variants}"
