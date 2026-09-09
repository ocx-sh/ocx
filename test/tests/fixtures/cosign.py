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
`signing_config`, `cosign_binary`).

cosign runs from **this project's own toolchain** — `ocx.toml` binds
`cosign = "ocx.sh/sigstore/cosign:3.1.1"` and `ocx.lock` pins its digest per
platform — resolved through `ocx env` rather than looked up on the ambient
`PATH`. It used to run from a pinned container image, one `docker run --rm` per
cosign invocation, which bought a written-down version and cost a container
start on every call in the matrix. The binding buys the same pin (an exact
version, never a floating tag: a floating tag makes a green unattributable) out
of a store ocx already fetches, and the version check now interrogates the
**binary** rather than an image tag string.
"""

from __future__ import annotations

import json
import os
import subprocess
from functools import cache
from pathlib import Path

from src.helpers import PROJECT_ROOT

#: This repository's `ocx.toml`, whose `[tools] cosign` binding is the pin.
_PROJECT = PROJECT_ROOT / "ocx.toml"

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
    """Run the toolchain's cosign with ``workdir`` as its working directory.

    Every path argument is therefore relative to ``workdir``, exactly as it was
    when this ran in a container with ``workdir`` mounted at ``/work``. No call
    site passes an absolute path — every one of them names a file
    :func:`stage` just wrote — which is why the container could be dropped
    without touching any of them.

    The two properties the container was providing are kept deliberately:

    * **the environment is stated, never inherited**, so a run cannot depend on
      the shell that started it — ``COSIGN_PASSWORD`` (the one setting cosign
      takes no flag for, and which cannot go in an argv) is the reason ``env``
      exists at all;
    * **``HOME`` points at ``workdir``**, because cosign touches it even when
      every service is named explicitly, and a test must not write into the
      developer's real home.

    ``PATH`` carries the cosign directory alone: enough for anything cosign
    spawns to find its sibling, and not the ambient ``PATH`` that would let an
    installed cosign shadow the pinned one.
    """
    binary = cosign_binary()
    environment = {"HOME": str(workdir), "PATH": str(binary.parent), **(env or {})}
    return subprocess.run(
        [str(binary), *args],
        cwd=workdir,
        env=environment,
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
    in one ``workdir``, and every path handed to cosign is relative to it.
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
    """Write ``content`` into ``workdir``; return its relative name."""
    payload = json.dumps(content).encode() if isinstance(content, dict) else content
    (workdir / name).write_bytes(payload)
    return name


# ---------------------------------------------------------------------------
# Resolution — the pinned binary, out of this project's own toolchain
# ---------------------------------------------------------------------------


def _ocx() -> str:
    """The ocx under test, resolved as ``conftest.ocx_binary`` resolves it.

    Not the fixture: this module is imported by `test/recordings/setups.py` and
    by the hand-run golden-fixture regenerator, neither of which is inside a
    pytest session.
    """
    return os.environ.get("OCX_COMMAND") or str(PROJECT_ROOT / "test" / "bin" / "ocx")


def _ocx_json(*args: str) -> dict:
    result = subprocess.run(
        [_ocx(), "--format", "json", *args],
        capture_output=True, text=True, check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"`ocx --format json {' '.join(args)}` failed (rc={result.returncode}):\n{result.stderr}"
        )
    return json.loads(result.stdout)


@cache
def pinned_cosign_version() -> str:
    """The version `ocx.toml` pins, as cosign's ``GitVersion:`` spells it.

    One source of truth: the ``[tools] cosign`` binding. The tag it carries is
    an exact version by construction (`ocx lock` would happily pin a floating
    one, and a floating tag is what makes a green unattributable to a version),
    and this refuses anything that is not — a pin of ``:3`` would otherwise turn
    the drift guard below into a string that matches nothing and a check that
    can never be green.

    cosign reports ``v3.1.1`` where the binding says ``3.1.1``; the ``v`` is
    added here rather than written into the binding, because the binding's
    grammar is ocx's and the prefix is cosign's.
    """
    declared = _ocx_json("--project", str(_PROJECT), "status")["groups"]["default"]["tools"]["cosign"][
        "declared"
    ]
    tag = declared.rsplit(":", 1)[1]
    assert tag.count(".") == 2, (
        f"`[tools] cosign` is pinned at {declared!r}, which is not an exact version. The interop "
        "matrix's claims were measured against one exact build; a floating tag makes a green "
        "unattributable to a version."
    )
    return f"v{tag}"


def _reports_pinned_version(binary: Path) -> bool:
    """True when ``binary`` runs and calls itself :func:`pinned_cosign_version`.

    Both halves matter. A file that exists proves nothing — a partial pull, a
    binary left by a previous pin, and a directory named `cosign` are all
    "present". Asking the tool what it is closes that, and it is the same
    question the matrix's C-004 guard asks.
    """
    if not os.access(binary, os.X_OK):
        return False
    probe = subprocess.run([str(binary), "version"], capture_output=True, text=True, check=False)
    return probe.returncode == 0 and any(
        line.split(":", 1)[1].strip() == pinned_cosign_version()
        for line in probe.stdout.splitlines()
        if line.startswith("GitVersion:")
    )


@cache
def resolved_cosign_digest() -> str:
    """The manifest digest `ocx.lock` pins for this host platform.

    Read through `ocx env`, which resolves the binding against the lock, so this
    is the lock's pin rather than a second reading of it. It replaces a
    `docker image inspect … RepoDigests` lookup that could only answer at all
    when the image happened to be present locally, and answered
    ``"unresolved"`` otherwise — a provenance field that silently degraded to
    nothing is worse than one that fails.
    """
    for binary in _ocx_json("--project", str(_PROJECT), "env")["binaries"]:
        if binary["name"] == "cosign":
            return binary["package"].split("@", 1)[1]
    raise RuntimeError("`ocx env` lists no `cosign` binary; is `[tools] cosign` still declared?")


@cache
def cosign_binary() -> Path:
    """The pinned cosign executable, resolved out of this project's toolchain.

    ``ocx env`` on this repository's own `ocx.toml` yields the composed `PATH`
    entries for the locked toolchain; the one holding an executable `cosign` is
    the answer. Deterministic in a way an ambient `PATH` lookup is not: it does
    not depend on the shell the suite was started from, on direnv having been
    allowed, or on a consent stamp — and an installed cosign of a different
    version cannot shadow the pinned one.

    :func:`run` needs the file; the cast recorder needs the *directory* on
    `PATH`, because a reader copy-pastes what the terminal shows and
    `docker run --rm --network host -v … cosign verify` is not a command
    anybody has. One resolver, both consumers — see `Path.parent`.
    """
    entries = [
        Path(entry["value"])
        for entry in _ocx_json("--project", str(_PROJECT), "env")["entries"]
        if entry["type"] == "path"
    ]
    name = "cosign.exe" if os.name == "nt" else "cosign"
    for directory in entries:
        candidate = directory / name
        if os.access(candidate, os.X_OK):
            if not _reports_pinned_version(candidate):
                raise RuntimeError(
                    f"{candidate} does not report GitVersion {pinned_cosign_version()}; the locked "
                    "toolchain and the pin in ocx.toml disagree. Run `ocx pull` from the repository "
                    "root."
                )
            return candidate
    raise RuntimeError(
        "no `cosign` on the project toolchain's composed PATH. `[tools] cosign` is declared in "
        "ocx.toml, so this means the package is not materialized: run `ocx pull` from the "
        f"repository root. Searched: {[str(entry) for entry in entries]}"
    )
