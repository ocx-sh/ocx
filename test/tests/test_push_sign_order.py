# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""``ocx package push --sign/--sbom`` writes no tag before every platform manifest it reaches is signed.

A push whose signing fails after the manifest landed must leave the destination's tag set unchanged:
no version tag, no cascade tag and no keep tag points at unsigned content from a run that reported red.
Key mode with an unset ``env://`` reference is the failure: it resolves at signing time, after the
platform manifest is already on the registry.
"""
from __future__ import annotations

import json
import urllib.request
from pathlib import Path

import pytest

from src.helpers import make_package
from src.runner import OcxRunner
from tests.fixtures import attestations
from tests.test_push import manifest_status, push

pytestmark = pytest.mark.command("package_push")

UNSET_KEY = ["--key", "env://OCX_TEST_SIGNING_KEY_THAT_IS_NOT_SET"]


def registry_tags(registry: str, repo: str) -> set[str]:
    with urllib.request.urlopen(f"http://{registry}/v2/{repo}/tags/list", timeout=10) as response:
        return set(json.load(response).get("tags") or [])


@pytest.mark.parametrize("target", ["sign", "sbom"])
def test_a_push_whose_signing_fails_writes_no_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, target: str
) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    before = registry_tags(ocx.registry, pkg.repo)

    if target == "sign":
        flags = ["--sign"]
    else:
        sbom = tmp_path / "sbom.cdx.json"
        sbom.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
        flags = ["--sbom", str(sbom)]
    identifier = pkg.fq.rsplit(":", 1)[0] + ":2.0.0"
    result = push(ocx, pkg, tmp_path, ["--cascade", *flags, *UNSET_KEY], identifier=identifier)

    assert result.returncode != 0, f"a signing failure must fail the push: {result.stdout}"
    error = json.loads(result.stdout)["error"]
    assert error["detail"] == "key_unreadable", f"the push must fail at the signing step, not earlier: {error}"
    # The signing error names the staged platform manifest by digest: staging ran, tagging did not.
    staged_ref = error["context"]["identifier"]
    assert "@sha256:" in staged_ref, f"the signing error must name the staged manifest digest: {staged_ref}"
    staged_digest = staged_ref.rsplit("@", 1)[1]
    assert manifest_status(ocx.registry, pkg.repo, staged_digest) == 200, (
        f"the platform manifest must be staged by digest before signing: {staged_digest}"
    )
    after = registry_tags(ocx.registry, pkg.repo)
    assert after == before, f"tags written by a push that failed to sign: {sorted(after - before)}"
