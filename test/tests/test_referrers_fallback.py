# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The OCI referrers tag-schema fallback, against the registry that needs it.

`design_spec_cosign_parity.md` WP1/WP2 make OCX read and write the referrers
index a registry without the OCI 1.1 Referrers API gets instead: an ordinary
image index, parked at an ordinary tag, `<algorithm>-<encoded truncated to 64>`.

The read/write algorithm itself — the append, the dedupe, the read-back and the
lost-update retry — is unit-tested against a scripted registry in
`oci/client/transport.rs`, where two writers can be forced to interleave. What
those tests cannot check is that the mechanism's **premises** hold on a real
registry, and premises are what rot silently:

1. `registry:2` really does 404 the Referrers API. If it ever gains one, the
   `legacy_registry` fixture stops being the Referrers-API-absent half of the
   matrix and half the cosign-parity suite becomes an unchecked green.
2. It really does accept an image index PUT at a `sha256-<hex>` tag. Nothing in
   the distribution spec obliges a registry to, and the whole fallback is
   built on it — if a registry refused, `append_referrer_fallback_index` would
   be exercising a path no registry serves.

The end-to-end assertions — `ocx package sign` and `ocx package attest` actually
landing in this index, and `ocx package verify` reading it — live with the
commands they exercise, in `test_sign.py`, `test_attest.py` and
`test_verify.py::test_verify_without_referrers_api_or_fallback_tag_exits_79`.
What stays here is the pair of premises above, which no end-to-end green can
check: a sign that passes tells you the write succeeded, not that it took the
fallback path rather than an API this fixture was supposed to lack.
"""
from __future__ import annotations

import json

from src.registry import (
    fetch_manifest_raw,
    list_referrers,
    push_minimal_image,
    put_manifest,
    referrers_fallback_tag,
)

IMAGE_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
BUNDLE_ARTIFACT_TYPE = "application/vnd.dev.sigstore.bundle.v0.3+json"


def test_the_referrers_api_is_absent_on_the_fallback_registry(legacy_registry: str, unique_repo: str) -> None:
    """The premise the whole fallback rests on, restated as a check.

    A `registry:2` that started answering the Referrers API would not fail any
    other test in the suite — it would just quietly make the Referrers-API-absent
    half of the cosign matrix test the same thing as the present half.
    """
    subject_digest, _ = push_minimal_image(legacy_registry, unique_repo)

    status, index = list_referrers(legacy_registry, unique_repo, subject_digest)

    assert status == 404, f"expected no Referrers API on the fallback fixture, got HTTP {status}: {index}"


def test_the_fallback_registry_accepts_an_image_index_at_the_referrers_tag(
    legacy_registry: str, unique_repo: str
) -> None:
    """A registry with no Referrers API still accepts the fallback index.

    The tag schema works precisely because nothing about this PUT is special
    from the registry's side — it is an ordinary image index at an ordinary
    tag. That is an assumption, and this is where it is checked.
    """
    subject_digest, _ = push_minimal_image(legacy_registry, unique_repo)
    referrer_digest, referrer_size = push_minimal_image(legacy_registry, unique_repo, payload=b"signature-bundle")

    index = {
        "schemaVersion": 2,
        "mediaType": IMAGE_INDEX_MEDIA_TYPE,
        "manifests": [
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": referrer_digest,
                "size": referrer_size,
                "artifactType": BUNDLE_ARTIFACT_TYPE,
                "annotations": {"dev.sigstore.bundle.content": "dsse-envelope"},
            }
        ],
    }
    tag = referrers_fallback_tag(subject_digest)
    put_manifest(
        legacy_registry,
        unique_repo,
        tag,
        json.dumps(index).encode(),
        IMAGE_INDEX_MEDIA_TYPE,
    )

    served, _ = fetch_manifest_raw(legacy_registry, unique_repo, tag)
    round_tripped = json.loads(served)

    entry = round_tripped["manifests"][0]
    assert entry["digest"] == referrer_digest
    assert entry["artifactType"] == BUNDLE_ARTIFACT_TYPE, (
        "artifactType must survive the registry round trip — it is the field "
        "cosign's own fallback write loses (sigstore/cosign#4641)"
    )
    assert entry["annotations"]["dev.sigstore.bundle.content"] == "dsse-envelope", (
        "annotations must survive the registry round trip"
    )


def test_the_fallback_tag_truncates_the_encoded_section_for_every_algorithm() -> None:
    """The suite's `referrers_fallback_tag` helper, checked against the spec.

    **It exercises no OCX code** — `package::tag::referrer_fallback_tag` is
    pinned by `referrer_fallback_tag_truncates_the_encoded_section_to_64` in
    `package/tag.rs`, and this cannot go red for any change to it. What it is
    for is the two tests above, which derive the tag they PUT to from this
    helper: if the helper drifted from the spec, they would happily assert a
    round trip through the wrong tag. Truncation is a no-op for sha256 and is
    not for sha384 or sha512, so checking only sha256 would not catch that.
    """
    assert referrers_fallback_tag("sha256:" + "a" * 64) == "sha256-" + "a" * 64
    assert referrers_fallback_tag("sha384:" + "b" * 96) == "sha384-" + "b" * 64
    assert referrers_fallback_tag("sha512:" + "c" * 128) == "sha512-" + "c" * 64
