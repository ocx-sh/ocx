# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package sbom --dependencies`.

By default `sbom` lists the attestations of the named package alone. With
`--dependencies` it lists those of every package in the dependency closure for the
selected platform, each verified against that package's own signer set (its
operator `[[trust.policy]]`), never as one merged document. Every entry carries the
identifier of the package it belongs to.
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from pathlib import Path

import pytest

from src.helpers import make_package
from src.index_signing import authority, stack_table
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures.sigstore_stack import SigstoreStack
from tests.test_attest import attest, cyclonedx_predicate

pytestmark = pytest.mark.command("package_sbom*")

OTHER_IDENTITY = "someone-else@example.com"


@dataclass(slots=True)
class Closure:
    root: PackageInfo
    dependency: PackageInfo


def policy(
    ocx: OcxRunner, pkg: PackageInfo, stack: SigstoreStack, identity: str
) -> str:
    return (
        f'[[trust.policy]]\nscope = "{ocx.registry}/{pkg.repo}"\n'
        f'signers = [{{ kind = "keyless", identity = "{identity}", oidc_issuer = "{stack.issuer}", sigstore = "{authority(stack)}" }}]\n'
    )


@pytest.fixture()
def closure(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> Closure:
    """A root package depending on a second one; both carry a signed SBOM."""
    dependency = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    digest = fetch_platform_manifest_digest(
        ocx.registry, dependency.repo, dependency.tag
    )
    entry = {"identifier": f"{dependency.fq}@{digest}", "visibility": "public"}
    root = make_package(
        ocx, f"{unique_repo}_root", "1.0.0", tmp_path, dependencies=[entry]
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(stack_table(sigstore_stack))
    predicate = cyclonedx_predicate(tmp_path)
    for pkg in (dependency, root):
        result = attest(ocx, sigstore_stack, identity_token, pkg, predicate)
        assert result.returncode == 0, f"attest setup failed: {result.stderr}"
    return Closure(root, dependency)


def trust(
    ocx: OcxRunner,
    stack: SigstoreStack,
    closure: Closure,
    *,
    dependency_identity: str | None = None,
) -> None:
    text = stack_table(stack)
    text += policy(ocx, closure.root, stack, stack.identity)
    text += policy(
        ocx, closure.dependency, stack, dependency_identity or stack.identity
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(text)


def sbom(ocx: OcxRunner, ref: str, *flags: str) -> subprocess.CompletedProcess[str]:
    return ocx.run(
        "package", "sbom", "--platform", current_platform(), *flags, ref, check=False
    )


def packages_of(entries: list[dict]) -> set[str]:
    return {entry["package"] for entry in entries}


def test_sbom_without_the_flag_lists_the_root_documents_only_and_names_the_package(
    ocx: OcxRunner, closure: Closure, sigstore_stack: SigstoreStack
) -> None:
    trust(ocx, sigstore_stack, closure)

    result = sbom(ocx, closure.root.short)

    assert result.returncode == 0, result.stderr
    [entry] = json.loads(result.stdout)["attestations"]
    assert closure.root.repo in entry["package"]
    assert closure.dependency.repo not in entry["package"]


def test_dependencies_lists_every_closure_package_verified_on_its_own_signers(
    ocx: OcxRunner, closure: Closure, sigstore_stack: SigstoreStack
) -> None:
    trust(ocx, sigstore_stack, closure)

    result = sbom(ocx, closure.root.short, "--dependencies")

    assert result.returncode == 0, result.stderr
    document = json.loads(result.stdout)
    owners = packages_of(document["attestations"])
    for repo in (closure.root.repo, closure.dependency.repo):
        assert any(repo in owner for owner in owners), f"no entry for {repo}: {owners}"
    assert len(document["attestations"]) == 2, "one entry set per package, never merged"
    assert document["refused"] == []


def test_a_dependency_signed_by_someone_its_own_policy_does_not_list_aborts_the_listing(
    ocx: OcxRunner, closure: Closure, sigstore_stack: SigstoreStack
) -> None:
    trust(ocx, sigstore_stack, closure, dependency_identity=OTHER_IDENTITY)

    result = sbom(ocx, closure.root.short, "--dependencies")

    assert result.returncode == 77, (
        "one untrusted signer in the closure fails the run, never a half-trusted listing: "
        + result.stderr
    )
    assert closure.dependency.repo in result.stderr, "the refused package is named"


def test_the_last_of_the_dependencies_flag_pair_wins(
    ocx: OcxRunner, closure: Closure, sigstore_stack: SigstoreStack
) -> None:
    trust(ocx, sigstore_stack, closure)

    result = sbom(ocx, closure.root.short, "--dependencies", "--no-dependencies")

    assert result.returncode == 0, result.stderr
    assert len(json.loads(result.stdout)["attestations"]) == 1


@pytest.mark.parametrize("output", ["-", "sbom.json"])
def test_dependencies_conflicts_with_output(
    ocx: OcxRunner, closure: Closure, tmp_path: Path, output: str
) -> None:
    target = output if output == "-" else str(tmp_path / output)

    result = sbom(ocx, closure.root.short, "--dependencies", "--output", target)

    assert result.returncode == 64, result.stdout + result.stderr


def test_dependencies_conflicts_with_a_pinned_identity(
    ocx: OcxRunner, closure: Closure
) -> None:
    result = sbom(
        ocx,
        closure.root.short,
        "--dependencies",
        "--certificate-identity",
        "someone@example.com",
        "--certificate-oidc-issuer",
        "https://issuer.example.com",
    )

    assert result.returncode == 64, result.stdout + result.stderr


def test_a_dependency_that_was_never_signed_is_one_refused_entry_not_an_abort(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    unsigned = make_package(ocx, f"{unique_repo}_unsigned", "1.0.0", tmp_path)
    digest = fetch_platform_manifest_digest(ocx.registry, unsigned.repo, unsigned.tag)
    entry = {"identifier": f"{unsigned.fq}@{digest}", "visibility": "public"}
    root = make_package(
        ocx, f"{unique_repo}_root", "1.0.0", tmp_path, dependencies=[entry]
    )
    result = attest(
        ocx, sigstore_stack, identity_token, root, cyclonedx_predicate(tmp_path)
    )
    assert result.returncode == 0, f"attest setup failed: {result.stderr}"
    text = stack_table(sigstore_stack)
    text += policy(ocx, root, sigstore_stack, sigstore_stack.identity)
    text += policy(ocx, unsigned, sigstore_stack, sigstore_stack.identity)
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(text)

    listed = sbom(ocx, root.short, "--dependencies")

    assert listed.returncode == 0, listed.stderr
    document = json.loads(listed.stdout)
    assert document["summary"]["status"] == "partial_failure", document
    [refused] = document["refused"]
    assert unsigned.repo in refused["package"]
    assert refused["reason_kind"] == "no_signatures_found"
    assert any(root.repo in owner for owner in packages_of(document["attestations"]))

    only_root = sbom(ocx, unsigned.short)
    assert only_root.returncode == 79, "an unsigned root keeps its error: " + only_root.stderr
