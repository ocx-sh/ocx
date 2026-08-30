#!/usr/bin/env python3
"""Build `trusted_root.json` for the local Sigstore stack from the committed keys.

The output is a Sigstore `TrustedRoot` (protobuf-JSON), the same shape
`cosign trusted-root create` emits and the shape
`SigstoreTrustRoot::from_trusted_root_json_unchecked` consumes. Generating it
here rather than shelling out to cosign keeps the test path free of a Go
toolchain.

Because every input is committed and static, the output is static too: it is
committed alongside the keys and only regenerated when they are. It is also the
worked example for the self-hosting documentation -- an operator running their
own Fulcio/Rekor/CT log produces exactly this file for their own material.

Usage: ./generate-trusted-root.py [--keys DIR] [--out FILE]
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import pathlib
import subprocess
import sys

# The stack's key material is minted with no meaningful start date, so anything
# at or before the CA's notBefore works. Epoch keeps the file free of a
# regeneration-time timestamp, which would otherwise make the output non-static.
VALID_FROM = "1970-01-01T00:00:00Z"

MEDIA_TYPE = "application/vnd.dev.sigstore.trustedroot+json;version=0.1"
KEY_DETAILS = "PKIX_ECDSA_P256_SHA_256"


def run(*args: str) -> bytes:
    result = subprocess.run(args, capture_output=True, check=False)
    if result.returncode != 0:
        sys.exit(f"{' '.join(args)} failed: {result.stderr.decode().strip()}")
    return result.stdout


def der_spki(pub_pem: pathlib.Path) -> bytes:
    """The DER SubjectPublicKeyInfo -- what `rawBytes` carries."""
    return run("openssl", "pkey", "-pubin", "-in", str(pub_pem), "-outform", "DER")


def der_cert(cert_pem: pathlib.Path) -> bytes:
    return run("openssl", "x509", "-in", str(cert_pem), "-outform", "DER")


def b64(raw: bytes) -> str:
    return base64.b64encode(raw).decode()


def log_entry(pub_pem: pathlib.Path, base_url: str) -> dict:
    """A tlog/ctlog entry. `logId.keyId` is sha256 over the DER SPKI."""
    spki = der_spki(pub_pem)
    return {
        "baseUrl": base_url,
        "hashAlgorithm": "SHA2_256",
        "publicKey": {
            "rawBytes": b64(spki),
            "keyDetails": KEY_DETAILS,
            "validFor": {"start": VALID_FROM},
        },
        "logId": {"keyId": b64(hashlib.sha256(spki).digest())},
    }


def main() -> None:
    here = pathlib.Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keys", type=pathlib.Path, default=here / "keys")
    parser.add_argument("--out", type=pathlib.Path, default=here / "trusted_root.json")
    parser.add_argument("--fulcio-url", default="http://localhost:5555")
    parser.add_argument("--rekor-url", default="http://localhost:3000")
    parser.add_argument("--ct-url", default="http://localhost:6962/ocx-test")
    args = parser.parse_args()

    ca_der = der_cert(args.keys / "fulcio-ca.crt.pem")
    subject = run(
        "openssl", "x509", "-in", str(args.keys / "fulcio-ca.crt.pem"), "-noout", "-subject"
    ).decode().strip()

    trusted_root = {
        "mediaType": MEDIA_TYPE,
        "certificateAuthorities": [
            {
                "subject": {"organization": "ocx test", "commonName": "ocx test fulcio CA"},
                "uri": args.fulcio_url,
                "certChain": {"certificates": [{"rawBytes": b64(ca_der)}]},
                "validFor": {"start": VALID_FROM},
            }
        ],
        "tlogs": [log_entry(args.keys / "rekor.pub.pem", args.rekor_url)],
        "ctlogs": [log_entry(args.keys / "ct.pub.pem", args.ct_url)],
        "timestampAuthorities": [],
    }

    args.out.write_text(json.dumps(trusted_root, indent=2, sort_keys=True) + "\n")
    print(f"wrote {args.out}")
    print(f"  CA        {subject}")
    print(f"  rekor key {trusted_root['tlogs'][0]['logId']['keyId']}")
    print(f"  ct key    {trusted_root['ctlogs'][0]['logId']['keyId']}")


if __name__ == "__main__":
    main()
