# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Signing a package again under the same identity adds no second referrer (ocx#701).

A release re-dispatch re-runs `package sign` over manifests it already signed;
each run used to attach one more bundle referrer per platform. Keyless and key
mode both name one identity across the two runs here: the same token file, or
the same key, so the second run must find the first referrer and write nothing.
An attestation is skipped only for the same document: a changed SBOM is attached.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from src.helpers import make_package
from src.registry import fetch_manifest_digest, list_referrers
from src.runner import OcxRunner
from tests.fixtures.sigstore_stack import SigstoreStack

pytestmark = pytest.mark.command("package_sign", "package_attest")

SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"
_KEYS = Path(__file__).parent / "fixtures" / "golden" / "keys"
COSIGN_KEY = _KEYS / "cosign.key"
KEY_PASSWORD = "ocxtest"


def _sign(
    ocx: OcxRunner, mode: str, stack: SigstoreStack, token: Path, ref: str
) -> dict:
    flags = ["--signature-format", "bundle"]
    env: dict[str, str] = {}
    if mode == "key":
        flags += ["--key", str(COSIGN_KEY)]
        env["OCX_KEY_PASSWORD"] = KEY_PASSWORD
    else:
        flags += [
            "--fulcio-url",
            stack.fulcio_url,
            "--rekor-url",
            stack.rekor_url,
            "--identity-token-file",
            str(token),
        ]
    result = ocx.run("package", "sign", *flags, ref, check=False, env_overrides=env)
    assert result.returncode == 0, (
        f"sign failed ({result.returncode})\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)


def _bundle_referrers(registry: str, repo: str, digest: str) -> list[dict]:
    status, index = list_referrers(registry, repo, digest)
    assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
    return [
        m for m in index["manifests"] if m.get("artifactType") == SIGSTORE_BUNDLE_V03
    ]


@pytest.mark.parametrize("mode", ("keyless", "key"))
def test_signing_twice_under_the_same_identity_keeps_one_referrer(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    mode: str,
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "pkg")
    ref = f"{unique_repo}:1.0.0"
    subject = fetch_manifest_digest(ocx.registry, unique_repo, "1.0.0")

    _sign(ocx, mode, sigstore_stack, identity_token, ref)
    before = _bundle_referrers(ocx.registry, unique_repo, subject)
    assert len(before) == 1, f"the first sign must write one referrer: {before!r}"

    _sign(ocx, mode, sigstore_stack, identity_token, ref)
    after = _bundle_referrers(ocx.registry, unique_repo, subject)
    assert [m["digest"] for m in after] == [before[0]["digest"]], (
        f"a second sign under the same identity changed the referrers: {after!r}"
    )


def _attest(
    ocx: OcxRunner, stack: SigstoreStack, token: Path, ref: str, predicate: Path
) -> dict:
    result = ocx.run(
        "package",
        "attest",
        *stack.sign_args(token),
        "--predicate",
        str(predicate),
        "--type",
        "cyclonedx",
        ref,
        check=False,
    )
    assert result.returncode == 0, (
        f"attest failed ({result.returncode})\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)


def _sbom(path: Path, version: int) -> Path:
    path.write_text(
        json.dumps(
            {"bomFormat": "CycloneDX", "specVersion": "1.6", "version": version}
        ),
        encoding="utf-8",
    )
    return path


def test_attesting_the_same_document_twice_keeps_one_referrer(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "pkg")
    ref = f"{unique_repo}:1.0.0"
    predicate = _sbom(tmp_path / "sbom.cdx.json", 1)

    subject = _attest(ocx, sigstore_stack, identity_token, ref, predicate)[
        "subject_digest"
    ]
    before = _bundle_referrers(ocx.registry, unique_repo, subject)
    assert len(before) == 1, f"the first attest must write one referrer: {before!r}"

    _attest(ocx, sigstore_stack, identity_token, ref, predicate)
    after = _bundle_referrers(ocx.registry, unique_repo, subject)
    assert [m["digest"] for m in after] == [before[0]["digest"]], (
        f"re-attesting the same document changed the referrers: {after!r}"
    )


def test_attesting_a_changed_document_adds_a_second_referrer(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "pkg")
    ref = f"{unique_repo}:1.0.0"

    first = _sbom(tmp_path / "old.cdx.json", 1)
    subject = _attest(ocx, sigstore_stack, identity_token, ref, first)["subject_digest"]
    before = _bundle_referrers(ocx.registry, unique_repo, subject)
    assert len(before) == 1, f"the first attest must write one referrer: {before!r}"

    _attest(
        ocx, sigstore_stack, identity_token, ref, _sbom(tmp_path / "new.cdx.json", 2)
    )
    after = _bundle_referrers(ocx.registry, unique_repo, subject)
    assert len(after) == 2, (
        f"a corrected document under the same identity must be attached: {after!r}"
    )
