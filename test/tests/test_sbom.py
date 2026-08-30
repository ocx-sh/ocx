# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package sbom``.

Contract source: ``.claude/artifacts/adr_sbom_attestations.md`` +
``.claude/state/plans/plan_sbom_attestations.md`` scenarios S-006, S-007,
S-008, S-017, S-019, plus the signature/attestation scan-isolation half of
S-012. The writing side (``package attest``, ``push --sbom``) is
``test_attest.py``.

Two verification modes are resolved per invocation, mirroring
``crates/ocx_cli/src/command/package_sbom.rs``: **demand** (``--verify``, or by
default when identity flags or a matching ``[trust.policy]`` are present) runs
the full keyless pipeline and refuses an unsigned attachment outright;
**permissive** (``--no-verify``, or by default when neither of those is
present) runs no cryptography at all and labels every document -- signed or
not -- unverified. Most tests below drive demand mode via identity flags
(``sbom_args``); the permissive-mode and mode-selection tests use
``no_identity_args`` or the ``--verify``/``--no-verify`` flags directly. The
edge-case matrix at the end of this file exercises every corner of the mode
resolution itself.
"""
from __future__ import annotations

import json
import os
import pty
import subprocess
from pathlib import Path

from src import registry as reg
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial, attestations
from tests.fixtures.sigstore_stack import SigstoreStack

CYCLONEDX_URI = "https://cyclonedx.org/bom"
SPDX_URI = "https://spdx.dev/Document"

# ──────────────────────────────────────────────────────────────────────────────
# Helpers
# ──────────────────────────────────────────────────────────────────────────────


def attest(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
    predicate: Path,
    *,
    predicate_type: str = "cyclonedx",
):
    """Attach one attestation, insisting it succeeded.

    Raises on failure rather than returning: every test below is about what
    `sbom` reads, so a broken *write* must not be reported as a read defect.
    """
    result = ocx.run(
        "package", "attest",
        *stack.sign_args(token),
        "--predicate", str(predicate),
        "--type", predicate_type,
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, (
        f"attest (setup) failed\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)["data"]


def sbom_args(stack: SigstoreStack, *, identity: str | None = None) -> list[str]:
    """``verify_args`` with an overridable expected identity.

    `SigstoreStack.verify_args` hardcodes the identity the stack mints, which is
    what nearly every test wants; the foreign-signer test needs to substitute a
    different one, and rebuilding the whole flag list at that one call site
    would let the two spellings drift.
    """
    args = stack.verify_args()
    if identity is not None:
        args[args.index("--certificate-identity") + 1] = identity
    return args


def no_identity_args(stack: SigstoreStack) -> list[str]:
    """Platform/Rekor/trust-root flags with no certificate identity at all.

    Omitting ``--certificate-identity``/``--certificate-oidc-issuer`` (and never
    passing ``--verify``) is what resolves ``VerificationMode::Permissive`` by
    the invocation's own default: no identity flags and no matching
    ``[trust.policy]`` mean nothing was asked to be verified
    (``package_sbom.rs::mode``).
    """
    return [
        "--platform", current_platform(),
        "--rekor-url", stack.rekor_url,
        "--sigstore-trusted-root", str(stack.trust_root),
    ]


def write_json(target: Path, document: dict) -> Path:
    target.write_text(json.dumps(document))
    return target


def predicate_value_bytes() -> bytes:
    """The exact bytes ``--output`` must write back for the pretty fixture.

    A predicate travels inside the signed Statement as a JSON *value*, so what
    survives is the value's own text — every byte of the fixture's odd interior
    whitespace, key order and multi-byte UTF-8 — while the newline that
    terminates the *file* is framing outside that value and does not.
    """
    return attestations.PRETTY_CYCLONEDX_PATH.read_bytes().strip()


def push_sbom_referrer(
    registry: str,
    repo: str,
    subject_digest: str,
    subject_size: int,
    *,
    media_type: str,
    payload: bytes,
    layer_media_type: str | None = None,
    subject_media_type: str | None = None,
) -> str:
    """Push a raw, unsigned SBOM referrer with `media_type` as its artifactType.

    ``registry.push_referrer`` (``test/src/registry.py``) hardcodes its layer
    to ``application/octet-stream`` regardless of ``artifact_type`` -- correct
    for its own signature-bundle callers, where the bundle's wrapped content
    is what matters, but not what a real SBOM attach produces. A genuine
    unsigned attach (``cosign attach sbom``, ``oras attach``, ``syft attest
    --output``) types its *layer* by the SBOM's own media type, and that is
    what OCX's own read path both gates and LABELS on
    (``VerifyErrorKind::SbomMediaTypeUnsupported`` and every unverified row's
    ``predicate_type`` read ``layer.media_type``, never the manifest's
    ``artifactType`` -- see ``oci/verify/pipeline.rs``). This helper
    reproduces that exact shape so the interop tests below simulate a real
    foreign tool rather than ``push_referrer``'s signature-bundle-shaped
    default.

    ``layer_media_type`` defaults to `media_type` (the ordinary, self-consistent
    case); pass a different value to simulate a registry listing whose
    artifactType and served layer disagree (the cross-family mislabel the read
    path is checked against).

    ``subject_media_type`` defaults to the image-manifest type, which is what a
    per-platform attach produces; pass the image-index type to attach to a
    multi-platform tag's index the way `cosign attach sbom <tag>` does.
    """
    config_digest = reg.push_blob(registry, repo, b"{}", insecure=True)
    layer_digest = reg.push_blob(registry, repo, payload, insecure=True)
    manifest = {
        "schemaVersion": 2,
        "mediaType": reg.IMAGE_MANIFEST_MEDIA_TYPE,
        "artifactType": media_type,
        "config": {
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": config_digest,
            "size": 2,
        },
        "layers": [
            {"mediaType": layer_media_type or media_type, "digest": layer_digest, "size": len(payload)},
        ],
        "subject": {
            "mediaType": subject_media_type or reg.IMAGE_MANIFEST_MEDIA_TYPE,
            "digest": subject_digest,
            "size": subject_size,
        },
    }
    digest, _ = reg.push_manifest(registry, repo, manifest, insecure=True)
    return digest


#: Mirrors `oci::referrer::media_types::SBOM_CYCLONEDX`.
SBOM_CYCLONEDX_MEDIA_TYPE = "application/vnd.cyclonedx+json"

CYCLONEDX_MINIMAL = b'{"bomFormat":"CycloneDX","specVersion":"1.6","components":[]}'

#: Mirrors `oci::referrer::media_types::SBOM_SPDX_TEXT`.
SBOM_SPDX_TEXT_MEDIA_TYPE = "text/spdx"


# ──────────────────────────────────────────────────────────────────────────────
# S-006 — the listing, and the identity it insists on
# ──────────────────────────────────────────────────────────────────────────────


def test_sbom_lists_the_verified_attestation_with_its_signer_and_time(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-006: every verified attestation, with identity, issuer, type and time.

    ``summary`` is asserted field by field rather than by measuring ``entries``:
    a consumer is meant to branch on ``summary.status`` without counting an
    array (PKG-25), and a summary that disagreed with its own entries would be
    the worst of both.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attested = attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    result = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert result.returncode == 0, (
        f"sbom failed\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    data = json.loads(result.stdout)["data"]
    assert data["summary"] == {
        "status": "success", "verification": "verified", "exit_code": 0, "total": 1, "verified": 1, "unverified": 0, "refused": 0,
    }
    assert data["refused"] == []

    [entry] = data["entries"]
    assert entry["predicate_type"] == CYCLONEDX_URI
    assert entry["subject_digest"] == attested["subject_digest"]
    assert entry["referrer_digest"] == attested["referrer_digest"]
    assert entry["certificate_identity"] == sigstore_stack.identity
    assert entry["certificate_oidc_issuer"] == sigstore_stack.issuer
    assert entry["signed_at"].endswith("Z"), (
        f"signed_at is RFC 3339 with an explicit Z (PLAT-31), got {entry['signed_at']!r}"
    )
    assert "summary" not in entry, "the per-document summary appears only under --summary"
    # S-004: ``shadowed`` is present on every entry, always. Unlike the optional
    # certificate fields it is never omitted -- ``false`` is a true statement
    # (nothing supersedes this document), so a consumer branches on the key
    # without first testing for its presence.
    assert entry["shadowed"] is False


def test_sbom_without_identity_flags_defaults_to_permissive_listing(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """No identity flags and no policy now default to PERMISSIVE, not a refusal.

    Superseded by the owner's mode-matrix ruling: absence of an identity source
    used to mean "refuse at 64"; it now means "nothing to verify against, so
    read permissively". ``--verify`` is what still demands an identity -- see
    ``test_edge_6_verify_flag_with_no_identity_source_is_a_usage_error`` below.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    result = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False,
    )
    assert result.returncode == 0, (
        f"no identity source must default to a permissive listing, not a refusal\\n"
        f"stdout: {result.stdout}\\nstderr: {result.stderr}"
    )
    data = json.loads(result.stdout)["data"]
    assert data["summary"]["verification"] == "unverified"
    [entry] = data["entries"]
    assert entry["verified"] is False
    # S-004, unverified half: `shadowed` is emitted on every entry regardless of
    # trust class -- it answers "is this superseded", not "was this checked".
    assert entry["shadowed"] is False


