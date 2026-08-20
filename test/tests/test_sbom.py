# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package sbom``.

Contract source: ``.claude/artifacts/adr_sbom_attestations.md`` +
``.claude/state/plans/plan_sbom_attestations.md`` scenarios S-006, S-007,
S-008, S-017, S-019, plus the signature/attestation scan-isolation half of
S-012. The writing side (``package attest``, ``push --sbom``) is
``test_attest.py``.

Verification is unconditional here — there is no ``--no-verify`` — so every
listing assertion is also an assertion that the whole keyless pipeline agreed
with itself: Fulcio chain, Rekor inclusion, DSSE envelope, subject binding.
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
        "status": "success", "exit_code": 0, "total": 1, "verified": 1, "refused": 0,
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


def test_sbom_without_an_identity_refuses_rather_than_listing_unverified(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """S-006 error case: no identity flags and no policy is 64, never a listing.

    An unverified listing is registry-controlled text presented as fact. The
    refusal must also say what to do about it — an operator who is told only
    "no trusted identity" cannot tell a missing flag from a missing policy.
    """
    result = ocx.run(
        "package", "sbom",
        "--platform", current_platform(),
        "--rekor-url", sigstore_stack.rekor_url,
        "--sigstore-trusted-root", str(sigstore_stack.trust_root),
        published_package.short,
        check=False,
    )

    assert result.returncode == 64, f"expected a usage error, got {result.returncode}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "no_identity_provided"
    message = envelope["error"]["message"]
    assert "--certificate-identity" in message and "trust.policy" in message, (
        f"the refusal must name both remedies, got: {message!r}"
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
        "status": "partial_failure", "exit_code": 0, "total": 1, "verified": 0, "refused": 1,
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
        "status": "partial_failure", "exit_code": 0, "total": 2, "verified": 1, "refused": 1,
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
