# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Push cosign's committed bytes into a real registry.

The fixtures under [`golden/`](golden/) are cosign v3.1.1's own output, captured
from the registry that stored it (`golden/generate.py`). This module is the
other half: it puts those bytes *back* into a registry so the shipped `ocx`
binary can be pointed at them. Nothing here re-signs, re-canonicalises or
re-derives anything — every signature-bearing byte is the file's.

# Why this is reachable while the suite's own signed path is not

`ocx package sign` is red on this branch for a reason outside loop D, so every
acceptance test that signs *and then* verifies is red with it. None of that
touches this module: verifying an artifact cosign wrote needs no OCX signer, no
Fulcio, no dex and no Rekor. What it needs is a registry and the committed trust
root, which is exactly the dependency set here.

# The subject is reproduced, not transcribed

Every golden fixture signs the same minimal OCI image — empty config, one layer
holding `SUBJECT_PAYLOAD` — so [`push_subject`] rebuilds it and *asserts* the
result hashes to the digest cosign's own referrer names. A drifted helper
(different key order, different separators, a changed default) fails there
rather than three layers down in a verification that quietly stopped covering
the golden bytes.

# What is rewritten, and why each rewrite is inert

Two descriptors cannot be replayed verbatim, and neither is read by the verifier:

* **The bundle layer descriptor.** The committed `*_bundle.json` is
  pretty-printed, so it does not hash to the digest cosign's referrer names.
  The referrer is rebuilt around the bytes actually pushed. The DSSE signature
  covers the base64 `payload` *inside* the bundle, never the file's whitespace.
* **The sidecar config descriptor.** `simplesigning_*_manifest.json` names a
  233-byte OCI image config whose bytes were not captured, and a registry
  refuses a manifest whose config blob is absent. It is repointed at `{}`.
  `simplesigning_read.rs` reads a sidecar's *layers* and never its config.
