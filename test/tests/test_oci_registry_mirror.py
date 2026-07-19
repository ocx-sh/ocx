# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for client-declared OCI registry mirror support.

These tests run against the CURRENT STUBS (Phase 3, contract-first TDD).  All
six scenarios are expected to FAIL against the stubs — the contracts they
encode are the Phase 4 implementation targets.

Design record:
    .claude/artifacts/adr_oci_registry_mirror.md  (behaviour authority)
    .claude/state/plans/plan_oci_registry_mirror.md  (scenarios 1–6, Step 3.2b)

Dual-registry harness:
    The ``mirror_registry`` fixture (test/conftest.py) provides a second
    ``registry:2`` service at localhost:5001.  Tests are automatically skipped
    if that service is not reachable so a single-registry environment does not
    regress.

Poison (digest mismatch) gate:
    Covered at the unit layer by
    ``oci::client::tests::verify_blob_digest_rejects_tampered_content_and_deletes_blob``
    (client.rs).  A corrupting-proxy acceptance test is **deferred** —
    ``registry:2`` cannot host poisoned bytes (it is content-addressed and
    rejects blobs whose bytes ≠ digest), so a real proxy-intercept mock HTTP
    server would be needed.  This note is here so the deferral is visible, not
    silently skipped.
"""
from __future__ import annotations

import re
import subprocess
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_REPOSITORY_RE = re.compile(r'repository\s*=\s*"([^"@:]+(?::\d+)?/[^"@:]+)"')
"""Matches the V2 ``repository = "<registry>/<repo>"`` coordinate in ocx.lock.

The V2 lock stores a bare registry/repo coordinate per tool (no tag, no
digest) plus a per-platform leaf map; the canonical registry must appear here,
never the mirror host. Reuses the regex shape from test_lock.py so the
lockfile format is asserted consistently across the suite.
"""


def write_home_config(ocx: OcxRunner, content: str) -> Path:
    """Write *content* to ``$OCX_HOME/config.toml``.

    Mirrors the helper in test_config.py (DAMP — tests readable in isolation).
    """
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    path.write_text(content)
    return path


# Accept header covering both single-platform manifests and image indexes.
# registry:2 returns 404 for a HEAD whose Accept set does not include the
# stored manifest's media type, so an OCX package (pushed as an OCI image
# index at the tag) is only visible via HEAD when the index media type is
# advertised. Mirror these the way the OCI client does so the HEAD probe
# reflects real presence, not an Accept-header mismatch.
_MANIFEST_ACCEPT = ", ".join(
    [
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    ]
)


def head_manifest(registry: str, repo: str, tag: str) -> int:
    """Return the HTTP status for ``HEAD /v2/<repo>/manifests/<tag>``.

    Returns 200 when present, 404 when absent, -1 on connection error.
    Used to verify that a package is or is not present on a given registry
    without downloading the full manifest. The Accept header advertises the
    OCI image-index media type so registry:2 reports an OCX package (stored as
    an image index) as present rather than 404 on an Accept mismatch.
    """
    url = f"http://{registry}/v2/{repo}/manifests/{tag}"
    req = urllib.request.Request(
        url, method="HEAD", headers={"Accept": _MANIFEST_ACCEPT}
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status
    except urllib.error.HTTPError as exc:
        return exc.code
    except (urllib.error.URLError, OSError):
        return -1


def run_with_env(
    ocx: OcxRunner,
    *args: str,
    extra_env: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run an ocx command with extra env vars merged into the runner env."""
    env = {**ocx.env}
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary)] + list(args)
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)
    if check and result.returncode != 0:
        raise AssertionError(
            f"ocx {' '.join(args)} failed (rc={result.returncode})\n"
            f"stdout: {result.stdout.strip()}\n"
            f"stderr: {result.stderr.strip()}"
        )
    return result


def _registry_slug(registry: str) -> str:
    """Filesystem-safe registry name (mirrors OCX's relaxed-slug encoding)."""
    return re.sub(r"[^a-zA-Z0-9._-]", "_", registry)


# ---------------------------------------------------------------------------
# Scenario 1 — mirror routing: push to mirror only, install via upstream id
# ---------------------------------------------------------------------------