def test_sbom_without_platform_runs_against_what_resolved(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """C-010. ``package sbom`` with no ``--platform`` is not a usage error.

    Same relaxation as ``package verify``: absent the flag the command acts on
    whatever the reference resolved to, which for a multi-platform tag is the
    index — where cosign attaches an index-level attestation.

    ``sbom_args`` always passes ``--platform``, so the flags are spelled out
    here without it. A clap refusal exits 64 with an empty stdout, so a
    parseable envelope carrying a pipeline verdict is what proves the grammar
    accepted the invocation.
    """
    result = ocx.run(
        "package", "sbom",
        "--rekor-url", sigstore_stack.rekor_url,
        "--sigstore-trusted-root", str(sigstore_stack.trust_root),
        "--certificate-identity", sigstore_stack.identity,
        "--certificate-oidc-issuer", sigstore_stack.issuer,
        published_package.short,
        check=False,
    )
    assert result.returncode != 64, (
        f"--platform must be optional; clap refused the invocation\n"
        f"stderr: {result.stderr.strip()}"
    )
    assert json.loads(result.stdout)["error"]["detail"] == "no_signatures_found", (
        f"the run must reach the pipeline and report on the resolved object; "
        f"got {result.stdout}"
    )


def test_sbom_on_a_package_carrying_nothing_is_not_found(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """A package with no referrers at all is 79 ``no_signatures_found``.

    Distinct from S-017's ``attestation_not_found``, which means candidates were
    examined and none carried the requested type. Collapsing the two would tell
    an operator "nothing is attested" when the truth is "nothing of that type".
    """
    result = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)

    assert result.returncode == 79, f"expected not-found, got {result.returncode}"
    assert json.loads(result.stdout)["error"]["detail"] == "no_signatures_found"


# ──────────────────────────────────────────────────────────────────────────────
# S-007 — --output writes the exact signed bytes, and refuses to guess
# ──────────────────────────────────────────────────────────────────────────────


def test_sbom_output_writes_back_the_exact_bytes_that_were_signed(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-007: the extracted document is the signed sub-slice, not a re-encoding.

    The fixture is deliberately non-canonical (odd interior whitespace, unsorted
    keys, multi-byte UTF-8), so an implementation that parsed the predicate and
    re-serialized it would produce an equal JSON *value* and different *bytes* —
    and a consumer recomputing a digest over the extracted file would then get a
    hash that matches nothing anyone published. Byte equality is the contract;
    `json.loads(a) == json.loads(b)` would pass for exactly the implementation
    this exists to reject.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    extracted = tmp_path / "extracted.json"
    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack),
        "--type", "cyclonedx",
        "--output", str(extracted),
        published_package.short,
        check=False,
    )
    assert result.returncode == 0, (
        f"extraction failed\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    written = extracted.read_bytes()
    assert written == predicate_value_bytes(), (
        "the extracted predicate is not byte-identical to what was attested; "
        "something on the path re-serialized it"
    )
    # Independent of the equality above: prove the non-canonical shape actually
    # survived, so a future fixture that drifted canonical could not make the
    # assertion vacuous.
    assert b'"components"    :' in written
    assert "模块".encode() in written


def test_sbom_output_refuses_to_choose_between_two_attestations_of_one_type(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-007 error case: >1 match is 65 ``multiple_attestations`` naming each digest.

    Picking one would let the registry's listing order decide which document a
    consumer reads — a silent, reproducible-looking wrong answer. The refusal
    names both referrer digests because "narrow with --type" is not actionable
    when both candidates already carry the requested type.
    """
    first = write_json(tmp_path / "a.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1, "components": [],
    })
    second = write_json(tmp_path / "b.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 2, "components": [],
    })
    digests = {
        attest(ocx, sigstore_stack, identity_token, published_package, first)["referrer_digest"],
        attest(ocx, sigstore_stack, identity_token, published_package, second)["referrer_digest"],
    }

    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack),
        "--type", "cyclonedx",
        "--output", str(tmp_path / "never-written.json"),
        published_package.short,
        check=False,
    )

    assert result.returncode == 65, f"expected a data error, got {result.returncode}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "multiple_attestations"
    for digest in digests:
        assert digest in envelope["error"]["message"], (
            f"the refusal must name every candidate; {digest} is missing from "
            f"{envelope['error']['message']!r}"
        )
    assert not (tmp_path / "never-written.json").exists(), (
        "a refused extraction must not leave a partial file behind"
    )


def test_sbom_output_names_every_predicate_type_when_the_match_set_is_mixed(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """UF-2: mixed-type match set, no ``--type`` narrowing.

    ``--output`` with an SPDX AND a CycloneDX attestation both present, and no
    ``--type`` to narrow between them, is ``multiple_attestations`` naming
    BOTH predicate types -- not just the first, and not just the digests.
    Per ``VerifyErrorKind::MultipleAttestations``'s own doc comment: a message
    naming only one type would state something untrue about the other
    candidate and withhold the value that actually resolves the ambiguity
    (``--type <the missing value>``).

    Distinct from ``test_sbom_output_refuses_to_choose_between_two_attestations_of_one_type``
    above: that test's two candidates already share one type under an explicit
    ``--type cyclonedx`` filter, so only the digest list discriminates them.
    Here the types themselves differ and no ``--type`` is given at all.
    """
    cyclonedx = write_json(tmp_path / "sbom.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "components": [],
    })
    spdx = write_json(tmp_path / "doc.spdx.json", {
        "spdxVersion": "SPDX-2.3", "name": "mixed-set-doc", "packages": [],
    })
    cdx_digest = attest(ocx, sigstore_stack, identity_token, published_package, cyclonedx)["referrer_digest"]
    spdx_digest = attest(
        ocx, sigstore_stack, identity_token, published_package, spdx, predicate_type="spdxjson",
    )["referrer_digest"]

    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack),
        "--output", str(tmp_path / "never-written.json"),
        published_package.short,
        check=False,
    )

    assert result.returncode == 65, f"expected a data error, got {result.returncode}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "multiple_attestations"
    message = envelope["error"]["message"]
    assert CYCLONEDX_URI in message, f"CycloneDX type missing from {message!r}"
    assert SPDX_URI in message, f"SPDX type missing from {message!r}"
    assert "--type" in message, f"the refusal must name the --type remedy, got {message!r}"
    for digest in (cdx_digest, spdx_digest):
        assert digest in message, f"{digest} missing from {message!r}"
    assert not (tmp_path / "never-written.json").exists(), (
        "a refused extraction must not leave a partial file behind"
    )


def test_sbom_output_dash_refuses_a_terminal(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """S-008: ``--output -`` onto a TTY is 64, before any network round-trip.

    The predicate is authored by whoever holds an admitted identity, so
    "verified" does not mean "safe to print": written verbatim to a terminal, a
    component description carrying an OSC 52 sequence sets the operator's
    clipboard (CWE-150). The bytes must stay exact for S-007, so the terminal is
    declined instead of the document being sanitized.

    Driven through a real pty because that is the only way `is_terminal()`
    answers yes — a captured pipe would make this pass for the wrong reason, and
    the refusal is what the whole scenario is about.
    """
    controller, follower = pty.openpty()
    try:
        result = subprocess.run(
            [
                str(ocx.binary), "--format", "json",
                "package", "sbom", *sbom_args(sigstore_stack),
                "--output", "-",
                published_package.short,
            ],
            stdout=follower,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            env=ocx.env,
            text=True,
        )
        os.close(follower)
        follower = None
        emitted = os.read(controller, 65536).decode(errors="replace")
    finally:
        if follower is not None:
            os.close(follower)
        os.close(controller)

    assert result.returncode == 64, (
        f"writing raw predicate bytes to a terminal must be refused (64), got "
        f"{result.returncode}\nstdout: {emitted}\nstderr: {result.stderr}"
    )
    assert "terminal" in emitted, (
        f"the refusal must say the destination is the problem, got: {emitted!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-017 — narrowing to a type nobody published
# ──────────────────────────────────────────────────────────────────────────────


def test_sbom_narrowed_to_an_unpublished_type_reports_not_found(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-017: referrers exist, none carries the requested type — 79.

    The unnarrowed listing is asserted first, and that is the point of the test
    rather than setup: without it, a run in which *every* candidate was refused
    for an unrelated reason would produce the same 79 and the test would pass
    while proving nothing about narrowing. Proving the attestation is verifiable
    and only then narrowing it away is what makes the code attributable.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    listed = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"the attestation must verify before narrowing means anything: {listed.stdout}"
    assert json.loads(listed.stdout)["data"]["summary"]["verified"] == 1

    narrowed = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack),
        "--type", "spdxjson",
        published_package.short,
        check=False,
    )
    assert narrowed.returncode == 79, f"expected not-found, got {narrowed.returncode}"
    assert json.loads(narrowed.stdout)["error"]["detail"] == "attestation_not_found", (
        "a narrowing miss is attestation_not_found, never no_signatures_found — "
        "candidates were examined"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-019 — --summary parses, or refuses; it never reports an empty summary
# ──────────────────────────────────────────────────────────────────────────────


def test_sbom_summary_reports_the_cyclonedx_document_fields(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-019: ``--summary`` reads the document's own fields, not the annotation."""
    predicate = write_json(tmp_path / "sbom.cdx.json", {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": "urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79",
        "version": 1,
        "metadata": {"component": {"type": "application", "name": "ocx-test-app"}},
        "components": [
            {"type": "library", "name": "left-pad", "version": "1.0.0"},
            {"type": "library", "name": "right-pad", "version": "2.0.0"},
        ],
    })
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack), "--summary",
        published_package.short, check=False,
    )
    assert result.returncode == 0, f"summary failed\nstdout: {result.stdout}\nstderr: {result.stderr}"

    [entry] = json.loads(result.stdout)["data"]["entries"]
    assert entry["summary"] == {
        "spec_version": "1.6",
        "serial_number": "urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79",
        "component_count": 2,
        "top_level_component": "ocx-test-app",
    }


