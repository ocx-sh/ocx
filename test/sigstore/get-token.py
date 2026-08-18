#!/usr/bin/env python3
"""Mint an OIDC ID token from the local dex, non-interactively.

dex is configured with `oauth2.passwordConnector: local`, so the resource-owner
password grant works: one HTTP call, no browser, no callback listener. That is
what makes the real Sigstore stack usable from CI and from pytest.

Prints the raw ID token to stdout.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_TOKEN_URL = "http://localhost:{port}/dex/token"
CLIENT_ID = "ocx-test"
CLIENT_SECRET = "ocx-test-secret"
USERNAME = "ocx-test@example.com"
PASSWORD = "ocxtest"


def fetch_id_token(token_url: str) -> str:
    body = urllib.parse.urlencode(
        {
            "grant_type": "password",
            "username": USERNAME,
            "password": PASSWORD,
            "scope": "openid email",
        }
    ).encode()
    basic = base64.b64encode(f"{CLIENT_ID}:{CLIENT_SECRET}".encode()).decode()
    request = urllib.request.Request(
        token_url,
        data=body,
        headers={
            "Authorization": f"Basic {basic}",
            "Content-Type": "application/x-www-form-urlencoded",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as exc:  # surface dex's own error text
        sys.exit(f"dex token request failed: {exc.code} {exc.read().decode()}")
    token = payload.get("id_token")
    if not token:
        sys.exit(f"dex returned no id_token; keys were {sorted(payload)}")
    return token


def decode_claims(token: str) -> dict:
    segment = token.split(".")[1]
    segment += "=" * (-len(segment) % 4)
    return json.loads(base64.urlsafe_b64decode(segment))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--port",
        default=os.environ.get("OCX_TEST_DEX_PORT", "5556"),
        help="host port dex is published on (default: 5556)",
    )
    parser.add_argument(
        "--claims", action="store_true", help="print decoded claims instead of the token"
    )
    args = parser.parse_args()

    token = fetch_id_token(DEFAULT_TOKEN_URL.format(port=args.port))
    if args.claims:
        print(json.dumps(decode_claims(token), indent=2, sort_keys=True))
    else:
        print(token)


if __name__ == "__main__":
    main()
