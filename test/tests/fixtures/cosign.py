"""Drive the upstream `cosign` CLI against the local Sigstore stack.

Interop with cosign is a bundle-format contract, not a discovery one: ocx
attaches its signature through the OCI 1.1 Referrers API and cosign 3.x
discovers only through its own `sha256-<hex>.sig` tag schema, and ocx has no
tag-schema fallback by design. So both directions go through cosign's blob
commands, which take the bundle as a file and the signed bytes as a file — the
exact two artifacts the two implementations must agree on.

cosign runs from a pinned container image rather than an installed binary:
docker is already a hard dependency of the `sigstore` compose profile, the
version under test is then written down in one place, and CI needs no extra
install step. `--network host` is what lets the container reach the stack and
the registry on localhost.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

#: Pinned deliberately. The floor is cosign >= 3.0 (issue #197 drops pre-3.0
#: compatibility); a floating tag would make a green run unattributable to a
#: version.
COSIGN_IMAGE = "ghcr.io/sigstore/cosign/cosign:v3.1.1"

#: A start time old enough that any certificate the stack mints falls inside the
#: service's validity window. cosign requires the key, and its value is
#: irrelevant to what these tests assert.
_SERVICE_EPOCH = "2000-01-01T00:00:00Z"


def run(workdir: Path, *args: str, check: bool = False) -> subprocess.CompletedProcess[str]:
    """Run cosign with ``workdir`` mounted at /work and localhost shared.

    Every path argument is therefore relative to ``workdir`` — a host path would
    not resolve inside the container.
    """
    return subprocess.run(
        [
            "docker", "run", "--rm", "--network", "host",
            # As the host user, so cosign reads the files pytest wrote and writes
            # files pytest can read back. The image's own non-root user owns
            # neither. HOME has to move with it: the image's is not writable by
            # an arbitrary uid, and cosign touches it even when every service is
            # named explicitly.
            "--user", f"{os.getuid()}:{os.getgid()}",
            "-e", "HOME=/work",
            "-v", f"{workdir}:/work", "-w", "/work",
            COSIGN_IMAGE, *args,
        ],
        capture_output=True,
        text=True,
        check=check,
    )


def signing_config(workdir: Path, *, fulcio_url: str, rekor_url: str, oidc_url: str) -> str:
    """Write a signing config naming the local services; return its relative name.

    cosign 3 removed `--fulcio-url`/`--rekor-url` from the signing commands in
    favour of this file, so pointing it at a self-hosted stack is not optional
    plumbing — it is the only route.
    """
    name = "signing-config.json"
    result = run(
        workdir,
        "signing-config", "create",
        "--no-default-fulcio", "--no-default-rekor",
        "--no-default-oidc", "--no-default-tsa",
        f"--fulcio=url={fulcio_url},api-version=1,start-time={_SERVICE_EPOCH},operator=ocx-test",
        f"--rekor=url={rekor_url},api-version=1,start-time={_SERVICE_EPOCH},operator=ocx-test",
        "--rekor-config=ANY",
        f"--oidc-provider=url={oidc_url},api-version=1,start-time={_SERVICE_EPOCH},operator=ocx-test",
        "--out", name,
    )
    assert result.returncode == 0, f"signing-config create failed:\n{result.stdout}\n{result.stderr}"
    return name


def stage(workdir: Path, name: str, content: bytes | dict) -> str:
    """Write ``content`` into the mounted directory; return its relative name."""
    payload = json.dumps(content).encode() if isinstance(content, dict) else content
    (workdir / name).write_bytes(payload)
    return name
