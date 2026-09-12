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
    signer = os.environ.get("OCX_TEST_TRILLIAN_SIGNER_PORT", "8091")
    return [
        ("dex", f"http://localhost:{dex}/dex/healthz", "the OIDC issuer can mint tokens"),
        ("sigstore-ct", f"http://localhost:{ct}/ocx-test/ct/v1/get-roots",
         "the CT log accepts the Fulcio CA as a root"),
        ("fulcio", f"http://localhost:{fulcio}/api/v1/rootCert",
         "the CA is serving its root certificate"),
        ("rekor", f"http://localhost:{rekor}/api/v1/log",
         "Rekor reached Trillian and has a tree"),
        # Rekor's endpoint above is a *read*, and it answers as soon as Rekor
        # can dial the log **server**. A sign is a write, and a write is only
        # integrated once the **signer** is sequencing, so without this row the
        # gate goes green over a stack that cannot sign. Measured: the signer
        # sat at `lookup sigstore-mysql: no such host` from 13:40:53 to
        # 13:57:01 while Rekor held `restarts=0`, and 44 signing rows failed
        # with exit 83 (`transparency_log_unavailable`) inside that window.
        # `/healthz` is the honest probe because it checks database access --
        # it answers 503 with that very string while the signer is stuck.
        ("trillian-log-signer", f"http://localhost:{signer}/healthz",
         "the sequencer reaches its database, so a Rekor entry gets integrated"),
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
    checks = endpoints()
    pending = {name: (url, why) for name, url, why in checks}
    ready: dict[str, float] = {}

    # Re-probe every endpoint on every pass, and release only when they all
    # answer in the *same* sweep. Readiness must not latch: a service that
    # flaps -- the log signer restarting against a database it cannot yet
    # resolve -- answers once between restarts, and a latched pass would open
    # the gate on that single sample and never look again.
    while time.monotonic() - started < args.timeout:
        pending = {}
        for name, url, why in checks:
            if probe(url):
                if name not in ready:
                    ready[name] = time.monotonic() - started
                    if not args.quiet:
                        print(f"  ready  {name:14} {ready[name]:6.1f}s", flush=True)
            else:
                ready.pop(name, None)
                pending[name] = (url, why)
        if not pending:
            break
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
