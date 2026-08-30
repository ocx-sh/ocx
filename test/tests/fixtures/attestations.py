# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Attestation and SBOM acceptance fixtures (WP10-fix, plan `plan_sbom_attestations.md`).

Builders here feed WP10a (`test_attest.py`, `test_sbom.py`) and WP10b
(`test_verify.py`, `test_sign.py`) once those land in wave 6 — this module
predates every one of its consumers. Each function's docstring names the
scenario ID(s) (`S-NNN`, see the plan's "User-experience scenarios" table) and
the ADR Part III checklist row it exists to prove.

Two static sibling files hold data no generator should reconstruct on every
run: `pretty_cyclonedx.json` (hand-authored, non-canonical byte form — the
point is the *exact bytes*, which a generator would only re-canonicalize) and
`malformed_payload_valid_signature.json` (a genuine ECDSA signature; minting a
fresh one per test run would work just as well but adds nothing — a pinned
signature is exactly as valid a positive/negative pair and never changes).
Both are self-checked by `self_check()` below — run directly:

    uv run python3 tests/fixtures/attestations.py

Wire shapes mirror `crates/ocx_lib/src/oci/attest/{dsse,statement}.rs`
byte-for-byte: `DsseEnvelope` is `{payload, payloadType, signatures: [{sig,
keyid}]}` (all three payload-bearing fields base64), `Statement` is `{_type,
subject: [{name, digest}], predicateType, predicate}`. Getting either shape
wrong here would make a consuming test assert against something OCX's own
parser was never going to produce or accept.
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
from typing import Any

_HERE = Path(__file__).parent

#: S-001, S-007, S-019. Pretty-printed (odd whitespace, unsorted keys) and
#: carrying multi-byte UTF-8 (accented Latin, CJK, an emoji) so a byte-fidelity
#: bug has more than ASCII to hide in. Chosen non-canonical deliberately: a
#: compact, already-sorted document would round-trip even through a
#: re-serializing implementation and pass the test for the wrong reason (this
#: is the ADR's own stated reason for the property, Testing Strategy > Interop).
#: Feeds `ocx package attest --predicate` (S-001) and the `sbom --output`
#: byte-compare (S-007, ADR Part III checklist row 2 / red-before-green #4).
PRETTY_CYCLONEDX_PATH = _HERE / "pretty_cyclonedx.json"

#: Must track `MAX_PREDICATE_FILE_BYTES` in `crates/ocx_lib/src/oci/attest.rs`.
#: No cross-language import exists; `self_check()` cannot detect drift, a
#: human re-reading both sides after either changes can.
MAX_PREDICATE_FILE_BYTES = 15 * 1024 * 1024


def predicate_of_size(byte_length: int) -> bytes:
    """A syntactically valid CycloneDX-shaped JSON predicate of exactly `byte_length` bytes.

    S-004: probes the `MAX_PREDICATE_FILE_BYTES` boundary from both sides
    without checking in megabytes of fixture data — pads a trailing string
    field to hit the target length precisely. Valid JSON at every length so a
    failure at the cap is attributable to size alone, never to a parse error
    riding along.
    """
    prefix = b'{"bomFormat":"CycloneDX","specVersion":"1.6","padding":"'
    suffix = b'"}'
    overhead = len(prefix) + len(suffix)
    if byte_length < overhead:
        raise ValueError(f"byte_length must be >= {overhead} (fixed JSON overhead)")
    return prefix + b"a" * (byte_length - overhead) + suffix


def predicate_at_size_cap() -> bytes:
    """Exactly `MAX_PREDICATE_FILE_BYTES` — the accept side of S-004's pair."""
    return predicate_of_size(MAX_PREDICATE_FILE_BYTES)


def predicate_over_size_cap() -> bytes:
    """One byte over `MAX_PREDICATE_FILE_BYTES` — the refuse side of S-004's pair."""
    return predicate_of_size(MAX_PREDICATE_FILE_BYTES + 1)


def multi_subject_statement_target_at_subject_one(
    target_digest_hex: str,
    *,
    predicate_type: str = "https://cyclonedx.org/bom",
    predicate: dict[str, Any] | None = None,
    decoy_name: str = "localhost:5000/attestations/decoy-subject",
    decoy_digest_hex: str = "0" * 64,
) -> dict[str, Any]:
    """An in-toto v1 Statement whose ONLY matching subject sits at `subject[1]`.

    S-009. Documents a recorded, deliberate limitation (ADR "Subject binding,
    precisely"): OCX's own `binds_subject` (`attest/statement.rs`) iterates
    every subject, but the delegated sigstore-rs verifier call additionally
    hard-requires `subject[0]` itself to match the target and fails closed on
    anything else (`verifier.rs:76-80`) — so a multi-subject Statement naming
    the target only at a later index is refused overall, even though OCX's
    own check alone would have accepted it. That makes OCX **stricter than
    cosign and sigstore-go**, which iterate all subjects; cosign-produced
    attestations are always single-subject, so interop is unaffected. This
    fixture is what turns that sentence into a checked claim: feed it through
    verification and assert refusal, not success — the wrong assertion here
    (asserting success) would silently re-open exactly the acceptance gap the
    limitation exists to close.

    `target_digest_hex` is the *hex-only* sha256 (no `sha256:` prefix, matching
    `Subject.digest`'s wire shape) of whatever subject the caller's test
    actually verifies against — computed at test time, never hardcoded here.
    """
    if predicate is None:
        predicate = {"bomFormat": "CycloneDX", "specVersion": "1.6", "components": []}
    return {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [
            {"name": decoy_name, "digest": {"sha256": decoy_digest_hex}},
            {"name": "target", "digest": {"sha256": target_digest_hex}},
        ],
        "predicateType": predicate_type,
        "predicate": predicate,
    }


def two_signature_envelope(payload: dict[str, Any] | bytes | None = None) -> dict[str, Any]:
    """A DSSE envelope carrying two signatures over an otherwise-valid payload.

    S-009, ADR Part III checklist row 8 (`MultipleSignatures`). Structural
    refusal, never reaches crypto: `DsseEnvelope::parse` hard-rejects
    `signatures.len() != 1` before either signature is checked (row 8's whole
    point — verifying one out of several would report "verified" for an
    envelope whose other signer nobody checked), so neither `sig` value here
    needs to verify against anything. The payload defaults to a well-formed,
    single-subject Statement so this fixture isolates the signature-count
    property alone, not payload validity too.
    """
    if payload is None:
        payload = {
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{"name": "target", "digest": {"sha256": "0" * 64}}],
            "predicateType": "https://cyclonedx.org/bom",
            "predicate": {"bomFormat": "CycloneDX", "specVersion": "1.6", "components": []},
        }
    payload_bytes = json.dumps(payload).encode() if isinstance(payload, dict) else payload
    return {
        "payload": base64.b64encode(payload_bytes).decode(),
        "payloadType": "application/vnd.in-toto+json",
        "signatures": [
            {"sig": base64.b64encode(b"first-signature-not-cryptographically-valid").decode(), "keyid": "key-a"},
            {"sig": base64.b64encode(b"second-signature-not-cryptographically-valid").decode(), "keyid": "key-b"},
        ],
    }