def test_mirror_install_routes_to_configured_mirror(
    ocx: OcxRunner,
    registry: str,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Package pushed to the *mirror* only; upstream id installed → served by mirror.

    Success proves OCX routed the read to the mirror and not to the upstream.
    The package is absent from the upstream so a direct pull would fail —
    success is only possible if routing worked.

    Traces: plan scenario 1 — "push tool to mirror only; configure
    ``[mirrors] "<upstream>" = "<mirror>"``; ``ocx package install <upstream>``
    → served by mirror"; ADR R2 (replace semantics).
    """
    # Create a runner that targets the mirror registry for pushing.
    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)

    mirror_pkg = make_package(mirror_ocx, unique_repo, "1.0.0", tmp_path)

    # Precondition: absent from the upstream registry.
    assert head_manifest(registry, unique_repo, "1.0.0") == 404, (
        "package must be absent from upstream before the test proves mirror routing"
    )

    # Configure the mirror: upstream registry → mirror registry.
    write_home_config(
        ocx,
        f'[mirrors]\n"{registry}" = "http://{mirror_registry}"\n',
    )

    # Install using the upstream identifier — OCX must route to the mirror.
    # The mirror is a plain-HTTP endpoint, so its host must be listed in
    # OCX_INSECURE_REGISTRIES (ADR F2: the mirror host is what gets contacted).
    fq = f"{registry}/{unique_repo}:1.0.0"
    run_with_env(
        ocx,
        "package",
        "install",
        fq,
        extra_env={"OCX_INSECURE_REGISTRIES": f"{registry},{mirror_registry}"},
    )

    # Verify the package installed (candidate symlink present).
    home = Path(ocx.env["OCX_HOME"])
    from src.assertions import assert_symlink_exists  # noqa: PLC0415
    assert_symlink_exists(
        home
        / "symlinks"
        / _registry_slug(registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )

    # Bidirectional negative: the mirror-routed install must NOT have populated
    # the upstream/canonical registry as a side effect. Replace semantics route
    # reads to the mirror only; OCX never writes to the origin on the read path.
    # The origin must still 404 for the repo after a successful install.
    assert head_manifest(registry, unique_repo, "1.0.0") == 404, (
        "the canonical (upstream) registry must STILL be empty after a "
        "mirror-routed install — OCX must not populate the origin as a "
        "side effect of reading through the mirror"
    )


# ---------------------------------------------------------------------------
# Scenario 2 — push not redirected: canonical registry receives push
# ---------------------------------------------------------------------------


def test_mirror_push_targets_canonical_registry_not_mirror(
    ocx: OcxRunner,
    registry: str,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """With a mirror configured, ``ocx package push`` lands on the canonical
    registry, NOT on the mirror.

    Precondition: assert both registries are empty before pushing (plan
    Codex F7 — both registries are writable registry:2 instances so the
    negative must be established explicitly first).

    Traces: plan scenario 2 — "push not redirected: canonical registry gets
    manifest; mirror still 404"; ADR Q5 (push stays canonical, read-only proxy).
    """
    # Precondition: both registries are empty.
    assert head_manifest(registry, unique_repo, "1.0.0") == 404, (
        "upstream must be empty before the test"
    )
    assert head_manifest(mirror_registry, unique_repo, "1.0.0") == 404, (
        "mirror must be empty before the test"
    )

    # Configure the mirror so we prove push ignores it. The plain-HTTP mirror
    # host must be listed in OCX_INSECURE_REGISTRIES (ADR F2) so the cascade
    # tag-listing read can reach it; the push itself stays canonical (ADR Q5).
    write_home_config(
        ocx,
        f'[mirrors]\n"{registry}" = "http://{mirror_registry}"\n',
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{registry},{mirror_registry}"

    # Push to the canonical (upstream) registry. Skip the local-index refresh:
    # the configured replace-mirror does not carry this repo, so `ocx index
    # update` would 404 (and now propagates that failure). This test only asserts
    # against the registries over HTTP and never consults the local index.
    make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)

    # Canonical registry must have the manifest after the push.
    canonical_status = head_manifest(registry, unique_repo, "1.0.0")
    assert canonical_status == 200, (
        f"canonical registry must have the pushed manifest (HEAD={canonical_status})"
    )

    # Mirror must still be empty — push must NOT redirect.
    mirror_status = head_manifest(mirror_registry, unique_repo, "1.0.0")
    assert mirror_status == 404, (
        f"mirror registry must NOT receive the push (HEAD={mirror_status}); "
        "remote/proxy repos are read-only by design"
    )


# ---------------------------------------------------------------------------
# Scenario 3 — no [mirrors] config is a regression guard
# ---------------------------------------------------------------------------


def test_no_mirrors_config_behaviour_unchanged(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Without a ``[mirrors]`` section, install behaviour is byte-identical to
    today — the feature must introduce zero regression when not configured.

    Traces: plan scenario 3 — "no [mirrors] config → identical to today
    (regression guard)".
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    ocx.plain("package", "install", pkg.short)

    home = Path(ocx.env["OCX_HOME"])
    from src.assertions import assert_symlink_exists  # noqa: PLC0415
    assert_symlink_exists(
        home
        / "symlinks"
        / _registry_slug(registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )


# ---------------------------------------------------------------------------
# Scenario 4 — ocx.lock records canonical host + digest (not mirror)
# ---------------------------------------------------------------------------


def test_mirror_ocx_lock_records_canonical_host_and_digest(
    ocx: OcxRunner,
    registry: str,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """``ocx.lock`` records the canonical host + digest even when a mirror
    routes the actual network traffic — lockfiles must be portable across
    mirrored and direct-egress hosts.

    Traces: plan scenario 4 — "ocx.lock records canonical host + digest (not
    the mirror host)"; ADR R4 (lockfile portability).
    """
    # Push to the mirror only so the upstream is absent.
    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)
    make_package(mirror_ocx, unique_repo, "1.0.0", tmp_path)

    assert head_manifest(registry, unique_repo, "1.0.0") == 404, (
        "upstream must be empty so lock resolution goes through the mirror"
    )

    write_home_config(
        ocx,
        f'[mirrors]\n"{registry}" = "http://{mirror_registry}"\n',
    )
    # Plain-HTTP mirror host must be in OCX_INSECURE_REGISTRIES (ADR F2) so the
    # lock resolution read can reach it.
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{registry},{mirror_registry}"

    # Create a project and lock it.
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    fq_id = f"{registry}/{unique_repo}:1.0.0"
    (project_dir / "ocx.toml").write_text(
        f'[tools]\ntool = "{fq_id}"\n'
    )
    result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, (
        f"ocx lock failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    lock_text = (project_dir / "ocx.lock").read_text()
    matches = _REPOSITORY_RE.findall(lock_text)
    assert matches, "ocx.lock must contain at least one repository coordinate"

    for canonical_path in matches:
        assert canonical_path.startswith(registry), (
            f"repository coordinate must use canonical registry '{registry}', got "
            f"'{canonical_path}' — mirror host must NOT appear in lock"
        )
        assert mirror_registry not in canonical_path, (
            f"mirror host '{mirror_registry}' must NOT appear in ocx.lock "
            "(lockfile portability: lock must work without the mirror configured)"
        )


# ---------------------------------------------------------------------------
# Scenario 5 — docker.io mirror with library/ repository path verbatim
# ---------------------------------------------------------------------------


def test_mirror_library_prefix_repo_routes_verbatim(
    ocx: OcxRunner,
    registry: str,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """``[mirrors."<upstream>"]`` keyed scenario: an identifier carrying a
    ``library/`` path segment routes to ``<mirror>/<prefix>/library/...``
    verbatim — OCX does no Docker short-name expansion on the mirror path.

    We use our upstream registry as a stand-in for docker.io and publish a
    package under ``library/<repo>`` so the routing is testable without a real
    docker.io dependency.

    Traces: plan scenario 5 — "[mirrors.'docker.io'] with a library/ repo →
    routes to <mirror>/<prefix>/library/... verbatim"; ADR R2 + plan deferred
    finding on docker.io library/ expansion.
    """
    # Use a library/-prefixed repo to simulate docker.io/library/nginx.
    library_repo = f"library/{unique_repo}"

    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)
    make_package(mirror_ocx, library_repo, "1.0.0", tmp_path)

    # Precondition: absent from upstream.
    assert head_manifest(registry, library_repo, "1.0.0") == 404, (
        "upstream must be empty before the test"
    )

    write_home_config(
        ocx,
        f'[mirrors]\n"{registry}" = "http://{mirror_registry}"\n',
    )

    # Install with the library/ prefix in the identifier. The plain-HTTP mirror
    # host must be listed in OCX_INSECURE_REGISTRIES (ADR F2).
    fq = f"{registry}/{library_repo}:1.0.0"
    run_with_env(
        ocx,
        "package",
        "install",
        fq,
        extra_env={"OCX_INSECURE_REGISTRIES": f"{registry},{mirror_registry}"},
    )

    home = Path(ocx.env["OCX_HOME"])
    from src.assertions import assert_symlink_exists  # noqa: PLC0415
    assert_symlink_exists(
        home
        / "symlinks"
        / _registry_slug(registry)
        / library_repo
        / "candidates"
        / "1.0.0"
    )


# ---------------------------------------------------------------------------
# Scenario 6a — plain-HTTP mirror + host in OCX_INSECURE_REGISTRIES → success
# ---------------------------------------------------------------------------


def test_plain_http_mirror_with_insecure_flag_succeeds(
    ocx: OcxRunner,
    registry: str,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """``http://`` mirror + its host in ``OCX_INSECURE_REGISTRIES`` → install
    succeeds.

    Traces: plan scenario 6 — "plain-HTTP mirror with host in
    OCX_INSECURE_REGISTRIES → install succeeds"; ADR F2 footgun mitigation.
    """
    # Push to the mirror (plain-HTTP registry) only.
    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)
    make_package(mirror_ocx, unique_repo, "1.0.0", tmp_path)

    assert head_manifest(registry, unique_repo, "1.0.0") == 404, (
        "upstream must be empty"
    )

    write_home_config(
        ocx,
        f'[mirrors]\n"{registry}" = "http://{mirror_registry}"\n',
    )

    fq = f"{registry}/{unique_repo}:1.0.0"
    # Install with the mirror host explicitly in OCX_INSECURE_REGISTRIES.
    run_with_env(
        ocx,
        "package",
        "install",
        fq,
        extra_env={"OCX_INSECURE_REGISTRIES": f"{registry},{mirror_registry}"},
    )

    home = Path(ocx.env["OCX_HOME"])
    from src.assertions import assert_symlink_exists  # noqa: PLC0415
    assert_symlink_exists(
        home
        / "symlinks"
        / _registry_slug(registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )


# ---------------------------------------------------------------------------
# Scenario 6b — plain-HTTP mirror without insecure flag → actionable error
# ---------------------------------------------------------------------------


def test_plain_http_mirror_without_insecure_flag_gives_actionable_error(
    ocx: OcxRunner,
    registry: str,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """``http://`` mirror WITHOUT its host in ``OCX_INSECURE_REGISTRIES`` →
    fails loud at resolve time with an actionable error that NAMES
    ``OCX_INSECURE_REGISTRIES`` and the offending mirror host — not an opaque
    TLS hang or timeout mid-transport.

    The failure surfaces at command startup (``Context::try_init`` resolves the
    mirror map before any network work), so it is deterministic and does not
    depend on the registry being reachable.

    Traces: plan scenario 6 + Finding B (CWE-319) — omitting the mirror host
    from OCX_INSECURE_REGISTRIES must fail loud with a hint to add it, not
    silently downgrade to HTTP or stall on a TLS handshake against a plain-HTTP
    endpoint.
    """
    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)
    make_package(mirror_ocx, unique_repo, "1.0.0", tmp_path)

    assert head_manifest(registry, unique_repo, "1.0.0") == 404

    write_home_config(
        ocx,
        f'[mirrors]\n"{registry}" = "http://{mirror_registry}"\n',
    )

    fq = f"{registry}/{unique_repo}:1.0.0"
    # Run WITHOUT adding the mirror host to OCX_INSECURE_REGISTRIES (only
    # the upstream registry is listed, which is the existing default).
    result = run_with_env(
        ocx,
        "package",
        "install",
        fq,
        extra_env={"OCX_INSECURE_REGISTRIES": registry},
        check=False,
    )

    assert result.returncode != 0, (
        "install against an http:// mirror without OCX_INSECURE_REGISTRIES "
        "must fail with a non-zero exit code"
    )

    combined = result.stdout + result.stderr
    # The error must NAME OCX_INSECURE_REGISTRIES so the operator knows the
    # exact fix, and name the offending mirror host so they know what to add.
    assert "OCX_INSECURE_REGISTRIES" in combined, (
        "error message must NAME OCX_INSECURE_REGISTRIES so the operator knows "
        f"the fix, got:\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert mirror_registry in combined, (
        "error message must name the offending mirror host, got:\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
