# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Phase 10 specification tests for the JSON-Schema generation pipeline.

These tests run the ``ocx_schema`` binary directly (built once via
``cargo build --release -p ocx_schema``) and validate that each schema
variant emits the canonical ``$id`` URL. The architect's path-1 finding
(project-lock schema must carry a ``$comment`` flagging it as
machine-generated) is asserted here end-to-end so a regression in either
the schema generator's hand-written ``JsonSchema`` impls or the binary's
dispatch fires.

Plan reference: ``.claude/state/plans/plan_project_toolchain.md`` lines
859–873 (Phase 10 deliverable 1 — JSON Schema for ``ocx.toml`` and
``ocx.lock``).

Operational notes
-----------------
We invoke the binary at ``target/release/ocx_schema`` directly rather
than going through the Taskfile because:

1. The schema taskfile is included as ``internal: true`` from the
   website taskfile, blocking ``task website:schema:*`` invocation.
2. Invoking ``task -t website/schema.taskfile.yml`` from the project
   root makes ``{{.ROOT_DIR}}`` resolve to the website directory,
   doubling the output path.
3. The binary's stdout is the schema, so we don't need the taskfile's
   ``mkdir -p && cargo run > out/v1.json`` ceremony — the test reads
   stdout directly and compares.

This approach is faster (one-time compile + N stdout captures) and
isolated (no real ``website/src/public/schemas/`` files mutated).

