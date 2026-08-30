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
    as an I/O error — exit 74, ``error.kind`` ``"io_error"`` — never the
    offline refusal's 77 / ``"permission_denied"``. A regression that moved
    the offline guard below token resolution would collapse an offline run
    onto that same outcome instead of the clean 77 refusal, which is exactly
    what the assertions below would catch.

    The control was exit 1 ``internal`` until the token reads were typed
    through ``file_error``: a missing credential file is an operator's typo,
    not an ocx bug. Only the *value* moved — the control's job is to be a
    terminal state distinct from 77, which 74 is.
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
    assert control.returncode == 74, (
        "control invalid: reaching resolve_override_token with a missing "
        "token file must fail as an I/O error (74), not a policy "
        f"refusal, got {control.returncode}\n"
        f"stdout: {control.stdout}\nstderr: {control.stderr}"
    )
    control_envelope = json.loads(control.stdout)
    assert control_envelope["error"]["kind"] == "io_error", (
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
# Registry capability — no referrers API → the tag-schema fallback index
# (the attest pipeline runs its OWN probe and its own referrer write, so this
# is not covered by the identically-shaped row in test_sign.py)
# ──────────────────────────────────────────────────────────────────────────────


def test_attest_lands_in_the_fallback_index_on_a_registry_without_the_referrers_api(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Attesting on a `registry:2` writes the fallback index instead of exiting 84.

    `adr_oci_referrers_signing_v1.md` Amendment 10 reverses S1-F for both
    pipelines. Wiring only `sign` would have left `attest` refusing on exactly
    the registries `sign` had just learnt to serve, which is why this row exists
    beside the one in `test_sign.py` rather than deferring to it.

    The 404 is asserted here rather than assumed: without it a green would be
    satisfied by a registry that grew a Referrers API, and the fallback write —
    the only code this test is for — would never have run.
    """
    legacy_ocx = OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)
    pkg = make_package(legacy_ocx, unique_repo, "1.0.0", tmp_path)
    result = attest(
        legacy_ocx, sigstore_stack, identity_token, pkg,
        cyclonedx_predicate(tmp_path),
    )
    assert result.returncode == 0, (
        f"attest must succeed on a registry without the Referrers API, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
    data = json.loads(result.stdout)["data"]

    status, _ = reg.list_referrers(legacy_registry, pkg.repo, data["subject_digest"])
    assert status == 404, (
        f"this fixture must have no Referrers API, else the attest above could "
        f"have been carried by one; got HTTP {status}"
    )

    served, _ = reg.fetch_manifest_raw(
        legacy_registry, pkg.repo, reg.referrers_fallback_tag(data["subject_digest"])
    )
    index = json.loads(served)
    entry = next(
        (item for item in index["manifests"] if item["digest"] == data["referrer_digest"]),
        None,
    )
    assert entry is not None, (
        f"the referrer attest reported ({data['referrer_digest']}) is not named "
        f"in the fallback index: {index['manifests']!r}"
    )
    pushed = reg.get_manifest(legacy_registry, pkg.repo, data["referrer_digest"])
    assert entry["artifactType"] == pushed["artifactType"], (
        "the index entry must carry the referrer's own artifactType — it is "
        "what a reader filters on, and the field cosign's own fallback write "
        "loses (sigstore/cosign#4641)"
    )


# ──────────────────────────────────────────────────────────────────────────────
# `--platform` is a narrowing modifier, not a required selector (WP1)
# ──────────────────────────────────────────────────────────────────────────────


def _endpoint_args_without_platform(stack: SigstoreStack, token: Path) -> list[str]:
    """`sign_args` with the `--platform` it hard-codes removed.

    The flag's absence is the subject of this section, so it is spelled here
    rather than added as a parameter to the shared fixture helper — a helper
    that could be asked to include it invites the call site that proves
    nothing.
    """
    return [
        "--fulcio-url", stack.fulcio_url,
        "--rekor-url", stack.rekor_url,
        "--identity-token-file", str(token),
    ]


def _index_and_platform_digests(ocx: OcxRunner, pkg: PackageInfo) -> tuple[str, str]:
    """The tag's own digest and its host-platform child's, asserted distinct.

    Load-bearing rather than defensive: the cases below tell "acted on the
    index" from "narrowed to the child" by comparing against these two, so a
    fixture pushing a bare manifest would pass them while measuring nothing.
    """
    index_digest = reg.fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    platform_digest = reg.fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=current_platform()
    )
    assert index_digest != platform_digest, (
        f"{pkg.short} must resolve to an image index for this section to mean "
        f"anything, but the tag and its child share {index_digest}"
    )
    return index_digest, platform_digest


def test_attest_without_platform_attaches_to_the_index(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """No `--platform`: the Statement's subject is the index the tag resolves to.

    An index-level SBOM is a legal subject (``adr_sbom_attestations.md``), and
    `--platform required = true` made it unreachable through this command.
    """
    pkg = published_package
    index_digest, platform_digest = _index_and_platform_digests(ocx, pkg)

    result = ocx.run(
        "package", "attest",
        *_endpoint_args_without_platform(sigstore_stack, identity_token),
        "--predicate", str(cyclonedx_predicate(tmp_path)),
        "--type", "cyclonedx",
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == index_digest, (
        f"absent --platform must act on the resolved index ({index_digest}), "
        f"not narrow to the child ({platform_digest})"
    )
    assert data["platform"] == "any", (
        f"an absent narrowing reports as `any`, got {data['platform']!r}"
    )


def test_attest_with_platform_against_a_bare_manifest_is_refused(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """`--platform` against a reference that resolved to a single manifest.

    Exit 79 and `target_not_an_index` — the same word `sign` and `verify`
    report, because it is the same refusal from the same shared rule. The
    reference is digest-pinned, so the branch is on what resolution returned
    rather than on the reference's form.
    """
    pkg = published_package
    _index_digest, platform_digest = _index_and_platform_digests(ocx, pkg)

    result = ocx.run(
        "package", "attest",
        *sigstore_stack.sign_args(identity_token),
        "--predicate", str(cyclonedx_predicate(tmp_path)),
        "--type", "cyclonedx",
        f"{pkg.repo}@{platform_digest}",
        check=False,
    )
    assert result.returncode == 79, (
        f"expected NotFound (79), got {result.returncode}\n{result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "target_not_an_index", (
        f"expected the dedicated slug, got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# WP4 — `--signature-format` and the `sha256-<hex>.att` cosign sidecar
# ──────────────────────────────────────────────────────────────────────────────

#: Mirrors `oci::referrer::media_types::DSSE_ENVELOPE_MEDIA_TYPE`. A cosign
#: `.att` layer is a bare DSSE envelope, never the Sigstore bundle wrapping it.
DSSE_ENVELOPE_MEDIA_TYPE = "application/vnd.dsse.envelope.v1+json"


def sidecar_manifest(ocx: OcxRunner, repo: str, tag: str) -> dict | None:
    """The cosign sidecar manifest at ``tag``, or ``None`` when it is absent.

    ``None`` rather than an exception so the "no sidecar was written" half can
    be a plain assertion beside the "it was" half, in one test, against one
    subject.
    """
    try:
        return reg.get_manifest(ocx.registry, repo, tag)
    except RuntimeError:
        return None


def test_attest_signature_format_decides_whether_the_att_sidecar_is_written(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Both halves of the spec's "written only under ``simplesigning|both``".

    Driven against **one** subject in one test on purpose: the sidecar tag has
    to be absent after the default run and present after the flagged one, and
    two separate tests would let a permanently-absent tag pass the first half
    and a permanently-present one pass the second.
    """
    pkg = published_package
    predicate = cyclonedx_predicate(tmp_path)
    platform_digest = reg.fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=current_platform(),
    )
    att_tag = f"{platform_digest.replace(':', '-')}.att"

    # Half 1 — the default. A referrer, and no sidecar tag at all.
    default = attest(ocx, sigstore_stack, identity_token, pkg, predicate)
    assert default.returncode == 0, f"attest failed\n{default.stderr}"
    data = json.loads(default.stdout)["data"]
    assert FULL_SHA256_DIGEST_RE.match(data["referrer_digest"])
    assert "sidecar_digest" not in data, (
        f"the default wrote a sidecar: {data}"
    )
    assert sidecar_manifest(ocx, pkg.repo, att_tag) is None, (
        f"the default wrote {att_tag}"
    )

    # Half 2 — the same subject under `--signature-format simplesigning`.
    sidecar = ocx.run(
        "package", "attest",
        *sigstore_stack.sign_args(identity_token),
        "--signature-format", "simplesigning",
        "--predicate", str(predicate),
        "--type", "cyclonedx",
        pkg.short,
        check=False,
    )
    assert sidecar.returncode == 0, f"attest failed\n{sidecar.stderr}"
    data = json.loads(sidecar.stdout)["data"]
    assert FULL_SHA256_DIGEST_RE.match(data["sidecar_digest"])
    assert "referrer_digest" not in data, (
        f"--signature-format simplesigning must publish no referrer bundle: {data}"
    )

    manifest = sidecar_manifest(ocx, pkg.repo, att_tag)
    assert manifest is not None, f"--signature-format simplesigning wrote no {att_tag}"
    layers = manifest["layers"]
    assert len(layers) == 1, f"expected one attestation layer, got {layers!r}"
    assert layers[0]["mediaType"] == DSSE_ENVELOPE_MEDIA_TYPE, (
        "an .att layer is a bare DSSE envelope, not the bundle wrapping it"
    )

    annotations = layers[0]["annotations"]
    assert annotations["dev.sigstore.cosign/certificate"].startswith("-----BEGIN CERTIFICATE-----")
    assert json.loads(annotations["dev.sigstore.cosign/bundle"])["Payload"]["logIndex"] > 0
    # Present and empty, which is cosign's own shape: `attach attestation`
    # writes the key with an empty value (pinned by
    # fixtures/golden/attestation_sidecar_key_manifest.json) and
    # `cosign verify-attestation` refuses a layer that lacks the key entirely.
    # The value stays empty because a DSSE envelope carries its signature
    # inside, in signatures[].sig -- so the key is a presence marker, not
    # material. Asserted exactly rather than merely tolerated: anything else in
    # this position would be material claimed but not carried.
    assert annotations["dev.cosignproject.cosign/signature"] == "", (
        "cosign refuses an .att layer with no signature annotation, and writes the key "
        f"empty itself; the value must stay empty: {annotations!r}"
    )

    # The layer blob is the envelope itself: it parses, and its payload is the
    # in-toto Statement the attestation signed.
    envelope = json.loads(reg.get_blob(ocx.registry, pkg.repo, layers[0]["digest"]))
    statement = json.loads(base64.b64decode(envelope["payload"]))
    assert statement["predicateType"] == CYCLONEDX_URI
    assert statement["subject"][0]["digest"]["sha256"] == platform_digest.removeprefix("sha256:")


def test_verify_reads_back_the_att_sidecar_attest_wrote(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The round trip: OCX reads the ``.att`` sidecar its own ``attest`` wrote.

    ``--signature-format simplesigning`` published ``sha256-<hex>.att`` and the
    verifier reported the subject unattested — OCX wrote an artifact it could
    not read. Exit 79 / ``no_signatures_found`` is the shape of that hole; exit
    0 with ``discovery_method: sidecar_tag`` is the shape of it closed.

    Three things make the pass attributable to the sidecar door and nothing
    else. The write publishes **no referrer**, asserted rather than assumed, so
    the bundle door has nothing to find. ``verify`` is run with no
    ``--signature-format`` pin at all, so what answers is the default search a
    user gets, not a flag steering it at the answer. And the verified row's
    ``referrer_digest`` is checked against the layer digest read back off the
    registry, so "some attestation verified" cannot stand in for "this one did".
    """
    pkg = published_package
    predicate = cyclonedx_predicate(tmp_path)
    platform_digest = reg.fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=current_platform(),
    )
    att_tag = f"{platform_digest.replace(':', '-')}.att"

    written = ocx.run(
        "package", "attest",
        *sigstore_stack.sign_args(identity_token),
        "--signature-format", "simplesigning",
        "--predicate", str(predicate),
        "--type", "cyclonedx",
        pkg.short,
        check=False,
    )
    assert written.returncode == 0, f"attest failed\n{written.stderr}"
    assert "referrer_digest" not in json.loads(written.stdout)["data"], (
        "the read-back below is only about the sidecar door if this run left no "
        "referrer for the bundle door to find"
    )

    manifest = sidecar_manifest(ocx, pkg.repo, att_tag)
    assert manifest is not None, f"attest wrote no {att_tag}"
    layer_digest = manifest["layers"][0]["digest"]

    verify = ocx.run(
        "package", "verify",
        "--attestation", "--type", "cyclonedx",
        *sigstore_stack.verify_args(),
        pkg.short,
        check=False,
    )
    assert verify.returncode == 0, (
        f"verify --attestation must accept the .att sidecar `attest` just "
        f"published, got {verify.returncode}\n"
        f"stdout: {verify.stdout.strip()}\nstderr: {verify.stderr.strip()}"
    )
    data = json.loads(verify.stdout)["data"]
    [entry] = data["signatures"]
    assert entry["discovery_method"] == "sidecar_tag", (
        f"the sidecar tag is the only door open on this subject: {entry}"
    )
    assert entry["signature_format"] == "simplesigning", entry
    assert entry["referrer_digest"] == layer_digest, (
        f"verify must name the .att layer attest published ({layer_digest}), got {entry}"
    )
    # The keyless `.att` layer carries a real Rekor bundle (the write test above
    # pins `logIndex > 0` on it), so the verified row must credit that evidence
    # rather than report the certificate's own `notBefore` back as a signing
    # instant. A reader that discards the annotation still reaches exit 0 here;
    # only these two fields tell the two apart.
    assert entry["signed_at"], f"a logged .att must report its integratedTime: {entry}"
    assert entry["rekor_log_index"] > 0, f"a logged .att must report its log index: {entry}"


# ---------------------------------------------------------------------------
# WP2 — `--tags` / `--tags-file` index sweep
#
# Same contract `sign` carries: sweep the indices the recorded tags now resolve
# to, skip a tag that resolves to a bare manifest, survive a per-tag failure and
# report every one.
# ---------------------------------------------------------------------------


def _sweep_rows(result) -> dict[str, dict]:
    """The sweep report's rows, keyed by tag."""
    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package attest"
    return {row["tag"]: row for row in envelope["data"]["tags"]}


def test_attest_tags_sweeps_every_named_index(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A sweep over N tags attests the index each of the N resolves to.

    Two versions rather than two cascade aliases of one: aliases share an index
    digest, so a sweep that visited only the first would still report the right
    subject for the second and prove nothing.
    """
    first, second = published_two_versions
    first_index = reg.fetch_manifest_digest(ocx.registry, first.repo, first.tag)
    second_index = reg.fetch_manifest_digest(ocx.registry, second.repo, second.tag)
    assert first_index != second_index, "two versions must be two indices"

    result = ocx.run(
        "package", "attest",
        *_endpoint_args_without_platform(sigstore_stack, identity_token),
        "--predicate", str(cyclonedx_predicate(tmp_path)),
        "--type", "cyclonedx",
        "--tags", f"{first.tag},{second.tag}",
        first.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr

    rows = _sweep_rows(result)
    assert set(rows) == {first.tag, second.tag}
    for tag, index_digest in ((first.tag, first_index), (second.tag, second_index)):
        assert rows[tag]["status"] == "completed", rows[tag]
        assert rows[tag]["report"]["subject_digest"] == index_digest

    # The rows are a claim about the registry; check the registry agrees.
    for index_digest in (first_index, second_index):
        status, referrers = reg.list_referrers(ocx.registry, first.repo, index_digest)
        assert status == 200, f"referrers lookup for {index_digest} returned {status}"
        assert any(
            entry["artifactType"] == SIGSTORE_BUNDLE_V03
            for entry in referrers["manifests"]
        ), f"no attestation referrer landed on {index_digest}"


def test_attest_tags_skips_a_bare_manifest_tag_without_failing_the_run(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    registry: str,
    tmp_path: Path,
) -> None:
    """A swept tag resolving to a bare manifest is skipped, not an error.

    The exit code is the load-bearing assertion: a skip that became a failure
    would still print a warning naming the tag.
    """
    import requests

    pkg = published_package
    leaf_digest = reg.fetch_platform_manifest_digest(registry, pkg.repo, pkg.tag)
    leaf_bytes, _ = reg.fetch_manifest_raw(registry, pkg.repo, leaf_digest)
    requests.put(
        f"http://{registry}/v2/{pkg.repo}/manifests/9.9.9",
        data=leaf_bytes,
        headers={"Content-Type": reg.IMAGE_MANIFEST_MEDIA_TYPE},
        timeout=10,
    ).raise_for_status()

    result = ocx.run(
        "package", "attest",
        *_endpoint_args_without_platform(sigstore_stack, identity_token),
        "--predicate", str(cyclonedx_predicate(tmp_path)),
        "--type", "cyclonedx",
        "--tags", f"{pkg.tag},9.9.9",
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, (
        f"a skipped bare-manifest tag must not fail the run; got "
        f"{result.returncode}\n{result.stderr}"
    )
    rows = _sweep_rows(result)
    assert rows["9.9.9"]["status"] == "skipped", rows["9.9.9"]
    assert rows[pkg.tag]["status"] == "completed", rows[pkg.tag]
    assert "9.9.9" in result.stderr, (
        f"the skip must be warned about on stderr, naming the tag:\n{result.stderr}"
    )


def test_attest_tags_continues_past_a_failure_and_lists_every_one(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The sweep survives a per-tag failure and names every failure at the end.

    Tag 2 of 4 fails and so does tag 4; tags 3 and 4 must still have been
    attempted, which is what separates "continued" from "aborted".
    """
    first, second = published_two_versions
    second_index = reg.fetch_manifest_digest(ocx.registry, second.repo, second.tag)

    result = ocx.run(
        "package", "attest",
        *_endpoint_args_without_platform(sigstore_stack, identity_token),
        "--predicate", str(cyclonedx_predicate(tmp_path)),
        "--type", "cyclonedx",
        "--tags", f"{first.tag},no-such-tag-a,{second.tag},no-such-tag-b",
        first.short,
        check=False,
    )
    assert result.returncode == 79, (
        f"expected NotFound (79) for a sweep whose failures agree, got "
        f"{result.returncode}\n{result.stderr}"
    )

    rows = _sweep_rows(result)
    assert set(rows) == {first.tag, "no-such-tag-a", second.tag, "no-such-tag-b"}
    assert rows[second.tag]["status"] == "completed", (
        "the tag after the first failure must still have been attempted"
    )
    assert rows[second.tag]["report"]["subject_digest"] == second_index
    for missing in ("no-such-tag-a", "no-such-tag-b"):
        assert rows[missing]["status"] == "failed", rows[missing]
        assert rows[missing]["kind"], f"{missing} must name its failure kind"
        assert rows[missing]["message"], f"{missing} must carry a cause"


def test_attest_without_tags_keeps_the_single_reference_report(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """No `--tags`: the document is the single-reference report, unchanged."""
    pkg = published_package
    index_digest = reg.fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)

    result = ocx.run(
        "package", "attest",
        *_endpoint_args_without_platform(sigstore_stack, identity_token),
        "--predicate", str(cyclonedx_predicate(tmp_path)),
        "--type", "cyclonedx",
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert "tags" not in data, f"an unswept run must not emit a sweep document: {data}"
    assert data["subject_digest"] == index_digest
    assert data["platform"] == "any"


def test_attest_refuses_a_platform_alongside_a_sweep(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """`--platform` is exclusive with both `--tags` and `--tags-file` (exit 64)."""
    pkg = published_package
    for sweep in (["--tags", "1.0.0"], ["--tags-file", "tags.txt"]):
        result = ocx.plain(
            "package", "attest",
            "--platform", current_platform(),
            "--predicate", str(cyclonedx_predicate(tmp_path)),
            "--type", "cyclonedx",
            *sweep,
            pkg.short,
            check=False,
        )
        assert result.returncode == 64, (
            f"expected a usage error (64) for --platform with {sweep}, got "
            f"{result.returncode}\n{result.stderr}"
        )
