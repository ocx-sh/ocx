#!/usr/bin/env python3
"""Check that `trusted_root.json` matches the services actually running.

The failure this catches is cheap to cause and expensive to diagnose: regenerate
the keys, forget to regenerate the trust root, and every signing test fails with
an opaque verification error that looks like a bug in ocx.

Compares the trust root's material against what Fulcio and Rekor serve about
themselves. No crypto here beyond a byte comparison -- if the bytes match, the
anchors are the anchors.

Usage: ./verify-trusted-root.py [--root FILE] [--fulcio URL] [--rekor URL]
Exit 0 when they agree, 1 when they do not.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import pathlib
import ssl
import sys
import urllib.request

from cryptography import x509
from cryptography.hazmat.primitives import serialization


def fetch(url: str) -> bytes:
    ctx = ssl.create_default_context()
    with urllib.request.urlopen(url, timeout=15, context=ctx) as response:
        return response.read()


def main() -> int:
    here = pathlib.Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=here / "trusted_root.json")
    parser.add_argument("--fulcio", default="http://localhost:5555")
    parser.add_argument("--rekor", default="http://localhost:3000")
    args = parser.parse_args()

    root = json.loads(args.root.read_text())
    failures: list[str] = []

    # Fulcio: the CA in the trust root must be the CA Fulcio issues under.
    want_ca = base64.b64decode(
        root["certificateAuthorities"][0]["certChain"]["certificates"][0]["rawBytes"]
    )
    live_ca = x509.load_pem_x509_certificate(
        fetch(f"{args.fulcio}/api/v1/rootCert")
    ).public_bytes(serialization.Encoding.DER)
    if want_ca == live_ca:
        print("ok   fulcio CA matches trusted_root.json")
    else:
        failures.append(
            "fulcio CA differs -- regenerate with ./generate-trusted-root.py"
        )

    # Rekor: the log key in the trust root must be the key Rekor signs with, and
    # the logId must be its sha256, since that is what identifies the log.
    want_key = base64.b64decode(root["tlogs"][0]["publicKey"]["rawBytes"])
    live_key = serialization.load_pem_public_key(
        fetch(f"{args.rekor}/api/v1/log/publicKey")
    ).public_bytes(
        serialization.Encoding.DER,
        serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    if want_key == live_key:
        print("ok   rekor key matches trusted_root.json")
    else:
        failures.append("rekor key differs -- regenerate with ./generate-trusted-root.py")

    want_id = root["tlogs"][0]["logId"]["keyId"]
    derived_id = base64.b64encode(hashlib.sha256(want_key).digest()).decode()
    if want_id == derived_id:
        print("ok   rekor logId is sha256 of its own key")
    else:
        failures.append(f"rekor logId {want_id} is not sha256 of the key it ships with")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