"""

from __future__ import annotations

import base64
import hashlib
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from cryptography import x509

from src import registry as reg
from src.helpers import SIGSTORE_DIR

_HERE = Path(__file__).parent
GOLDEN = _HERE / "golden"
#: OCX-authored negatives derived from the golden bytes — see that directory's
#: README. Committed rather than minted here, per the rule it states.
NEGATIVES = _HERE / "simplesigning"

#: The bytes every golden fixture's subject image is built from.
SUBJECT_PAYLOAD = b"ocx-golden-subject"

#: The local Fulcio CA, CT-log key and **pinned** Rekor public key. Committed
#: and deterministic, so a keyless verify of the golden bytes needs no stack.
TRUST_ROOT = SIGSTORE_DIR

#: A loopback port nothing binds, handed to `--rekor-url` on purpose.
#:
#: The trust root pins the Rekor public key, so no transparency-log key is ever
#: fetched and the URL is only SSRF-resolved at the CLI boundary. Pointing it at
#: a dead port makes that structural: if any of these cells ever *did* dial
#: Rekor it would fail here, instead of silently depending on a running stack.
DEAD_REKOR_URL = "http://localhost:1"

#: An empty OCI image config. Two bytes, and the substitute for the sidecar
#: config blob cosign wrote but the capture did not keep.
_EMPTY_CONFIG = b"{}"

SIGNATURE_ANNOTATION = "dev.cosignproject.cosign/signature"


def golden(name: str) -> dict[str, Any]:
    """One committed golden fixture, parsed."""
    return json.loads((GOLDEN / name).read_text())


def push_subject(registry: str, repo: str, *, reference: str | None = None) -> tuple[str, int]:
    """Rebuild the image cosign signed and push it. Returns ``(digest, size)``.

    The assertion is the point: it ties the reproduction to the committed
    referrer's own ``subject`` descriptor, so a helper that drifts fails here
    rather than leaving a verification pointed at bytes nobody checked.
    """
    digest, size = reg.push_minimal_image(registry, repo, payload=SUBJECT_PAYLOAD, reference=reference)
    subject = golden("keyless_referrer_manifest.json")["subject"]
    assert (digest, size) == (subject["digest"], subject["size"]), (
        f"the reproduced subject is {digest} ({size}B) but cosign's referrer names "
        f"{subject['digest']} ({subject['size']}B) — the signature would not be over it"
    )
    return digest, size


def bundle_bytes(label: str, *, corrupt: bool = False) -> bytes:
    """The committed Sigstore bundle, optionally with one signature byte flipped.

    Minted here rather than committed as a second 5 KB near-duplicate: the
    corruption is then one reviewable line, and the branch that does *not*
    corrupt is asserted byte-identical to cosign's file — so the flipped byte is
    provably the only difference between the cell that must pass and the cell
    that must fail. (The sidecar negatives are committed instead; that shape has
    a standing rule, see `simplesigning/README.md`.)
    """
    raw = (GOLDEN / f"{label}_bundle.json").read_bytes()
    bundle = json.loads(raw)
    if not corrupt:
        assert _reserialize(bundle) == raw, (
            f"{label}_bundle.json does not survive a JSON round trip unchanged; "
            "the uncorrupted push would not be cosign's bytes"
        )
        return raw
    signature = bundle["dsseEnvelope"]["signatures"][0]
    signature["sig"] = _flip_last_byte(signature["sig"])
    return _reserialize(bundle)


@dataclass(frozen=True, slots=True)
class PushedReferrer:
    """What a bundle referrer push left in the registry."""

    #: Digest of the referrer manifest — the value `signatures[].referrer_digest`
    #: must report.
    digest: str
    #: Its exact byte length, needed to describe it inside a fallback index.
    size: int
    #: Digest of the Sigstore bundle blob the referrer's single layer names.
    blob_digest: str


def push_bundle_referrer(
    registry: str,
    repo: str,
    label: str,
    subject: dict[str, Any],
    *,
    corrupt: bool = False,
) -> PushedReferrer:
    """Push a golden bundle and the referrer pointing at it.

    ``subject`` is the OCI descriptor the referrer attaches to — a parameter
    rather than a derivation, so a caller can deliberately park a signature on
    the *wrong* object.
    """
    blob = bundle_bytes(label, corrupt=corrupt)
    blob_digest = reg.push_blob(registry, repo, blob)
    config_digest = reg.push_blob(registry, repo, _EMPTY_CONFIG)

    manifest = golden(f"{label}_referrer_manifest.json")
    assert manifest["config"]["digest"] == config_digest, "cosign's empty config is not `{}`"
    manifest["layers"][0]["digest"] = blob_digest
    manifest["layers"][0]["size"] = len(blob)
    manifest["subject"] = subject
    referrer_digest, _ = reg.push_manifest(registry, repo, manifest)
    return PushedReferrer(referrer_digest, len(json.dumps(manifest).encode()), blob_digest)


def push_fallback_index(registry: str, repo: str, subject_digest: str, referrer: PushedReferrer) -> str:
    """Park a referrer under the OCI tag-schema fallback tag, cosign's way.

    The child descriptor is `fallback_index.json`'s — cosign's own, which keeps
    `artifactType` and carries **no** annotations (cosign#4641). Preserving that
    absence is the point: a reader that filtered referrers by
    `dev.sigstore.bundle.content` would find nothing here, and this is the only
    door open on a registry with no Referrers API.
    """
    index = golden("fallback_index.json")
    child = index["manifests"][0]
    assert not child.get("annotations"), "cosign's fallback child carries no annotations"
    child["digest"] = referrer.digest
    child["size"] = referrer.size
    tag = "sha256-" + subject_digest.removeprefix("sha256:")
    reg.push_manifest(registry, repo, index, reference=tag)
    return tag


def push_sidecar(registry: str, repo: str, subject_digest: str, manifest_path: Path) -> tuple[str, str]:
    """Push a cosign `sha256-<hex>.sig` sidecar and the payload layer it names.

    Returns ``(tag, layer_digest)``. The payload always comes from the golden
    capture: every negative under `simplesigning/` changes the *manifest* and
    keeps the layer digest, so both the good and the tampered manifest name the
    same committed, byte-exact signed message.
    """
    manifest = json.loads(manifest_path.read_text())
    layer = manifest["layers"][0]
    payload = _payload_for(layer["digest"])
    reg.push_blob(registry, repo, payload)
    config_digest = reg.push_blob(registry, repo, _EMPTY_CONFIG)
    manifest["config"] = {
        "mediaType": manifest["config"]["mediaType"],
        "digest": config_digest,
        "size": len(_EMPTY_CONFIG),
    }
    tag = "sha256-" + subject_digest.removeprefix("sha256:") + ".sig"
    reg.push_manifest(registry, repo, manifest, reference=tag)
    return tag, layer["digest"]


def served_bundle_signature(registry: str, repo: str, blob_digest: str) -> str:
    """The DSSE signature the registry now serves, read back off the wire."""
    bundle = json.loads(reg.get_blob(registry, repo, blob_digest))
    return bundle["dsseEnvelope"]["signatures"][0]["sig"]


def served_sidecar_signature(registry: str, repo: str, tag: str) -> str:
    """The simplesigning signature annotation the registry now serves."""
    manifest = reg.get_manifest(registry, repo, tag)
    return manifest["layers"][0]["annotations"][SIGNATURE_ANNOTATION]


def golden_bundle_signature(label: str) -> str:
    """The DSSE signature as cosign wrote it."""
    return golden(f"{label}_bundle.json")["dsseEnvelope"]["signatures"][0]["sig"]


def golden_sidecar_signature(label: str) -> str:
    """The simplesigning signature annotation as cosign wrote it."""
    annotations = golden(f"simplesigning_{label}_manifest.json")["layers"][0]["annotations"]
    return annotations[SIGNATURE_ANNOTATION]


def golden_certificate_identity(label: str) -> tuple[str, str]:
    """The SAN and Fulcio OIDC issuer inside a golden keyless bundle's leaf.

    Read out of the certificate rather than restated as a constant: the cells
    then assert that what `ocx` reported is what *this fixture* carries, not
    that two hand-written strings agree with each other.
    """
    material = golden(f"{label}_bundle.json")["verificationMaterial"]
    der = base64.b64decode(material["certificate"]["rawBytes"])
    certificate = x509.load_der_x509_certificate(der)
    [san] = certificate.extensions.get_extension_for_class(x509.SubjectAlternativeName).value.get_values_for_type(
        x509.RFC822Name
    )
    # Fulcio's OIDC-issuer extension. The `.1` form; the deprecated `.1.1` DER
    # wrapper is what the retired in-repo fake wrote and real Fulcio does not.
    issuer = certificate.extensions.get_extension_for_oid(
        x509.ObjectIdentifier("1.3.6.1.4.1.57264.1.8")
    ).value.value
    # UTF8String: two DER header bytes, then the value.
    return san, issuer[2:].decode()


def golden_integrated_time(label: str) -> int:
    """The Rekor `integratedTime` a golden bundle's log entry carries."""
    entry = golden(f"{label}_bundle.json")["verificationMaterial"]["tlogEntries"][0]
    return int(entry["integratedTime"])


def _payload_for(layer_digest: str) -> bytes:
    """The committed simplesigning payload that hashes to ``layer_digest``.

    Looked up by digest rather than by label so a manifest and the payload it
    names can never be paired wrongly: a negative fixture that changed its layer
    digest would fail here instead of being pushed against a payload that does
    not match it.
    """
    for candidate in sorted(GOLDEN.glob("simplesigning_*_payload.json")) + sorted(NEGATIVES.glob("*_payload.json")):
        raw = candidate.read_bytes()
        if "sha256:" + hashlib.sha256(raw).hexdigest() == layer_digest:
            return raw
    raise AssertionError(f"no committed simplesigning payload hashes to {layer_digest}")


def _reserialize(document: dict[str, Any]) -> bytes:
    """`generate.py`'s `_pretty` form — the shape every golden JSON is committed in."""
    return (json.dumps(document, indent=2) + "\n").encode()


def _flip_last_byte(b64: str) -> str:
    """Flip the last byte of a base64 signature. One byte, nothing else."""
    raw = bytearray(base64.b64decode(b64))
    raw[-1] ^= 0xFF
    return base64.b64encode(bytes(raw)).decode()
