#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Golden cosign fixtures: regenerator and structural validator.

The sibling JSON files in this directory are **cosign's own output**, captured
from the registry that stored it. They exist so a test can be written against
the bytes cosign 3.x actually writes without a Sigstore stack, a registry and a
docker daemon standing behind every assertion.

Committed rather than regenerated per run, for the reason
`tests/fixtures/attestations.py` gives for its two static siblings: the point of
a golden fixture is an *exact shape*, and a generator that reproduces it on
every run only re-canonicalises it — the fixture then agrees with the generator
rather than with cosign, and a change in cosign's output stops being visible in
a diff. This module is therefore run by hand, and the diff it produces is the
finding.

Running it
----------

The `sigstore` compose profile must be reachable; the script brings it up
itself, but docker has to be available and the images pulled::

    cd test
    uv run python3 tests/fixtures/golden/generate.py               # validate what is committed
    uv run python3 tests/fixtures/golden/generate.py --regenerate  # re-capture, then validate
    uv run python3 tests/fixtures/golden/generate.py --check DIR   # validate a copy elsewhere

`--check DIR` exists for the red/green proof: copy this directory, break one
fixture in the copy, and watch the validator reject it. A green result nobody
has seen go red is not a check.

What is captured, and why each one
----------------------------------

cosign 3.1.1 chooses its **output format from the registry**, not from a flag —
`--new-bundle-format` does not exist on any subcommand in this version, and
`--tlog-upload=false` is rejected once a signing config is in play. So the five
shapes below are reached by pointing the same command at different registries
and by descending to the deprecated `generate` / `attach` pair:

===========================  ================================================
`*_referrer_manifest.json`   OCI 1.1 referrer + `*_bundle.json`, from a
   + `*_bundle.json`         registry that has the Referrers API (zot).
`fallback_index.json`        The `sha256-<hex>` tag a registry *without* the
                             Referrers API gets instead (registry:2). Evidence
                             for cosign#4641 — see `provenance.json`.
`simplesigning_*_manifest`   The legacy `sha256-<hex>.sig` shape, unreachable
   + `simplesigning_*_payload`  from `cosign sign` in 3.x at all.
`attestation_sidecar_key_*`  The legacy `sha256-<hex>.att` shape, likewise
                             unreachable from `cosign attest` in 3.x. Its
                             layer is a DSSE envelope, its manifest carries
                             neither `artifactType` nor `subject` -- which is
                             the measured evidence that cosign has **no**
                             attestation artifact type and `.att` is a
                             tag-only shape (see `ATT_CERTIFICATE_GAP`).
`sbom_sidecar_*`             The legacy `sha256-<hex>.sbom` shape. Same two
                             absences as `.att` — no `artifactType`, no
                             `subject` — but its layer is the SBOM DOCUMENT
                             rather than anything signed, which is why its
                             reader is a document reader on the permissive
                             listing path and not a fourth `SidecarKind`
                             (see `SBOM_SIGNATURE_GAP`). No keyless/key
                             variant: `attach sbom` signs nothing at all.
===========================  ================================================

Each *signature* pair comes in a **keyless** and a **key** variant, because the
two differ
in exactly the place a verifier branches: `verificationMaterial.certificate`
(a Fulcio chain to walk) versus `verificationMaterial.publicKey` (a bare hint
that names a key the verifier must already hold).

The distinguishing property, and the reason the two `spike_cosign_*.json`
fixtures one directory up do not serve, is that an image **signature** carries
predicateType `https://sigstore.dev/cosign/sign/v1` over an **empty**
predicate. An attestation carries a real predicate. `self_check()` asserts the
emptiness, so a fixture that silently becomes an attestation fails here rather
than in whatever consumer trusted it.

