"""Drive the upstream `cosign` CLI against the local Sigstore stack.

Neither caller of this module is working around a discovery gap. Probe P3 in
`analysis_cosign_interop_probes.md` measured `cosign verify <ref>` reading the
Referrers API, the OCI fallback tag, and the `.sig` sidecar — cosign discovers
an ocx-produced signature on a real registry just fine. `test_cosign_interop.py`
uses the blob commands here (`verify-blob`, `attest-blob`, `sign-blob`) for a
narrower reason: those tests assert payload agreement, not discovery, so each
hands cosign the bundle as a file and a pass says only that the two
implementations agree on the bytes of a signature. `test_cosign_matrix_*.py` is
where discovery itself is asserted, driving `cosign sign`/`verify` against real
registries with the primitives this module provides (`run`, `run_registry`,
`signing_config`, `COSIGN_IMAGE`).

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


#: cosign refuses a plaintext registry without it, and every registry the
#: compose stack starts is plaintext. Named rather than inlined because
#: :func:`run_registry` has to know where in an argv it belongs.
ALLOW_HTTP_REGISTRY = "--allow-http-registry"


def run(
    workdir: Path,
    *args: str,
    env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run cosign with ``workdir`` mounted at /work and localhost shared.

    Every path argument is therefore relative to ``workdir`` — a host path would
    not resolve inside the container.

    ``env`` reaches cosign as container environment. It exists for the one
    setting cosign takes no flag for: ``COSIGN_PASSWORD``, which unlocks an
    encrypted private key. Passing it as an argument is not an option, and
    inheriting the host environment would make a run depend on the shell that
    started it.
    """
    environment: list[str] = []
    for key, value in (env or {}).items():
        environment += ["-e", f"{key}={value}"]
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
            *environment,
            "-v", f"{workdir}:/work", "-w", "/work",
            COSIGN_IMAGE, *args,
        ],
        capture_output=True,
        text=True,
        check=check,
    )


def registry_args(*args: str) -> tuple[str, ...]:
    """``args`` with ``--allow-http-registry`` inserted where cosign will read it.

    After the leading run of non-flag tokens — the subcommand path, one word for
    ``sign``, two for ``attach signature``. Not appended: a trailing flag lands
    after the image reference, and relying on cobra's interspersed-argument
    parsing to still see it is a coin flip nobody should have to call. Not
    prepended either, because cosign's root command does not own it.

    Public so a caller that has to *record* the command it ran — a fixture
    generator writing provenance, say — prints the argv cosign actually got
    rather than a plausible reconstruction of it.
    """
    split = next((i for i, arg in enumerate(args) if arg.startswith("-")), len(args))
    # `generate <ref>` carries no flag at all, so the scan above runs off the end
    # and would append. Clamp it back in front of the last positional.
    split = min(split, max(len(args) - 1, 1))
    return (*args[:split], ALLOW_HTTP_REGISTRY, *args[split:])


