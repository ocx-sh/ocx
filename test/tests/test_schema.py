"""Acceptance tests — Group 3.10: JSON Schema includes entrypoints.

Verifies that `task schema:generate` emits `entrypoints` as an additive-optional
property on Bundle. Also validates round-trip TOML/JSON for omitted / empty /
populated entrypoints shapes.

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
        if "entrypoints" in val.get("properties", {}):
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


def test_schema_includes_entrypoints_as_additive_optional_property() -> None:
    """Bundle schema must contain entrypoints as a non-required property.

    ADR §3 + plan §1.2: entrypoints is additive-optional.
    `#[serde(default, skip_serializing_if = "Entrypoints::is_empty")]` means
    the JSON Schema must NOT list entrypoints in the `required` array.
    """
    schema = load_schema()
    bundle = find_bundle_definition(schema)

    props = bundle.get("properties", {})
    assert "entrypoints" in props, (
        f"Bundle schema must include 'entrypoints' property. "
        f"Found properties: {list(props.keys())}"
    )

    required = bundle.get("required", [])
    assert "entrypoints" not in required, (
        "entrypoints must NOT be in required[] (it is additive-optional)"
    )


def _resolve(schema: dict, node: dict) -> dict:
    """Follow a single ``$ref`` hop into ``$defs``; pass inline nodes through."""
    if "$ref" in node:
        ref_key = node["$ref"].split("/")[-1]
        defs = schema.get("$defs", schema.get("definitions", {}))
        return defs.get(ref_key, node)
    return node


def test_schema_entrypoints_is_array_type() -> None:
    """entrypoints in schema must be an ordered array of Entrypoint items.

    The wire shape is a JSON array (`[{"name": "cmake", "target": "..."}]`) —
    a transparent wrapper over `Vec<Entrypoint>`. Uniqueness is enforced at
    deserialization (`Entrypoints::new`), not by JSON object-key semantics.
    """
    schema = load_schema()
    bundle = find_bundle_definition(schema)
    ep_schema = _resolve(schema, bundle["properties"]["entrypoints"])

    assert ep_schema.get("type") == "array", (
        f"entrypoints must have type=array in schema, got: {ep_schema}"
    )
    items = ep_schema.get("items")
    assert items is not None, (
        f"entrypoints array must declare items; got: {ep_schema}"
    )
    item_schema = _resolve(schema, items)
    assert item_schema.get("type") == "object", (
        f"entrypoints items must resolve to the Entrypoint object; got: {item_schema}"
    )


def test_schema_entrypoint_item_is_object_with_name_and_target() -> None:
    """Each Entrypoint item declares required ``name`` and ``target``.

    An Entrypoint is ``{name, target}``: the invocable name and the template
    string for its binary target within the package content. Both are
    required — an empty ``{}`` is not a valid entrypoint.
    """
    schema = load_schema()
    bundle = find_bundle_definition(schema)
    ep_schema = _resolve(schema, bundle["properties"]["entrypoints"])
    item_schema = _resolve(schema, ep_schema.get("items", {}))

    assert item_schema.get("type") == "object", (
        f"Entrypoint item must be type=object; got: {item_schema}"
    )
    props = item_schema.get("properties", {})
    assert "name" in props and "target" in props, (
        f"Entrypoint must declare 'name' and 'target' properties; got: {item_schema}"
    )
    required = item_schema.get("required", [])
    assert "name" in required and "target" in required, (
        f"'name' and 'target' must both be required; got: {item_schema}"
    )


def test_schema_entrypoint_name_has_slug_pattern() -> None:
    """The Entrypoint ``name`` type must carry the slug pattern and maxLength.

    The Rust `EntrypointName` newtype enforces `^[a-z0-9][a-z0-9_-]*$` with a
    64-byte cap. The JSON Schema must carry matching constraints on the `name`
    field so validators and editors surface the restriction without running
    the binary.
    """
    schema = load_schema()
    bundle = find_bundle_definition(schema)
    ep_schema = _resolve(schema, bundle["properties"]["entrypoints"])
    item_schema = _resolve(schema, ep_schema.get("items", {}))
    name_schema = _resolve(schema, item_schema.get("properties", {}).get("name", {}))

    assert name_schema.get("pattern") == r"^[a-z0-9][a-z0-9_-]*$", (
        f"name.pattern must be the slug regex; got: {name_schema.get('pattern')!r}"
    )
    assert name_schema.get("maxLength") == 64, (
        f"name.maxLength must be 64; got: {name_schema.get('maxLength')!r}"
    )


# ---------------------------------------------------------------------------
# 3.10 Round-trip: metadata JSON with omitted / empty / populated entrypoints
# ---------------------------------------------------------------------------


def test_old_metadata_without_entrypoints_parses_successfully(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Old metadata.json without entrypoints field must deserialize successfully.

    ADR §Schema Evolution: old ocx reading new metadata → field defaults to empty.
    New ocx reading old metadata → same path.
    """
    import stat  # noqa: PLC0415
    import sys  # noqa: PLC0415
    from src.helpers import make_package  # noqa: PLC0415

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    # install must succeed (backward compat — no entrypoints in old metadata).
    result = ocx.run("package", "install", pkg.short, check=False)
    assert result.returncode == 0, (
        f"Old metadata without entrypoints must install successfully; "
        f"rc={result.returncode}, stderr={result.stderr.strip()}"
    )


def test_metadata_with_empty_entrypoints_object_installs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Metadata with explicit empty entrypoints: {} must install without error."""
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
        "type": "bundle", "version": 1, "entrypoints": {},
        "env": [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    }
    metadata_path.write_text(_json.dumps(metadata_obj))

    bundle = tmp_path / "bundle-empty-ep.tar.xz"
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(pkg_dir))

    plat = current_platform()
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    push_result = ocx.run(
        "package", "push", "-p", plat, "-m", str(metadata_path), "-n", "-i", fq, str(bundle),
        check=False,
    )
    # Empty entrypoints is valid — push must succeed.
    assert push_result.returncode == 0, (
        f"push with empty entrypoints must succeed; "
        f"rc={push_result.returncode}, stderr={push_result.stderr.strip()}"
    )

    short = f"{unique_repo}:1.0.0"
    ocx.plain("index", "update", unique_repo)
    install_result = ocx.run("package", "install", short, check=False)
    assert install_result.returncode == 0, (
        f"install with empty entrypoints must succeed; "
        f"rc={install_result.returncode}, stderr={install_result.stderr.strip()}"
    )


# Import OcxRunner for type hints in function signatures above.
from src.runner import OcxRunner  # noqa: E402