def malformed_payload_valid_signature() -> dict[str, Any]:
    """A DSSE envelope whose signature genuinely verifies but whose payload does not parse.

    S-009, the CVE-2026-39395 shape, ADR Part III checklist row 2. Loads the
    checked-in `malformed_payload_valid_signature.json` — see that file's
    `_provenance`/`note` fields for how it was made and exactly what it
    proves. Returns the whole loaded document: `envelope` (ready to feed a
    parser expecting `DsseEnvelope`'s wire shape) plus `public_key_pem` (what
    a consumer verifies `envelope.signatures[0].sig` against).
    """
    return json.loads((_HERE / "malformed_payload_valid_signature.json").read_text())


def _attestation_referrer(registry: str, repo: str, subject_digest: str) -> dict[str, Any]:
    """The one DSSE referrer attached to ``subject_digest``.

    Attestations and signatures share the Sigstore-bundle ``artifactType`` and
    are told apart by ``dev.sigstore.bundle.content`` — the same discrimination
    ocx itself performs (`oci/attest/pipeline.rs`), so a helper that filtered on
    ``artifactType`` alone would hand a message-signature bundle to a caller
    expecting an envelope.

    Raises rather than returning ``None`` on any count but one: a consumer that
    silently found nothing would assert against nothing.
    """
    from src import registry as reg
    from tests.fixtures import adversarial

    status, index = reg.list_referrers(
        registry, repo, subject_digest, artifact_type=adversarial.SIGSTORE_BUNDLE_V03
    )
    if status != 200 or index is None:
        raise RuntimeError(f"referrers list failed ({status}) for {repo}@{subject_digest}")
    candidates = [
        manifest
        for manifest in index.get("manifests") or []
        if (manifest.get("annotations") or {}).get("dev.sigstore.bundle.content") == "dsse-envelope"
    ]
    if len(candidates) != 1:
        raise RuntimeError(f"expected exactly 1 attestation referrer, found {len(candidates)}")
    return candidates[0]


def attestation_bundle(registry: str, repo: str, subject_digest: str) -> dict[str, Any]:
    """The decoded Sigstore bundle of the one attestation on ``subject_digest``.

    The attestation twin of `adversarial.signature_bundle`. Used by the cosign
    interop tests, which hand the bundle to another implementation rather than
    mutating it — but the "exactly one candidate" insistence is the same one
    every mutator needs.
    """
    from src import registry as reg

    referrer = _attestation_referrer(registry, repo, subject_digest)
    manifest = reg.get_manifest(registry, repo, referrer["digest"])
    return json.loads(reg.get_blob(registry, repo, manifest["layers"][0]["digest"]))


