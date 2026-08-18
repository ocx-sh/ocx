#!/usr/bin/env python3
"""Block until every service in the `sigstore` compose profile answers.

Four of the seven images are distroless -- no shell, no curl -- so a compose
`healthcheck:` cannot run inside them. Readiness is therefore polled from the
host, which is also where a failure can say something useful. This is the gate
the acceptance fixtures call before the first sign.

Exit 0 when all endpoints answer, 1 with a per-service report otherwise.

Usage: ./wait-for-stack.py [--timeout SECONDS] [--quiet]
"""

from __future__ import annotations

import argparse
import os
import sys
import time
import urllib.error
import urllib.request

# (service, url, what a 200 proves)
def endpoints() -> list[tuple[str, str, str]]:
    dex = os.environ.get("OCX_TEST_DEX_PORT", "5556")
    fulcio = os.environ.get("OCX_TEST_FULCIO_PORT", "5555")
    rekor = os.environ.get("OCX_TEST_REKOR_PORT", "3000")
    ct = os.environ.get("OCX_TEST_CT_PORT", "6962")
    return [
        ("dex", f"http://localhost:{dex}/dex/healthz", "the OIDC issuer can mint tokens"),
        ("sigstore-ct", f"http://localhost:{ct}/ocx-test/ct/v1/get-roots",
         "the CT log accepts the Fulcio CA as a root"),
        ("fulcio", f"http://localhost:{fulcio}/api/v1/rootCert",
         "the CA is serving its root certificate"),
        ("rekor", f"http://localhost:{rekor}/api/v1/log",
         "Rekor reached Trillian and has a tree"),
    ]


def probe(url: str) -> bool:
    try:
        with urllib.request.urlopen(url, timeout=3) as r:
            return 200 <= r.status < 300
    except (urllib.error.URLError, urllib.error.HTTPError, OSError, TimeoutError):
        return False


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    started = time.monotonic()
    pending = {name: (url, why) for name, url, why in endpoints()}
    ready: dict[str, float] = {}

    while pending and time.monotonic() - started < args.timeout:
        for name in list(pending):
            if probe(pending[name][0]):
                ready[name] = time.monotonic() - started
                del pending[name]
                if not args.quiet:
                    print(f"  ready  {name:14} {ready[name]:6.1f}s", flush=True)
        if pending:
            time.sleep(1.0)

    if pending:
        print(f"\nstack not ready after {args.timeout:.0f}s", file=sys.stderr)
        for name, (url, why) in sorted(pending.items()):
            print(f"  DOWN   {name:14} {url}\n         expected: {why}", file=sys.stderr)
        print("\n  docker compose --profile sigstore logs --tail=40 "
              + " ".join(sorted(pending)), file=sys.stderr)
        return 1

    if not args.quiet:
        print(f"stack ready in {max(ready.values()):.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