def run_registry(
    workdir: Path,
    *args: str,
    env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    """:func:`run` for the subcommands that dial a registry, with HTTP allowed."""
    return run(workdir, *registry_args(*args), env=env, check=check)


def signing_config(
    workdir: Path,
    *,
    rekor_url: str,
    fulcio_url: str | None = None,
    oidc_url: str | None = None,
    name: str = "signing-config.json",
) -> str:
    """Write a signing config naming the local services; return its relative name.

    cosign 3 removed `--fulcio-url`/`--rekor-url` from the signing commands in
    favour of this file, so pointing it at a self-hosted stack is not optional
    plumbing — it is the only route. It is also not skippable: `--tlog-upload=false`
    is rejected once a signing config is in play, and without one the signing
    commands fail at "failed to verify log inclusion: not enough verified log
    entries" against the public-good defaults they cannot reach.

    Omitting ``fulcio_url`` and ``oidc_url`` yields the **key-mode** config: a
    `--key` signer mints no certificate and needs no identity, so naming a CA and
    an issuer it will never call would only invite cosign to try. The two are
    optional together rather than separately — a Fulcio with no issuer to
    authenticate against is not a configuration anything can use.

    ``name`` is a parameter because a caller may need both variants side by side
    in one mounted directory, and the container sees only relative paths.
    """
    services = [
        f"--rekor=url={rekor_url},api-version=1,start-time={_SERVICE_EPOCH},operator=ocx-test",
        "--rekor-config=ANY",
    ]
    if fulcio_url and oidc_url:
        services += [
            f"--fulcio=url={fulcio_url},api-version=1,start-time={_SERVICE_EPOCH},operator=ocx-test",
            f"--oidc-provider=url={oidc_url},api-version=1,start-time={_SERVICE_EPOCH},operator=ocx-test",
        ]
    elif fulcio_url or oidc_url:
        raise ValueError("fulcio_url and oidc_url are keyless mode: pass both or neither")
    result = run(
        workdir,
        "signing-config", "create",
        "--no-default-fulcio", "--no-default-rekor",
        "--no-default-oidc", "--no-default-tsa",
        *services,
        "--out", name,
    )
    assert result.returncode == 0, f"signing-config create failed:\n{result.stdout}\n{result.stderr}"
    return name


def stage(workdir: Path, name: str, content: bytes | dict) -> str:
    """Write ``content`` into the mounted directory; return its relative name."""
    payload = json.dumps(content).encode() if isinstance(content, dict) else content
    (workdir / name).write_bytes(payload)
    return name


# ---------------------------------------------------------------------------
# Native binary — for the recorded cast, which must show a bare `cosign …`
# ---------------------------------------------------------------------------

#: The tag half of :data:`COSIGN_IMAGE`, which is also exactly what
#: `cosign version` prints as `GitVersion`. Derived rather than re-spelled so
#: the version-drift guard (`cosign_matrix.C-004`) keeps covering this path too.
PINNED_COSIGN_VERSION = COSIGN_IMAGE.rsplit(":", 1)[1]

#: Cache keyed by the pinned version, so a bump lands in a fresh directory
#: instead of reusing whatever binary a previous pin left behind. Outside any
#: git tree and outside `/tmp` (reaped): a cast recording must not pay the
#: extraction cost once per run.
_BIN_DIR = Path.home() / ".cache" / "ocx-cosign" / PINNED_COSIGN_VERSION

#: Where ko puts the entrypoint in the distroless image.
_IMAGE_BINARY = "/ko-app/cosign"


def _reports_pinned_version(binary: Path) -> bool:
    """True when ``binary`` runs and calls itself :data:`PINNED_COSIGN_VERSION`.

    Both halves matter. A file that exists proves nothing — a truncated
    `docker cp`, a binary left by a previous pin, and a directory named
    `cosign` are all "present". Asking the tool what it is closes that, and it
    is the same question the matrix's C-004 guard asks of the image tag.
    """
    if not os.access(binary, os.X_OK):
        return False
    probe = subprocess.run([str(binary), "version"], capture_output=True, text=True)
    return probe.returncode == 0 and any(
        line.split(":", 1)[1].strip() == PINNED_COSIGN_VERSION
        for line in probe.stdout.splitlines()
        if line.startswith("GitVersion:")
    )


def ensure_cosign_binary() -> Path:
    """Extract :data:`COSIGN_IMAGE`'s binary to the cache; return its directory.

    :func:`run` is the right shape for a test that only needs cosign's answer,
    and the wrong one for a *cast*: a reader copy-pastes what the terminal
    shows, and `docker run --rm --network host -v … cosign verify` is not a
    command anybody has. A recording therefore needs cosign on `$PATH` as
    itself. Lifting the binary out of the image we already pin gives that
    without a second version source and without a download step in CI — the
    image is already a dependency of the interop matrix.

    The image is distroless, so there is no shell to copy the file out with;
    `docker create` + `docker cp` reads the layer without ever starting it.

    Safe under `pytest -n auto` without a lock: the container name carries the
    pid, so two workers never contend for it, and the binary is renamed into
    place from a staging file, so a concurrent reader sees either the previous
    contents or a complete one — never a half-written file. A redundant second
    extraction is the whole cost of skipping the lock.
    """
    binary = _BIN_DIR / "cosign"
    if _reports_pinned_version(binary):
        return _BIN_DIR

    _BIN_DIR.mkdir(parents=True, exist_ok=True)
    container = f"ocx-cosign-extract-{os.getpid()}"
    staged = _BIN_DIR / f"cosign.{os.getpid()}.partial"
    created = subprocess.run(
        ["docker", "create", "--name", container, COSIGN_IMAGE],
        capture_output=True, text=True,
    )
    if created.returncode != 0:
        raise RuntimeError(f"docker create {COSIGN_IMAGE} failed:\n{created.stdout}\n{created.stderr}")
    try:
        copied = subprocess.run(
            ["docker", "cp", f"{container}:{_IMAGE_BINARY}", str(staged)],
            capture_output=True, text=True,
        )
        if copied.returncode != 0:
            raise RuntimeError(f"docker cp {_IMAGE_BINARY} failed:\n{copied.stdout}\n{copied.stderr}")
        staged.chmod(0o755)
        staged.replace(binary)  # atomic within one filesystem
    finally:
        staged.unlink(missing_ok=True)
        subprocess.run(["docker", "rm", "-f", container], capture_output=True)

    if not _reports_pinned_version(binary):
        raise RuntimeError(
            f"the binary extracted from {COSIGN_IMAGE} does not report "
            f"GitVersion {PINNED_COSIGN_VERSION}; refusing to put it on a cast's PATH"
        )
    return _BIN_DIR
