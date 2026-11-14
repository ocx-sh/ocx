"""the release cycle of the package's own pipeline, against the public fixtures.

The fixtures are `ghcr.io/ocx-sh/e2e-package` version tags published by that repository's `release.yml` and
`backfill.yml`, and the `e2e/release` root of `ocx-sh/e2e-index` (logical registry `e2e.ocx.sh`) that lists them. A
case skips with its reason while the tag or the row it needs does not exist yet; once it does, it is a real check.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import urllib.error
import urllib.request
from collections.abc import Callable

import pytest

REGISTRY = "e2e.ocx.sh"
INDEX_URL = os.environ.get("OCX_E2E_INDEX_URL", "https://ocx-sh.github.io/e2e-index")

Run = Callable[..., subprocess.CompletedProcess[str]]

pytestmark = pytest.mark.e2e_public

PLATFORMS = ("linux/amd64+libc.glibc", "linux/arm64+libc.glibc")
RELEASE = "0.1.0"
BACKFILLED = "0.0.1"
PRUNED_CANDIDATE = re.compile(r"^0\.1\.0-rc[1-9][0-9]*$")  # the 0.1.0 candidates the 0.1.1 release prunes
ANY_CANDIDATE = re.compile(r"^(\d+\.\d+\.\d+)-rc[1-9][0-9]*$")
needs_new_client = pytest.mark.usefixtures("new_client")
RELEASE_IDENTITY = "/ocx-sh/e2e-package/.github/workflows/release.yml@refs/heads/main"
BACKFILL_IDENTITY = "/ocx-sh/e2e-package/.github/workflows/backfill.yml@refs/heads/main"


def pkg(tag: str) -> str:
    return f"{REGISTRY}/e2e/release:{tag}"


@pytest.fixture(scope="module")
def rows() -> dict[str, dict]:
    """The tag rows of the published `e2e/release` root; skips the module when the root is not served yet."""
    try:
        with urllib.request.urlopen(f"{INDEX_URL}/p/e2e/release.json", timeout=30) as response:
            return json.load(response)["tags"]
    except urllib.error.HTTPError as error:
        if error.code == 404:
            pytest.skip("the e2e/release root is not published yet")
        raise


def need(rows: dict[str, dict], tag: str, why: str) -> None:
    if tag not in rows:
        pytest.skip(f"{tag} has no row in e2e/release yet: {why}")


def attestations(run_ocx: Run, tag: str, platform: str | None) -> list[dict]:
    argv = ["package", "sbom", *(("--platform", platform) if platform else ()), pkg(tag)]
    result = run_ocx(*argv)
    assert result.returncode == 0, f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
    return json.loads(result.stdout)["attestations"]


def assert_ok(result: subprocess.CompletedProcess[str]) -> None:
    assert result.returncode == 0, f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"


def test_release_installs(run_ocx: Run, rows: dict[str, dict]) -> None:
    need(rows, RELEASE, "published by release.yml mode=release")
    assert_ok(run_ocx("package", "install", pkg(RELEASE)))


@needs_new_client
@pytest.mark.parametrize("platform", PLATFORMS)
def test_release_is_signed_and_attested_on_every_platform(run_ocx: Run, rows: dict[str, dict], platform: str) -> None:
    need(rows, RELEASE, "published by release.yml mode=release")
    assert_ok(run_ocx("package", "verify", "--platform", platform, pkg(RELEASE)))
    entries = attestations(run_ocx, RELEASE, platform)
    assert entries, f"no SBOM attestation on {platform}"
    assert all(e["verified"] and e["certificate_identity"].endswith(RELEASE_IDENTITY) for e in entries)


@needs_new_client
def test_verify_and_sbom_need_no_platform_flag(run_ocx: Run, rows: dict[str, dict]) -> None:
    # The host default: the same call exits 79 on ocx 0.6.6, which looks at the unsigned image index.
    need(rows, RELEASE, "published by release.yml mode=release")
    assert_ok(run_ocx("package", "verify", pkg(RELEASE)))
    assert attestations(run_ocx, RELEASE, None)


def test_backfilled_version_installs_under_the_backfill_identity(run_ocx: Run, rows: dict[str, dict]) -> None:
    # Before backfill.yml has run, 0.0.1 carries no signature and this install exits 79 (the recorded red run).
    need(rows, BACKFILLED, "published unsigned by unsigned-v.yml, listed once announced")
    assert_ok(run_ocx("package", "install", pkg(BACKFILLED)))


@needs_new_client
def test_second_backfill_adds_no_referrer(run_ocx: Run, rows: dict[str, dict]) -> None:
    # The run that backfills 0.0.1 attests it once; every later run skips it. After two or more runs the backfill
    # identity therefore still has exactly one attestation on the platform the unsigned version was published for.
    need(rows, BACKFILLED, "published unsigned by unsigned-v.yml, listed once announced")
    entries = attestations(run_ocx, BACKFILLED, PLATFORMS[0])
    by_backfill = [e for e in entries if e["certificate_identity"].endswith(BACKFILL_IDENTITY)]
    assert len(by_backfill) == 1, f"{len(by_backfill)} attestations by the backfill identity: {entries}"


def test_candidate_installs_by_pin_and_leaves_the_floating_tags(run_ocx: Run, rows: dict[str, dict]) -> None:
    # A standing candidate: `X.Y.Z-rcN` whose line X.Y.Z has no final-release row (so no prune has removed it).
    standing = sorted(
        tag for tag in rows if (m := ANY_CANDIDATE.match(tag)) and m.group(1) not in rows
    )
    if not standing:
        pytest.skip("no standing X.Y.Z-rcN candidate (line without a final row) in e2e/release yet")
    candidate = standing[0]
    assert rows[candidate].get("ephemeral") is True, f"{candidate} is not marked ephemeral"
    assert_ok(run_ocx("package", "install", pkg(candidate)))
    for floating in ("latest", "0.1", "0"):
        if floating in rows:
            assert rows[floating]["content"] != rows[candidate]["content"], f"{floating} moved onto {candidate}"


def test_pruned_candidate_is_gone(run_ocx: Run, rows: dict[str, dict]) -> None:
    # A later release (0.1.1, prune_after_days=0) prunes the candidates of 0.1.0; its own row is the precondition,
    # so this cannot pass merely because the candidate was never announced.
    need(rows, "0.1.1", "the release run that prunes the candidates of 0.1.0")
    assert not any(PRUNED_CANDIDATE.match(tag) for tag in rows), "a candidate row survived the prune"
    result = run_ocx("package", "install", pkg("0.1.0-rc1"))
    assert result.returncode == 79, f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
