# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Infrastructure smoke tests for the referrers-capable acceptance harness (#195).

These prove the docker-compose harness upgraded in #195 is fit for the
supply-chain milestone, independent of the ocx binary and the sign/verify
pipelines (#194):

  * the primary ``registry`` (zot) serves the OCI 1.1 Referrers API — a pushed
    referrer is listed back via ``GET /v2/<name>/referrers/<digest>``, and the
    push returns the ``OCI-Subject`` header;
  * the permanent ``legacy_registry`` (registry:2) negative fixture does NOT
    serve the Referrers API — the harness carries a real referrers-unsupported
    registry for ``test_referrers_capability.py`` (#106), not a mock.

Referrers are pushed via raw HTTP (``src/registry.py`` helpers) so the test
exercises the registry endpoint directly, not a client abstraction.
"""
from __future__ import annotations

from src.registry import list_referrers, push_minimal_image, push_referrer

_ARTIFACT_TYPE = "application/vnd.ocx.test.signature"


def test_referrers_api_round_trip(registry: str, unique_repo: str) -> None:
    """Push a subject + a referrer to the primary registry; list the referrer back."""
    subject_digest, subject_size = push_minimal_image(
        registry, unique_repo, payload=b"referrers-smoke-subject"
    )

    referrer_digest, headers = push_referrer(
        registry,
        unique_repo,
        subject_digest,
        subject_size,
        artifact_type=_ARTIFACT_TYPE,
        payload=b"referrers-smoke-referrer",
    )

    # OCI-Subject on the push response proves the registry processed the
    # subject natively (spec §push — a referrers-capable registry MUST echo it).
    oci_subject = {k.lower(): v for k, v in headers.items()}.get("oci-subject")
    assert oci_subject == subject_digest, (
        f"registry must return OCI-Subject={subject_digest} on referrer push; "
        f"got {oci_subject!r} — the primary registry lacks native referrers support"
    )

    status, index = list_referrers(registry, unique_repo, subject_digest)
    assert status == 200, f"referrers API must return 200, got {status}"
    assert index is not None
    descriptors = index.get("manifests", [])
    digests = [d["digest"] for d in descriptors]
    assert referrer_digest in digests, (
        f"pushed referrer {referrer_digest} must appear in the referrers list; got {digests}"
    )
    listed = next(d for d in descriptors if d["digest"] == referrer_digest)
    assert listed.get("artifactType") == _ARTIFACT_TYPE, (
        f"referrer descriptor must carry artifactType={_ARTIFACT_TYPE}, got {listed!r}"
    )


def test_legacy_registry_reports_referrers_unsupported(
    legacy_registry: str, unique_repo: str
) -> None:
    """The permanent registry:2 negative fixture must NOT serve the Referrers API.

    Confirms the harness carries a genuine referrers-unsupported registry for
    ``test_referrers_capability.py`` (#106) — a real v2 registry, not a mock.
    Probes with the REAL subject digest, never a synthetic all-zero one (some
    registries 400 on a fabricated digest; #106 probe-digest caveat).
    """
    subject_digest, _ = push_minimal_image(
        legacy_registry, unique_repo, payload=b"referrers-neg-subject"
    )

    status, index = list_referrers(legacy_registry, unique_repo, subject_digest)
    # registry:2 (distribution v2) has no referrers route → Go's default mux
    # returns exactly 404 (confirmed for #195). Any non-200 means "no Referrers
    # API", but 404 is the precise, stable signal we pin here.
    assert status == 404, (
        f"registry:2 negative fixture must 404 the Referrers API, got {status}"
    )
    assert index is None