Coverage
--------
``task website:build`` includes ``schema:generate*`` as build
dependencies, so the existing website-build gate already exercises the
Taskfile wiring end-to-end. These tests pin the binary's contract; the
website-build gate pins the wiring.
"""
from __future__ import annotations

import json
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_BINARY = PROJECT_ROOT / "target" / "release" / "ocx_schema"


# (schema_kind, expected_id, friendly_label)
SCHEMA_VARIANTS = [
    ("metadata", "https://ocx.sh/schemas/metadata/v1.json", "metadata"),
    ("config", "https://ocx.sh/schemas/config/v1.json", "config"),
    ("project", "https://ocx.sh/schemas/project/v1.json", "project"),
    (
        "project-lock",
        "https://ocx.sh/schemas/project-lock/v1.json",
        "project-lock",
    ),
]


@pytest.fixture(scope="module")
def schema_binary() -> Path:
    """Ensure ``target/release/ocx_schema`` exists.

    Builds the binary once per session if it's missing. Skips the test
    module if the build fails (e.g. environment without cargo).
    """
    if not SCHEMA_BINARY.exists():
        result = subprocess.run(
            ["cargo", "build", "--release", "-p", "ocx_schema"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.skip(
                f"failed to build ocx_schema binary: {result.stderr.strip()}"
            )
        if not SCHEMA_BINARY.exists():
            pytest.skip(
                f"ocx_schema build succeeded but binary missing at {SCHEMA_BINARY}"
            )
    return SCHEMA_BINARY


def _run_schema(binary: Path, kind: str) -> str:
    """Run the schema binary with the given kind argument and return stdout.

    Asserts a clean exit and non-empty stdout. Failures bubble up with
    full stderr so red-bar diagnostics are useful.
    """
    result = subprocess.run(
        [str(binary), kind],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, (
        f"ocx_schema {kind} failed (exit {result.returncode})\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert result.stdout, f"ocx_schema {kind} produced empty stdout"
    return result.stdout


@pytest.fixture()
def parsed_schema(
    schema_binary: Path, request: pytest.FixtureRequest
) -> Iterator[dict]:
    """Parametrized fixture: run the binary, parse stdout as JSON."""
    kind, _expected_id, _label = request.param
    raw = _run_schema(schema_binary, kind)
    try:
        yield json.loads(raw)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"ocx_schema {kind} produced invalid JSON: {exc}\n"
            f"first 500 chars: {raw[:500]}"
        )


@pytest.mark.parametrize(
    "parsed_schema",
    SCHEMA_VARIANTS,
    indirect=True,
    ids=[v[2] for v in SCHEMA_VARIANTS],
)
def test_schema_variant_emits_canonical_id(
    parsed_schema: dict, request: pytest.FixtureRequest
) -> None:
    """Each schema variant must emit the canonical published ``$id`` URL."""
    expected_id = request.node.callspec.params["parsed_schema"][1]
    actual_id = parsed_schema.get("$id")
    assert actual_id == expected_id, (
        f"schema $id mismatch: expected {expected_id!r}, got {actual_id!r}"
    )


def test_project_schema_describes_tools_and_groups(
    schema_binary: Path,
) -> None:
    """The project schema must declare object-typed ``tools`` and a
    named-groups property (``groups`` per plan spec, or ``group`` per
    current ``rename`` source). Failing here means BOTH are missing."""
    raw = _run_schema(schema_binary, "project")
    schema = json.loads(raw)
    properties = schema.get("properties")
    assert isinstance(properties, dict), (
        "project schema must declare a top-level `properties` object"
    )

    tools = properties.get("tools")
    assert isinstance(tools, dict), (
        "project schema must declare `properties.tools`"
    )
    assert tools.get("type") == "object", (
        "`properties.tools.type` must be `object`"
    )

    groups = properties.get("groups") or properties.get("group")
    assert isinstance(groups, dict), (
        f"project schema must declare a named-groups property "
        f"(`properties.groups` or `properties.group`). "
        f"Properties present: {list(properties.keys())}"
    )
    assert groups.get("type") == "object", (
        "named-groups property must be an `object`"
    )


def test_project_lock_schema_carries_machine_generated_comment(
    schema_binary: Path,
) -> None:
    """Architect path-1 finding: the project-lock schema must carry a
    top-level ``$comment`` flagging the format as machine-generated and
    subject to evolution. Mirrors the unit-level assertion in
    ``crates/ocx_schema/tests/schema_outputs.rs``.
    """
    raw = _run_schema(schema_binary, "project-lock")
    schema = json.loads(raw)
    comment = schema.get("$comment")
    assert isinstance(comment, str), (
        "project-lock schema must carry a top-level `$comment` "
        "(architect path-1 finding — flag the format as machine-generated)"
    )
    lower = comment.lower()
    assert "machine" in lower, (
        f"project-lock $comment must mention `machine`: {comment!r}"
    )
    assert (
        "evolve" in lower or "evolution" in lower or "evolving" in lower
    ), (
        f"project-lock $comment must indicate the format may evolve: "
        f"{comment!r}"
    )
    assert len(comment) >= 60, (
        f"$comment too short to be useful: {comment!r}"
    )


def test_project_lock_schema_pins_lock_version_to_one(
    schema_binary: Path,
) -> None:
    """The project-lock schema must constrain ``lock_version`` to
    ``[1]`` so a future v2 manuscript fed to a v1 schema fails
    validation rather than being silently accepted.
    """
    raw = _run_schema(schema_binary, "project-lock")
    schema = json.loads(raw)

    metadata = schema.get("properties", {}).get("metadata")
    assert isinstance(metadata, dict), (
        "project-lock schema must declare `properties.metadata`"
    )

    # `metadata` may be inline or a $ref into $defs/LockMetadata.
    metadata_props = metadata.get("properties")
    if metadata_props is None and "$ref" in metadata:
        ref = metadata["$ref"]
        target = ref.split("/")[-1]
        metadata_props = (
            schema.get("$defs", {}).get(target, {}).get("properties")
        )
    assert isinstance(metadata_props, dict), (
        "project-lock metadata must expose a `properties` object "
        "(inline or via $ref into $defs)"
    )

    lock_version = metadata_props.get("lock_version")
    assert lock_version is not None, (
        "metadata must declare a `lock_version` property"
    )

    # `lock_version` may be inline or a $ref into $defs/LockVersion.
    if "$ref" in lock_version:
        ref = lock_version["$ref"]
        target = ref.split("/")[-1]
        lock_version = schema.get("$defs", {}).get(target, {})

    enum_values = lock_version.get("enum")
    assert enum_values == [1], (
        f"lock_version must constrain to `[1]` (rejects v2 manuscripts); "
        f"got {enum_values!r}"
    )


def test_unknown_schema_kind_exits_nonzero(schema_binary: Path) -> None:
    """The binary must surface unknown kinds as a usage error so callers
    don't silently accept unsupported variants.
    """
    result = subprocess.run(
        [str(schema_binary), "nonsense-kind"],
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0, (
        "unknown schema kind must produce a non-zero exit code; "
        f"got {result.returncode}\nstdout: {result.stdout}\n"
        f"stderr: {result.stderr}"
    )
