"""the publisher-trust expectation matrix against the public fixtures.

Fixtures: `ocx-sh/e2e-package` (GHCR tags) and `ocx-sh/e2e-index` (GitHub Pages index,
logical registry `e2e.ocx.sh`). Every case asserts the exit code and the `error.detail` slug.
"""

from __future__ import annotations

import json
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

REGISTRY = "e2e.ocx.sh"
Run = Callable[..., subprocess.CompletedProcess[str]]

pytestmark = pytest.mark.e2e_public


def pkg(root: str, tag: str) -> str:
    return f"{REGISTRY}/e2e/{root}:{tag}"


def slug(result: subprocess.CompletedProcess[str]) -> str:
    return json.loads(result.stdout)["error"]["detail"]


def assert_refused(result: subprocess.CompletedProcess[str], code: int, detail: str) -> None:
    assert result.returncode == code, f"expected {code}, got {result.returncode}\n{result.stdout}\n{result.stderr}"
    assert slug(result) == detail


def assert_installs(result: subprocess.CompletedProcess[str]) -> None:
    assert result.returncode == 0, f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"


@pytest.mark.parametrize("root", ["claims", "oid"])
@pytest.mark.parametrize(
    ("tag", "code", "detail"),
    [
        ("main", 0, None),
        ("key", 0, None),
        ("tag", 77, "publisher_mismatch"),
        ("other", 77, "publisher_mismatch"),
        ("unsigned", 79, "no_signatures_found"),
    ],
)
def test_claims_roots_verify_against_declared_publishers(run_ocx: Run, root: str, tag: str, code: int, detail: str | None) -> None:
    result = run_ocx("package", "install", pkg(root, tag))
    if detail is None:
        assert_installs(result)
    else:
        assert_refused(result, code, detail)


def test_tagged_root_accepts_the_tag_triggered_release(run_ocx: Run) -> None:
    assert_installs(run_ocx("package", "install", pkg("tagged", "tag")))


def test_root_without_publishers_installs_unsigned(run_ocx: Run) -> None:
    assert_installs(run_ocx("package", "install", pkg("none", "unsigned")))


def test_root_without_publishers_is_refused_under_required(run_ocx: Run) -> None:
    result = run_ocx("package", "install", pkg("none", "unsigned"), signatures="required")
    assert_refused(result, 81, "publishers_required")


def test_root_with_only_unrecognised_future_entry_is_refused(run_ocx: Run) -> None:
    # `future` carries a `{"kind": "future"}` entry beside a keyless entry with an extra `sigstore`
    # field; `sigstore` is unrecognised, so both entries are, and the root has no usable publisher.
    result = run_ocx("package", "install", pkg("future", "main"))
    assert_refused(result, 82, "publishers_unrecognized")


def test_sbom_of_two_attestations_returns_the_newer(run_ocx: Run, tmp_path: Path) -> None:
    # The fixture's two CycloneDX SBOMs differ only by the property `e2e:sequence` (1 older, 2 newer).
    ref = pkg("claims", "two-sboms")
    platform = ("--platform", "linux/amd64+libc.glibc")
    listing = run_ocx("package", "sbom", *platform, ref)
    assert listing.returncode == 0, listing.stdout + listing.stderr
    assert len(json.loads(listing.stdout)["attestations"]) >= 2, "the fixture publishes two SBOMs minutes apart"

    out = tmp_path / "sbom.json"
    extracted = run_ocx("package", "sbom", *platform, "--output", str(out), ref)
    assert extracted.returncode == 0, extracted.stdout + extracted.stderr
    sbom = json.loads(out.read_text(encoding="utf-8"))
    assert sbom["bomFormat"] == "CycloneDX"
    sequence = [p["value"] for p in sbom["metadata"]["properties"] if p["name"] == "e2e:sequence"]
    assert sequence == ["2"], "the older SBOM (sequence 1) came back instead of the newer"


@pytest.mark.parametrize(("tag", "code", "detail"), [("main", 0, None), ("other", 77, "publisher_mismatch")])
def test_attestation_verify_with_no_flags_uses_the_declared_publishers(
    run_ocx: Run, tag: str, code: int, detail: str | None,
) -> None:
    result = run_ocx("package", "verify", "--attestation", "--platform", "linux/amd64+libc.glibc", pkg("claims", tag))
    if detail is None:
        assert_installs(result)
    else:
        assert_refused(result, code, detail)