No public-good Sigstore material appears anywhere: every certificate is minted
by the local Fulcio and every transparency-log entry comes from the local
Rekor. `self_check()` greps for the public-good Rekor hostname across the whole
directory -- including this file -- and fails on a single hit.
"""

from __future__ import annotations

import base64
import hashlib
import json
import shutil
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

_HERE = Path(__file__).parent
_TEST_ROOT = _HERE.parents[2]

# `pyproject.toml`'s `pythonpath = [".", "src"]` is a *pytest* setting, and this
# module is deliberately not a pytest module. Standalone, sys.path[0] is this
# directory, so `src` and `tests` have to be put on the path by hand.
if str(_TEST_ROOT) not in sys.path:
    sys.path.insert(0, str(_TEST_ROOT))

from src import registry as reg
from src.helpers import (
    SIGSTORE_DIR,
    mint_identity_token,
    start_sigstore_stack,
)
from tests.fixtures import cosign

# --------------------------------------------------------------------------
# The stack, as this script addresses it
# --------------------------------------------------------------------------

#: zot. Serves `/v2/<repo>/referrers/<digest>`, so cosign writes an OCI 1.1
#: referrer here.
REFERRERS_REGISTRY = "localhost:5000"

#: registry:2. No Referrers API, so cosign falls back to the `sha256-<hex>` tag
#: schema here. The whole point of keeping a second registry in the fixture set.
FALLBACK_REGISTRY = "localhost:5001"

FULCIO_URL = "http://localhost:5555"
REKOR_URL = "http://localhost:3000"
#: The host-side dex address. cosign fetches from it; the `iss` claim inside the
#: token names the in-network address instead, and the two never have to agree.
OIDC_URL = "http://localhost:5556/dex"

#: Committed alongside the fixtures, and not a secret — see `keys/README.md`.
KEY_PASSWORD = "ocxtest"

SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"
SIMPLESIGNING_MEDIA_TYPE = "application/vnd.dev.cosign.simplesigning.v1+json"
DSSE_ENVELOPE_MEDIA_TYPE = "application/vnd.dsse.envelope.v1+json"
DSSE_PAYLOAD_TYPE = "application/vnd.in-toto+json"
CYCLONEDX_PREDICATE_TYPE = "https://cyclonedx.org/bom"
#: The layer media type `cosign attach sbom --type cyclonedx` writes for a JSON
#: input. cosign's table also reaches `text/spdx+json` (`--type spdx`, its
#: DEFAULT), `text/spdx` (`--input-format text`),
#: `application/vnd.cyclonedx+xml` and `application/vnd.syft+json`; the first
#: four are what `oci::attest::predicate::sbom_predicate_type_uri` maps, and syft
#: is refused by name because no in-toto predicateType URI names that format.
SBOM_CYCLONEDX_MEDIA_TYPE = "application/vnd.cyclonedx+json"
EMPTY_CONFIG_MEDIA_TYPE = "application/vnd.oci.empty.v1+json"
EMPTY_CONFIG_DIGEST = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
COSIGN_SIGN_PREDICATE_TYPE = "https://sigstore.dev/cosign/sign/v1"
IN_TOTO_STATEMENT_V1 = "https://in-toto.io/Statement/v1"

SIGNATURE_ANNOTATION = "dev.cosignproject.cosign/signature"
CERTIFICATE_ANNOTATION = "dev.sigstore.cosign/certificate"

#: The bytes every fixture's subject image is built from. Fixed, so the subject
#: digest is the same on every regeneration and a re-capture diffs to nothing
#: but the fields cosign genuinely varies (timestamps, signatures, certs).
SUBJECT_PAYLOAD = b"ocx-golden-subject"

#: Fixed repositories. Not suffixed per run: `cosign generate` writes the
#: repository name into the simplesigning claim, so a rotating name would churn
#: a fixture's *content*. Prior artifacts are deleted before each capture
#: instead — `cosign sign` and `cosign attach signature` both APPEND to what
#: they find (a second referrer, a second layer) rather than replacing it.
REPOS = {
    "keyless": "golden/keyless",
    "key": "golden/key",
    "fallback": "golden/fallback",
    "simplesigning_keyless": "golden/simplesigning-keyless",
    "simplesigning_key": "golden/simplesigning-key",
    "attestation_sidecar_key": "golden/attestation-sidecar-key",
    "sbom_sidecar": "golden/sbom-sidecar",
}

#: Documented dead end, recorded so nobody re-derives it. `cosign attach
#: signature --rekor-response` is wired (a non-JSON file makes it exit 1) but
#: writes no `dev.sigstore.cosign/bundle` annotation in v3.1.1 — measured
#: against both accepted shapes, the Rekor `GET /api/v1/log/entries` response
#: map and the legacy `{SignedEntryTimestamp, Payload{...}}` bundle, with and
#: without `--certificate`. The simplesigning fixtures therefore carry the
#: signature and the certificate but no offline transparency-log material; that
#: material is in the sibling `*_bundle.json` fixtures instead.
REKOR_RESPONSE_GAP = (
    "cosign v3.1.1 `attach signature --rekor-response` writes no "
    "`dev.sigstore.cosign/bundle` annotation: the flag parses its input (garbage "
    "JSON exits 1) but the result never reaches the layer. Verified against the "
    "Rekor API response map and the legacy RekorBundle shape, with and without "
    "--certificate. Offline tlog material for the legacy shape is consequently "
    "not capturable with this cosign; use the *_bundle.json fixtures for it."
)

#: The second documented dead end, and the reason the `.att` fixture below is
#: key-mode only. `cosign attach attestation` takes an envelope and nothing
#: else -- no `--certificate`, no `--rekor-response` -- and `cosign attest` in
#: 3.x writes a Sigstore-bundle referrer instead of an `.att` tag, so the
#: keyless `.att` shape cosign 2.x used to write (envelope layer +
#: `dev.sigstore.cosign/certificate` annotation) cannot be produced by this
#: binary at all.
ATT_CERTIFICATE_GAP = (
    "cosign v3.1.1 cannot write a KEYLESS `sha256-<hex>.att` sidecar: `attach "
    "attestation` accepts only --attestation (no --certificate/--chain), and "
    "`attest` no longer writes the tag at all -- it writes an OCI 1.1 referrer "
    "typed application/vnd.dev.sigstore.bundle.v0.3+json. The committed `.att` "
    "fixture is therefore key-mode, where the DSSE envelope carries its own "
    "signature and needs no annotation; ocx's keyless `.att` arm is covered by a "
    "Rust unit test built from the cosign-authored keyless_bundle.json instead."
)

#: Not a gap in the capture -- a property of the shape, recorded so nobody reads
#: the fixture as an unsigned *accident*. `cosign attach sbom` signs nothing and
#: says so on stderr; the manifest carries no signature, no certificate and no
#: transparency material because there is no cosign command that would put one
#: there. Modern cosign signs an SBOM by attesting it (`cosign attest --predicate
#: sbom.json`), which is a DSSE bundle referrer and a different fixture entirely.
#: So this document lists `verified: false` under `ocx package sbom --no-verify`
#: and is refused as `unsigned_rejected_by_policy` under `--verify`, exactly as a
#: raw unsigned referrer is -- never a third mode.
SBOM_SIGNATURE_GAP = (
    "a `sha256-<hex>.sbom` sidecar is unsigned BY CONSTRUCTION: `cosign attach "
    "sbom` writes the document as the layer and signs nothing (it prints "
    "\"Attaching SBOMs this way does not sign them\"), and no cosign command "
    "signs that tag afterwards. The fixture therefore carries no signature, no "
    "certificate and no Rekor material, and ocx lists it verified:false in "
    "permissive mode and refuses it (unsigned_rejected_by_policy, exit 77) in "
    "demand mode."
)


# --------------------------------------------------------------------------
# Capture
# --------------------------------------------------------------------------


def _pretty(raw: bytes) -> bytes:
    """Re-indent a registry response without reordering it.

    `json.dumps` preserves insertion order, so the field order is still the
    registry's. Only whitespace differs from what was served, which is why the
    served digest is recorded in `provenance.json` rather than being
    recomputable from the file.
    """
    return (json.dumps(json.loads(raw), indent=2) + "\n").encode()


def _delete_if_present(registry: str, repo: str, reference: str) -> None:
    """Remove a manifest so the next cosign write starts from nothing.

    registry:2 ships with deletion disabled and has no volume, so a stale
    fallback index there can only be cleared by recreating the service. Say so
    rather than silently capturing an index with two entries in it.
    """
    try:
        _, digest = reg.fetch_manifest_raw(registry, repo, reference)
    except RuntimeError:
        # What `fetch_manifest_raw` raises when the registry does not serve the
        # reference — the good case here. Deliberately not broader: a connection
        # failure must not be read as "absent", or a down registry would let the
        # capture proceed and produce a fixture with two signatures in it.
        return
    try:
        reg.delete_manifest(registry, repo, digest)
    except RuntimeError as error:
        raise RuntimeError(
            f"{registry}/{repo}:{reference} already exists and cannot be deleted ({error}).\n"
            "cosign APPENDS to what it finds, so capturing over it would produce a "
            "fixture with two signatures in it. Recreate the registry and retry:\n"
            f"    cd {_TEST_ROOT} && docker compose rm -sf mirror-registry && "
            "docker compose --profile sigstore up -d"
        ) from error


def _sole_referrer(registry: str, repo: str, subject: str) -> dict[str, Any]:
    status, index = reg.list_referrers(registry, repo, subject)
    if status != 200 or index is None:
        raise RuntimeError(f"{registry} has no Referrers API (status {status}) — wrong registry?")
    candidates = [m for m in index["manifests"] if m.get("artifactType") == SIGSTORE_BUNDLE_V03]
    if len(candidates) != 1:
        raise RuntimeError(f"expected exactly 1 sigstore referrer, found {len(candidates)}")
    return candidates[0]


def _ensure_key_pair(keys_dir: Path) -> None:
    """Mint the committed signing key, once.

    Never regenerated when present: the public key is what a consuming test
    pins, and rotating it on every capture would invalidate every such test for
    no reason.
    """
    if (keys_dir / "cosign.key").exists() and (keys_dir / "cosign.pub").exists():
        return
    keys_dir.mkdir(parents=True, exist_ok=True)
    result = cosign.run(
        keys_dir, "generate-key-pair", env={"COSIGN_PASSWORD": KEY_PASSWORD}
    )
    if result.returncode != 0:
        raise RuntimeError(f"generate-key-pair failed:\n{result.stdout}\n{result.stderr}")


def _capture_referrer_signature(
    work: Path, out: Path, label: str, *, config: str, key: str | None, token: str | None
) -> dict[str, Any]:
    """`cosign sign` against zot: an OCI 1.1 referrer holding a Sigstore bundle."""
    repo = REPOS[label]
    subject, _ = reg.push_minimal_image(REFERRERS_REGISTRY, repo, payload=SUBJECT_PAYLOAD)
    status, index = reg.list_referrers(REFERRERS_REGISTRY, repo, subject)
    for stale in (index or {}).get("manifests", []) if status == 200 else []:
        reg.delete_manifest(REFERRERS_REGISTRY, repo, stale["digest"])

    reference = f"{REFERRERS_REGISTRY}/{repo}@{subject}"
    args = ["sign", "--signing-config", config, "--trusted-root", "trusted_root.json", "--yes"]
    if key:
        args += ["--key", key]
    if token:
        args += ["--identity-token", token]
    result = cosign.run_registry(
        work, *args, reference, env={"COSIGN_PASSWORD": KEY_PASSWORD} if key else None
    )
    if result.returncode != 0:
        raise RuntimeError(f"cosign sign ({label}) failed:\n{result.stdout}\n{result.stderr}")

    descriptor = _sole_referrer(REFERRERS_REGISTRY, repo, subject)
    raw, manifest_digest = reg.fetch_manifest_raw(REFERRERS_REGISTRY, repo, descriptor["digest"])
    manifest = json.loads(raw)
    bundle_descriptor = manifest["layers"][0]
    bundle = reg.get_blob(REFERRERS_REGISTRY, repo, bundle_descriptor["digest"])

    (out / f"{label}_referrer_manifest.json").write_bytes(_pretty(raw))
    (out / f"{label}_bundle.json").write_bytes(_pretty(bundle))
    return {
        f"{label}_referrer_manifest.json": {
            "what": f"the OCI 1.1 referrer cosign wrote for a {label} image signature",
            "source": f"{REFERRERS_REGISTRY}/{repo}, fetched by digest",
            "served_digest": manifest_digest,
            "subject_digest": subject,
            "command": _recorded(*args, reference),
            "form": "pretty-printed (2-space); registry served compact bytes",
        },
        f"{label}_bundle.json": {
            "what": f"the Sigstore bundle that referrer points at ({label} mode)",
            "source": f"{REFERRERS_REGISTRY}/{repo} blob {bundle_descriptor['digest']}",
            "served_digest": bundle_descriptor["digest"],
            "subject_digest": subject,
            "command": "(the layer of the manifest above)",
            "form": "pretty-printed (2-space); registry served compact bytes",
        },
    }


def _capture_fallback_index(work: Path, out: Path, *, config: str, key: str) -> dict[str, Any]:
    """`cosign sign` against registry:2: the `sha256-<hex>` tag-schema fallback.

    Key mode rather than keyless, deliberately: the fixture's subject is the
    *index shape*, and a certificate chain in it would only add bytes that vary
    per capture.
    """
    repo = REPOS["fallback"]
    subject, _ = reg.push_minimal_image(FALLBACK_REGISTRY, repo, payload=SUBJECT_PAYLOAD)
    fallback_tag = "sha256-" + subject.removeprefix("sha256:")
    _delete_if_present(FALLBACK_REGISTRY, repo, fallback_tag)

    reference = f"{FALLBACK_REGISTRY}/{repo}@{subject}"
    args = [
        "sign", "--key", key, "--signing-config", config,
        "--trusted-root", "trusted_root.json", "--yes",
    ]
    result = cosign.run_registry(work, *args, reference, env={"COSIGN_PASSWORD": KEY_PASSWORD})
    if result.returncode != 0:
        raise RuntimeError(f"cosign sign (fallback) failed:\n{result.stdout}\n{result.stderr}")

    raw, served = reg.fetch_manifest_raw(FALLBACK_REGISTRY, repo, fallback_tag)
    (out / "fallback_index.json").write_bytes(_pretty(raw))
    return {
        "fallback_index.json": {
            "what": (
                "the referrers index cosign writes under the `sha256-<hex>` tag when the "
                "registry has no Referrers API. cosign#4641 evidence: the child descriptor "
                "keeps `artifactType` but LOSES all three annotations the same cosign puts "
                "on a real referrer (`dev.sigstore.bundle.content`, "
                "`dev.sigstore.bundle.predicateType`, `org.opencontainers.image.created`), "
                "so a consumer that filters referrers by annotation finds nothing here."
            ),
            "source": f"{FALLBACK_REGISTRY}/{repo}:{fallback_tag} (registry:2, no Referrers API)",
            "served_digest": served,
            "subject_digest": subject,
            "command": _recorded(*args, reference),
            "form": "pretty-printed (2-space); registry served compact bytes",
        }
    }


def _capture_simplesigning(
    work: Path, out: Path, label: str, *, config: str, key: str | None, token: str | None
) -> dict[str, Any]:
    """`generate` -> `sign-blob` -> `attach signature`: the legacy `.sig` shape.

    Unreachable from `cosign sign` in 3.x — that command has no simplesigning
    writer left — so the only route is the three deprecated commands in
    sequence. Both `generate` and `attach` print a deprecation warning and work.
    """
    repo = REPOS[f"simplesigning_{label}"]
    subject, _ = reg.push_minimal_image(REFERRERS_REGISTRY, repo, payload=SUBJECT_PAYLOAD)
    sig_tag = "sha256-" + subject.removeprefix("sha256:") + ".sig"
    _delete_if_present(REFERRERS_REGISTRY, repo, sig_tag)

    reference = f"{REFERRERS_REGISTRY}/{repo}@{subject}"
    generated = cosign.run_registry(work, "generate", reference)
    if generated.returncode != 0:
        raise RuntimeError(f"cosign generate failed:\n{generated.stdout}\n{generated.stderr}")
    claim = "claim.json"
    (work / claim).write_bytes(generated.stdout.encode())

    blob_bundle = f"blob-bundle-{label}.json"
    sign_blob = ["sign-blob", "--signing-config", config, "--trusted-root", "trusted_root.json",
                 "--bundle", blob_bundle, "--yes"]
    if key:
        sign_blob += ["--key", key]
    if token:
        sign_blob += ["--identity-token", token]
    result = cosign.run(
        work, *sign_blob, claim, env={"COSIGN_PASSWORD": KEY_PASSWORD} if key else None
    )
    if result.returncode != 0:
        raise RuntimeError(f"cosign sign-blob ({label}) failed:\n{result.stdout}\n{result.stderr}")

    # v3.1.1's sign-blob has no --output-signature/--output-certificate, and it
    # prints nothing to stdout once --bundle is given. Both artifacts have to be
    # lifted out of the bundle it wrote.
    bundle = json.loads((work / blob_bundle).read_text())
    (work / "signature.b64").write_text(bundle["messageSignature"]["signature"])
    attach = ["attach", "signature", "--payload", claim, "--signature", "signature.b64"]
    if "certificate" in bundle["verificationMaterial"]:
        der = base64.b64decode(bundle["verificationMaterial"]["certificate"]["rawBytes"])
        (work / "certificate.pem").write_text(_pem(der))
        attach += ["--certificate", "certificate.pem"]
    attached = cosign.run_registry(work, *attach, reference)
    if attached.returncode != 0:
        raise RuntimeError(f"cosign attach failed:\n{attached.stdout}\n{attached.stderr}")

    raw, served = reg.fetch_manifest_raw(REFERRERS_REGISTRY, repo, sig_tag)
    manifest = json.loads(raw)
    payload = reg.get_blob(REFERRERS_REGISTRY, repo, manifest["layers"][0]["digest"])

    (out / f"simplesigning_{label}_manifest.json").write_bytes(_pretty(raw))
    # VERBATIM, unlike every other fixture here: this blob is the signed message,
    # so re-indenting it would break its own signature and the layer digest that
    # names it.
    (out / f"simplesigning_{label}_payload.json").write_bytes(payload)
    return {
        f"simplesigning_{label}_manifest.json": {
            "what": f"the legacy `sha256-<hex>.sig` manifest, {label} mode",
            "source": f"{REFERRERS_REGISTRY}/{repo}:{sig_tag}",
            "served_digest": served,
            "subject_digest": subject,
            "command": (
                f"{_recorded('generate', reference)}"
                f" > {claim} ; {_recorded(*sign_blob, claim, registry=False)}"
                f" ; {_recorded(*attach, reference)}"
            ),
            "form": "pretty-printed (2-space); registry served compact bytes",
            "gap": REKOR_RESPONSE_GAP,
        },
        f"simplesigning_{label}_payload.json": {
            "what": "the simplesigning claim the manifest's single layer holds",
            "source": f"{REFERRERS_REGISTRY}/{repo} blob {manifest['layers'][0]['digest']}",
            "served_digest": manifest["layers"][0]["digest"],
            "subject_digest": subject,
            "command": _recorded("generate", reference),
            "form": "VERBATIM — these are the signed bytes; the layer digest is over them",
        },
    }


def _capture_attestation_sidecar(work: Path, out: Path, *, config: str, key: str) -> dict[str, Any]:
    """`attest-blob` -> `attach attestation`: the legacy `sha256-<hex>.att` shape.

    Unreachable from `cosign attest` in 3.x, exactly as the simplesigning shape
    is unreachable from `cosign sign` -- measured, not assumed: `attest` against
    a Referrers-API registry writes an OCI 1.1 referrer typed
    `application/vnd.dev.sigstore.bundle.v0.3+json`, and against a registry
    without one it writes the `sha256-<hex>` fallback *index*. Neither is an
    `.att` tag, and `--registry-referrers-mode` is not a flag on `attest` at
    all. So the only route left is the deprecated `attach attestation`.

    The signed blob is the **subject manifest's own bytes**, so the Statement's
    single subject digest is the manifest digest a verifier binds it to. Signing
    a stand-in payload would produce a well-formed envelope bound to nothing
    this repository can check.

    Key mode only -- see `ATT_CERTIFICATE_GAP`.
    """
    repo = REPOS["attestation_sidecar_key"]
    subject, _ = reg.push_minimal_image(REFERRERS_REGISTRY, repo, payload=SUBJECT_PAYLOAD)
    att_tag = "sha256-" + subject.removeprefix("sha256:") + ".att"
    _delete_if_present(REFERRERS_REGISTRY, repo, att_tag)

    subject_bytes, _ = reg.fetch_manifest_raw(REFERRERS_REGISTRY, repo, subject)
    blob = "subject.manifest"
    (work / blob).write_bytes(subject_bytes)
    shutil.copy(_TEST_ROOT / "tests" / "fixtures" / "pretty_cyclonedx.json", work / "predicate.cdx.json")

    attest_blob = [
        "attest-blob", "--key", key, "--signing-config", config,
        "--trusted-root", "trusted_root.json", "--predicate", "predicate.cdx.json",
        "--type", "cyclonedx", "--bundle", "att-blob-bundle.json", "--yes",
    ]
    result = cosign.run(work, *attest_blob, blob, env={"COSIGN_PASSWORD": KEY_PASSWORD})
    if result.returncode != 0:
        raise RuntimeError(f"cosign attest-blob failed:\n{result.stdout}\n{result.stderr}")

    # `attach attestation` takes the bare DSSE envelope, which is exactly the
    # `dsseEnvelope` half of the bundle above -- not a re-encoding of it.
    bundle = json.loads((work / "att-blob-bundle.json").read_text())
    envelope = "attestation.json"
    (work / envelope).write_text(json.dumps(bundle["dsseEnvelope"]))

    reference = f"{REFERRERS_REGISTRY}/{repo}@{subject}"
    attach = ["attach", "attestation", "--attestation", envelope]
    attached = cosign.run_registry(work, *attach, reference)
    if attached.returncode != 0:
        raise RuntimeError(f"cosign attach attestation failed:\n{attached.stdout}\n{attached.stderr}")

    raw, served = reg.fetch_manifest_raw(REFERRERS_REGISTRY, repo, att_tag)
    manifest = json.loads(raw)
    payload = reg.get_blob(REFERRERS_REGISTRY, repo, manifest["layers"][0]["digest"])

    (out / "attestation_sidecar_key_manifest.json").write_bytes(_pretty(raw))
    # VERBATIM, for the same reason the simplesigning payload is: the layer
    # digest is over these bytes, and the DSSE signature is over a PAE derived
    # from the `payload` field inside them.
    (out / "attestation_sidecar_key_envelope.json").write_bytes(payload)
    return {
        "attestation_sidecar_key_manifest.json": {
            "what": "the legacy `sha256-<hex>.att` manifest, key mode",
            "source": f"{REFERRERS_REGISTRY}/{repo}:{att_tag}",
            "served_digest": served,
            "subject_digest": subject,
            "command": (
                f"{_recorded(*attest_blob, blob, registry=False)}"
                f" ; {_recorded(*attach, reference)}"
            ),
            "form": "pretty-printed (2-space); registry served compact bytes",
            "gap": ATT_CERTIFICATE_GAP,
        },
        "attestation_sidecar_key_envelope.json": {
            "what": "the DSSE envelope the manifest's single layer holds",
            "source": f"{REFERRERS_REGISTRY}/{repo} blob {manifest['layers'][0]['digest']}",
            "served_digest": manifest["layers"][0]["digest"],
            "subject_digest": subject,
            "command": _recorded(*attest_blob, blob, registry=False),
            "form": "VERBATIM -- the layer digest is over these bytes",
        },
    }


def _capture_sbom_sidecar(work: Path, out: Path) -> dict[str, Any]:
    """`attach sbom`: the legacy `sha256-<hex>.sbom` shape.

    The third tag-only cosign shape, and the one that is *not* a signature. No
    signing config, no key and no identity: `attach sbom` signs nothing, and says
    so on stderr ("Attaching SBOMs this way does not sign them"). The document is
    the layer, typed by what it is -- which is why aiming a simplesigning or DSSE
    reader at this tag returns an empty scan, and why its reader is
    `pipeline::read_sbom_sidecar_tag` on the permissive listing path rather than
    a fourth `SidecarKind`.

    Deprecated in v3.1.1 and still functional: `cosign attach` prints "attach
    will be removed in v4.0.0" and `attach sbom` adds its own 2024 deprecation
    notice. Both warnings are the reason to capture it now -- the `.sbom`
    sidecars already in registries outlive the command that wrote them, and
    reading what is out there is the parity requirement.

    Captured in the DEFAULT (legacy) referrers mode, because that is the one that
    writes the tag at all. `COSIGN_EXPERIMENTAL=1 ... --registry-referrers-mode
    oci-1-1` writes an OCI 1.1 referrer instead, typed
    `application/vnd.dev.cosign.artifact.sbom.v1+json` on the descriptor with the
    document's own type on the layer; that shape needs no fixture here because a
    referrers listing reaches it and `pipeline`'s own stubs cover it.
    """
    repo = REPOS["sbom_sidecar"]
    subject, _ = reg.push_minimal_image(REFERRERS_REGISTRY, repo, payload=SUBJECT_PAYLOAD)
    sbom_tag = "sha256-" + subject.removeprefix("sha256:") + ".sbom"
    _delete_if_present(REFERRERS_REGISTRY, repo, sbom_tag)

    document = "sbom.cdx.json"
    shutil.copy(_TEST_ROOT / "tests" / "fixtures" / "pretty_cyclonedx.json", work / document)

    reference = f"{REFERRERS_REGISTRY}/{repo}@{subject}"
    attach = ["attach", "sbom", "--sbom", document, "--type", "cyclonedx"]
    attached = cosign.run_registry(work, *attach, reference)
    if attached.returncode != 0:
        raise RuntimeError(f"cosign attach sbom failed:\n{attached.stdout}\n{attached.stderr}")

    raw, served = reg.fetch_manifest_raw(REFERRERS_REGISTRY, repo, sbom_tag)
    manifest = json.loads(raw)
    payload = reg.get_blob(REFERRERS_REGISTRY, repo, manifest["layers"][0]["digest"])

    (out / "sbom_sidecar_manifest.json").write_bytes(_pretty(raw))
    # VERBATIM, for the same reason the other two payloads are: the layer digest
    # is over these bytes, and a reader that re-indented them would report a
    # document the registry never served.
    (out / "sbom_sidecar_document.json").write_bytes(payload)
    return {
        "sbom_sidecar_manifest.json": {
            "what": "the legacy `sha256-<hex>.sbom` manifest",
            "source": f"{REFERRERS_REGISTRY}/{repo}:{sbom_tag}",
            "served_digest": served,
            "subject_digest": subject,
            "command": _recorded(*attach, reference),
            "form": "pretty-printed (2-space); registry served compact bytes",
            "gap": SBOM_SIGNATURE_GAP,
        },
        "sbom_sidecar_document.json": {
            "what": "the CycloneDX document the manifest's single layer holds",
            "source": f"{REFERRERS_REGISTRY}/{repo} blob {manifest['layers'][0]['digest']}",
            "served_digest": manifest["layers"][0]["digest"],
            "subject_digest": subject,
            "command": _recorded(*attach, reference),
            "form": "VERBATIM -- the layer digest is over these bytes",
        },
    }


def _pem(der: bytes) -> str:
    body = base64.b64encode(der).decode()
    lines = "\n".join(body[i : i + 64] for i in range(0, len(body), 64))
    return f"-----BEGIN CERTIFICATE-----\n{lines}\n-----END CERTIFICATE-----\n"


def _recorded(*args: str, registry: bool = True) -> str:
    """The command as it was issued, for `provenance.json`.

    Two things a naive `" ".join(args)` would get wrong, and both matter to
    somebody re-running it by hand: `--allow-http-registry` is injected by
    `cosign.run_registry` rather than passed, and the identity token is a real
    credential.
    """
    issued = list(cosign.registry_args(*args) if registry else args)
    for i, arg in enumerate(issued[:-1]):
        if arg == "--identity-token":
            issued[i + 1] = "<identity-token-file>"
    return "cosign " + " ".join(issued)


def regenerate(out: Path = _HERE) -> None:
    """Re-capture every fixture from a live stack. Overwrites; diff the result."""
    start_sigstore_stack()
    _ensure_key_pair(out / "keys")

    work = out / ".work"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir()
    try:
        shutil.copy(SIGSTORE_DIR / "trusted_root.json", work / "trusted_root.json")
        shutil.copytree(out / "keys", work / "keys")
        token = mint_identity_token(work / "identity-token").name
        keyless_config = cosign.signing_config(
            work,
            fulcio_url=FULCIO_URL,
            rekor_url=REKOR_URL,
            oidc_url=OIDC_URL,
            name="signing-config-keyless.json",
        )
        key_config = cosign.signing_config(
            work, rekor_url=REKOR_URL, name="signing-config-key.json"
        )
        key = "keys/cosign.key"

        fixtures: dict[str, Any] = {}
        fixtures |= _capture_referrer_signature(
            work, out, "keyless", config=keyless_config, key=None, token=token
        )
        fixtures |= _capture_referrer_signature(
            work, out, "key", config=key_config, key=key, token=None
        )
        fixtures |= _capture_fallback_index(work, out, config=key_config, key=key)
        fixtures |= _capture_simplesigning(
            work, out, "keyless", config=keyless_config, key=None, token=token
        )
        fixtures |= _capture_simplesigning(
            work, out, "key", config=key_config, key=key, token=None
        )
        fixtures |= _capture_attestation_sidecar(work, out, config=key_config, key=key)
        fixtures |= _capture_sbom_sidecar(work, out)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    (out / "provenance.json").write_text(
        json.dumps(
            {
                "_note": (
                    "Provenance for every fixture in this directory. One file rather than a "
                    "sidecar each: the cosign image, the stack and the date are common to all "
                    "of them, and repeating them eleven times would be eleven things to update. "
                    "Regenerate with `uv run python3 tests/fixtures/golden/generate.py "
                    "--regenerate` from `test/`."
                ),
                "cosign": {
                    "image": cosign.COSIGN_IMAGE,
                    "resolved_digest": _cosign_image_digest(),
                },
                "stack": {
                    "referrers_registry": f"{REFERRERS_REGISTRY} (zot, OCI 1.1 Referrers API)",
                    "fallback_registry": f"{FALLBACK_REGISTRY} (registry:2, no Referrers API)",
                    "fulcio": FULCIO_URL,
                    "rekor": REKOR_URL,
                    "oidc": f"{OIDC_URL} (dex)",
                    "trusted_root": "test/sigstore/trusted_root.json",
                    "note": (
                        "All four services are the local `sigstore` compose profile. No "
                        "public-good Sigstore service is contacted, and no material from one "
                        "appears in any fixture — self_check() fails on a single hit for the "
                        "public-good Rekor hostname anywhere in this directory."
                    ),
                },
                "generated": datetime.now(UTC).date().isoformat(),
                "subject": {
                    "payload": SUBJECT_PAYLOAD.decode(),
                    "note": (
                        "Every fixture signs the same minimal OCI image (empty config, one "
                        "layer holding the payload above), so all six subject digests match "
                        "and a diff across a re-capture shows only what cosign genuinely varies."
                    ),
                },
                "known_gaps": [REKOR_RESPONSE_GAP, ATT_CERTIFICATE_GAP, SBOM_SIGNATURE_GAP],
                "fixtures": fixtures,
            },
            indent=2,
        )
        + "\n"
    )


def _cosign_image_digest() -> str:
    """The registry digest of the pinned cosign image, as docker resolved it."""
    result = subprocess.run(
        ["docker", "image", "inspect", cosign.COSIGN_IMAGE, "--format", "{{json .RepoDigests}}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return "unresolved (image not present locally at capture time)"
    digests = json.loads(result.stdout)
    return next((d.split("@", 1)[1] for d in digests if "@" in d), "unresolved")


# --------------------------------------------------------------------------
# Validation
# --------------------------------------------------------------------------


def _pae(payload_type: str, payload: bytes) -> bytes:
    """DSSE Pre-Authentication Encoding — the bytes a DSSE signature covers."""
    return (
        b"DSSEv1 "
        + str(len(payload_type)).encode()
        + b" "
        + payload_type.encode()
        + b" "
        + str(len(payload)).encode()
        + b" "
        + payload
    )


def _check_bundle(root: Path, label: str, *, expect_certificate: bool) -> None:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec

    manifest = json.loads((root / f"{label}_referrer_manifest.json").read_text())
    bundle = json.loads((root / f"{label}_bundle.json").read_text())

    assert manifest["artifactType"] == SIGSTORE_BUNDLE_V03, manifest.get("artifactType")
    assert manifest["config"]["mediaType"] == EMPTY_CONFIG_MEDIA_TYPE
    assert manifest["config"]["digest"] == EMPTY_CONFIG_DIGEST
    annotations = manifest["annotations"]
    assert annotations["dev.sigstore.bundle.content"] == "dsse-envelope"
    assert annotations["dev.sigstore.bundle.predicateType"] == COSIGN_SIGN_PREDICATE_TYPE
    assert "org.opencontainers.image.created" in annotations
    subject_digest = manifest["subject"]["digest"]
    assert manifest["layers"][0]["mediaType"] == SIGSTORE_BUNDLE_V03

    assert bundle["mediaType"] == SIGSTORE_BUNDLE_V03
    material = bundle["verificationMaterial"]
    if expect_certificate:
        assert "certificate" in material, "keyless bundle must carry a Fulcio certificate"
        assert "publicKey" not in material
    else:
        assert "publicKey" in material, "key-mode bundle must carry a public-key hint"
        assert "certificate" not in material
    assert material["tlogEntries"], "no transparency-log entry — signed without Rekor?"

    envelope = bundle["dsseEnvelope"]
    assert envelope["payloadType"] == "application/vnd.in-toto+json"
    payload = base64.b64decode(envelope["payload"])
    statement = json.loads(payload)
    assert statement["_type"] == IN_TOTO_STATEMENT_V1
    assert statement["predicateType"] == COSIGN_SIGN_PREDICATE_TYPE, statement["predicateType"]
    # THE discriminator between an image signature and an attestation, and the
    # reason the two spike_cosign_*.json fixtures do not serve: an attestation's
    # predicate carries a document, a signature's is empty.
    assert statement["predicate"] == {}, (
        f"non-empty predicate — an attestation, not a signature: {statement['predicate']!r}"
    )
    assert len(statement["subject"]) == 1
    assert "sha256:" + statement["subject"][0]["digest"]["sha256"] == subject_digest

    if expect_certificate:
        der = base64.b64decode(material["certificate"]["rawBytes"])
        public_key = x509.load_der_x509_certificate(der).public_key()
    else:
        public_key = serialization.load_pem_public_key((root / "keys" / "cosign.pub").read_bytes())
    assert isinstance(public_key, ec.EllipticCurvePublicKey)
    signature = base64.b64decode(envelope["signatures"][0]["sig"])
    public_key.verify(
        signature, _pae(envelope["payloadType"], payload), ec.ECDSA(hashes.SHA256())
    )
    print(f"[OK] {label}: referrer + bundle, empty cosign/sign/v1 predicate, DSSE signature verifies")


def _check_fallback_index(root: Path) -> None:
    index = json.loads((root / "fallback_index.json").read_text())
    assert index["mediaType"] == "application/vnd.oci.image.index.v1+json"
    children = [m for m in index["manifests"] if m.get("artifactType") == SIGSTORE_BUNDLE_V03]
    assert len(children) == 1, f"expected 1 sigstore child, found {len(children)}"
    child = children[0]
    # cosign#4641, asserted positively rather than merely noted: the fallback
    # path keeps artifactType and drops every annotation. If a future cosign
    # fixes it, this line fails and the fixture stops being evidence for a bug
    # that no longer exists — which is the correct failure.
    assert not child.get("annotations"), (
        "fallback index child now carries annotations — cosign#4641 may be fixed; "
        f"re-capture and re-read the claim: {child.get('annotations')!r}"
    )
    print("[OK] fallback_index: 1 sigstore child, artifactType kept, annotations dropped (cosign#4641)")


def _check_simplesigning(root: Path, label: str, *, expect_certificate: bool) -> None:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec

    manifest = json.loads((root / f"simplesigning_{label}_manifest.json").read_text())
    payload = (root / f"simplesigning_{label}_payload.json").read_bytes()

    assert len(manifest["layers"]) == 1, (
        f"{len(manifest['layers'])} layers — `cosign attach signature` appends, so this "
        "fixture was captured over a stale one"
    )
    layer = manifest["layers"][0]
    assert layer["mediaType"] == SIMPLESIGNING_MEDIA_TYPE
    # Proves the payload file is the registry's bytes and was never reformatted:
    # both the digest and the length are over the file as committed.
    assert layer["digest"] == "sha256:" + hashlib.sha256(payload).hexdigest(), (
        f"simplesigning_{label}_payload.json does not hash to the digest its manifest "
        "names — the file was reformatted, and it is the signed message"
    )
    assert layer["size"] == len(payload), (
        f"simplesigning_{label}_payload.json is {len(payload)} bytes, manifest says "
        f"{layer['size']}"
    )

    claim = json.loads(payload)
    assert claim["critical"]["type"] == "cosign container image signature"
    provenance = json.loads((root / "provenance.json").read_text())
    subject = provenance["fixtures"][f"simplesigning_{label}_manifest.json"]["subject_digest"]
    assert claim["critical"]["image"]["docker-manifest-digest"] == subject, (
        "claim names a different image than the one it was captured against"
    )

    annotations = layer["annotations"]
    signature = base64.b64decode(annotations[SIGNATURE_ANNOTATION])
    if expect_certificate:
        pem = annotations[CERTIFICATE_ANNOTATION].encode()
        public_key = x509.load_pem_x509_certificate(pem).public_key()
    else:
        assert CERTIFICATE_ANNOTATION not in annotations, "key mode must attach no certificate"
        public_key = serialization.load_pem_public_key((root / "keys" / "cosign.pub").read_bytes())
    assert isinstance(public_key, ec.EllipticCurvePublicKey)
    public_key.verify(signature, payload, ec.ECDSA(hashes.SHA256()))

    # Negative control, matching attestations.py: a flipped byte must NOT
    # verify, so the check above is discriminating rather than vacuous.
    corrupted = bytearray(signature)
    corrupted[-1] ^= 0xFF
    try:
        public_key.verify(bytes(corrupted), payload, ec.ECDSA(hashes.SHA256()))
        raise AssertionError("corrupted signature verified — the positive check proves nothing")
    except Exception as error:  # noqa: BLE001 -- generic: cryptography's own InvalidSignature
        assert "the positive check proves nothing" not in str(error)
    print(f"[OK] simplesigning_{label}: 1 layer, payload byte-exact, signature verifies over it")


def _check_attestation_sidecar(root: Path) -> None:
    """The `.att` sidecar: a DSSE envelope layer, and no referrer anywhere in it.

    The two negative assertions are the fixture's whole point. A `.att` manifest
    carries **no** `artifactType` and **no** `subject`, so it is invisible to the
    Referrers API by construction -- which is the measured answer to "what is
    cosign's attestation artifact type": there is none, and `.att` is reachable
    only by its tag.
    """
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec

    manifest = json.loads((root / "attestation_sidecar_key_manifest.json").read_text())
    envelope_bytes = (root / "attestation_sidecar_key_envelope.json").read_bytes()

    assert "artifactType" not in manifest, (
        "an `.att` sidecar declares no artifactType -- if this one does, cosign grew "
        f"an attestation artifact type and the read path must be revisited: {manifest.get('artifactType')!r}"
    )
    assert "subject" not in manifest, (
        "an `.att` sidecar is not a referrer -- a subject here means cosign started "
        "writing one, and referrer discovery would then reach it"
    )
    assert len(manifest["layers"]) == 1, (
        f"{len(manifest['layers'])} layers -- `cosign attach attestation` appends, so this "
        "fixture was captured over a stale one"
    )
    layer = manifest["layers"][0]
    assert layer["mediaType"] == DSSE_ENVELOPE_MEDIA_TYPE, layer["mediaType"]
    assert layer["digest"] == "sha256:" + hashlib.sha256(envelope_bytes).hexdigest(), (
        "attestation_sidecar_key_envelope.json does not hash to the digest its manifest names"
    )
    assert layer["size"] == len(envelope_bytes), (
        f"envelope is {len(envelope_bytes)} bytes, manifest says {layer['size']}"
    )

    envelope = json.loads(envelope_bytes)
    assert envelope["payloadType"] == DSSE_PAYLOAD_TYPE, envelope["payloadType"]
    assert len(envelope["signatures"]) == 1, envelope["signatures"]

    payload = base64.b64decode(envelope["payload"])
    statement = json.loads(payload)
    assert statement["predicateType"] == CYCLONEDX_PREDICATE_TYPE, statement["predicateType"]
    assert statement["predicate"], (
        "empty predicate -- an image signature, not an attestation; the whole "
        "discriminator this fixture exists on the far side of"
    )
    provenance = json.loads((root / "provenance.json").read_text())
    subject = provenance["fixtures"]["attestation_sidecar_key_manifest.json"]["subject_digest"]
    assert len(statement["subject"]) == 1
    assert "sha256:" + statement["subject"][0]["digest"]["sha256"] == subject, (
        "the Statement binds a different manifest than the tag is attached to"
    )

    # The DSSE signature is over the PAE, not the payload -- the encoding's
    # whole reason. Key mode, so the committed public key is the verifier and
    # no annotation carries anything (see ATT_CERTIFICATE_GAP).
    assert not layer.get("annotations", {}).get(CERTIFICATE_ANNOTATION), (
        "cosign attached a certificate to an `.att` layer -- ATT_CERTIFICATE_GAP may "
        "be closed, and the keyless arm can then be covered by a golden fixture"
    )
    public_key = serialization.load_pem_public_key((root / "keys" / "cosign.pub").read_bytes())
    assert isinstance(public_key, ec.EllipticCurvePublicKey)
    signature = base64.b64decode(envelope["signatures"][0]["sig"])
    pae = _pae(envelope["payloadType"], payload)
    public_key.verify(signature, pae, ec.ECDSA(hashes.SHA256()))

    # Negative control, matching every sibling: the positive check above must be
    # discriminating rather than vacuous.
    corrupted = bytearray(signature)
    corrupted[-1] ^= 0xFF
    try:
        public_key.verify(bytes(corrupted), pae, ec.ECDSA(hashes.SHA256()))
        raise AssertionError("corrupted signature verified -- the positive check proves nothing")
    except Exception as error:  # noqa: BLE001 -- generic: cryptography's own InvalidSignature
        assert "the positive check proves nothing" not in str(error)
    print("[OK] attestation_sidecar_key: tag-only (no artifactType, no subject), DSSE envelope verifies over its PAE")


#: The public-good Rekor hostname, assembled rather than written.
#: A scanner whose needle appears verbatim in a file it scans is measuring
#: itself: it would either match this module and fail forever, or have to
#: exclude this module and stop covering it. Split so the literal exists
#: nowhere on disk, and the scan can then cover every file including this one.
_PUBLIC_REKOR_HOST = b"rekor." + b"sigstore" + b".dev"


def _check_sbom_sidecar(root: Path) -> None:
    """The `.sbom` sidecar: a document layer, no referrer fields, no signature.

    Three negative assertions carry this fixture. The first two are the `.att`
    check's, for the same reason -- no `artifactType`, no `subject`, so no
    listing reaches the tag and the tag is the whole discovery story. The third
    is this shape's own: nothing in the manifest is a signature, which is what
    makes `ocx package sbom --verify` refuse it rather than a policy decision
    somebody could soften.

    The positive assertion is that the layer is typed by the DOCUMENT, not by
    cosign. That single fact is why a simplesigning or DSSE reader aimed here
    returns an empty scan, and why the reader lives on the permissive listing
    path instead.
    """
    manifest = json.loads((root / "sbom_sidecar_manifest.json").read_text())
    document = (root / "sbom_sidecar_document.json").read_bytes()

    assert "artifactType" not in manifest, (
        "a `.sbom` sidecar declares no artifactType -- if this one does, cosign started "
        "typing the legacy tag and referrer discovery would reach it: "
        f"{manifest.get('artifactType')!r}"
    )
    assert "subject" not in manifest, (
        "a `.sbom` sidecar is not a referrer -- a subject here means cosign started "
        "writing one, and the tag would no longer be the only door"
    )
    assert len(manifest["layers"]) == 1, (
        f"{len(manifest['layers'])} layers -- a second `cosign attach sbom` REPLACES the "
        "manifest rather than appending, so more than one layer means this fixture was "
        "not written by `attach sbom`, and `read_sbom_sidecar_tag` reads only the first"
    )
    layer = manifest["layers"][0]
    assert layer["mediaType"] == SBOM_CYCLONEDX_MEDIA_TYPE, (
        f"the layer must be typed by the document, not by cosign: {layer['mediaType']!r}"
    )
    assert layer["digest"] == "sha256:" + hashlib.sha256(document).hexdigest(), (
        "sbom_sidecar_document.json does not hash to the digest its manifest names"
    )
    assert layer["size"] == len(document), (
        f"document is {len(document)} bytes, manifest says {layer['size']}"
    )

    # Nothing here is signed, and the assertion is over the whole file rather
    # than a field list: cosign carries a simplesigning signature and a
    # certificate in manifest *annotations*, so a fixture that grew either would
    # grow it somewhere a named-key check would not be looking.
    serialized = json.dumps(manifest)
    for marker in (SIGNATURE_ANNOTATION, CERTIFICATE_ANNOTATION, "dev.sigstore.cosign/bundle"):
        assert marker not in serialized, (
            f"{marker} appears in a `.sbom` sidecar -- `attach sbom` signs nothing, so "
            "either the capture is wrong or cosign grew a signed SBOM sidecar and the "
            "demand-mode refusal must be revisited"
        )

    assert json.loads(document)["bomFormat"] == "CycloneDX", (
        "the layer bytes must be the CycloneDX document the manifest types them as"
    )
    print("[OK] sbom_sidecar: tag-only (no artifactType, no subject), layer typed by the document, nothing signed")


def _check_no_public_good_rekor(root: Path) -> None:
    """No public-good Sigstore material, anywhere in the directory.

    A fixture carrying a public-good Rekor checkpoint pins a transparency log
    this project does not control, cannot reproduce, and must never make a test
    depend on. The scan covers every file under `root`, this module included.
    """
    hits = [
        str(path.relative_to(root))
        for path in sorted(root.rglob("*"))
        if path.is_file() and _PUBLIC_REKOR_HOST in path.read_bytes()
    ]
    assert not hits, f"public-good Rekor material in: {hits}"
    print("[OK] no public-good Rekor hostname in any file under the fixture directory")


def self_check(root: Path = _HERE) -> None:
    """Structurally and cryptographically validate every committed fixture."""
    expected = [
        "provenance.json",
        "keys/cosign.key",
        "keys/cosign.pub",
        *(f"{label}_{part}.json" for label in ("keyless", "key")
          for part in ("referrer_manifest", "bundle")),
        "fallback_index.json",
        *(f"simplesigning_{label}_{part}.json" for label in ("keyless", "key")
          for part in ("manifest", "payload")),
        "attestation_sidecar_key_manifest.json",
        "attestation_sidecar_key_envelope.json",
        "sbom_sidecar_manifest.json",
        "sbom_sidecar_document.json",
    ]
    missing = [name for name in expected if not (root / name).exists()]
    assert not missing, f"missing fixtures: {missing}"

    _check_bundle(root, "keyless", expect_certificate=True)
    _check_bundle(root, "key", expect_certificate=False)
    _check_fallback_index(root)
    _check_simplesigning(root, "keyless", expect_certificate=True)
    _check_simplesigning(root, "key", expect_certificate=False)
    _check_attestation_sidecar(root)
    _check_sbom_sidecar(root)
    _check_no_public_good_rekor(root)
    print("\nAll golden cosign fixtures self-check clean.")


if __name__ == "__main__":
    if "--regenerate" in sys.argv:
        regenerate()
        self_check()
    elif "--check" in sys.argv:
        self_check(Path(sys.argv[sys.argv.index("--check") + 1]).resolve())
    else:
        self_check()
