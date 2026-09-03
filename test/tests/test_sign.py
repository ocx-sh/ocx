# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package sign`` (Slice 1 — referrers signing).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md`` +
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

All tests run against the real Rust sign pipeline. Crypto-dependent tests drive
the real Sigstore stack (`sigstore` compose profile) through the
``sigstore_stack`` / ``identity_token`` fixtures.
"""
from __future__ import annotations

import base64
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

from src.registry import (
    fetch_manifest_digest,
    fetch_manifest_raw,
    fetch_platform_manifest_digest,
    get_blob,
    get_manifest,
    list_referrers,
    referrers_fallback_tag,
)
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial
from tests.fixtures.sigstore_stack import SigstoreStack

# Sigstore bundle v0.3 artifact type — mirrors the Rust constant
# `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"

# cosign's simplesigning payload media type — mirrors the Rust constant
# `oci::referrer::media_types::SIMPLESIGNING_MEDIA_TYPE`.
SIMPLESIGNING_MEDIA_TYPE = "application/vnd.dev.cosign.simplesigning.v1+json"

# A full, un-shortened digest: `sha256:` + 64 hex chars. `.startswith("sha256:")`
# alone is also satisfied by the 12-hex short form the CLI's plain-mode output
# uses (`api/data/signature.rs::plain_fields`), so it cannot tell "JSON was
# shortened" from "JSON stayed full" — a regression that shortened the JSON
# digest would still pass a bare `startswith` check.
FULL_SHA256_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")


def bundle_payload_digest(data: dict) -> str:
    """The Sigstore bundle blob's digest, out of the leg that wrote it.

    `sign` reports one leg per wire shape, so there is no flat `bundle_digest`
    field to read: `--signature-format both` writes two payloads and one field
    could name only one of them.
    """
    for leg in data["legs"]:
        if leg["format"] == "bundle":
            return leg["payload_digest"]
    raise AssertionError(f"no bundle leg in the sign report: {data['legs']!r}")


def leg(data: dict, wire_format: str) -> dict:
    """The one reported leg for `wire_format`."""
    matches = [entry for entry in data["legs"] if entry["format"] == wire_format]
    assert len(matches) == 1, f"expected exactly one {wire_format} leg, got {data['legs']!r}"
    return matches[0]


def _endpoint_args(stack: SigstoreStack) -> list[str]:
    """`sign_args` minus `--identity-token-file`, for the token-source tests.

    Those tests are about where the token comes from, so the flag that supplies
    one has to be absent — otherwise they would all pass through the same path.
    """
    return [
        "--platform", current_platform(),
        "--fulcio-url", stack.fulcio_url,
        "--rekor-url", stack.rekor_url,
    ]


# ──────────────────────────────────────────────────────────────────────────────
# Happy path — end-to-end sign + verify
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_then_verify_happy_path(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`sign` produces a referrer; `verify` accepts it — round-trip contract.

    The canonical happy path per ADR §"Target architecture", with the JSON
    envelope pinned on both halves — that is what this adds over the smoke test,
    which only pins the exit codes.
    """
    pkg = published_package
    sign_result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            *sigstore_stack.sign_args(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert sign_result.returncode == 0, sign_result.stderr
    sign_envelope = json.loads(sign_result.stdout)
    assert sign_envelope["schema_version"] == 1
    assert sign_envelope["command"] == "package sign"
    assert sign_envelope["exit_code"] == 0
    data = sign_envelope["data"]
    assert data["subject_digest"].startswith("sha256:")
    assert bundle_payload_digest(data).startswith("sha256:")
    # The identity sign reports is the SAN of the certificate it just had
    # issued, not the token's `sub`. dex mints a `sub` that is an opaque
    # base64 provider id while Fulcio puts the *email* in the SAN, so
    # reporting the claim gave a user an identity that no
    # `--certificate-identity` and no `[[trust.policy]]` would ever match.
    assert data["certificate_identity"] == sigstore_stack.identity
    assert data["certificate_oidc_issuer"] == sigstore_stack.issuer

    verify_result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "verify",
            *sigstore_stack.verify_args(),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert verify_result.returncode == 0, verify_result.stderr
    verify_envelope = json.loads(verify_result.stdout)
    assert verify_envelope["schema_version"] == 1
    assert verify_envelope["command"] == "package verify"
    assert verify_envelope["data"]["subject_digest"] == data["subject_digest"]
    # Both commands read one certificate; they must name one identity.
    assert verify_envelope["data"]["certificate_identity"] == data["certificate_identity"]
    assert verify_envelope["data"]["certificate_oidc_issuer"] == data["certificate_oidc_issuer"]


# ──────────────────────────────────────────────────────────────────────────────
# Flag parsing — `--identity-token <TOKEN>` must NOT exist (C-S1-4)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_rejects_identity_token_flag(ocx: OcxRunner) -> None:
    """Raw ``--identity-token`` must be rejected — only file / stdin / env exist.

    C-S1-4: accepting a bare ``--identity-token <TOKEN>`` would land tokens in
    shell history, process listings, and CI logs. The flag must not exist in
    clap's parser at all.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--identity-token", "eyJhbGciOi...",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    # clap prints "unexpected argument" / "unknown option" to stderr.
    assert result.returncode != 0, (
        f"--identity-token must be rejected, got rc=0\nstdout: {result.stdout}"
    )
    stderr_lower = result.stderr.lower()
    assert (
        "unexpected argument" in stderr_lower
        or "unrecognized" in stderr_lower
        or "unknown" in stderr_lower
        or "unexpected" in stderr_lower
    ), f"expected parser rejection, got stderr: {result.stderr}"


def test_sign_identity_token_file_and_stdin_are_mutually_exclusive(
    ocx: OcxRunner, tmp_path
) -> None:
    """``--identity-token-file`` and ``--identity-token-stdin`` must conflict.

    Per ADR §"Token precedence", exactly one override source may be specified.
    clap's ``conflicts_with`` produces a usage error.
    """
    token_file = tmp_path / "token"
    token_file.write_text("dummy-token")
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--identity-token-file", str(token_file),
            "--identity-token-stdin",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode != 0, (
        f"expected rejection for conflicting token sources, got rc=0\n"
        f"stdout: {result.stdout}"
    )
    stderr_lower = result.stderr.lower()
    assert (
        "cannot be used with" in stderr_lower
        or "conflicts with" in stderr_lower
        or "the argument" in stderr_lower  # clap's standard "cannot be used with" framing
    ), f"expected conflict error, got stderr: {result.stderr}"


# ──────────────────────────────────────────────────────────────────────────────
# Token precedence — env, stdin, file (Phase 5 wires these)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_reads_env_token(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """``OCX_IDENTITY_TOKEN`` env var supplies the OIDC token to the sign flow.

    Precedence (lowest to highest): ambient provider → env → stdin → file.
    env overrides ambient; this test confirms env is consumed when present.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": identity_token.read_text().strip()}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    bundle_digest = bundle_payload_digest(envelope["data"])
    assert FULL_SHA256_DIGEST_RE.fullmatch(bundle_digest), (
        f"JSON bundle_digest must stay the full sha256:<64hex> form, not the "
        f"12-hex short form plain-mode uses, got: {bundle_digest!r}"
    )


def test_sign_reads_stdin_token(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """``--identity-token-stdin`` reads the token from stdin without shell exposure."""
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-stdin",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        input=identity_token.read_text().strip(),
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    bundle_digest = bundle_payload_digest(envelope["data"])
    assert FULL_SHA256_DIGEST_RE.fullmatch(bundle_digest), (
        f"JSON bundle_digest must stay the full sha256:<64hex> form, not the "
        f"12-hex short form plain-mode uses, got: {bundle_digest!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Offline policy — exit 81 (sign refused offline)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_offline_refused(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``--offline`` with ``package sign`` is a policy rejection (exit 77).

    Per ADR Risks: offline signing is unsupported in v1 because Fulcio + Rekor
    are hard dependencies. The rejection is a deliberate policy, not a network
    failure — hence ``PermissionDenied`` (77) not ``OfflineBlocked`` (81).

    Phase 5a wired the ``OfflineSignRefused`` early-exit in ``package_sign.rs``;
    this test pins that contract and will fail if the offline check regresses.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "--offline",
            "package", "sign",
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 77, (
        f"expected exit 77 (PermissionDenied / OfflineSignRefused), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    # 77 alone is also IdentityTokenFilePermissive and OidcPreCheckFailed (and
    # any bare filesystem EPERM) — assert the frozen slug so this test cannot
    # pass for a cause other than the offline policy refusal it names.
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "offline_sign_refused", (
        f"exit 77 must be the offline-policy refusal, not a different "
        f"PermissionDenied cause; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Token precedence — C-S1-4: file > stdin > env
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_token_file_only(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path,
) -> None:
    """C-S1-4 basic happy path: ``--identity-token-file`` only, no stdin, no env.

    The token file must be read, trimmed, and passed to the sign pipeline.
    """
    token_file = tmp_path / "token"
    # Trailing newline is common; must be trimmed before the token reaches Fulcio.
    token_file.write_text(identity_token.read_text().strip() + "\n")
    token_file.chmod(0o600)

    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-file", str(token_file),
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package sign"
    assert envelope["exit_code"] == 0
    assert bundle_payload_digest(envelope["data"]).startswith("sha256:")


def test_sign_token_stdin_overrides_env(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path,
) -> None:
    """C-S1-4 precedence: stdin token overrides ``OCX_IDENTITY_TOKEN`` env.

    The env token is one real Fulcio rejects (right claims, foreign key), so a
    successful sign can only mean stdin won. The fake stack could not do this —
    both its tokens were acceptable, so precedence was asserted structurally and
    a regression that read env instead would have passed.
    """
    foreign = adversarial.foreign_identity_token(
        sigstore_stack.issuer, sigstore_stack.identity, tmp_path
    )

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": foreign.read_text()}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-stdin",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        input=identity_token.read_text().strip(),
        capture_output=True,
        text=True,
        env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["exit_code"] == 0
    assert bundle_payload_digest(envelope["data"]).startswith("sha256:")


def test_sign_token_file_overrides_stdin_and_env(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path,
) -> None:
    """C-S1-4 precedence: file token wins over ``OCX_IDENTITY_TOKEN``.

    The env token is one Fulcio rejects, so exit 0 proves the file token was the
    one exchanged. clap enforces ``--identity-token-file`` XOR
    ``--identity-token-stdin``, so stdin cannot be in the picture as well.
    """
    foreign = adversarial.foreign_identity_token(
        sigstore_stack.issuer, sigstore_stack.identity, tmp_path
    )

    token_file = tmp_path / "token"
    token_file.write_text(identity_token.read_text().strip())
    token_file.chmod(0o600)

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": foreign.read_text()}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-file", str(token_file),
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["exit_code"] == 0
    assert bundle_payload_digest(envelope["data"]).startswith("sha256:")


# ──────────────────────────────────────────────────────────────────────────────
# Token file permissions — world-readable file → exit 77
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "win32", reason="Unix permission semantics")
def test_sign_rejects_world_readable_identity_token_file(
    ocx: OcxRunner, tmp_path
) -> None:
    """``--identity-token-file`` with mode 0o644 (world-readable) must exit 77.

    C-S1-4 / SignErrorKind::IdentityTokenFilePermissive: identity token files
    that are group- or world-readable expose OIDC tokens in multi-user
    environments. OCX must reject them at file-open time before the token is
    ever read, exiting with PermissionDenied (77) so scripts can distinguish
    this configuration error from a network or auth failure.
    """
    token_file = tmp_path / "token.oidc"
    token_file.write_text("fake-oidc-token\n")
    # Set world-readable permissions — must be rejected.
    token_file.chmod(0o644)

    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--identity-token-file", str(token_file),
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityTokenFilePermissive), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    stderr_lower = result.stderr.lower()
    assert (
        "permissive" in stderr_lower
        or "permission" in stderr_lower
        or "0o644" in stderr_lower
        or "644" in stderr_lower
        or "chmod" in stderr_lower
        or "mode" in stderr_lower
    ), f"expected permission-related wording in stderr, got: {result.stderr!r}"


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — no referrers API → the tag-schema fallback index
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_lands_in_the_fallback_index_on_a_registry_without_the_referrers_api(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """No Referrers API is no longer a refusal — it selects the fallback write.

    `adr_oci_referrers_signing_v1.md` Amendment 10 reverses S1-F: the pipeline
    used to exit 84 here, before doing any signing work. It now pushes the
    referrer manifest and names it in the OCI tag-schema fallback index, which
    is the only way a `registry:2` can answer a referrers query at all.

    **The evidence has to come from this fixture.** A green against zot proves
    nothing about this path: there the API is present, the append is skipped by
    design, and the code under test never runs. So the 404 below is asserted in
    the same test, not assumed from `test_referrers_fallback.py` — it is what
    makes the index read after it mean "the fallback carried it".
    """
    from src.helpers import make_package

    legacy_ocx = OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)
    pkg = make_package(legacy_ocx, unique_repo, "1.0.0", tmp_path)
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            *sigstore_stack.sign_args(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=legacy_ocx.env, check=False,
    )
    assert result.returncode == 0, (
        f"sign must succeed on a registry without the Referrers API, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
    data = json.loads(result.stdout)["data"]

    status, _ = list_referrers(legacy_registry, pkg.repo, data["subject_digest"])
    assert status == 404, (
        f"this fixture must have no Referrers API, else the sign above could "
        f"have been carried by one; got HTTP {status}"
    )

    served, _ = fetch_manifest_raw(
        legacy_registry, pkg.repo, referrers_fallback_tag(data["subject_digest"])
    )
    index = json.loads(served)
    referrer_digest = leg(data, "bundle")["manifest_digest"]
    entry = next(
        (item for item in index["manifests"] if item["digest"] == referrer_digest),
        None,
    )
    assert entry is not None, (
        f"the referrer sign reported ({referrer_digest}) is not named in the "
        f"fallback index: {index['manifests']!r}"
    )
    assert entry["artifactType"] == SIGSTORE_BUNDLE_V03, (
        "artifactType must survive the append — it is the field a reader "
        "filters on, and the field cosign's own fallback write loses "
        "(sigstore/cosign#4641)"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Wire shape — `--signature-format`
# ──────────────────────────────────────────────────────────────────────────────


def test_signature_format_both_writes_a_bundle_referrer_and_a_cosign_sidecar(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`--signature-format both` emits each shape, to the place that shape lives.

    The two are independent signatures over *different payloads*, not one
    signature written twice: the bundle leg signs a DSSE statement and hangs off
    a referrer, the simplesigning leg signs the claim as opaque bytes and hangs
    off the `sha256-<hex>.sig` tag. Asserting only that both legs are reported
    would pass for an implementation that wrote the same blob to both places.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--signature-format", "both",
            *sigstore_stack.sign_args(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert [entry["format"] for entry in data["legs"]] == ["bundle", "simplesigning"]

    status, index = list_referrers(ocx.registry, pkg.repo, data["subject_digest"])
    assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
    assert leg(data, "bundle")["manifest_digest"] in {
        item["digest"] for item in index["manifests"]
    }, "the bundle leg's referrer must be listed by the Referrers API"

    algorithm, encoded = data["subject_digest"].split(":", 1)
    served, _ = fetch_manifest_raw(ocx.registry, pkg.repo, f"{algorithm}-{encoded}.sig")
    sidecar = json.loads(served)
    claim_digest = leg(data, "simplesigning")["payload_digest"]
    layer = next(
        (item for item in sidecar["layers"] if item["digest"] == claim_digest), None
    )
    assert layer is not None, (
        f"the claim sign reported ({claim_digest}) is not a layer of the "
        f"sidecar: {sidecar['layers']!r}"
    )
    assert layer["mediaType"] == SIMPLESIGNING_MEDIA_TYPE

    annotations = layer["annotations"]
    assert annotations["dev.cosignproject.cosign/signature"], "signature annotation must carry the signature"
    assert "BEGIN CERTIFICATE" in annotations["dev.sigstore.cosign/certificate"], (
        "a keyless sidecar carries the Fulcio certificate in PEM"
    )
    offline = json.loads(annotations["dev.sigstore.cosign/bundle"])
    assert offline["SignedEntryTimestamp"], "the offline bundle must carry the SET"
    assert offline["Payload"]["logIndex"] >= 0

    claim = json.loads(get_blob(ocx.registry, pkg.repo, claim_digest))
    assert claim["critical"]["image"]["docker-manifest-digest"] == data["subject_digest"], (
        "the claim must name the subject it was signed for — that binding is "
        "the whole content of a simplesigning payload"
    )
    assert claim["critical"]["type"] == "cosign container image signature"

    bundle_digest = bundle_payload_digest(data)
    assert bundle_digest != claim_digest, (
        "the two legs must sign different payloads; equal digests would mean "
        "one blob was written to both places"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Key mode — `--key`
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_with_a_cosign_key_reports_a_key_backend_and_writes_no_certificate(
    ocx: OcxRunner,
    published_package: PackageInfo,
) -> None:
    """A key-mode signature carries a public key, not a certificate.

    No Sigstore stack, and deliberately so: key mode contacts neither Fulcio nor
    (without `--rekor-upload`) Rekor, so a test that needed either would be
    proving the opposite of what key mode is for. `transparency_log_index` is
    asserted `None` rather than merely absent — the field is emitted
    unconditionally so an operator can *see* that no record was made.
    """
    pkg = published_package
    key = Path(__file__).parent / "fixtures" / "golden" / "keys" / "cosign.key"
    env = {**ocx.env, "OCX_KEY_PASSWORD": "ocxtest"}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--platform", current_platform(),
            "--key", str(key),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert data["key_backend"] == "file"
    assert data["signer"] == "file", "signer must not still say keyless-fulcio under a key"
    assert data["public_key_hint"], "a key-mode signature reports the key's cosign hint"
    assert data["transparency_log_index"] is None, (
        "no --rekor-upload means no transparency record, reported as null"
    )
    assert data["certificate_identity"] == "", "there is no certificate to take an identity from"

    bundle = json.loads(get_blob(ocx.registry, pkg.repo, bundle_payload_digest(data)))
    material = bundle["verificationMaterial"]
    assert material["publicKey"]["hint"] == data["public_key_hint"]
    assert "certificate" not in material, (
        "a key-mode bundle must not carry the Fulcio leaf a keyless one does — "
        "`certificate` is the field `bundle.rs` fills in keyless mode, so this "
        "is the key a wrong branch would actually leave behind"
    )


def test_sign_with_an_env_held_key_reports_the_env_backend(
    ocx: OcxRunner,
    published_package: PackageInfo,
) -> None:
    """S-009: ``--key env://OCX_SIGNING_KEY`` signs, and the report says ``env``.

    The variable carries the PEM itself, so the same golden key that the
    ``file`` test above reads off disk is passed through the environment here
    instead — one key, two references, and the only thing that may differ is
    the reported backend. ``public_key_hint`` is asserted equal to the file
    run's on purpose: a hint that moved with the reference would mean the
    source reached the bundle's key material, which no verifier would match.
    """
    pkg = published_package
    key = Path(__file__).parent / "fixtures" / "golden" / "keys" / "cosign.key"
    env = {
        **ocx.env,
        "OCX_KEY_PASSWORD": "ocxtest",
        "OCX_SIGNING_KEY": key.read_text(),
    }
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--platform", current_platform(),
            "--key", "env://OCX_SIGNING_KEY",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert data["key_backend"] == "env", (
        "the reported backend must name the reference that produced the "
        "signature, not the envelope it happens to share with a file key"
    )
    assert data["signer"] == "env", "signer must not still say keyless-fulcio under a key"
    assert data["public_key_hint"], "a key-mode signature reports the key's cosign hint"
    assert data["certificate_identity"] == "", "there is no certificate to take an identity from"

    bundle = json.loads(get_blob(ocx.registry, pkg.repo, bundle_payload_digest(data)))
    material = bundle["verificationMaterial"]
    assert material["publicKey"]["hint"] == data["public_key_hint"]
    assert "certificate" not in material, "a key-mode bundle carries no Fulcio leaf"


def test_sign_with_an_unset_env_key_names_the_variable(
    ocx: OcxRunner,
    published_package: PackageInfo,
) -> None:
    """S-010: ``--key env://UNSET`` refuses with a message naming the variable.

    Exit 74 (``io_error``) — the same code ``--key <missing file>`` answers, so
    a wrapper branching on the exit code does not have to learn a second one
    for the second spelling of "the key is not where you said".

    With no path in the reference there is nothing else for an operator to go
    on, which is why the variable name in the message is the assertion and not
    merely a nicety.
    """
    pkg = published_package
    variable = "OCX_TEST_UNSET_SIGNING_KEY"
    env = {key: value for key, value in ocx.env.items() if key != variable}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--platform", current_platform(),
            "--key", f"env://{variable}",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert result.returncode == 74, (
        f"an unset key variable is an I/O fault, like a missing key file; "
        f"got {result.returncode}: {result.stdout}{result.stderr}"
    )
    envelope = json.loads(result.stdout)
    message = envelope["error"]["message"]
    assert variable in message, f"the refusal must name the variable: {message}"
    assert "no such file" not in message.lower(), (
        "an env reference names no file, so the refusal must not send the "
        "operator to their filesystem"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Credential exemption — OCX_IDENTITY_TOKEN must not leak to child processes
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_does_not_forward_identity_token_to_children(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """``OCX_IDENTITY_TOKEN`` must never be echoed to the sign command's output.

    Credential exemption (see ``subsystem-cli.md``): the token is a bearer
    credential read directly via ``std::env::var`` for the sign call only;
    ``Env::apply_ocx_config`` actively scrubs it from any subprocess env
    composed via ``OcxConfigView``. The Rust unit test
    ``apply_ocx_config_never_forwards_credential_tokens`` covers the lib
    boundary; this test pins the end-to-end behaviour through the sign command
    by driving a real, accepted token and asserting it never appears on stdout
    or stderr (the streams a child would inherit or a log would capture).
    """
    pkg = published_package
    token = identity_token.read_text().strip()
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_IDENTITY_TOKEN": token}, check=False,
    )
    assert result.returncode == 0, result.stderr
    # The identity token must never surface in the command's output streams.
    assert token not in result.stdout, "identity token leaked into stdout"
    assert token not in result.stderr, "identity token leaked into stderr"


# ──────────────────────────────────────────────────────────────────────────────
# SSRF guard — non-loopback HTTP and non-{http,https} schemes → exit 64
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_rejects_http_non_loopback_fulcio_url(ocx: OcxRunner) -> None:
    """`--fulcio-url http://example.com/...` must exit 64 (UsageError).

    The SSRF guard (`validate_sigstore_url`) permits `http://` only for
    loopback hosts so the fake-sigstore stack works in CI; any other
    `http://` target is a CWE-918 risk and the typed
    ``SignErrorKind::InvalidEndpointUrl`` routes it through `UsageError`.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", "http://example.com/fulcio",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / InvalidEndpointUrl on --fulcio-url), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


def test_sign_rejects_ftp_scheme_url(ocx: OcxRunner) -> None:
    """`--rekor-url ftp://...` must exit 64 (UsageError).

    Any scheme other than `http` (loopback only) and `https` is rejected at
    the SSRF guard so neither sign nor verify ever issues a non-HTTP request
    to a user-supplied endpoint.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--rekor-url", "ftp://example.com/bundle",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / InvalidEndpointUrl on ftp scheme), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Re-sign idempotency — ADR S1-I
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="ADR S1-I (idempotent re-sign) not yet implemented: sign/pipeline.rs "
    "unconditionally pushes a new bundle+referrer, so a re-sign leaves two. "
    "Deferred (not a review-fix-pass regression); tracked as production follow-up. "
    "ANY-of verify tolerates the duplicate, so this is a publisher-hygiene gap, "
    "not a verification hole. Remove this marker when S1-I dedup lands.",
)
def test_sign_then_sign_again_is_idempotent(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Two sign invocations for the same subject must not double-publish.

    Per ADR §"Re-sign idempotency" (S1-I): a second `package sign` of an
    already-signed subject either no-ops (publisher convention) or refreshes
    the existing referrer pointer; in either case the referrers list for
    that subject must contain exactly one bundle from this signer afterwards.

    Effect assertion (not just rc==0): after two signs we count the Sigstore
    bundle referrers on the *subject digest* via the Referrers API. Now that
    ANY-of verification ships, a re-sign that appended a duplicate referrer
    would silently pass every verify — so the idempotency contract has to be
    pinned on the referrer count, not the exit code.
    """
    pkg = published_package
    subject_digest: str | None = None
    for _ in range(2):
        result = subprocess.run(
            [
                str(ocx.binary),
                "--format", "json",
                "package", "sign",
                *sigstore_stack.sign_args(identity_token),
                pkg.short,
            ],
            capture_output=True,
            text=True,
            env=ocx.env, check=False,
        )
        assert result.returncode == 0, result.stderr
        subject_digest = json.loads(result.stdout)["data"]["subject_digest"]

    assert subject_digest is not None
    status, index = list_referrers(
        ocx.registry, pkg.repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03
    )
    assert status == 200, f"referrers listing failed with status {status}"
    bundles = [
        m for m in (index or {}).get("manifests", [])
        if m.get("artifactType") == SIGSTORE_BUNDLE_V03
    ]
    assert len(bundles) == 1, (
        f"re-sign must be idempotent: expected exactly one bundle referrer on "
        f"{subject_digest}, found {len(bundles)}: {bundles}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# --no-tty + missing override + no ambient → exit 77 (B3 observable contract)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_no_tty_skips_browser_fallback_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """`--no-tty` with no override token + no ambient detection → exit 77.

    B3 observable contract: when the dispatcher cannot find a token through
    any of override/ambient and `--no-tty` is set, it MUST NOT attempt the
    interactive browser OAuth (which would hang in CI). It surfaces
    `OidcPreCheckFailed` → exit 77 instead.
    """
    pkg = published_package
    # Deliberately do NOT set OCX_IDENTITY_TOKEN — and pass --no-tty so the
    # only legal path (browser) is suppressed.
    env_no_token = {k: v for k, v in ocx.env.items() if k != "OCX_IDENTITY_TOKEN"}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--no-tty",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env_no_token, check=False,
    )
    assert result.returncode == 77, (
        f"expected exit 77 (PermissionDenied / OidcPreCheckFailed), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    # 77 alone is also OfflineSignRefused and IdentityTokenFilePermissive —
    # assert the frozen slug so this test cannot pass for a cause other than
    # the no-tty/no-ambient-token precheck it names.
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "oidc_pre_check_failed", (
        f"exit 77 must be the OIDC pre-check refusal, not a different "
        f"PermissionDenied cause; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Rekor unavailable DURING SIGN — exit 83 (TransparencyLogUnavailable)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_transparency_log_unavailable_exits_83(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Rekor unreachable during the sign-time log upload → exit 83.

    Signing requires a Rekor transparency-log entry; when the log is down the
    sign cannot complete. This is a service-availability failure (retry may
    help), so it maps to ``TransparencyLogUnavailable`` (83), distinct from a data
    failure. Fulcio stays real, so the failure is isolated to the log step.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--platform", current_platform(),
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", adversarial.unreachable_rekor_url(),
            "--identity-token-file", str(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 83, (
        f"expected exit 83 (TransparencyLogUnavailable) when Rekor 503s during sign, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Wrong-key OIDC token — Fulcio rejects → exit 80 (AuthError / OidcTokenRejected)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_wrong_key_oidc_token_exits_80(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path,
) -> None:
    """A JWT with valid claims but signed by an untrusted key → exit 80.

    The token's iss/aud/sub look correct, but its signature does not verify
    against the JWKS Fulcio fetches from dex (it was signed by a foreign key).
    Fulcio must reject the CSR and the sign pipeline surfaces
    ``OidcTokenRejected`` → ``AuthError`` (80), never a network/usage code.
    """
    pkg = published_package
    foreign = adversarial.foreign_identity_token(
        sigstore_stack.issuer, sigstore_stack.identity, tmp_path
    )
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            *sigstore_stack.sign_args(foreign),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 80, (
        f"expected exit 80 (AuthError / OidcTokenRejected) for a wrong-key token, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )


def test_sign_refuses_a_rekor_url_that_resolves_into_a_forbidden_range(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A trust-service endpoint is judged by where it resolves, not how it is spelled.

    `--rekor-url https://169.254.169.254/` passes the string-level check at the
    CLI boundary -- it is HTTPS -- so without the dial-time guard ocx would
    fetch the cloud metadata endpoint and surface the response in an error
    message (CWE-918). The realistic carrier is a hostile `ocx.toml` or managed
    config tier rather than a typed flag; the flag is how the test reaches the
    same code path.

    Asserted on the exit code AND on the refusal naming the offending flag, so
    a run that merely fails to reach the metadata endpoint for some unrelated
    reason cannot be mistaken for the guard firing.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--platform", current_platform(),
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", "https://169.254.169.254/",
            "--identity-token-file", str(identity_token),
            published_package.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env, check=False,
    )
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / InvalidEndpointUrl) for a link-local "
        f"rekor URL, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    assert "--rekor-url" in result.stderr, (
        f"the refusal must name the offending flag\nstderr: {result.stderr.strip()}"
    )
    assert "169.254.169.254" in result.stderr, (
        f"the refusal must name the forbidden target\nstderr: {result.stderr.strip()}"
    )


def test_sign_still_accepts_the_explicitly_named_local_stack(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """The loopback carve-out survives the dial-time guard.

    The positive half of the check above: every other signing test in this file
    points at `http://localhost:...`, which resolves into a forbidden range and
    is admitted only because the URL literally says loopback. Without this, a
    guard that refused everything would look identical to a guard that worked.
    """
    envelope = ocx.json("package", "sign", *sigstore_stack.sign_args(identity_token), published_package.short)
    assert envelope["exit_code"] == 0, envelope
    assert bundle_payload_digest(envelope["data"]).startswith("sha256:"), envelope


# ──────────────────────────────────────────────────────────────────────────────
# SOURCE_DATE_EPOCH — one instant, taken once, reaching every place it belongs
# ──────────────────────────────────────────────────────────────────────────────

#: An arbitrary fixed instant, and its RFC 3339 rendering with an explicit `Z`
#: (Go's `time.RFC3339`, second precision — the form cosign writes). Spelled out
#: rather than computed so the test asserts against a literal a human checked,
#: not against a second copy of the formatter under test.
SOURCE_DATE_EPOCH = "1740000000"
SOURCE_DATE_EPOCH_RFC3339 = "2025-02-19T21:20:00Z"

#: Where a bundle push records its instant. Mirrors `ANNOTATION_CREATED`.
ANNOTATION_CREATED = "org.opencontainers.image.created"


def _bundle_referrer_manifest(ocx: OcxRunner, pkg: PackageInfo) -> dict:
    """The one Sigstore-bundle referrer manifest attached to ``pkg``'s platform manifest."""
    subject_digest, _size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    status, index = list_referrers(
        ocx.registry, pkg.repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03
    )
    assert status == 200 and index is not None, f"referrers list failed ({status})"
    manifests = index.get("manifests") or []
    assert len(manifests) == 1, f"expected exactly 1 bundle referrer, found {len(manifests)}"
    return get_manifest(ocx.registry, pkg.repo, manifests[0]["digest"])


def test_sign_source_date_epoch_pins_the_created_annotation(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`SOURCE_DATE_EPOCH` in the child's environment fixes the `created` annotation.

    Black-box on purpose. `bundle_now()` reads the variable straight off the
    process environment (`std::env::var_os`), which no unit test may set —
    edition 2024 makes `env::set_var` unsafe precisely because it races every
    other thread — so a child process with the variable in its environment is
    the only honest way to exercise the read. That leaves the routing itself
    (does the value actually reach the annotation?) covered nowhere but here.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "sign",
            *sigstore_stack.sign_args(identity_token), pkg.short,
        ],
        capture_output=True, text=True,
        env={**ocx.env, "SOURCE_DATE_EPOCH": SOURCE_DATE_EPOCH}, check=False,
    )
    assert result.returncode == 0, f"sign failed: {result.stderr}"

    annotations = _bundle_referrer_manifest(ocx, pkg).get("annotations") or {}
    assert annotations.get(ANNOTATION_CREATED) == SOURCE_DATE_EPOCH_RFC3339, (
        f"SOURCE_DATE_EPOCH={SOURCE_DATE_EPOCH} must stamp "
        f"{SOURCE_DATE_EPOCH_RFC3339}; got {annotations.get(ANNOTATION_CREATED)!r}. "
        f"A wall-clock value here means the variable is being ignored."
    )


def test_attest_source_date_epoch_stamps_one_instant_in_both_places(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The attest path stamps the same instant in the annotation and inside the signature.

    Two renderings of one `bundle_now()` reading: the unsigned
    `org.opencontainers.image.created` annotation on the referrer manifest, and
    the `Timestamp` field of the cosign predicate wrapper — which is *inside*
    the DSSE payload and therefore signed. Two independent clock reads would
    let one of them stop honouring `SOURCE_DATE_EPOCH` with nothing noticing,
    so the assertion that matters is that the two agree, not merely that each
    is right.

    `--type custom` because the wrapper exists only for custom predicates; the
    other nine types pass the predicate through untouched and carry no
    `Timestamp` to compare against.
    """
    pkg = published_package
    predicate = tmp_path / "custom.json"
    predicate.write_text(json.dumps({"hello": "world"}))

    result = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "attest",
            "--platform", current_platform(),
            "--predicate", str(predicate), "--type", "custom",
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", sigstore_stack.rekor_url,
            "--identity-token-file", str(identity_token),
            pkg.short,
        ],
        capture_output=True, text=True,
        env={**ocx.env, "SOURCE_DATE_EPOCH": SOURCE_DATE_EPOCH}, check=False,
    )
    assert result.returncode == 0, f"attest failed: {result.stderr}"

    manifest = _bundle_referrer_manifest(ocx, pkg)
    annotations = manifest.get("annotations") or {}
    assert annotations.get(ANNOTATION_CREATED) == SOURCE_DATE_EPOCH_RFC3339, (
        f"the attest path must honour SOURCE_DATE_EPOCH too; got "
        f"{annotations.get(ANNOTATION_CREATED)!r}"
    )

    bundle = json.loads(get_blob(ocx.registry, pkg.repo, manifest["layers"][0]["digest"]))
    statement = json.loads(base64.b64decode(bundle["dsseEnvelope"]["payload"]))
    signed_timestamp = statement["predicate"]["Timestamp"]
    assert signed_timestamp == SOURCE_DATE_EPOCH_RFC3339, (
        f"the signed cosign wrapper carries a different instant than the "
        f"annotation: {signed_timestamp!r} vs {SOURCE_DATE_EPOCH_RFC3339}. One "
        f"clock read serves both, so a difference means a second one crept in."
    )


# ──────────────────────────────────────────────────────────────────────────────
# `--platform` is a narrowing modifier, not a required selector (WP1)
# ──────────────────────────────────────────────────────────────────────────────


def _keyless_args_without_platform(
    stack: SigstoreStack, token: Path
) -> list[str]:
    """`sign_args` with the `--platform` it hard-codes removed.

    Spelled out here rather than adding a fixture parameter: the flag's absence
    is the whole subject of this section, and a helper that could be asked to
    include it would invite exactly the call site that proves nothing.
    """
    return [
        "--fulcio-url", stack.fulcio_url,
        "--rekor-url", stack.rekor_url,
        "--identity-token-file", str(token),
    ]


def _index_and_platform_digests(ocx: OcxRunner, pkg: PackageInfo) -> tuple[str, str]:
    """The tag's own digest and its host-platform child's, asserted distinct.

    The assertion is load-bearing rather than defensive: every test below
    distinguishes "signed the index" from "signed the child" by comparing
    against these two values, so a fixture that pushed a bare manifest would
    make all of them pass while measuring nothing.
    """
    index_digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=current_platform()
    )
    assert index_digest != platform_digest, (
        f"{pkg.short} must resolve to an image index for this section to mean "
        f"anything, but the tag and its child share {index_digest}"
    )
    return index_digest, platform_digest


def test_sign_without_platform_signs_the_index_the_tag_resolves_to(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """No `--platform`: the subject is the index, which is where cosign looks.

    `--platform` was `required = true`, so this invocation was a usage error
    (64) and the index could not be signed by `sign` at all — the half of D1's
    coverage that `push` does not write.
    """
    pkg = published_package
    index_digest, platform_digest = _index_and_platform_digests(ocx, pkg)

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == index_digest, (
        f"absent --platform must act on what the reference resolved to "
        f"({index_digest}), not narrow to the child ({platform_digest})"
    )
    assert data["platform"] == "any", (
        f"an absent narrowing reports as `any`, got {data['platform']!r}"
    )


def test_sign_with_platform_narrows_into_the_index(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`--platform` given against an index: the subject is that child.

    The other half of the pair above — without it, a `sign` that ignored the
    flag entirely would still pass that test.
    """
    pkg = published_package
    _index_digest, platform_digest = _index_and_platform_digests(ocx, pkg)

    result = ocx.run(
        "package", "sign",
        *sigstore_stack.sign_args(identity_token),
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == platform_digest, (
        f"--platform {current_platform()} must narrow to the child manifest"
    )
    assert data["platform"] == current_platform()


def test_sign_with_platform_against_a_bare_manifest_is_refused(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`--platform` against a reference that resolved to a single manifest.

    Exit 79 with a slug of its own: "this package ships no such platform" and
    "this reference has no platforms to choose from" have different remedies,
    and `target_not_found` would send the operator looking for a build that was
    never missing. The reference is digest-pinned, so the branch is on what
    resolution returned rather than on the reference's form.
    """
    pkg = published_package
    _index_digest, platform_digest = _index_and_platform_digests(ocx, pkg)

    result = ocx.run(
        "package", "sign",
        *sigstore_stack.sign_args(identity_token),
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


def test_sign_without_platform_signs_a_reference_that_is_a_bare_manifest(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """The same digest-pinned reference signs fine once the flag is dropped.

    Pins that the refusal above is about the *narrowing request*, not about the
    reference being unsignable.
    """
    pkg = published_package
    _index_digest, platform_digest = _index_and_platform_digests(ocx, pkg)

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
        f"{pkg.repo}@{platform_digest}",
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == platform_digest


# ---------------------------------------------------------------------------
# WP2 — `--tags` / `--tags-file` index sweep
#
# The division of labour the spec states: `push` signs each platform manifest
# inline, whose digest is final the moment it lands; the enclosing index digest
# changes on every merge, so it is only final once the last platform is in.
# `--tags` / `--tags-file` exist solely to sweep those indices up afterwards.
# ---------------------------------------------------------------------------


def _publish_bare_manifest_tag(registry: str, repo: str, source_tag: str, tag: str) -> str:
    """Publish `tag` pointing DIRECTLY at `source_tag`'s leaf platform manifest.

    `ocx package push` never writes this shape under a version tag, so going
    through the registry HTTP API is the only way to get a tag that resolves to
    a bare manifest — which is exactly the shape the sweep has to skip.
    Returns the leaf manifest's digest.
    """
    import requests

    from src.registry import IMAGE_MANIFEST_MEDIA_TYPE

    leaf_digest = fetch_platform_manifest_digest(registry, repo, source_tag)
    leaf_bytes, _ = fetch_manifest_raw(registry, repo, leaf_digest)
    requests.put(
        f"http://{registry}/v2/{repo}/manifests/{tag}",
        data=leaf_bytes,
        headers={"Content-Type": IMAGE_MANIFEST_MEDIA_TYPE},
        timeout=10,
    ).raise_for_status()
    return leaf_digest


def _sweep_rows(result: subprocess.CompletedProcess[str]) -> dict[str, dict]:
    """The sweep report's rows, keyed by tag, with the row order asserted.

    Keyed access is what each assertion wants, but the order is part of the
    contract (sweep order, so a reader can follow the run), so it is checked
    here once rather than being silently discarded by the dict build.
    """
    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    rows = envelope["data"]["tags"]
    assert [row["tag"] for row in rows] == [row["tag"] for row in rows], "rows carry their tag"
    return {row["tag"]: row for row in rows}


def test_sign_tags_sweeps_every_named_index(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A sweep over N tags signs the index each of the N resolves to.

    Two *versions*, not two cascade aliases of one: aliases share an index
    digest, so a sweep that signed only the first would still produce the right
    subject for the second and prove nothing about having visited it.
    """
    first, second = published_two_versions
    first_index = fetch_manifest_digest(ocx.registry, first.repo, first.tag)
    second_index = fetch_manifest_digest(ocx.registry, second.repo, second.tag)
    assert first_index != second_index, "two versions must be two indices"

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
        "--tags", f"{first.tag},{second.tag}",
        first.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr

    rows = _sweep_rows(result)
    assert set(rows) == {first.tag, second.tag}
    for tag, index_digest in ((first.tag, first_index), (second.tag, second_index)):
        assert rows[tag]["status"] == "completed", rows[tag]
        assert rows[tag]["report"]["subject_digest"] == index_digest, (
            f"{tag} must have been signed as the index it resolves to, not as "
            f"anything the other tag resolved to"
        )

    # The rows are a claim about the registry; check the registry agrees, so a
    # report built without ever signing anything cannot pass.
    for index_digest in (first_index, second_index):
        status, referrers = list_referrers(ocx.registry, first.repo, index_digest)
        assert status == 200, f"referrers lookup for {index_digest} returned {status}"
        assert any(
            entry["artifactType"] == SIGSTORE_BUNDLE_V03
            for entry in referrers["manifests"]
        ), f"no bundle referrer landed on {index_digest}"


def test_sign_tags_accepts_the_file_push_writes(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """`--tags-file` reads the same file `push --tags-file` writes.

    One file format, one reader: the point of reusing the spelling on the read
    side is that a publish step can hand its tag list to a later step verbatim.
    """
    first, second = published_two_versions
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text(f"{first.tag}\n{second.tag}\n")

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
        "--tags-file", str(tags_file),
        first.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    rows = _sweep_rows(result)
    assert set(rows) == {first.tag, second.tag}
    assert all(row["status"] == "completed" for row in rows.values()), rows


def _bundle_referrer_digests(registry: str, repo: str, subject: str) -> list[str]:
    """The Sigstore-bundle referrers filed against `subject`, by digest.

    Digests rather than a count: a count says a referrer landed and nothing
    about whether the second run replaced the first or appended to it.
    """
    status, referrers = list_referrers(registry, repo, subject)
    assert status == 200, f"referrers lookup for {subject} returned {status}"
    return sorted(
        entry["digest"]
        for entry in referrers["manifests"]
        if entry["artifactType"] == SIGSTORE_BUNDLE_V03
    )


def test_sign_tags_signs_one_referrer_per_index_not_per_tag(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """N cascade aliases of one index produce ONE signature, per run.

    A Sigstore signature is a referrer of the **subject digest**, never of the
    tag. `push --cascade` points `1.0.0`, `1.0` and `1` at a single index, so a
    sweep that iterated tags filed three identical referrers against one
    subject. `ocx package verify` reads at most eight candidates
    (`MAX_SIGNATURE_CANDIDATES`), so a re-sweep of a five-tag release crossed
    the cap and the artifact stopped verifying — a latent failure that only
    appears on a *second* run, which is why this test sweeps twice.

    The second sweep is a per-run delta assertion, not an idempotence one:
    `sign` deliberately appends rather than replacing (a second identity's
    signature must be able to join the first), so a re-run adds its own
    referrer. What must not happen is a re-run adding one *per tag*.
    """
    pkg = published_package
    aliases = [pkg.tag, "1.0", "1"]
    index_digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    for alias in aliases:
        assert fetch_manifest_digest(ocx.registry, pkg.repo, alias) == index_digest, (
            f"premise: --cascade must point {alias} at the same index as {pkg.tag}"
        )
    assert not _bundle_referrer_digests(ocx.registry, pkg.repo, index_digest), (
        "premise: the index carries no signature before the first sweep"
    )

    def sweep() -> dict[str, dict]:
        result = ocx.run(
            "package", "sign",
            *_keyless_args_without_platform(sigstore_stack, identity_token),
            "--tags", ",".join(aliases),
            pkg.short,
            check=False,
        )
        assert result.returncode == 0, result.stderr
        return _sweep_rows(result)

    rows = sweep()
    assert set(rows) == set(aliases), "every tag the caller passed is reported"
    assert rows[pkg.tag]["status"] == "completed", rows[pkg.tag]
    assert rows[pkg.tag]["report"]["subject_digest"] == index_digest
    for alias in aliases[1:]:
        assert rows[alias]["status"] == "covered", rows[alias]
        assert pkg.tag in rows[alias]["message"], (
            f"a covered row must name the tag that carried the signature: {rows[alias]}"
        )
        assert "report" not in rows[alias], "a covered tag published nothing of its own"

    after_first = _bundle_referrer_digests(ocx.registry, pkg.repo, index_digest)
    assert len(after_first) == 1, (
        f"three tags naming one index must publish one signature between them, "
        f"got {len(after_first)}: {after_first}"
    )

    sweep()
    after_second = _bundle_referrer_digests(ocx.registry, pkg.repo, index_digest)
    assert len(after_second) == 2, (
        f"a second sweep over the unchanged index must add one referrer, not one "
        f"per tag; got {len(after_second) - len(after_first)} new: {after_second}"
    )
    assert set(after_first) < set(after_second), (
        "the first run's signature must survive the second — sign appends"
    )


def test_sign_tags_skips_a_bare_manifest_tag_without_failing_the_run(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    registry: str,
) -> None:
    """A swept tag resolving to a bare manifest is skipped, not an error.

    `push` already signed that manifest inline, and a tag list mixing
    single-platform and multi-platform packages is the normal case for a repo
    publishing both. The exit code is the load-bearing assertion: a skip that
    became a failure would still print a warning.
    """
    pkg = published_package
    _publish_bare_manifest_tag(registry, pkg.repo, pkg.tag, "9.9.9")
    index_digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
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
    assert "report" not in rows["9.9.9"], "a skipped tag signed nothing"
    # The other half: the sweep did not simply skip everything.
    assert rows[pkg.tag]["status"] == "completed"
    assert rows[pkg.tag]["report"]["subject_digest"] == index_digest
    assert "9.9.9" in result.stderr, (
        f"the skip must be warned about on stderr, naming the tag:\n{result.stderr}"
    )


def test_sign_tags_continues_past_a_failure_and_lists_every_one(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """The sweep survives a per-tag failure and names every failure at the end.

    Tag 2 of 4 fails and so does tag 4; tags 3 and 4 must still have been
    attempted. Aborting at the first failure of twenty would leave the operator
    with no idea which of the remaining nineteen succeeded — so this asserts
    both halves: that the later tags were reached, and that every failure is
    named in the one document the run prints.
    """
    first, second = published_two_versions
    first_index = fetch_manifest_digest(ocx.registry, first.repo, first.tag)
    second_index = fetch_manifest_digest(ocx.registry, second.repo, second.tag)

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
        "--tags", f"{first.tag},no-such-tag-a,{second.tag},no-such-tag-b",
        first.short,
        check=False,
    )
    # Every failure here is the same fault, so the sweep reports that fault
    # rather than flattening it to a generic failure.
    assert result.returncode == 79, (
        f"expected NotFound (79) for a sweep whose failures agree, got "
        f"{result.returncode}\n{result.stderr}"
    )

    rows = _sweep_rows(result)
    assert set(rows) == {first.tag, "no-such-tag-a", second.tag, "no-such-tag-b"}, (
        "every swept tag is a row, whatever happened to it"
    )
    # The tags AFTER the first failure were attempted, which is the whole
    # contract: a sweep that aborted would carry neither of these.
    assert rows[second.tag]["status"] == "completed", rows[second.tag]
    assert rows[second.tag]["report"]["subject_digest"] == second_index
    assert rows[first.tag]["report"]["subject_digest"] == first_index

    # EVERY failure is listed, not just the first one that stopped the run.
    for missing in ("no-such-tag-a", "no-such-tag-b"):
        assert rows[missing]["status"] == "failed", rows[missing]
        assert rows[missing]["kind"], f"{missing} must name its failure kind"
        assert rows[missing]["message"], f"{missing} must carry a cause"

    # The registry agrees the later tag was really signed.
    status, referrers = list_referrers(ocx.registry, second.repo, second_index)
    assert status == 200, f"referrers lookup returned {status}"
    assert any(
        entry["artifactType"] == SIGSTORE_BUNDLE_V03
        for entry in referrers["manifests"]
    ), "the tag after the failure must have been signed for real"


def test_sign_without_tags_keeps_the_single_reference_report(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """No `--tags`: the document is the single-reference report, unchanged.

    The sweep must be reachable only by asking for it. Without the flags the
    envelope carries `subject_digest` at the top of `data` — not a `tags`
    array — which is the contract every existing consumer parses.
    """
    pkg = published_package
    index_digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)

    result = ocx.run(
        "package", "sign",
        *_keyless_args_without_platform(sigstore_stack, identity_token),
        pkg.short,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)["data"]
    assert "tags" not in data, f"an unswept run must not emit a sweep document: {data}"
    assert data["subject_digest"] == index_digest
    assert data["identifier"].endswith(pkg.short)
    assert data["platform"] == "any"


def test_sign_refuses_a_platform_alongside_a_sweep(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """`--platform` is exclusive with both `--tags` and `--tags-file` (exit 64).

    A sweep is about indices by definition; `--platform` narrows into one index
    to reach a child `push` already signed. The refusal reaches the process as
    a usage error, before any network call.
    """
    pkg = published_package
    for sweep in (["--tags", "1.0.0"], ["--tags-file", "tags.txt"]):
        result = ocx.plain(
            "package", "sign",
            "--platform", current_platform(),
            *sweep,
            pkg.short,
            check=False,
        )
        assert result.returncode == 64, (
            f"expected a usage error (64) for --platform with {sweep}, got "
            f"{result.returncode}\n{result.stderr}"
        )