def tamper_attestation_payload(
    registry: str,
    repo: str,
    subject_digest: str,
    subject_size: int,
    *,
    replace: tuple[bytes, bytes],
) -> None:
    """Edit the signed document inside a published attestation, in place.

    The attestation twin of `adversarial.py`'s signature mutators, and built the
    same way and for the same reason: against a real Fulcio and Rekor there is
    no server-side switch that produces a hostile artifact, so one is made the
    way an attacker would make one — take a genuine, fully verifying bundle off
    the registry, change exactly one thing, and put it back. Everything else
    (certificate chain, Rekor SET, inclusion proof, subject binding) stays
    byte-for-byte as the stack produced it, so a test that passes proves the
    DSSE signature check runs over the payload rather than that some unrelated
    field was malformed.

    `replace` is a `(needle, replacement)` pair applied to the DECODED DSSE
    payload. Pass equal-length values to leave the payload's byte count
    untouched — otherwise a refusal could come from a length-derived bound
    rather than from the signature.

    The mutated bundle replaces the original referrer (push, then delete the
    old manifest) so the following scan sees exactly one candidate and cannot
    pass by finding the untouched original.

    Raises rather than returning quietly on every "found the wrong number of
    things" case: a tamper helper that silently no-ops leaves the test
    asserting against a valid bundle.
    """
    # Local imports: `self_check()` runs this module standalone, and neither the
    # registry client nor the signature mutators are needed for that.
    from src import registry as reg
    from tests.fixtures import adversarial

    referrer = _attestation_referrer(registry, repo, subject_digest)
    manifest = reg.get_manifest(registry, repo, referrer["digest"])
    bundle = json.loads(reg.get_blob(registry, repo, manifest["layers"][0]["digest"]))

    envelope = bundle["dsseEnvelope"]
    needle, replacement = replace
    payload = base64.b64decode(envelope["payload"])
    if needle not in payload:
        raise RuntimeError(f"{needle!r} is absent from the signed payload — the mutation would be a no-op")
    envelope["payload"] = base64.b64encode(payload.replace(needle, replacement)).decode()

    reg.push_referrer(
        registry,
        repo,
        subject_digest,
        subject_size,
        artifact_type=adversarial.SIGSTORE_BUNDLE_V03,
        payload=json.dumps(bundle, separators=(",", ":")).encode(),
    )
    reg.delete_manifest(registry, repo, referrer["digest"])


def replace_attestation_envelope(
    registry: str,
    repo: str,
    subject_digest: str,
    subject_size: int,
    *,
    dsse_envelope: dict[str, Any],
) -> None:
    """Swap the WHOLE ``dsseEnvelope`` of a genuine, published attestation.

    The WP-R5 sibling of `tamper_attestation_payload`: that helper edits bytes
    *inside* an existing payload (equal-length, so a stale signature stays
    plausible-looking); this one replaces the envelope outright, for fixtures
    (`two_signature_envelope`, `malformed_payload_valid_signature`,
    `multi_subject_statement_target_at_subject_one`) that are already a
    complete, self-contained envelope dict.

    ``verificationMaterial`` and ``mediaType`` travel from the genuine donor
    bundle untouched -- `BundleParts::from_bundle` (`verify/pipeline.rs`)
    requires a structurally complete certificate and tlog entry before OCX's
    own envelope checks ever run, so a donor with none would refuse for the
    wrong reason before reaching the fixture's own property. What the reused
    material does NOT buy is a signature that verifies over the NEW payload:
    for `two_signature_envelope` and `malformed_payload_valid_signature` this
    is moot (both are refused structurally, before any crypto runs, per their
    own docstrings); a caller relying on the delegated crypto stage being
    reached is asserting a genuinely different, weaker property and must say
    so.

    Same "push new, delete old" shape as `tamper_attestation_payload` and for
    the same reason: the following scan must see exactly one candidate.
    """
    # Local imports: `self_check()` runs this module standalone, and neither the
    # registry client nor the signature mutators are needed for that.
    from src import registry as reg
    from tests.fixtures import adversarial

    referrer = _attestation_referrer(registry, repo, subject_digest)
    manifest = reg.get_manifest(registry, repo, referrer["digest"])
    bundle = json.loads(reg.get_blob(registry, repo, manifest["layers"][0]["digest"]))
    bundle["dsseEnvelope"] = dsse_envelope

    reg.push_referrer(
        registry,
        repo,
        subject_digest,
        subject_size,
        artifact_type=adversarial.SIGSTORE_BUNDLE_V03,
        payload=json.dumps(bundle, separators=(",", ":")).encode(),
        # Preserves the ONE thing `_attestation_referrer`'s own filter reads,
        # so a caller chaining two replacements (comparing two mutated
        # envelopes against the same subject) still finds the referrer the
        # second time. ocx's own scan never needed this -- see this
        # function's docstring and `push_referrer`'s.
        annotations={"dev.sigstore.bundle.content": "dsse-envelope"},
    )
    reg.delete_manifest(registry, repo, referrer["digest"])


