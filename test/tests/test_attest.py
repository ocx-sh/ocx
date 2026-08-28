# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package attest`` and ``ocx package push --sbom``.

Contract source: ``.claude/artifacts/adr_sbom_attestations.md`` +
``.claude/state/plans/plan_sbom_attestations.md`` scenarios S-001..S-005,
S-010, S-018. The sbom-reading half (S-006..S-008, S-017, S-019) lives in
``test_sbom.py``.

Everything here drives the real Sigstore stack (``sigstore`` compose profile)
through the ``sigstore_stack`` / ``identity_token`` fixtures — there is no fake.
``SigstoreStack.sign_args`` already spells the four flags attest needs
(``--platform``, ``--fulcio-url``, ``--rekor-url``, ``--identity-token-file``),
so it is reused verbatim rather than duplicated as an ``attest_args``.
"""
from __future__ import annotations

import base64
import json
import re
from pathlib import Path

from src import registry as reg
from src.helpers import make_package, resolved_metadata_path
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial, attestations
from tests.fixtures.sigstore_stack import SigstoreStack

#: Mirrors `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`. An attestation and
#: a signature share this artifactType and are told apart by the
#: `dev.sigstore.bundle.content` annotation (`attest/pipeline.rs`), which is
#: exactly why the annotation assertions below are not decoration.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"

#: `sha256:` + 64 hex. `.startswith("sha256:")` is also satisfied by the 12-hex
#: short form plain output uses, so it cannot tell a shortened JSON digest from
#: a full one — only equality against this pattern can.
FULL_SHA256_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")

#: The resolved URI `--type cyclonedx` must produce (ADR D-c: the report echoes
#: what was published, never the alias the caller typed).
CYCLONEDX_URI = "https://cyclonedx.org/bom"


def no_identity_args(stack: SigstoreStack) -> list[str]:
    """Platform/Rekor/trust-root flags with no certificate identity at all.

    Reading back an unsigned attach with ``package sbom`` needs the permissive
    default (no identity flags, no policy): under demand mode a lone unsigned
    referrer is refused outright (``unsigned_rejected_by_policy``, exit 77)
    rather than listed -- see ``test_sbom.py``'s edge-case matrix. Mirrors
    ``test_sbom.py::no_identity_args``; kept local rather than imported so this
    module's own fixture surface stays self-contained.
    """
    return [
        "--platform", current_platform(),
        "--rekor-url", stack.rekor_url,
        "--sigstore-trusted-root", str(stack.trust_root),
    ]


def attest(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
    predicate: Path,
    *,
    predicate_type: str = "cyclonedx",
    **kwargs,
):
    """Run ``ocx package attest`` against the local stack, never raising."""
    return ocx.run(
        "package", "attest",
        *stack.sign_args(token),
        "--predicate", str(predicate),
        "--type", predicate_type,
        pkg.short,
        check=False,
        **kwargs,
    )


def attest_unsigned(
    ocx: OcxRunner,
    pkg: PackageInfo,
    predicate: Path,
    *,
    predicate_type: str = "cyclonedx",
    **kwargs,
):
    """Run ``ocx package attest`` with no signing material at all.

    No ``--identity-token-*`` flag and no ``OCX_IDENTITY_TOKEN`` in the child
    env. ``OcxRunner`` already strips ambient env down to PATH/HOME/OCX_* (see
    ``subsystem-tests.md``), so no ambient-CI provider can fire either --
    ``DispatchingTokenProvider::has_signing_material`` sees neither an override
    token nor a detected identity, and the pipeline takes the unsigned attach
    path (``AttestMode::Unsigned``). Also omits ``--fulcio-url``/``--rekor-url``:
    unsigned mode dials neither service, so the command needs no endpoint flags.
    """
    return ocx.run(
        "package", "attest",
        "--platform", current_platform(),
        "--no-tty",
        "--predicate", str(predicate),
        "--type", predicate_type,
        pkg.short,
        check=False,
        **kwargs,
    )


def cyclonedx_predicate(tmp_path: Path) -> Path:
    """The hand-authored, deliberately non-canonical CycloneDX fixture on disk."""
    target = tmp_path / "sbom.cdx.json"
    target.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    return target


def push_argv(pkg: PackageInfo, tmp_path: Path, sbom: Path) -> list[str]:
    """A full ``package push --sbom`` argv for a package `make_package` built.

    Reconstructed from the artifacts `make_package` left in ``tmp_path`` rather
    than re-deriving them: `-m` must point at the sidecar `create` wrote next to
    the bundle, which is the file carrying the resolved dependency pins.
    """
    bundles = sorted(tmp_path.glob("bundle-*.tar.xz"))
    assert bundles, f"make_package left no bundle in {tmp_path}"
    return [
        "package", "push",
        "-p", current_platform(),
        "-m", str(resolved_metadata_path(bundles[0])),
        "--sbom", str(sbom),
        "-i", pkg.fq,
        *(str(bundle) for bundle in bundles),
    ]


# ──────────────────────────────────────────────────────────────────────────────
# S-001 — attest publishes a referrer and echoes the RESOLVED predicate type
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_publishes_a_bundle_referrer_carrying_three_annotations(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-001: a keyless attestation lands as an OCI referrer on the platform manifest.

    The three annotations are the discrimination surface: attestations and
    signatures share one ``artifactType``, so a scan that ignored
    ``dev.sigstore.bundle.content`` would hand a DSSE envelope to the signature
    verifier. Asserting the annotations here is what makes the signature-mode
    isolation test in ``test_sbom.py`` mean something.
    """
    result = attest(
        ocx, sigstore_stack, identity_token, published_package,
        cyclonedx_predicate(tmp_path),
    )
    assert result.returncode == 0, f"attest failed\nstdout: {result.stdout}\nstderr: {result.stderr}"

    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package attest"
    assert envelope["exit_code"] == 0

    data = envelope["data"]
    assert data["predicate_type"] == CYCLONEDX_URI, (
        "the report must echo the RESOLVED predicateType URI, not the --type alias"
    )
    assert data["signed"] is True, (
        "signing material was supplied via --identity-token-file, so the attach must be signed"
    )
    assert data["certificate_identity"] == sigstore_stack.identity
    assert data["certificate_oidc_issuer"] == sigstore_stack.issuer
    for field in ("subject_digest", "bundle_digest", "referrer_digest"):
        assert FULL_SHA256_DIGEST_RE.match(data[field]), (
            f"JSON must carry the full digest for {field}, got {data[field]!r}"
        )

    # The subject is the platform manifest, not the index: an attestation on the
    # index would be invisible to a platform-scoped verify.
    platform_digest = reg.fetch_platform_manifest_digest(
        ocx.registry, published_package.repo, published_package.tag,
        platform=current_platform(),
    )
    assert data["subject_digest"] == platform_digest

    manifest = reg.get_manifest(ocx.registry, published_package.repo, data["referrer_digest"])
    assert manifest["artifactType"] == SIGSTORE_BUNDLE_V03
    annotations = manifest["annotations"]
    assert annotations["dev.sigstore.bundle.content"] == "dsse-envelope"
    assert annotations["dev.sigstore.bundle.predicateType"] == CYCLONEDX_URI
    assert annotations["org.opencontainers.image.created"].endswith("Z"), (
        "the created annotation is RFC 3339 with an explicit Z (PLAT-31)"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-002 — offline refusal lands BEFORE the identity token is touched
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_refuses_offline_without_reading_the_identity_token(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """S-002: exit 77 ``offline_attest_refused``, and no credential is read.

    Ordering is the contract, not merely the code: ``refuse_when_offline`` sits
    above ``resolve_override_token`` in ``package_attest.rs`` precisely so a run
    that was already refused never opens a credential. Moving it below still
    exits 77 and still passes every unit test, so this is where that regression
    would be caught.

    The probe is a token path that does not exist. Note what this does NOT
    prove: ``resolve_override_token``'s own error context deliberately omits
    the path (CWE-209 — see ``package_sign_common.rs``), so a ``not in
    stdout + stderr`` check on ``str(missing_token)`` is true either way and
    was dropped as unfalsifiable. What the ordering actually buys is a
    DIFFERENT terminal state: reaching ``resolve_override_token`` with this
    same missing path is verified below (positive control, TEST-08) to fail
    as a bare, untyped I/O error — exit 1, ``error.kind`` ``"internal"`` — never
    the offline refusal's 77 / ``"permission_denied"``. A regression that moved
    the offline guard below token resolution would collapse an offline run
    onto that same outcome instead of the clean 77 refusal, which is exactly
    what the assertions below would catch.
    """
    missing_token = tmp_path / "identity-token-that-does-not-exist"
    assert not missing_token.exists()

    result = attest(
        ocx, sigstore_stack, missing_token, published_package,
        cyclonedx_predicate(tmp_path),
        env_overrides={"OCX_OFFLINE": "1"},
    )

    assert result.returncode == 77, (
        f"an offline attest is a deliberate policy refusal (77), got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "offline_attest_refused"
    assert envelope["error"]["kind"] == "permission_denied"

    # Positive control (TEST-08): prove the check above can actually
    # discriminate. Run the identical command WITHOUT the offline override,
    # so `resolve_override_token` is genuinely reached and fails to open the
    # same missing path. If this control ever also produced 77, the offline
    # assertion above would be proving nothing -- both code paths would be
    # indistinguishable and a regressed ordering would go undetected.
    control = attest(
        ocx, sigstore_stack, missing_token, published_package,
        cyclonedx_predicate(tmp_path),
    )
    assert control.returncode == 1, (
        "control invalid: reaching resolve_override_token with a missing "
        "token file must fail as a bare untyped error (1), not a policy "
        f"refusal, got {control.returncode}\n"
        f"stdout: {control.stdout}\nstderr: {control.stderr}"
    )
    control_envelope = json.loads(control.stdout)
    assert control_envelope["error"]["kind"] == "internal", (
        "control invalid: reaching resolve_override_token classified the "
        "same way an offline refusal does -- the assertions above would not "
        f"discriminate ordering. Got: {control_envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-003 — a SLSA provenance version below v1 is a usage error, with a remedy
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_refuses_slsa_provenance_below_v1_and_names_the_flag_to_use(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-003: ``--type slsaprovenance`` resolves to v0.2 and is refused at 64.

    A usage error rather than a data error: the document is fine, the alias is
    the problem, and the remedy is a different invocation. The message must name
    ``slsaprovenance1`` — a refusal that does not say what to type instead makes
    the caller guess between four spellings.
    """
    result = attest(
        ocx, sigstore_stack, identity_token, published_package,
        cyclonedx_predicate(tmp_path),
        predicate_type="slsaprovenance",
    )

    assert result.returncode == 64, (
        f"expected a usage error, got {result.returncode}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "provenance_version_unsupported"
    assert "--type slsaprovenance1" in envelope["error"]["message"], (
        f"the refusal must name the remedy, got: {envelope['error']['message']!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-004 — the --predicate size boundary, from both sides
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_refuses_a_predicate_one_byte_over_the_size_cap(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-004, refuse side: one byte past ``MAX_PREDICATE_FILE_BYTES`` is 65.

    The fixture is valid JSON at every length, so a failure here is attributable
    to size alone and never to a parse error riding along.
    """
    oversized = tmp_path / "oversized.json"
    oversized.write_bytes(attestations.predicate_over_size_cap())

    result = attest(ocx, sigstore_stack, identity_token, published_package, oversized)

    assert result.returncode == 65, f"expected a data error, got {result.returncode}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "predicate_too_large"
    assert str(attestations.MAX_PREDICATE_FILE_BYTES) in envelope["error"]["message"], (
        "the refusal must name the limit it enforced"
    )


def test_attest_accepts_a_predicate_exactly_at_the_size_cap(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-004, accept side: exactly ``MAX_PREDICATE_FILE_BYTES`` is not too large.

    Paired with the refusal above, this is what makes the cap a boundary rather
    than an unreachable ceiling — the cap is deliberately 1 MiB below
    ``MAX_STATEMENT_PAYLOAD_BYTES`` so an at-limit predicate still produces an
    in-limit Statement (`oci/attest.rs`), and a regression that narrowed the
    reserve would surface here and nowhere else.

    Asserted as "the size gate did not trip" rather than "exit 0": what this
    boundary owns is the CLI's own bounded read. It cannot pass vacuously — the
    one-byte-larger sibling does trip it.
    """
    at_cap = tmp_path / "at-cap.json"
    at_cap.write_bytes(attestations.predicate_at_size_cap())

    result = attest(ocx, sigstore_stack, identity_token, published_package, at_cap)

    assert result.returncode == 0, (
        f"a predicate of exactly {attestations.MAX_PREDICATE_FILE_BYTES} bytes must be "
        f"accepted; the wrapper reserve has been narrowed\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-005 — a predicate that is not JSON, and one reached through a symlink
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_refuses_a_predicate_that_is_not_json(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-005: a non-JSON predicate is 65 ``predicate_not_json``.

    Refused before the Fulcio round trip: a document that cannot be embedded
    should cost no certificate and no log entry.
    """
    not_json = tmp_path / "not-really.json"
    not_json.write_bytes(b"this is not json at all\n")

    result = attest(ocx, sigstore_stack, identity_token, published_package, not_json)

    assert result.returncode == 65, f"expected a data error, got {result.returncode}"
    assert json.loads(result.stdout)["error"]["detail"] == "predicate_not_json"


def test_attest_refuses_a_predicate_reached_through_a_symlink(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-005: ``--predicate`` does not follow a symlink, and says so.

    The consequence of following one is not confidentiality-shaped but
    irreversible: whatever the link resolved to would be embedded, signed with
    the caller's identity, published, and hashed into an append-only log. The
    refusal must name the symlink — an operator who sees only ``os error 40``
    has no idea their CI wrote a link where a file was expected.
    """
    real = cyclonedx_predicate(tmp_path)
    link = tmp_path / "sbom-link.json"
    link.symlink_to(real)

    result = attest(ocx, sigstore_stack, identity_token, published_package, link)

    assert result.returncode == 74, (
        f"expected an I/O error, got {result.returncode}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert "symlink" in envelope["error"]["message"], (
        f"the refusal must name the symlink, got: {envelope['error']['message']!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Unsigned polarity: no signing material at all -> raw, unsigned SBOM referrer
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_with_no_signing_material_attaches_unsigned_and_lists_as_unverified(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """No override token, no ambient CI identity -> the raw-attach polarity.

    ``DispatchingTokenProvider::has_signing_material`` sees nothing, so the
    pipeline takes ``AttestMode::Unsigned``: the SBOM document itself becomes
    the referrer payload, typed by the resolved predicate's own SBOM media
    type, with no DSSE envelope and no Fulcio/Rekor round trip at all. The
    command still succeeds (0), still names the resolved predicate type, and
    the report says outright that nothing vouches for it.
    """
    result = attest_unsigned(ocx, published_package, cyclonedx_predicate(tmp_path))
    assert result.returncode == 0, f"unsigned attach failed\nstdout: {result.stdout}\nstderr: {result.stderr}"

    envelope = json.loads(result.stdout)
    data = envelope["data"]
    assert data["predicate_type"] == CYCLONEDX_URI
    assert data["signed"] is False
    assert "certificate_identity" not in data, "an unsigned attach has no certificate to name"
    assert "certificate_oidc_issuer" not in data
    assert FULL_SHA256_DIGEST_RE.match(data["referrer_digest"])

    listed = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), published_package.short, check=False)
    assert listed.returncode == 0, f"sbom failed\nstdout: {listed.stdout}\nstderr: {listed.stderr}"
    listing = json.loads(listed.stdout)["data"]
    assert listing["summary"]["unverified"] == 1
    assert listing["summary"]["verified"] == 0
    [entry] = listing["entries"]
    assert entry["verified"] is False
    assert entry["predicate_type"] == CYCLONEDX_URI
    assert "certificate_identity" not in entry
    assert "certificate_oidc_issuer" not in entry
    assert "signed_at" not in entry

    summarized = ocx.run(
        "package", "sbom", *no_identity_args(sigstore_stack), "--summary",
        published_package.short, check=False,
    )
    assert summarized.returncode == 0, (
        f"--summary must parse an unverified CycloneDX document like any other\n"
        f"stdout: {summarized.stdout}"
    )
    [summary_entry] = json.loads(summarized.stdout)["data"]["entries"]
    assert summary_entry["summary"]["component_count"] == 2


def test_push_with_sbom_and_no_signing_material_reports_unsigned_and_reads_back_unverified(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """``push --sbom`` with no OIDC identity available still succeeds, unsigned.

    Mirrors the standalone ``attest`` case above through the ``--sbom`` sugar:
    no ``OCX_IDENTITY_TOKEN`` and no ambient CI env, so the push's own attest
    step resolves the same ``AttestMode::Unsigned`` polarity ``package
    attest`` would.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = cyclonedx_predicate(tmp_path)

    result = ocx.run(*push_argv(pkg, tmp_path, sbom), check=False)
    assert result.returncode == 0, f"push failed\nstdout: {result.stdout}\nstderr: {result.stderr}"

    report = json.loads(result.stdout)
    assert report["status"] == "pushed"
    attestation = report["attestation"]
    assert attestation["status"] == "succeeded"
    assert attestation["signed"] is False

    listed = ocx.run("package", "sbom", *no_identity_args(sigstore_stack), pkg.short, check=False)
    assert listed.returncode == 0, f"sbom failed\nstdout: {listed.stdout}\nstderr: {listed.stderr}"
    listing = json.loads(listed.stdout)["data"]
    assert listing["summary"]["unverified"] == 1
    [entry] = listing["entries"]
    assert entry["verified"] is False
    assert entry["referrer_digest"] == attestation["referrer_digest"]


def test_attest_slsa_provenance_with_no_signing_material_refuses_naming_the_remedy(
    ocx: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """A non-SBOM ``--type`` has no unsigned home: exit 64, ``unsigned_type_unsupported``.

    An unsigned referrer records what it is in its own ``artifactType`` and
    nowhere else, so a provenance predicate -- which has no SBOM media type to
    declare -- cannot be attached without a signing identity, at any provenance
    version. The refusal must name the remedy (supply an identity), not just
    the failure, since the document itself is fine.
    """
    predicate = tmp_path / "provenance.json"
    predicate.write_text('{"predicateType":"https://slsa.dev/provenance/v1"}')

    result = attest_unsigned(
        ocx, published_package, predicate, predicate_type="slsaprovenance",
    )

    assert result.returncode == 64, (
        f"expected a usage error, got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "unsigned_type_unsupported"
    assert "OIDC identity" in envelope["error"]["message"], (
        f"the refusal must name the remedy, got: {envelope['error']['message']!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-018 — offline `push --sbom` refuses with the ATTEST code, not the offline one
# ──────────────────────────────────────────────────────────────────────────────


def test_offline_push_with_sbom_refuses_at_77_not_the_generic_offline_81(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """S-018: the ``--sbom`` refusal beats the generic offline error.

    Two exit codes are reachable for an offline ``push --sbom``: 77, because an
    offline attestation is a deliberate policy refusal, and 81, because
    ``remote_client()`` raises ``OfflineMode`` for any push. Which one arrives
    is decided purely by statement order in ``package_push.rs`` — the refusal
    block sits above ``Publisher::new``, and moving it below silently returns 81
    with no unit test reddening, because none of them reaches ``remote_client``.

    A script branching on 77 must see the same code here as it sees from
    ``ocx package attest``, so 81 is asserted against by name rather than merely
    being "not 77".
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = cyclonedx_predicate(tmp_path)

    result = ocx.run(
        *push_argv(pkg, tmp_path, sbom),
        check=False,
        env_overrides={"OCX_OFFLINE": "1"},
    )

    assert result.returncode != 81, (
        "the generic offline error (81) beat the attest refusal — the "
        "refuse_when_offline block has moved below the push client construction"
    )
    assert result.returncode == 77, (
        f"expected the attest refusal (77), got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["command"] == "package push", "the refusal is reported on the push route"
    assert envelope["error"]["detail"] == "offline_attest_refused", (
        "the push route must report the same slug as the attest route"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-010 — a failed attestation does not roll back a push that already landed
# ──────────────────────────────────────────────────────────────────────────────


def test_push_with_sbom_keeps_the_push_when_the_attestation_fails(
    ocx: OcxRunner,
    unique_repo: str,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-010: push lands, attestation fails, the report is still owed and emitted.

    A push is not undoable, so the failure mode worth guarding is a tool that
    treats a late attestation error as a reason to report nothing: the operator
    would then have a published package and no record of it. The report must
    reach stdout, carry ``status: pushed`` and the failed attestation outcome,
    and only then may the attestation failure own the exit code.

    The attestation is made to fail hermetically by routing outbound HTTPS to a
    closed local port. ``push --sbom`` has no endpoint flags and this test
    writes no ``[trust.sigstore]`` config, so it addresses builtin public
    Fulcio — through the dead proxy. The success half of S-010 is the
    config-driven test below, the one lever that points push at the local
    stack.
    """
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        dead = f"http://127.0.0.1:{probe.getsockname()[1]}"

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = cyclonedx_predicate(tmp_path)

    result = ocx.run(
        *push_argv(pkg, tmp_path, sbom),
        check=False,
        env_overrides={
            "HTTPS_PROXY": dead,
            "https_proxy": dead,
            "OCX_IDENTITY_TOKEN": identity_token.read_text().strip(),
        },
    )

    assert result.returncode != 0, "a failed attestation must decide a non-zero exit"

    report = json.loads(result.stdout)
    assert report["status"] == "pushed", (
        f"the push must not be rolled back by a later attestation failure, got "
        f"{report.get('status')!r}"
    )
    assert report["attestation"]["status"] == "failed"
    assert report["attestation"]["message"], "a failed outcome must say why"

    # The push really landed: the manifest is resolvable in the registry, not
    # merely claimed by a report that could have been assembled optimistically.
    digest = reg.fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=current_platform(),
    )
    assert FULL_SHA256_DIGEST_RE.match(digest)


def test_push_with_sbom_reaches_the_stack_through_trust_sigstore_config(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-010 (success half): `[trust.sigstore]` endpoints carry the pipeline.

    ``push --sbom`` has no ``--fulcio-url``/``--rekor-url`` flags, so the
    config tier is the only way to point it at the local stack — the one
    acceptance row where config-sourced endpoints must carry a real signing
    round-trip end to end, not just a unit-tested precedence rule.

    Exit 0 discriminates: the identity token below is minted by the local
    dex, which builtin public Fulcio rejects. A run that ignored the config
    could never reach ``attestation.status == "succeeded"``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = cyclonedx_predicate(tmp_path)

    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        "[trust.sigstore]\n"
        f'fulcio_url = "{sigstore_stack.fulcio_url}"\n'
        f'rekor_url = "{sigstore_stack.rekor_url}"\n'
    )

    result = ocx.run(
        *push_argv(pkg, tmp_path, sbom),
        check=False,
        env_overrides={
            "OCX_IDENTITY_TOKEN": identity_token.read_text().strip(),
            "OCX_NO_PROJECT": "1",
        },
    )

    assert result.returncode == 0, (
        f"expected a fully attested push, got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    report = json.loads(result.stdout)
    assert report["status"] == "pushed"
    attestation = report["attestation"]
    assert attestation["status"] == "succeeded", attestation
    assert attestation["predicate_type"] == CYCLONEDX_URI
    assert attestation["signed"] is True, (
        "OCX_IDENTITY_TOKEN is signing material -- an env-token push must still sign, "
        "never silently downgrade to an unsigned attach"
    )
    assert FULL_SHA256_DIGEST_RE.match(attestation["referrer_digest"]), (
        "the report must name the published referrer by full digest"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Sanity: the fixture the byte-fidelity contract rests on
# ──────────────────────────────────────────────────────────────────────────────


def test_the_predicate_fixture_is_deliberately_non_canonical() -> None:
    """The premise ``test_sbom.py``'s byte-fidelity test rests on.

    A compact, already-sorted document would round-trip even through an
    implementation that re-serialized it, and the extraction test would pass for
    the wrong reason. Asserted here so that failure reports itself as "the
    fixture changed" rather than as an unexplained pass three files away.
    """
    raw = attestations.PRETTY_CYCLONEDX_PATH.read_bytes()
    canonical = json.dumps(
        json.loads(raw), sort_keys=True, separators=(",", ":")
    ).encode()
    assert raw != canonical, "the fixture is canonical; it can no longer detect re-serialization"
    assert b'"components"    :' in raw, "the fixture lost the odd interior whitespace"
    assert "模块".encode() in raw and "café".encode() in raw, (
        "the fixture lost its multi-byte UTF-8, so a byte-fidelity bug has only ASCII to hide in"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-009 — structural / delegated refusal edge cases (WP-R5)
# ──────────────────────────────────────────────────────────────────────────────
#
# All three fixtures reuse a genuine, donor attestation's `verificationMaterial`
# (certificate + Rekor tlog entry + inclusion proof) rather than crafting one:
# `BundleParts::from_bundle` (`verify/pipeline.rs`) requires those to be
# structurally complete before OCX's own envelope checks ever run, so a bundle
# with none would refuse for the wrong reason before reaching the property each
# test exists to prove. See `attestations.replace_attestation_envelope`.


def _donor_bundle(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
    tmp_path: Path,
) -> tuple[str, int, dict]:
    """Publish one genuine attestation; return (subject_digest, size, its bundle)."""
    attested = attest(ocx, stack, token, pkg, cyclonedx_predicate(tmp_path))
    assert attested.returncode == 0, (
        f"donor attest failed\nstdout: {attested.stdout}\nstderr: {attested.stderr}"
    )
    digest, size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag, platform=current_platform())
    bundle = attestations.attestation_bundle(ocx.registry, pkg.repo, digest)
    return digest, size, bundle


def test_verify_refuses_a_dsse_envelope_carrying_two_signatures(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-009, ADR Part III checklist row 8 (``MultipleSignatures``).

    ``DsseEnvelope::parse`` hard-refuses ``signatures.len() != 1`` before either
    signature is checked (``attest/dsse.rs``), so neither of the fixture's two
    signatures needs to verify against anything -- the refusal is structural,
    reached before the delegated crypto call.
    """
    digest, size, _donor = _donor_bundle(ocx, sigstore_stack, identity_token, published_package, tmp_path)
    attestations.replace_attestation_envelope(
        ocx.registry, published_package.repo, digest, size,
        dsse_envelope=attestations.two_signature_envelope(),
    )

    result = ocx.run(
        "package", "verify", "--attestation", "--type", "cyclonedx",
        *sigstore_stack.verify_args(), published_package.short,
        check=False,
    )
    assert result.returncode != 0, (
        f"a two-signature envelope must be refused\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "multiple_signatures", envelope["error"]


def test_verify_refuses_a_dsse_payload_that_fails_to_parse_even_with_a_genuine_signature(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-009, the CVE-2026-39395 shape, ADR Part III checklist row 2.

    The fixture's payload does not parse as JSON, so the STRUCTURAL half
    (``verify_envelope``) refuses it before the delegated crypto call --
    refusal does not depend on whether the signature is genuine. The
    ``bundle_parse_failed`` slug is shared by ``parse_bundle``, the envelope
    re-serialize, and ``statement::parse``, so this test pins the property
    (genuine signature never implies acceptance), not which stage refused. The fixture's own signature genuinely verifies
    (proven standalone by ``attestations.py``'s ``self_check()``, over the
    real PAE bytes against ``public_key_pem``); this test proves ocx still
    refuses it end to end, closing the "signature verified, therefore
    accepted" gap the CVE names.

    Message-signature bundles carry no in-toto Statement -- ``statement::parse``
    runs only on the dsse-envelope content path -- so there is no
    message-signature analog of this fixture to also cover.
    """
    digest, size, _donor = _donor_bundle(ocx, sigstore_stack, identity_token, published_package, tmp_path)
    attestations.replace_attestation_envelope(
        ocx.registry, published_package.repo, digest, size,
        dsse_envelope=attestations.malformed_payload_valid_signature()["envelope"],
    )

    result = ocx.run(
        "package", "verify", "--attestation", "--type", "cyclonedx",
        *sigstore_stack.verify_args(), published_package.short,
        check=False,
    )
    assert result.returncode != 0, (
        f"a payload that fails to parse must be refused even with a genuine "
        f"signature\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "bundle_parse_failed", envelope["error"]


#: The structural, no-crypto-needed subject-binding refusals `binds_subject`
#: (`attest/statement.rs`) can produce. If a multi-subject statement's target
#: at a non-first index were ever refused BY OCX'S OWN CHECK, it would surface
#: as one of these -- reached before the delegated crypto call runs at all.
_STRUCTURAL_SUBJECT_BINDING_KINDS = frozenset({
    "statement_subject_mismatch",
    "statement_subject_absent",
    "statement_subject_weak_algorithm",
})


def test_multi_subject_target_at_index_one_is_not_refused_by_ocxs_own_subject_check(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-009 (ADR "Subject binding, precisely").

    OCX's own ``binds_subject`` iterates EVERY subject looking for a match, so
    a target present only at ``subject[1]`` binds exactly like a match at
    ``subject[0]`` would (``attest/statement.rs``, rows 4-6).

    The ADR records a STRONGER, separate claim on top of that: the delegated
    ``sigstore`` crate's own DSSE verification additionally hard-requires
    ``subject[0]`` itself to match, so this same bundle is refused overall
    even though OCX's own check alone would accept it (cited to the upstream
    crate's ``verifier.rs:76-80``). That claim is about a dependency's
    internals, not code this project owns (``quality-core.md`` "Don't Own
    Non-Domain Code"), and reproducing it end to end would need a genuinely
    Fulcio+Rekor-signed bundle over this exact multi-subject payload -- no
    tool available to this suite produces one: ``ocx package attest`` always
    builds its own single-subject Statement, and cosign's
    ``attest-blob``/``sign-blob`` each take exactly one blob argument with no
    flag for a second, decoy subject. Flagged here as a testability gap in the
    fixture's stronger claim, not asserted.

    What IS provable, and is the property OCX's own code is responsible for:
    reusing a genuine donor's verification material against a NEW payload
    makes its signature stale against either payload -- so a target-at-index-0
    control and the target-at-index-1 fixture are refused for the exact same
    reason (``signature_invalid``, from the delegated crypto stage), proving
    neither one was refused by OCX's OWN structural subject check. If
    ``binds_subject`` ever regressed to check only ``subject[0]``, the
    index-1 case would instead fail EARLIER with one of
    `_STRUCTURAL_SUBJECT_BINDING_KINDS` while the index-0 control kept
    passing structurally -- the two `detail` values would diverge, and this
    test would catch it.
    """
    digest, size, donor = _donor_bundle(ocx, sigstore_stack, identity_token, published_package, tmp_path)
    stale_signatures = donor["dsseEnvelope"]["signatures"]
    target_hex = digest.removeprefix("sha256:")

    def envelope_for(statement: dict) -> dict:
        payload_bytes = json.dumps(statement).encode()
        return {
            "payload": base64.b64encode(payload_bytes).decode(),
            "payloadType": "application/vnd.in-toto+json",
            # Reused verbatim: stale against BOTH payloads below, on purpose --
            # see the docstring. Neither signature needs to verify for this
            # comparison to be meaningful.
            "signatures": stale_signatures,
        }

    def run_verify() -> dict:
        result = ocx.run(
            "package", "verify", "--attestation", "--type", "cyclonedx",
            *sigstore_stack.verify_args(), published_package.short,
            check=False,
        )
        assert result.returncode != 0, (
            f"a stale-signature bundle must be refused\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
        return json.loads(result.stdout)["error"]

    multi_subject_statement = attestations.multi_subject_statement_target_at_subject_one(target_hex)
    attestations.replace_attestation_envelope(
        ocx.registry, published_package.repo, digest, size,
        dsse_envelope=envelope_for(multi_subject_statement),
    )
    multi_subject_error = run_verify()

    single_subject_statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": "target", "digest": {"sha256": target_hex}}],
        "predicateType": "https://cyclonedx.org/bom",
        "predicate": {"bomFormat": "CycloneDX", "specVersion": "1.6", "components": []},
    }
    attestations.replace_attestation_envelope(
        ocx.registry, published_package.repo, digest, size,
        dsse_envelope=envelope_for(single_subject_statement),
    )
    control_error = run_verify()

    assert control_error["detail"] == "signature_invalid", (
        "control invalid: the subject[0] baseline no longer reaches the "
        "delegated crypto stage -- if both runs fail at an earlier shared "
        "stage (e.g. bundle_parse_failed), the equality below proves "
        f"nothing about binds_subject. Got: {control_error}"
    )
    assert multi_subject_error["detail"] not in _STRUCTURAL_SUBJECT_BINDING_KINDS, (
        "target-at-subject[1] was refused by OCX's OWN structural subject "
        f"check: {multi_subject_error}"
    )
    assert multi_subject_error["detail"] == control_error["detail"], (
        "target-at-subject[1] was refused differently than target-at-subject[0] "
        f"under an identical stale signature -- multi: {multi_subject_error}, "
        f"control: {control_error}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — no referrers API → exit 84 (mirrors test_verify.py /
# test_sign.py's identically-named contract for the attest pipeline's OWN
# probe: `attest/pipeline.rs` calls `map_client_error` at its own call site,
# not through sign's error path, so this is not covered by the sign/verify rows)
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_referrers_unsupported_exits_84(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Registry without referrers API -> exit 84, naming its own slug.

    ``legacy_registry`` (``registry:2``, #106/#195 negative fixture) does not
    implement ``/v2/<name>/referrers/``. `attest/pipeline.rs` probes referrers
    support and maps `ClientError::ReferrersUnsupported` through its own
    `map_client_error` (not sign's) into `SignErrorKind::ReferrersUnsupported`
    -- both `error.kind` and `error.detail` read `referrers_unsupported`
    because the category and the specific kind coincide for this variant.
    """
    legacy_ocx = OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)
    pkg = make_package(legacy_ocx, unique_repo, "1.0.0", tmp_path)
    result = attest(
        legacy_ocx, sigstore_stack, identity_token, pkg,
        cyclonedx_predicate(tmp_path),
    )
    assert result.returncode == 84, (
        f"expected exit 84 (ReferrersUnsupported), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["kind"] == "referrers_unsupported", (
        f"wrong error category for a referrers-incapable registry: {envelope['error']}"
    )
    assert envelope["error"]["detail"] == "referrers_unsupported", (
        f"the refusal must name its own slug, not a generic one: {envelope['error']}"
    )
