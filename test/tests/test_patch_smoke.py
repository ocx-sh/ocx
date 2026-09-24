# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The `patch` verb's smoke-tier happy path (plan_crate_split_workspace.md C-013).

Every other `ocx patch` acceptance test lives in `test_patches.py`. A smoke
tier wants one parallel-safe test, so this one carries no `xdist_group` and
never goes near the bare registry's global descriptor at `<registry>/global`,
the one slot `test_patches.py`'s grouped tests write: the patch registry it
names is a UUID-scoped path under the test registry (so even the `global`
repository under it is this test's own), and `ocx patch test` composes the
descriptor from the file it is handed rather than from anything published.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("patch_test")


@pytest.mark.smoke
def test_patch_test_composes_a_companion_onto_a_base(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx patch test --descriptor <file> --registry <tier> <base>` dry-runs the
    descriptor: the companion's INTERFACE env var appears in the composed
    entries, and the report names the companion.

    `--registry` stands in for a `[patches]` config tier, so no config file is
    written and nothing is published — the base and the companion are the only
    registry writes, both under this test's own UUID-scoped repositories.
    """
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    companion_repo = f"{unique_repo}_companion"
    companion = make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path,
        bins=[],
        env=[
            {
                "key": "SMOKE_PATCH_VAR",
                "type": "constant",
                "value": "smoke-value",
                "visibility": "interface",
            }
        ],
        cascade=True,
        # An env-only companion is the canonical `any`-published package, and
        # `patch test` fans a descriptor out over the concrete platform matrix.
        platform="any",
    )

    descriptor = tmp_path / "smoke_descriptor.json"
    descriptor.write_text(
        json.dumps({"version": 1, "rules": [{"match": "*", "packages": [companion.fq]}]})
    )

    result = ocx.run(
        "patch", "test",
        "--descriptor", str(descriptor),
        "--registry", f"{registry}/{unique_repo}_patches",
        base.short,
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"`ocx patch test` must compose the descriptor; rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )

    report = json.loads(result.stdout)
    entries = report["entries"]
    assert entries, "the composed report must carry entries, not an empty env"
    smoke_var = next((e for e in entries if e["key"] == "SMOKE_PATCH_VAR"), None)
    assert smoke_var is not None, (
        f"SMOKE_PATCH_VAR must appear in the composed entries; got keys: "
        f"{[e['key'] for e in entries]}"
    )
    assert smoke_var["value"] == "smoke-value"
    assert report["companions"], "the report must list the matched companion"