def self_check() -> None:
    """Structural + cryptographic self-check for every fixture in this module.

    Not a pytest test (no consumer exists yet — WP10a/WP10b land in wave 6).
    Run directly: `uv run python3 tests/fixtures/attestations.py`.
    """
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec

    # -- pretty_cyclonedx.json ------------------------------------------------
    raw = PRETTY_CYCLONEDX_PATH.read_bytes()
    parsed = json.loads(raw)
    assert parsed["specVersion"] in ("1.5", "1.6", "1.7"), "outside the CycloneDX reader's accepted range"
    assert parsed["bomFormat"] == "CycloneDX"
    assert len(parsed["components"]) == 2
    canonical = json.dumps(parsed, sort_keys=True, separators=(",", ":")).encode()
    assert raw != canonical, "fixture is accidentally canonical -- round-trip test would pass for the wrong reason"
    assert "模块".encode() in raw and "café".encode() in raw
    print(f"[OK] pretty_cyclonedx.json: valid JSON, non-canonical, {len(raw)} bytes, unicode present")

    # -- predicate_of_size ----------------------------------------------------
    at_cap = predicate_at_size_cap()
    over_cap = predicate_over_size_cap()
    assert len(at_cap) == MAX_PREDICATE_FILE_BYTES
    assert len(over_cap) == MAX_PREDICATE_FILE_BYTES + 1
    json.loads(at_cap)  # both must still be valid JSON -- isolates size alone
    json.loads(over_cap)
    print(f"[OK] predicate_of_size: at-cap={len(at_cap)} over-cap={len(over_cap)} bytes, both valid JSON")

    # -- multi_subject_statement_target_at_subject_one -------------------------
    target = "ab" * 32
    stmt = multi_subject_statement_target_at_subject_one(target)
    assert stmt["subject"][0]["digest"]["sha256"] != target
    assert stmt["subject"][1]["digest"]["sha256"] == target
    print("[OK] multi_subject_statement_target_at_subject_one: target present only at index 1")

    # -- two_signature_envelope -------------------------------------------------
    env2 = two_signature_envelope()
    assert len(env2["signatures"]) == 2
    json.loads(base64.b64decode(env2["payload"]))  # payload itself must be well-formed
    print("[OK] two_signature_envelope: 2 signatures, payload independently valid")

    # -- malformed_payload_valid_signature.json ----------------------------
    fixture = malformed_payload_valid_signature()
    envelope = fixture["envelope"]
    payload_bytes = base64.b64decode(envelope["payload"])
    try:
        json.loads(payload_bytes)
        raise AssertionError("payload unexpectedly parses as JSON -- fixture no longer proves the CVE shape")
    except json.JSONDecodeError:
        pass  # expected: this is the whole point of the fixture

    def pae(payload_type: str, payload: bytes) -> bytes:
        out = bytearray(b"DSSEv1 ")
        out += str(len(payload_type)).encode() + b" " + payload_type.encode() + b" "
        out += str(len(payload)).encode() + b" " + payload
        return bytes(out)

    pub = serialization.load_pem_public_key(fixture["public_key_pem"].encode())
    assert isinstance(pub, ec.EllipticCurvePublicKey)
    sig_der = base64.b64decode(envelope["signatures"][0]["sig"])
    pae_bytes = pae(envelope["payloadType"], payload_bytes)
    pub.verify(sig_der, pae_bytes, ec.ECDSA(hashes.SHA256()))  # raises on failure
    print("[OK] malformed_payload_valid_signature: payload does not parse as JSON, signature verifies over its PAE")

    # Negative control: a corrupted signature must NOT verify -- proves the
    # positive check above is discriminating, not vacuously true.
    bad_sig = bytearray(sig_der)
    bad_sig[-1] ^= 0xFF
    try:
        pub.verify(bytes(bad_sig), pae_bytes, ec.ECDSA(hashes.SHA256()))
        raise AssertionError("corrupted signature unexpectedly verified")
    except Exception as error:  # noqa: BLE001 -- generic: cryptography's own InvalidSignature
        assert "unexpectedly verified" not in str(error)
    print("[OK] malformed_payload_valid_signature: negative control (flipped sig byte) correctly fails to verify")

    print("\nAll fixtures self-check clean.")


if __name__ == "__main__":
    self_check()