def test_sbom_summary_refuses_a_non_cyclonedx_predicate_but_listing_still_works(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-019 error case: an unparseable document refuses that entry, not the run.

    ``--summary`` augments a listing that works without it, so a document no
    reader understands costs the operator that document and nothing else: the
    entry moves to ``refused`` with its own slug and the process still exits 0.
    Aborting the whole listing would hand one malformed SBOM the power to hide
    every readable one beside it — the per-candidate independence the verify
    pipeline already guarantees, undone one layer up (PKG-22).

    Both halves are asserted in one test on purpose: the claim is not "summary
    refuses" but "summary refuses *and the listing is unaffected*". Split apart,
    the second half would be a separate happy-path test and nothing would pin
    that the refusal is scoped to the flag rather than to the attestation.
    """
    predicate = write_json(tmp_path / "doc.spdx.json", {
        "spdxVersion": "SPDX-2.3", "name": "not-a-cyclonedx-document", "packages": [],
    })
    attested = attest(
        ocx, sigstore_stack, identity_token, published_package, predicate, predicate_type="spdxjson",
    )

    summarized = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack), "--summary",
        published_package.short, check=False,
    )
    assert summarized.returncode == 0, (
        f"an unreadable document refuses its own entry, not the run, got "
        f"{summarized.returncode}\nstdout: {summarized.stdout}"
    )
    data = json.loads(summarized.stdout)["data"]
    assert data["summary"] == {
        "status": "partial_failure", "verification": "verified", "exit_code": 0, "total": 1, "verified": 0, "unverified": 0, "refused": 1,
    }
    assert data["entries"] == [], "nothing summarized, so nothing is listed as summarized"

    [refusal] = data["refused"]
    assert refusal["reason_kind"] == "sbom_summary_failed", (
        f"a script branches on the slug, got: {refusal!r}"
    )
    assert refusal["referrer_digest"] == attested["referrer_digest"]
    assert "--summary" in refusal["reason"], (
        f"the refusal must name the flag to drop, got: {refusal['reason']!r}"
    )

    listed = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, (
        f"the listing must still work without --summary\nstdout: {listed.stdout}"
    )
    [entry] = json.loads(listed.stdout)["data"]["entries"]
    assert entry["predicate_type"] == SPDX_URI
    assert "summary" not in entry


def test_sbom_summary_partial_failure_still_summarizes_the_readable_entry(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """UF-3: a genuinely MIXED set -- one summarizable CycloneDX attestation
    beside one non-CycloneDX (SPDX) one -- exits 0 with ``partial_failure``,
    the SPDX entry refused with its own slug, and the CycloneDX entry actually
    summarized.

    The harder half of the claim the test above this one does not cover: that
    test's package carries ONLY the unreadable document, so its
    ``entries == []`` says nothing about whether a bad document crowds out a
    good one sitting beside it (PKG-22, `--summary`'s own doc comment: "never
    the rest of the listing").
    """
    cyclonedx = write_json(tmp_path / "sbom.cdx.json", {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": "urn:uuid:8f1e6b1a-2c3d-4e5f-9a0b-1c2d3e4f5a6b",
        "version": 1,
        "metadata": {"component": {"type": "application", "name": "mixed-set-app"}},
        "components": [{"type": "library", "name": "left-pad", "version": "1.0.0"}],
    })
    spdx = write_json(tmp_path / "doc.spdx.json", {
        "spdxVersion": "SPDX-2.3", "name": "not-a-cyclonedx-document", "packages": [],
    })
    cdx_attested = attest(ocx, sigstore_stack, identity_token, published_package, cyclonedx)
    spdx_attested = attest(
        ocx, sigstore_stack, identity_token, published_package, spdx, predicate_type="spdxjson",
    )

    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack), "--summary",
        published_package.short, check=False,
    )
    assert result.returncode == 0, (
        f"a partial mixed set still exits 0\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    data = json.loads(result.stdout)["data"]
    assert data["summary"] == {
        "status": "partial_failure", "verification": "verified", "exit_code": 0, "total": 2, "verified": 1, "unverified": 0, "refused": 1,
    }

    [entry] = data["entries"]
    assert entry["predicate_type"] == CYCLONEDX_URI
    assert entry["referrer_digest"] == cdx_attested["referrer_digest"]
    assert entry["summary"] == {
        "spec_version": "1.6",
        "serial_number": "urn:uuid:8f1e6b1a-2c3d-4e5f-9a0b-1c2d3e4f5a6b",
        "component_count": 1,
        "top_level_component": "mixed-set-app",
    }

    [refusal] = data["refused"]
    assert refusal["reason_kind"] == "sbom_summary_failed", (
        f"a script branches on the slug, got: {refusal!r}"
    )
    assert refusal["referrer_digest"] == spdx_attested["referrer_digest"]
    assert "--summary" in refusal["reason"], (
        f"the refusal must name the flag to drop, got: {refusal['reason']!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-012 — arity selection per entry point: nine attestations beside one signature
# ──────────────────────────────────────────────────────────────────────────────


def test_nine_attestations_do_not_crowd_out_the_signature_or_each_other(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-012: one subject, ten referrers — each entry point finds its own kind.

    Attestations and signatures share one ``artifactType``, and the two scans
    have deliberately different budgets (8 signature candidates, 32 attestation
    ones). Nine attestations therefore exceed the signature budget on their own:
    a scan that took the first N referrers and then discriminated — rather than
    discriminating and then bounding — would drop the signature entirely and
    report the package unsigned. That selection is unit-tested at the
    ``ScanBudget`` level only, so this is the one place the two commands are
    driven against a real crowded subject.

    Both directions are asserted in one test because the property is a
    relationship: nine-and-one, seen from each end.
    """
    signed = ocx.run(
        "package", "sign", *sigstore_stack.sign_args(identity_token),
        published_package.short, check=False,
    )
    assert signed.returncode == 0, f"sign (setup) failed: {signed.stderr}"

    for index in range(9):
        predicate = write_json(tmp_path / f"sbom-{index}.cdx.json", {
            "bomFormat": "CycloneDX", "specVersion": "1.6", "version": index + 1,
            "components": [{"type": "library", "name": f"lib-{index}", "version": "1.0.0"}],
        })
        attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    subject = reg.fetch_platform_manifest_digest(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    status, index = reg.list_referrers(ocx.registry, published_package.repo, subject)
    assert status == 200 and index is not None
    assert len(index["manifests"]) == 10, (
        f"the subject must carry all ten referrers before the scans are meaningful, "
        f"found {len(index['manifests'])}"
    )

    verified = ocx.run("package", "verify", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert verified.returncode == 0, (
        f"nine attestations crowded out the signature — `verify` no longer finds it\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}"
    )

    listed = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"sbom failed\nstdout: {listed.stdout}\nstderr: {listed.stderr}"
    summary = json.loads(listed.stdout)["data"]["summary"]
    assert summary["verified"] == 9, (
        f"expected all nine attestations, got {summary['verified']} "
        f"(refused {summary['refused']}) — the signature must be discriminated "
        f"out, not counted, and no attestation may be dropped"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Verification failures — the listing is only worth as much as its refusals
# ──────────────────────────────────────────────────────────────────────────────


def test_sbom_refuses_an_attestation_signed_by_an_unexpected_identity(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A genuine attestation from the wrong signer is refused, not listed.

    The bundle verifies cryptographically and its Rekor entry is real; only the
    certificate SAN disagrees with what was asked for. Exit 0 here would mean
    ``sbom`` reports documents from anyone the registry will serve.

    The exact slug is asserted, not merely a non-zero exit: with verification
    broken for any reason at all every candidate is refused, and a test content
    with "it failed" would pass throughout an outage of the property it guards.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    result = ocx.run(
        "package", "sbom",
        *sbom_args(sigstore_stack, identity="someone-else@example.com"),
        published_package.short,
        check=False,
    )

    assert result.returncode == 77, (
        f"an unexpected signer must be refused at 77, got {result.returncode}\n"
        f"stdout: {result.stdout}"
    )
    assert json.loads(result.stdout)["error"]["detail"] == "identity_mismatch"


def test_sbom_refuses_an_attestation_whose_predicate_bytes_were_edited(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Editing the signed document after publication is detected, not served.

    The whole value of ``sbom`` is that the document it prints is the one the
    signer signed. This takes a real, fully verifying attestation off the
    registry, changes one component name inside the DSSE payload, puts it back,
    and asserts refusal — everything else about the bundle stays exactly as the
    stack produced it, so a pass isolates the payload check from the chain, the
    log and the identity.
    """
    predicate = write_json(tmp_path / "sbom.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1,
        "components": [{"type": "library", "name": "honest-lib", "version": "1.0.0"}],
    })
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    clean = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert clean.returncode == 0, (
        f"the attestation must verify before tampering proves anything: {clean.stdout}"
    )

    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    # Equal-length replacement: the payload's byte count is unchanged, so a pass
    # isolates the content check from any length-derived bound.
    attestations.tamper_attestation_payload(
        ocx.registry, published_package.repo, subject, size,
        replace=(b"honest-lib", b"evil-lib24"),
    )

    result = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert result.returncode != 0, "an edited predicate must never be listed as verified"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "signature_invalid", (
        f"expected the signature check to catch the edit, got "
        f"{envelope['error']['detail']!r}: {envelope['error']['message']!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Unsigned SBOM referrers: foreign-tool interop, cross-and-within-trust-class
# ambiguity, and the structural media-type check
# ──────────────────────────────────────────────────────────────────────────────


def test_sbom_lists_a_foreign_tools_raw_referrer_as_unverified(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """A raw SBOM referrer pushed exactly like `cosign attach sbom`/syft would.

    Nothing here goes through `ocx package attest` -- `push_sbom_referrer`
    types the layer by the SBOM's own media type, which is the shape a real
    foreign tool produces and the shape OCX's own read path recognizes. Read
    without identity flags (the permissive default): under demand mode a raw
    attachment is refused outright rather than listed (see the edge-case
    matrix below), so interop specifically needs the permissive path. The
    listing must surface it as an ordinary, unverified entry, and `--summary`
    must read it exactly as it reads a signed CycloneDX document.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    listed = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"sbom failed\\nstdout: {listed.stdout}\\nstderr: {listed.stderr}"
    data = json.loads(listed.stdout)["data"]
    assert data["summary"] == {
        "status": "success", "verification": "unverified", "exit_code": 0,
        "total": 1, "verified": 0, "unverified": 1, "refused": 0,
    }
    [entry] = data["entries"]
    assert entry["verified"] is False
    assert entry["predicate_type"] == CYCLONEDX_URI
    assert "certificate_identity" not in entry

    summarized = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--summary", published_package.short, check=False,
    )
    assert summarized.returncode == 0, f"stdout: {summarized.stdout}"
    [summary_entry] = json.loads(summarized.stdout)["data"]["entries"]
    assert summary_entry["summary"]["component_count"] == 0


# ──────────────────────────────────────────────────────────────────────────────
# S-010 / C-011 — a platform SBOM shadows an index one, per predicateType
# ──────────────────────────────────────────────────────────────────────────────


#: A second CycloneDX document, distinguishable from `CYCLONEDX_MINIMAL` so the
#: two referrers differ in more than their subject.
CYCLONEDX_INDEX_LEVEL = b'{"bomFormat":"CycloneDX","specVersion":"1.6","components":[{"type":"library","name":"index-level"}]}'

#: SPDX tag-value, the shape `SBOM_SPDX_TEXT_MEDIA_TYPE` types. Not JSON — which
#: is the point: discovery is format-agnostic, only `--summary` is not.
SPDX_TAG_VALUE = b"SPDXVersion: SPDX-2.3\nDataLicense: CC0-1.0\n"


def test_sbom_shadows_the_index_cyclonedx_but_never_the_index_spdx(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """S-010 / C-011. Three raw SBOMs across two subjects, read with --platform.

    A platform-level CycloneDX supersedes the index-level CycloneDX and leaves
    the index-level SPDX alone: they are different documents for different
    consumers, not substitutes, and a consumer asking for SPDX would otherwise
    be told the package carries none.

    Read permissively (`no_identity_args`), because a raw attach is what a
    foreign tool produces and demand mode refuses it before listing — the
    shadowing rule is format-and-subject arithmetic and does not depend on who
    signed. All three documents must still be listed: `--format json` is the
    machine channel and marks rather than hides.
    """
    index_bytes, index_digest = reg.fetch_manifest_raw(
        ocx.registry, published_package.repo, published_package.tag,
    )
    platform_digest, platform_size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    assert platform_digest != index_digest, (
        "this test needs a multi-platform tag; the fixture resolved to a single manifest"
    )

    platform_cyclonedx = push_sbom_referrer(
        ocx.registry, published_package.repo, platform_digest, platform_size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )
    index_cyclonedx = push_sbom_referrer(
        ocx.registry, published_package.repo, index_digest, len(index_bytes),
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_INDEX_LEVEL,
        subject_media_type=reg.IMAGE_INDEX_MEDIA_TYPE,
    )
    index_spdx = push_sbom_referrer(
        ocx.registry, published_package.repo, index_digest, len(index_bytes),
        media_type=SBOM_SPDX_TEXT_MEDIA_TYPE, payload=SPDX_TAG_VALUE,
        subject_media_type=reg.IMAGE_INDEX_MEDIA_TYPE,
    )

    listed = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"sbom failed\nstdout: {listed.stdout}\nstderr: {listed.stderr}"
    entries = {entry["referrer_digest"]: entry for entry in json.loads(listed.stdout)["data"]["entries"]}

    assert set(entries) == {platform_cyclonedx, index_cyclonedx, index_spdx}, (
        "all three documents must be listed: --format json marks a superseded "
        f"document, it never drops it; got {sorted(entries)}"
    )
    assert entries[index_spdx]["shadowed"] is False, (
        "an index-level SPDX is not a substitute for a platform CycloneDX; "
        "hiding it is data loss, not a preference"
    )
    assert entries[index_cyclonedx]["shadowed"] is True, (
        "the index-level CycloneDX is superseded by the platform-level one of the same type"
    )
    assert entries[platform_cyclonedx]["shadowed"] is False, (
        "the preferred document can never be its own shadow"
    )
    assert entries[platform_cyclonedx]["subject_digest"] == platform_digest
    assert entries[index_spdx]["subject_digest"] == index_digest

    # The plain default is the one rendering that collapses. The superseded
    # document's referrer digest leaves the table; the other two stay, so this
    # cannot pass on an empty render.
    plain = ocx.plain("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert plain.returncode == 0, f"stdout: {plain.stdout}\nstderr: {plain.stderr}"
    assert index_cyclonedx not in plain.stdout, (
        f"the human default collapses to the preferred document: {plain.stdout}"
    )
    for surviving in (platform_cyclonedx, index_spdx):
        assert surviving in plain.stdout, (
            f"{surviving} must still render; the table collapsed too far: {plain.stdout}"
        )


def test_sbom_without_platform_shadows_nothing(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """C-011 rule 3, and the discriminating control for the test above.

    Same registry shape, one flag removed: with no ``--platform`` nothing is
    narrowed, one subject is read, and no document can supersede another. A
    shadowing pass that ignored what was narrowed would mark the index-level
    CycloneDX here too.
    """
    index_bytes, index_digest = reg.fetch_manifest_raw(
        ocx.registry, published_package.repo, published_package.tag,
    )
    platform_digest, platform_size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, platform_digest, platform_size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )
    index_cyclonedx = push_sbom_referrer(
        ocx.registry, published_package.repo, index_digest, len(index_bytes),
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_INDEX_LEVEL,
        subject_media_type=reg.IMAGE_INDEX_MEDIA_TYPE,
    )

    listed = ocx.run(
        "package", "sbom",
        "--rekor-url", sigstore_stack.rekor_url,
        "--sigstore-trusted-root", str(sigstore_stack.trust_root),
        published_package.short,
        check=False,
    )
    assert listed.returncode == 0, f"stdout: {listed.stdout}\nstderr: {listed.stderr}"
    entries = {entry["referrer_digest"]: entry for entry in json.loads(listed.stdout)["data"]["entries"]}
    assert set(entries) == {index_cyclonedx}, (
        "with no --platform the index is the subject itself, so only its own "
        f"documents are read; got {sorted(entries)}"
    )
    assert entries[index_cyclonedx]["shadowed"] is False, (
        "nothing was narrowed, so nothing is superseded"
    )


def test_sbom_demand_mode_lists_the_verified_document_and_refuses_the_unsigned_sibling(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Demand mode: a verified document is listed, an unsigned sibling is refused.

    Supersedes the old
    `test_sbom_output_prefers_a_verified_document_over_an_unverified_one_silently`:
    under the mode matrix a demanded scan never lists an unsigned attachment at
    all (`refuse_unsigned` in `verify/pipeline.rs` refuses it before any
    fetch), so "prefers the verified one" is no longer a choice between two
    listed candidates -- the unsigned one never reaches the listing at all.
    `--output` still writes the verified document's exact bytes, and still
    warns nothing, because there is nothing unverified left to warn about.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    listed = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, listed.stdout
    data = json.loads(listed.stdout)["data"]
    assert data["summary"]["verification"] == "verified"
    assert (data["summary"]["verified"], data["summary"]["unverified"], data["summary"]["refused"]) == (1, 0, 1)
    [refusal] = data["refused"]
    assert refusal["reason_kind"] == "unsigned_rejected_by_policy"

    extracted = tmp_path / "extracted.json"
    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack), "--type", "cyclonedx",
        "--output", str(extracted), published_package.short, check=False,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\\nstderr: {result.stderr}"
    assert extracted.read_bytes() == predicate_value_bytes(), "the verified document must win, byte for byte"
    assert "unverified" not in result.stderr, (
        f"a verified winner must not warn about the refused unsigned sibling: {result.stderr!r}"
    )


def test_sbom_output_of_a_lone_unverified_document_warns_exactly_once(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """`--output` on a lone unsigned SBOM, read permissively, warns once.

    Under demand mode a raw attachment is refused outright (see the edge-case
    matrix), so reaching `--output`'s unverified branch needs the permissive
    default -- no identity flags. The bytes still have to be exact (S-007's
    contract applies to either trust class); the caller must additionally be
    told, once, that nothing vouches for what they are about to read.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    extracted = tmp_path / "extracted.json"
    result = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--type", "cyclonedx",
        "--output", str(extracted), published_package.short, check=False,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\\nstderr: {result.stderr}"
    assert extracted.read_bytes() == CYCLONEDX_MINIMAL
    assert result.stderr.count("unverified") == 1, (
        f"expected exactly one unverified warning, got: {result.stderr!r}"
    )
    assert "nothing vouches for what it says" in result.stderr, (
        f"the warning wording changed; update this assertion to match, got: {result.stderr!r}"
    )


def test_sbom_output_refuses_two_unverified_cyclonedx_candidates(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """Ambiguity is judged within a trust class too: two raw SBOMs collide.

    Read permissively (no identity flags): under demand mode two raw
    attachments are both refused outright with `unsigned_rejected_by_policy`
    and never reach `single_document` at all (see the edge-case matrix), so
    this within-trust-class ambiguity is only reachable permissively.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    first = b'{"bomFormat":"CycloneDX","specVersion":"1.6","version":1,"components":[]}'
    second = b'{"bomFormat":"CycloneDX","specVersion":"1.6","version":2,"components":[]}'
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=first,
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=second,
    )

    result = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--type", "cyclonedx",
        "--output", str(tmp_path / "never-written.json"), published_package.short, check=False,
    )
    assert result.returncode == 65, f"expected a data error, got {result.returncode}\\nstdout: {result.stdout}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "multiple_attestations"
    assert not (tmp_path / "never-written.json").exists(), "a refused extraction must not leave a partial file"


def test_verify_attestation_never_treats_an_unsigned_sbom_as_a_candidate(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """A subject carrying ONLY an unsigned SBOM has nothing `verify --attestation` can see.

    The signed listing keeps its server-side referrers filter
    (`artifactType == SIGSTORE_BUNDLE_V03`), so a raw SBOM referrer is never
    even returned by the registry query the attestation scan issues --
    structurally, not by a post-fetch check.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    result = ocx.run(
        "package", "verify", "--attestation", "--type", "cyclonedx",
        *sigstore_stack.verify_args(), published_package.short, check=False,
    )
    assert result.returncode == 79, (
        f"an unsigned-only subject must never satisfy an attestation verify, got {result.returncode}\n"
        f"stdout: {result.stdout}"
    )


def test_sbom_refuses_a_raw_referrer_whose_layer_media_type_disagrees_with_its_artifact_type(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """An unsigned referrer's structural claim is its LAYER media type, not its artifactType.

    `registry.push_referrer` always types its layer `application/octet-stream`
    regardless of the `artifact_type` argument -- a shape no real tool
    produces, but exactly what this test needs: an `artifactType` claiming
    CycloneDX with a layer that is not an SBOM at all.

    Read permissively (no identity flags): under demand mode a raw referrer is
    refused by `artifactType` alone, WITHOUT ever fetching its manifest
    (`refuse_unsigned` in `verify/pipeline.rs`), so the layer-media-type check
    this test exists to prove can never fire there -- it lives entirely in the
    permissive path's `read_unverified_referrer`, which does fetch. As the ONLY
    referrer on the subject, the refusal surfaces as the scan's own top-level
    error, not as a `refused` row beside other entries (there is nothing else
    to be beside).
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    reg.push_referrer(
        ocx.registry, published_package.repo, subject, size,
        artifact_type=SBOM_CYCLONEDX_MEDIA_TYPE,
    )

    result = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert result.returncode == 65, f"expected a data error, got {result.returncode}\\nstdout: {result.stdout}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "sbom_media_type_unsupported"
    assert "application/octet-stream" in envelope["error"]["message"], (
        f"the refusal must name the offending layer media type: {envelope['error']['message']!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Owner's edge-case matrix: demand vs permissive, exhaustively
# ──────────────────────────────────────────────────────────────────────────────


def test_edge_1_demand_mode_refuses_a_lone_unsigned_sbom_before_listing(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """Edge case 1: policy demands + ONLY an unsigned SBOM -> 77, before any listing.

    `refuse_unsigned` (`verify/pipeline.rs`) refuses a raw attachment WITHOUT
    fetching it under demand mode, and with nothing else on the subject the
    lone refusal is promoted to the scan's own top-level error -- a top-level
    envelope, not a `refused` row inside a listing. Identity flags are used as
    the demand trigger (flags are equivalent to a matching policy for mode
    resolution).

    Red proof: the identical invocation with the identity flags dropped
    (permissive default) succeeds with a listing instead -- the mode flag
    alone is what changed between red and green.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    demanded = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert demanded.returncode == 77, (
        f"a lone unsigned SBOM under demand mode must refuse at 77, got {demanded.returncode}\n"
        f"stdout: {demanded.stdout}"
    )
    assert json.loads(demanded.stdout)["error"]["detail"] == "unsigned_rejected_by_policy"

    # Red proof: same referrer, permissive mode -- succeeds instead of refusing.
    permitted = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert permitted.returncode == 0, (
        f"the identical referrer must list under permissive mode: {permitted.stdout}"
    )


def test_edge_2_demand_mode_with_an_invalid_bundle_and_an_unsigned_sbom_fails_as_a_signature_error(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Edge case 2: demand + unsigned + an INVALID-signature bundle -> 65, no downgrade.

    A tampered signed bundle beside a refused unsigned referrer must fail as
    the signature class -- `scan()`'s own `best_failure` picks the tampered
    bundle's `signature_invalid` (rank 4) and `run_attestations_inner`
    propagates it directly, because the unsigned-refusal promotion only fires
    when the signed pass finds NOTHING at all (`NoSignaturesFound` /
    `AttestationNotFound`), which is not the case here -- one signed candidate
    was examined and failed for a specific reason. Neither document is
    obtainable via `--output`.
    """
    predicate = write_json(tmp_path / "sbom.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1,
        "components": [{"type": "library", "name": "honest-lib", "version": "1.0.0"}],
    })
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    attestations.tamper_attestation_payload(
        ocx.registry, published_package.repo, subject, size,
        replace=(b"honest-lib", b"evil-lib24"),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    extracted = tmp_path / "never-written.json"
    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack),
        "--output", str(extracted), published_package.short, check=False,
    )
    assert result.returncode == 65, (
        f"a tampered bundle beside an unsigned referrer must fail as the signature "
        f"class (65), not be silently downgraded to the unsigned refusal (77), "
        f"got {result.returncode}\nstdout: {result.stdout}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "signature_invalid", (
        f"expected the tampered signature to be the reported failure, got "
        f"{envelope['error']['detail']!r}: {envelope['error']['message']!r}"
    )
    assert not extracted.exists(), "a refused extraction must not leave a partial file behind"


def test_edge_3_demand_mode_lists_the_valid_signed_document_beside_two_refusals(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Edge case 3: demand + THREE candidates -- valid signed, tampered signed, unsigned.

    Exactly one verified entry; the tampered bundle and the unsigned referrer
    both land in `refused` with their own slugs; `--output` writes the VALID
    document's exact bytes. The tamper happens first, while it is the only
    attestation on the subject (`_attestation_referrer` requires exactly one
    candidate) -- the valid attestation and the raw referrer are added
    afterward.
    """
    tampered_predicate = write_json(tmp_path / "tampered.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1,
        "components": [{"type": "library", "name": "honest-lib", "version": "1.0.0"}],
    })
    attest(ocx, sigstore_stack, identity_token, published_package, tampered_predicate)
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    attestations.tamper_attestation_payload(
        ocx.registry, published_package.repo, subject, size,
        replace=(b"honest-lib", b"evil-lib24"),
    )

    valid_predicate = tmp_path / "valid.cdx.json"
    valid_predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attest(ocx, sigstore_stack, identity_token, published_package, valid_predicate)

    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    listed = ocx.run("package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"stdout: {listed.stdout}\nstderr: {listed.stderr}"
    data = json.loads(listed.stdout)["data"]
    assert (data["summary"]["verified"], data["summary"]["unverified"], data["summary"]["refused"]) == (1, 0, 2), (
        f"expected 1 verified, 0 unverified, 2 refused; got {data['summary']!r}"
    )
    assert data["summary"]["verification"] == "verified"
    reasons = {row["reason_kind"] for row in data["refused"]}
    assert reasons == {"signature_invalid", "unsigned_rejected_by_policy"}, (
        f"expected the tampered bundle and the unsigned referrer both refused, got {reasons!r}"
    )

    extracted = tmp_path / "extracted.json"
    result = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack), "--type", "cyclonedx",
        "--output", str(extracted), published_package.short, check=False,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr}"
    assert extracted.read_bytes() == predicate_value_bytes(), "the valid document must win, byte for byte"


def test_edge_4_permissive_mode_extracts_a_signed_bundles_payload_unverified(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Edge case 4: permissive + a signed bundle only -- extracted, unverified, `--summary` parses.

    A genuinely signed attestation, read with no identity flags, gets its DSSE
    payload extracted with no cryptography run over it at all -- listed
    `verified: false`, no certificate fields, and `--summary` still parses the
    document's own CycloneDX fields.

    Red proof: the identical package, read WITH identity flags (demand),
    reports the very same document `verified: true` with certificate fields
    present -- the mode flag alone flips the classification of unchanged
    bytes.
    """
    predicate = write_json(tmp_path / "sbom.cdx.json", {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1,
        "components": [{"type": "library", "name": "left-pad", "version": "1.0.0"}],
    })
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    result = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--summary",
        published_package.short, check=False,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr}"
    data = json.loads(result.stdout)["data"]
    assert data["summary"]["verification"] == "unverified"
    assert (data["summary"]["verified"], data["summary"]["unverified"]) == (0, 1)
    [entry] = data["entries"]
    assert entry["verified"] is False
    assert "certificate_identity" not in entry
    assert "certificate_oidc_issuer" not in entry
    assert "signed_at" not in entry
    assert entry["summary"]["component_count"] == 1

    # Red proof: the same bytes, read with identity flags, verify instead.
    demanded = ocx.run(
        "package", "sbom", *sbom_args(sigstore_stack), published_package.short, check=False,
    )
    assert demanded.returncode == 0, demanded.stdout
    demanded_data = json.loads(demanded.stdout)["data"]
    assert demanded_data["summary"]["verification"] == "verified"
    [demanded_entry] = demanded_data["entries"]
    assert demanded_entry["verified"] is True
    assert demanded_entry["certificate_identity"] == sigstore_stack.identity


def test_edge_5_permissive_mode_lists_a_signed_and_an_unsigned_document_both_unverified(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Edge case 5: permissive mixed -- a signed bundle beside a raw unsigned referrer.

    No cryptography runs over either kind under permissive mode: both are
    listed `verified: false`, and the summary's own trust class is
    `unverified` for the whole run.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attest(ocx, sigstore_stack, identity_token, published_package, predicate)

    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    result = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr}"
    data = json.loads(result.stdout)["data"]
    assert data["summary"]["verification"] == "unverified"
    assert (data["summary"]["verified"], data["summary"]["unverified"]) == (0, 2), (
        f"both documents must list unverified under permissive mode, got {data['summary']!r}"
    )
    assert all(not entry["verified"] for entry in data["entries"])


def test_edge_6_verify_flag_with_no_identity_source_is_a_usage_error(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """Edge case 6: `--verify` with no identity flags and no policy -> 64.

    `--verify` demands verification; `resolve_mode` refuses `(Demand, policies
    empty)` with `NoIdentityProvided` rather than silently falling back to
    permissive -- an operator who typed `--verify` must not have it silently
    ignored. Each test runs against a fresh, per-test `OCX_HOME` (`OcxRunner`
    isolates `OCX_HOME`/`XDG_CONFIG_HOME` per instance), so there is no stray
    `[trust.policy]` to accidentally satisfy this.
    """
    result = ocx.run(
        "package", "sbom", "--verify",
        "--platform", current_platform(),
        "--rekor-url", sigstore_stack.rekor_url,
        "--sigstore-trusted-root", str(sigstore_stack.trust_root),
        published_package.short,
        check=False,
    )
    assert result.returncode == 64, f"expected a usage error, got {result.returncode}\nstdout: {result.stdout}"
    assert json.loads(result.stdout)["error"]["detail"] == "no_identity_provided"


def test_edge_7_no_verify_conflicts_with_certificate_identity_flags(
    ocx: OcxRunner,
    published_package: PackageInfo,
) -> None:
    """Edge case 7: `--no-verify` + `--certificate-identity`/`--certificate-oidc-issuer` -> 64.

    Supplying an identity while refusing to check it is contradictory, not the
    other half of a paired toggle -- clap `conflicts_with_all` on
    `Verification` (`options/verification.rs`) refuses to parse the
    combination at all, before any network round-trip.
    """
    result = ocx.run(
        "package", "sbom", "--no-verify",
        "--certificate-identity", "me@example.com",
        "--certificate-oidc-issuer", "https://example.com",
        published_package.short,
        check=False,
    )
    assert result.returncode == 64, (
        f"--no-verify with an identity must be a usage error, got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )


def test_edge_8_verify_and_no_verify_flags_last_win(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """Edge case 8: `--verify --no-verify` and `--no-verify --verify` diverge by order.

    POSIX last-wins (`overrides_with`, `options/verification.rs`): with the
    identity flags absent either way, `--no-verify` last resolves permissive
    and succeeds; `--verify` last resolves demand and refuses at 64 for lack
    of an identity source. The two invocations differ only in flag order, so
    this is its own red/green proof.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    permissive_last = ocx.run(
        "package", "sbom", "--verify", "--no-verify",
        *no_identity_args(sigstore_stack), published_package.short, check=False,
    )
    assert permissive_last.returncode == 0, (
        f"--no-verify last must win and list permissively, got {permissive_last.returncode}\n"
        f"stdout: {permissive_last.stdout}"
    )
    assert json.loads(permissive_last.stdout)["data"]["summary"]["verification"] == "unverified"

    demand_last = ocx.run(
        "package", "sbom", "--no-verify", "--verify",
        *no_identity_args(sigstore_stack), published_package.short, check=False,
    )
    assert demand_last.returncode == 64, (
        f"--verify last must win and demand an identity, got {demand_last.returncode}\n"
        f"stdout: {demand_last.stdout}"
    )
    assert json.loads(demand_last.stdout)["error"]["detail"] == "no_identity_provided"



# ──────────────────────────────────────────────────────────────────────────────
# W2: an unverified row is labelled by its LAYER media type, never the listing's
# artifactType -- the listing is a prefilter only.
# ──────────────────────────────────────────────────────────────────────────────


def test_w2_a_cross_family_mislabelled_referrer_is_listed_under_its_layer_type(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """Edge case W2-1: artifactType claims CycloneDX, the LAYER is `text/spdx`.

    Read permissively (no identity flags -- the artifactType/layer mismatch is
    checked at fetch time, in `read_unverified_referrer`, same as before). The
    row must be listed labelled SPDX -- the layer's own claim -- not refused,
    and not labelled CycloneDX by the listing's artifactType. `--type cyclonedx`
    must miss it (79, nothing else on the subject); `--type spdx` must hit it.

    Red proof, stated explicitly: under the prior (listing-artifactType-based)
    labelling this row would have reported `predicate_type == CYCLONEDX_URI` --
    the exact value the assertion below checks is ABSENT. That is the
    assertion that would have read differently before this fix.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE,
        layer_media_type=SBOM_SPDX_TEXT_MEDIA_TYPE,
        payload=b"SPDXVersion: SPDX-2.3\nDataLicense: CC0-1.0\n",
    )

    listed = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"a cross-family disagreement must be labelled, not refused\nstdout: {listed.stdout}"
    [entry] = json.loads(listed.stdout)["data"]["entries"]
    assert entry["predicate_type"] == SPDX_URI, (
        f"the layer served SPDX bytes, so SPDX is what the row must claim, got: {entry!r}"
    )
    # Red proof: the prior labelling read the listing's artifactType, which
    # here claims CycloneDX -- this value must NOT appear.
    assert entry["predicate_type"] != CYCLONEDX_URI

    missed = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--type", "cyclonedx",
        published_package.short, check=False,
    )
    assert missed.returncode == 79, (
        f"--type cyclonedx must ignore the listing's claim and miss the SPDX layer, "
        f"got {missed.returncode}\nstdout: {missed.stdout}"
    )
    assert json.loads(missed.stdout)["error"]["detail"] == "attestation_not_found"

    hit = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--type", "spdx",
        published_package.short, check=False,
    )
    assert hit.returncode == 0, f"--type spdx must match the layer's real type\nstdout: {hit.stdout}"
    [hit_entry] = json.loads(hit.stdout)["data"]["entries"]
    assert hit_entry["predicate_type"] == SPDX_URI


def test_w2_type_flag_narrows_a_correctly_typed_raw_attachment(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """Edge case W2-2: an ordinary (non-mislabelled) raw attachment still narrows correctly.

    `--type cyclonedx` hits a genuine CycloneDX attachment; `--type spdx` misses
    it (79) -- the layer-derived narrowing introduced for the mislabelled case
    must not regress the ordinary, self-consistent one.
    """
    subject, size = adversarial.subject_of(
        ocx.registry, published_package.repo, published_package.tag, platform=current_platform(),
    )
    push_sbom_referrer(
        ocx.registry, published_package.repo, subject, size,
        media_type=SBOM_CYCLONEDX_MEDIA_TYPE, payload=CYCLONEDX_MINIMAL,
    )

    hit = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--type", "cyclonedx",
        published_package.short, check=False,
    )
    assert hit.returncode == 0, f"stdout: {hit.stdout}"
    [entry] = json.loads(hit.stdout)["data"]["entries"]
    assert entry["predicate_type"] == CYCLONEDX_URI

    missed = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--type", "spdx",
        published_package.short, check=False,
    )
    assert missed.returncode == 79, f"stdout: {missed.stdout}"
    assert json.loads(missed.stdout)["error"]["detail"] == "attestation_not_found"
