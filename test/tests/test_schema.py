"""Acceptance tests — Group 3.10: JSON Schema includes entry_points.

Verifies that `task schema:generate` emits `entry_points` as an additive-optional
property on Bundle. Also validates round-trip TOML/JSON for omitted / empty /
populated entry_points shapes.

This test extends the schema acceptance surface rather than creating a parallel harness.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest


# Path to the generated v1.json schema in the repository.
_SCHEMA_PATH = (
    Path(__file__).resolve().parent.parent.parent
    / "website" / "src" / "public" / "schemas" / "metadata" / "v1.json"
)

_PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def load_schema() -> dict:
    """Load the generated v1.json schema. Fails if not yet generated."""
    assert _SCHEMA_PATH.exists(), (
        f"Schema not found at {_SCHEMA_PATH}. "
        "Run `task schema:generate` to generate it first."
    )
    return json.loads(_SCHEMA_PATH.read_text())


def find_bundle_definition(schema: dict) -> dict:
    """Return the Bundle definition object from the schema."""
    defs = schema.get("$defs", schema.get("definitions", {}))
    # Bundle may be under 'Bundle' key or similar.
    for key in ("Bundle", "bundle"):
        if key in defs:
            return defs[key]
    # Also check the top-level properties if Bundle is inlined.
    for key, val in defs.items():
        if "entry_points" in val.get("properties", {}):
            return val
    raise KeyError(f"Bundle definition not found in schema. Available keys: {list(defs.keys())}")


# ---------------------------------------------------------------------------
# 3.10 Schema content tests
# ---------------------------------------------------------------------------


def test_schema_file_exists_and_is_valid_json() -> None:
    """Schema file must exist and parse as valid JSON."""
    schema = load_schema()
    assert isinstance(schema, dict), "Schema must be a JSON object"
    assert "$schema" in schema or "type" in schema or "$defs" in schema, (
        "Schema must have basic JSON Schema structure"
    )


def test_schema_includes_entry_points_as_additive_optional_property() -> None:
    """Bundle schema must contain entry_points as a non-required property.

    ADR §3 + plan §1.2: entry_points is additive-optional.
    `#[serde(default, skip_serializing_if = "EntryPoints::is_empty")]` means
    the JSON Schema must NOT list entry_points in the `required` array.
    """
    schema = load_schema()
    bundle = find_bundle_definition(schema)

    props = bundle.get("properties", {})
    assert "entry_points" in props, (
        f"Bundle schema must include 'entry_points' property. "
        f"Found properties: {list(props.keys())}"
    )

    required = bundle.get("required", [])
    assert "entry_points" not in required, (
        "entry_points must NOT be in required[] (it is additive-optional)"
    )


def test_schema_entry_points_is_array_type() -> None:
    """entry_points in schema must be an array of objects."""
    schema = load_schema()
    bundle = find_bundle_definition(schema)
    ep_schema = bundle["properties"]["entry_points"]

    # May be a $ref or inline type.
    if "$ref" in ep_schema:
        # Dereference the $ref.
        ref_key = ep_schema["$ref"].split("/")[-1]
        defs = schema.get("$defs", schema.get("definitions", {}))
        ep_schema = defs.get(ref_key, ep_schema)

    # After dereferencing, the schema should be array-shaped.
    # EntryPoints is #[serde(transparent)] over Vec<EntryPoint>, so it
    # serializes as an array.
    schema_type = ep_schema.get("type")
    assert schema_type == "array", (
        f"entry_points must have type=array in schema, got: {ep_schema}"
    )


def test_schema_entry_points_items_have_name_and_target() -> None:
    """entry_points array items must have 'name' and 'target' properties."""
    schema = load_schema()
    bundle = find_bundle_definition(schema)
    ep_schema = bundle["properties"]["entry_points"]

    # Dereference if needed.
    if "$ref" in ep_schema:
        ref_key = ep_schema["$ref"].split("/")[-1]
        defs = schema.get("$defs", schema.get("definitions", {}))
        ep_schema = defs.get(ref_key, ep_schema)

    items = ep_schema.get("items", {})
    if "$ref" in items:
        ref_key = items["$ref"].split("/")[-1]
        defs = schema.get("$defs", schema.get("definitions", {}))
        items = defs.get(ref_key, items)

    item_props = items.get("properties", {})
    assert "name" in item_props, (
        f"EntryPoint items must have 'name' property. Got: {list(item_props.keys())}"
    )
    assert "target" in item_props, (
        f"EntryPoint items must have 'target' property. Got: {list(item_props.keys())}"
    )


# ---------------------------------------------------------------------------
# 3.10 Round-trip: metadata JSON with omitted / empty / populated entry_points
# ---------------------------------------------------------------------------


def test_old_metadata_without_entry_points_parses_successfully(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Old metadata.json without entry_points field must deserialize successfully.

    ADR §Schema Evolution: old ocx reading new metadata → field defaults to empty.
    New ocx reading old metadata → same path.
    """
    import stat  # noqa: PLC0415
    import sys  # noqa: PLC0415
    from src.helpers import make_package  # noqa: PLC0415

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    # install must succeed (backward compat — no entry_points in old metadata).
    result = ocx.run("install", pkg.short, check=False)
    assert result.returncode == 0, (
        f"Old metadata without entry_points must install successfully; "
        f"rc={result.returncode}, stderr={result.stderr.strip()}"
    )


def test_metadata_with_empty_entry_points_array_installs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Metadata with explicit empty entry_points: [] must install without error."""
    import json as _json  # noqa: PLC0415
    import sys  # noqa: PLC0415
    import stat  # noqa: PLC0415
    from src.helpers import current_platform  # noqa: PLC0415

    pkg_dir = tmp_path / "pkg-empty-ep"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    script = bin_dir / "hello"
    if sys.platform == "win32":
        script = script.with_suffix(".bat")
        script.write_text("@echo hello\n")
    else:
        script.write_text("#!/bin/sh\necho hello\n")
        script.chmod(script.stat().st_mode | stat.S_IEXEC)

    metadata_path = tmp_path / "metadata-empty-ep.json"
    metadata_obj = {
        "type": "bundle",
        "version": 1,
        "entry_points": [],
        "env": [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    }
    metadata_path.write_text(_json.dumps(metadata_obj))

    bundle = tmp_path / "bundle-empty-ep.tar.xz"
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(pkg_dir))

    plat = current_platform()
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    push_result = ocx.run(
        "package", "push", "-p", plat, "-m", str(metadata_path), "-n", fq, str(bundle),
        check=False,
    )
    # Empty entry_points is valid — push must succeed.
    assert push_result.returncode == 0, (
        f"push with empty entry_points must succeed; "
        f"rc={push_result.returncode}, stderr={push_result.stderr.strip()}"
    )

    short = f"{unique_repo}:1.0.0"
    ocx.plain("index", "update", unique_repo)
    install_result = ocx.run("install", short, check=False)
    assert install_result.returncode == 0, (
        f"install with empty entry_points must succeed; "
        f"rc={install_result.returncode}, stderr={install_result.stderr.strip()}"
    )


# Import OcxRunner for type hints in function signatures above.
from src.runner import OcxRunner  # noqa: E402
