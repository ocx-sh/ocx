# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance for signing a *published, multi-manifest* package.

Contract source: OCX-C-3 in
``ocx-mirror/.claude/state/plans/plan_mirror_signing.md``. Its bullets are
lettered here so every test below can cite one::

    (a) `push --sign` writes one signature per platform manifest,
        then `verify` per platform
    (b) `push --sign --fulcio-url --rekor-url` against the local stack
    (c) `sign --tags-file` over an index, then `verify` on the tag
    (d) `sign -p <platform> <tag>` narrowing
    (e) the documented error on a bare manifest
    (f) `--key env://NAME` + `OCX_KEY_PASSWORD`, over the same three
    (g) both `bundle` and `simplesigning` (an ocx property, kept)

(f) and (g) are not tests of their own: they are the ``mode`` and
``signature_format`` axes every other bullet is parametrized over, which is
what "over the same three" asks for. A separate key-mode test per bullet
would leave the two modes free to drift apart.

**What this file adds over its neighbours.** ``test_sign.py`` and
``test_push.py`` already cover one platform each — ``test_push.py`` says so
outright, and its ``sole_platform_digest`` helper is the tell. Every subject
here is a **two-platform index**, because that is the only shape where
"signed each platform manifest" is distinguishable from "signed the one
manifest there was", and where ``-p`` narrowing is distinguishable from
"narrowed to the only child".

**The division of labour it pins** (ADR D2, `adr_mirror_signing.md`): a push
signs the **platform manifests** it writes and never the index, whose digest
is rewritten every time another platform merges in; the index is signed only
by a later ``sign --tags-file`` sweep. Both halves are asserted — a signature
on each child, and the absence of one on the index.

Runs under the ``sigstore`` docker-compose profile: the ``sigstore_stack``
fixture brings the stack up itself and raises (never skips) when it cannot —
see ``tests/fixtures/sigstore_stack.py``.
"""
from __future__ import annotations

import json
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from src.helpers import make_package
from src.registry import (
    fetch_manifest_digest,
    fetch_manifest_from_registry,
    fetch_platform_manifest_digest,
    index_platforms,
    list_referrers,
    referrers_fallback_tag,
)
from src.runner import OcxRunner, PackageInfo
from tests.fixtures.sigstore_stack import SigstoreStack

#: Sigstore bundle v0.3 artifact type — mirrors the Rust constant
#: `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"

#: The cosign key pair `test_sign.py` and `test_push.py` both sign with, and
#: its password. One key across the suite, so a key-mode `push` and a key-mode
#: `sign` exercise one backend.
_KEYS = Path(__file__).parent / "fixtures" / "golden" / "keys"
COSIGN_KEY = _KEYS / "cosign.key"
COSIGN_PUB = _KEYS / "cosign.pub"
KEY_PASSWORD = "ocxtest"

#: The variable the PEM is read into for the `env://` reference. OCX-C-3 (f)
#: names that spelling specifically; `--key <path>` is a different backend and
#: `test_sign.py` / `test_push.py` already cover it.
KEY_VARIABLE = "OCX_SIGNING_KEY"

#: Key mode is honoured across bullets (a), (c), (d) and (e) per OCX-C-3 (f).
#: Bullet (b) is keyless-only — see its docstring.
SIGN_MODES = ("keyless", "key-env")

#: Wire shape is orthogonal to signing mode; OCX-C-3 (g) pins it as "an ocx
#: property, kept" across the whole surface.
SIGNATURE_FORMATS = ("bundle", "simplesigning")

#: Two platforms, neither of them necessarily the host's. Nothing here runs
#: the packaged binaries, so an `arm64`-labelled manifest built on `amd64` is
#: a legitimate index child — the idiom `test_cascade.py` already uses.
AMD64 = "linux/amd64"
ARM64 = "linux/arm64"


# ──────────────────────────────────────────────────────────────────────────────
# Argv and environment, per signing mode
#
# Every helper below takes `mode` and branches on it. The fixtures it may need
# are declared unconditionally by each test — `request.getfixturevalue` would
# hide the `sigstore` stack dependency from the fixture graph, and the stack is
# session-scoped, so a key-mode row costs nothing by naming it.
# ──────────────────────────────────────────────────────────────────────────────


def sign_flags(
    mode: str,
    stack: SigstoreStack,
    identity_token: Path,
    *,
    signature_format: str | None = None,
    platform: str | None = None,
) -> list[str]:
    """The `ocx package sign` flags for `mode`, minus the identifier.

    `platform` is omitted rather than defaulted: an absent `--platform` is the
    contract's "act on whatever the reference resolved to", which is how the
    sweep in (c) reaches the index at all, and passing one alongside a sweep is
    refused outright.
    """
    flags: list[str] = []
    if signature_format is not None:
        flags += ["--signature-format", signature_format]
    if platform is not None:
        flags += ["--platform", platform]
    if mode == "key-env":
        flags += ["--key", f"env://{KEY_VARIABLE}"]
    else:
        flags += [
            "--fulcio-url", stack.fulcio_url,
            "--rekor-url", stack.rekor_url,
            "--identity-token-file", str(identity_token),
        ]
    return flags


def sign_env(mode: str) -> dict[str, str]:
    """The environment `mode` signs under.

    Key mode carries the PEM in the variable the `env://` reference names and
    the passphrase beside it: ocx has no flag for a key password, and one in
    argv is world-readable in /proc. Keyless needs nothing here — its token
    reaches `sign` through `--identity-token-file`.
    """
    if mode == "key-env":
        return {"OCX_KEY_PASSWORD": KEY_PASSWORD, KEY_VARIABLE: COSIGN_KEY.read_text()}
    return {}


def expected_key_backend(mode: str) -> str:
    """The `key_backend` a sign report made under `mode` must name.

    OCX-C-3 (f) asks for the `env://NAME` spelling specifically, and `env` is
    the only value that proves the reference reached the backend: a run that
    resolved the same PEM off disk reports `file`, and a key-mode run that
    silently fell through to keyless reports `keyless`. Asserted in both
    directions so the `mode` axis cannot collapse.
    """
    return "env" if mode == "key-env" else "keyless"


def verify_flags(
    mode: str,
    stack: SigstoreStack,
    signature_format: str,
    *,
    platform: str | None = None,
) -> list[str]:
    """The `ocx package verify` flags for `mode`, minus the identifier.

    `--signature-format` is pinned rather than left to the unpinned scan: it is
    what makes the (g) axis falsifiable on the read side too, so a run that
    wrote the other shape fails here rather than being found by a scan that
    accepts either.

    Key mode names the public half only — `--key` conflicts with the
    certificate matchers, so exactly one half is ever spelled.
    """
    flags = ["--signature-format", signature_format]
    if platform is not None:
        flags += ["--platform", platform]
    if mode == "key-env":
        flags += ["--key", str(COSIGN_PUB)]
    else:
        flags += [
            "--rekor-url", stack.rekor_url,
            "--sigstore-trusted-root", str(stack.trust_root),
            "--certificate-identity", stack.identity,
            "--certificate-oidc-issuer", stack.issuer,
        ]
    return flags


def arm_push_signing(
    ocx: OcxRunner,
    mode: str,
    stack: SigstoreStack,
    identity_token: Path,
    signature_format: str,
) -> list[str]:
    """Put `mode`'s signing material where the push will find it; return its flags.

    Mutating ``ocx.env`` rather than passing ``env_overrides`` is forced by the
    seam: ``make_package`` owns the ``package push`` invocation and exposes only
    ``extra_push_args``, so material that has to be in the child's environment
    goes on the runner. The fixture is function-scoped, so the mutation dies
    with the test.

    Keyless goes through ``[trust.sigstore]``: ``push`` carries no endpoint
    flags until OCX-C-5 lands, so the config tier is the only way to point its
    keyless signing at the local stack (the shape
    ``test_attest.py::test_push_with_sbom_reaches_the_stack_through_trust_sigstore_config``
    already pins). Bullet (b) is the flag-borne half of the same journey.

    ``make_package`` returns a ``PackageInfo``, not the push report, so there is
    no ``key_backend`` to read on this path. The green is the evidence instead:
    ``env://OCX_SIGNING_KEY`` names no file, so a push that resolved it through
    the file backend could not have signed at all.
    """
    flags = ["--sign", "--signature-format", signature_format]
    if mode == "key-env":
        ocx.env["OCX_KEY_PASSWORD"] = KEY_PASSWORD
        ocx.env[KEY_VARIABLE] = COSIGN_KEY.read_text()
        flags += ["--key", f"env://{KEY_VARIABLE}"]
    else:
        (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
            "[trust.sigstore]\n"
            f'fulcio_url = "{stack.fulcio_url}"\n'
            f'rekor_url = "{stack.rekor_url}"\n'
        )
        ocx.env["OCX_IDENTITY_TOKEN"] = identity_token.read_text().strip()
        ocx.env["OCX_NO_PROJECT"] = "1"
    return flags


# ──────────────────────────────────────────────────────────────────────────────
# What "is signed" means on the wire, per format
# ──────────────────────────────────────────────────────────────────────────────


def sidecar_tag(subject_digest: str) -> str:
    """cosign's `sha256-<hex>.sig` sidecar tag for `subject_digest`.

    Derived from ``referrers_fallback_tag`` rather than re-split here, because
    that is how the production writer derives it too
    (``package::tag::sidecar_tag``): one spelling of the dash-separated digest
    half, one suffix on top. Re-splitting the digest locally would put a fourth
    copy of that rule in the suite, free to drift from the three that exist.
    ``tests/fixtures/cosign_matrix.py`` already spells it this way.
    """
    return referrers_fallback_tag(subject_digest) + ".sig"


def manifest_status(registry: str, repo: str, ref: str) -> int:
    """The HTTP status a manifest GET returns — 404 when the ref is absent.

    ``registry.get_manifest`` raises on any non-200 and so cannot tell "absent"
    from "the registry is down", which is exactly the distinction an assertion
    that *nothing was written* rests on.
    """
    request = urllib.request.Request(
        f"http://{registry}/v2/{repo}/manifests/{ref}",
        headers={"Accept": "application/vnd.oci.image.manifest.v1+json"},
        method="GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


def bundle_referrers(registry: str, repo: str, subject_digest: str) -> list[dict]:
    """The Sigstore-bundle referrers of `subject_digest`, via the Referrers API."""
    status, index = list_referrers(registry, repo, subject_digest)
    assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
    return [
        entry for entry in index["manifests"]
        if entry.get("artifactType") == SIGSTORE_BUNDLE_V03
    ]


def assert_signed(
    registry: str, repo: str, subject_digest: str, signature_format: str, what: str
) -> None:
    """`subject_digest` carries a signature, in `signature_format`'s place and no other.

    Both halves, in both directions: a bundle is a referrer and leaves the
    sidecar tag absent, a sidecar is the tag and leaves the referrers empty.
    Asserting only the positive half would pass for an implementation that
    ignored `--signature-format` and always wrote a bundle.
    """
    bundles = bundle_referrers(registry, repo, subject_digest)
    sidecar = manifest_status(registry, repo, sidecar_tag(subject_digest))
    if signature_format == "bundle":
        assert bundles, f"no bundle referrer on {what} ({subject_digest})"
        assert sidecar == 404, (
            f"{what} grew a cosign sidecar under --signature-format bundle"
        )
    else:
        assert sidecar == 200, (
            f"no cosign sidecar tag {sidecar_tag(subject_digest)} on {what}"
        )
        assert not bundles, (
            f"{what} grew a bundle referrer under --signature-format "
            f"simplesigning: {bundles!r}"
        )


def assert_unsigned(registry: str, repo: str, subject_digest: str, what: str) -> None:
    """Nothing signs `subject_digest`, in any of the three places one could.

    The Referrers API is where a bundle lands on a registry that serves it (zot
    does), the ``sha256-<hex>`` fallback index is where it lands on one that
    does not, and ``sha256-<hex>.sig`` is the cosign sidecar. Asserting only the
    first would pass for an implementation that wrote to either of the others.
    """
    status, index = list_referrers(registry, repo, subject_digest)
    assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
    assert index["manifests"] == [], (
        f"{what} ({subject_digest}) carries referrers: {index['manifests']!r}"
    )
    fallback = referrers_fallback_tag(subject_digest)
    assert manifest_status(registry, repo, fallback) == 404, (
        f"{what} has a referrers fallback index at {fallback}"
    )
    assert manifest_status(registry, repo, sidecar_tag(subject_digest)) == 404, (
        f"{what} has a cosign sidecar at {sidecar_tag(subject_digest)}"
    )


def two_platform_children(ocx: OcxRunner, repo: str, tag: str) -> dict[str, str]:
    """The `{platform: child digest}` map of a two-platform index, asserted distinct.

    Load-bearing rather than defensive: every test below distinguishes "acted on
    the named platform" from "acted on the only thing there was" by comparing
    these two values, so a fixture that pushed one platform would make all of
    them pass while measuring nothing.
    """
    manifest = fetch_manifest_from_registry(ocx.registry, repo, tag)
    assert index_platforms(manifest) == {AMD64, ARM64}, (
        f"{repo}:{tag} must be a two-platform index for this to mean anything, "
        f"got {index_platforms(manifest)}"
    )
    children = {
        platform: fetch_platform_manifest_digest(
            ocx.registry, repo, tag, platform=platform
        )
        for platform in (AMD64, ARM64)
    }
    assert children[AMD64] != children[ARM64], (
        f"two platforms must be two manifests, both are {children[AMD64]}"
    )
    return children


# ──────────────────────────────────────────────────────────────────────────────
# a — push --sign: one signature per platform manifest, then verify per platform
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("signature_format", SIGNATURE_FORMATS)
@pytest.mark.parametrize("mode", SIGN_MODES)
def test_push_sign_writes_one_signature_per_platform_then_verify_per_platform(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    mode: str,
    signature_format: str,
) -> None:
    """OCX-C-3 (a), (f), (g): every platform manifest of a two-platform index is
    signed by the push that wrote it, and `verify --platform` accepts each one.

    Two pushes, one repo, one version, two platforms — the shape a real mirror
    publishes and the only one where "one signature per platform manifest" can
    fail. `test_push.py` covers the single-platform push; there, signing the
    index by mistake and signing the sole child are the same digest.

    Three claims, none of which the single-platform case can make:

    * each child carries a signature after both pushes have landed;
    * the first platform's signature is still discoverable on its own child
      digest after the second push rewrote the enclosing index — the whole
      reason ADR D2 signs children rather than the index;
    * the index itself carries none. `push` signs manifests; the sweep in (c)
      signs indexes. An implementation that signed the index inline would leave
      that signature stranded on the next platform merge.
    """
    push_flags = arm_push_signing(ocx, mode, sigstore_stack, identity_token, signature_format)

    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "amd64",
        platform=AMD64, extra_push_args=push_flags,
    )
    # Read before the arm64 push rewrites the index: this digest is the claim
    # that a child signature survives an index it no longer belongs to.
    amd64_after_first_push = fetch_platform_manifest_digest(
        ocx.registry, unique_repo, "1.0.0", platform=AMD64
    )
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "arm64",
        platform=ARM64, extra_push_args=push_flags,
    )

    children = two_platform_children(ocx, unique_repo, "1.0.0")
    assert children[AMD64] == amd64_after_first_push, (
        "the amd64 manifest changed identity across the arm64 push; the "
        "signature the first push wrote would name a digest nothing resolves to"
    )

    for platform, digest in children.items():
        assert_signed(
            ocx.registry, unique_repo, digest, signature_format,
            f"the {platform} manifest",
        )

    index_digest = fetch_manifest_digest(ocx.registry, unique_repo, "1.0.0")
    assert index_digest not in children.values(), (
        "the tag must resolve to an index distinct from its children"
    )
    assert_unsigned(ocx.registry, unique_repo, index_digest, "the index push wrote")

    for platform, digest in children.items():
        result = ocx.run(
            "package", "verify",
            *verify_flags(mode, sigstore_stack, signature_format, platform=platform),
            f"{unique_repo}:1.0.0",
            check=False,
        )
        assert result.returncode == 0, (
            f"verify --platform {platform} failed ({result.returncode})\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
        data = json.loads(result.stdout)["data"]
        assert data["subject_digest"] == digest, (
            f"verify --platform {platform} accepted {data['subject_digest']}, "
            f"not that platform's manifest {digest}"
        )


# ──────────────────────────────────────────────────────────────────────────────
# b — push --sign --fulcio-url / --rekor-url against the local stack
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("signature_format", SIGNATURE_FORMATS)
def test_push_sign_against_local_fulcio_and_rekor_signs_and_verifies(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    signature_format: str,
) -> None:
    """OCX-C-3 (b), (g): `push --sign --fulcio-url --rekor-url` signs at push
    time against the local stack, and the result verifies.

    Keyless only, and not by preference: OCX-C-5 gives `--fulcio-url`
    `conflicts_with = "key"`, so `--sign --key --fulcio-url` is a usage error
    and there is no key-mode row to write. (`--rekor-url` *is* allowed in key
    mode, alongside `--rekor-upload`; that pairing is OCX-C-5's own unit-test
    row, not an acceptance shape.)

    **No `[trust.sigstore]` config is written here**, deliberately — that is the
    whole discriminator. (a) reaches the same stack through the config tier
    because push had no endpoint flags; this row proves the flags carry the
    endpoints on their own. The local dex mints the token below and builtin
    public Fulcio would reject it, so a run that ignored the flags and fell
    through to the builtin default could not reach exit 0.
    """
    ocx.env["OCX_IDENTITY_TOKEN"] = identity_token.read_text().strip()
    ocx.env["OCX_NO_PROJECT"] = "1"

    make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        platform=AMD64,
        extra_push_args=[
            "--sign",
            "--signature-format", signature_format,
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", sigstore_stack.rekor_url,
        ],
    )

    subject = fetch_platform_manifest_digest(
        ocx.registry, unique_repo, "1.0.0", platform=AMD64
    )
    assert_signed(
        ocx.registry, unique_repo, subject, signature_format, "the amd64 manifest"
    )

    result = ocx.run(
        "package", "verify",
        *verify_flags("keyless", sigstore_stack, signature_format, platform=AMD64),
        f"{unique_repo}:1.0.0",
        check=False,
    )
    assert result.returncode == 0, (
        f"verify failed ({result.returncode})\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == subject
    assert data["certificate_identity"] == sigstore_stack.identity, (
        "the certificate must come from the Fulcio the flag named"
    )
    assert data["certificate_oidc_issuer"] == sigstore_stack.issuer


# ──────────────────────────────────────────────────────────────────────────────
# c — sign --tags-file over an index, then verify on the tag
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("signature_format", SIGNATURE_FORMATS)
@pytest.mark.parametrize("mode", SIGN_MODES)
def test_sign_tags_file_over_an_index_then_verify_on_the_tag(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    mode: str,
    signature_format: str,
) -> None:
    """OCX-C-3 (c), (f), (g): a `--tags-file` sweep signs the index each listed
    tag resolves to, and `verify` on the tag afterwards accepts it.

    The other half of ADR D2's division of labour: (a) asserts push leaves the
    index unsigned, and this asserts the sweep is what signs it. Verifying *on
    the tag*, with no `--platform`, is the operator-visible point — that is the
    reference cosign resolves for a multi-platform image, and it works only
    because the subject is the index rather than a child.

    Two versions rather than two cascade aliases of one: aliases share an index
    digest, so a sweep that signed only the first would still produce the right
    subject for the second and prove nothing about having visited it.
    """
    first, second = published_two_versions
    index_digests = {
        pkg.tag: fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
        for pkg in (first, second)
    }
    assert index_digests[first.tag] != index_digests[second.tag], (
        "two versions must be two indices"
    )

    tags_file = tmp_path / "tags.txt"
    tags_file.write_text(f"{first.tag}\n{second.tag}\n")

    result = ocx.run(
        "package", "sign",
        *sign_flags(
            mode, sigstore_stack, identity_token, signature_format=signature_format
        ),
        "--tags-file", str(tags_file),
        first.short,
        check=False,
        env_overrides=sign_env(mode),
    )
    assert result.returncode == 0, (
        f"sweep failed ({result.returncode})\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    rows = {row["tag"]: row for row in json.loads(result.stdout)["data"]["tags"]}
    assert set(rows) == {first.tag, second.tag}, rows

    for tag, index_digest in index_digests.items():
        assert rows[tag]["status"] == "completed", rows[tag]
        assert rows[tag]["report"]["key_backend"] == expected_key_backend(mode), (
            f"{tag} was signed under a backend other than {mode}: {rows[tag]['report']!r}"
        )
        assert rows[tag]["report"]["subject_digest"] == index_digest, (
            f"{tag} must have been signed as the index it resolves to, not as "
            f"anything the other tag resolved to"
        )
        # The row is a claim about the registry; check the registry agrees, so
        # a report assembled without ever signing anything cannot pass.
        assert_signed(
            ocx.registry, first.repo, index_digest, signature_format,
            f"the index {tag} resolves to",
        )

        verify = ocx.run(
            "package", "verify",
            *verify_flags(mode, sigstore_stack, signature_format),
            f"{first.repo}:{tag}",
            check=False,
        )
        assert verify.returncode == 0, (
            f"verify on {tag} failed ({verify.returncode})\n"
            f"stdout: {verify.stdout}\nstderr: {verify.stderr}"
        )
        assert json.loads(verify.stdout)["data"]["subject_digest"] == index_digest, (
            f"verify on {tag} accepted something other than the index"
        )


# ──────────────────────────────────────────────────────────────────────────────
# d — sign -p <platform> <tag> narrows to that platform
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("signature_format", SIGNATURE_FORMATS)
@pytest.mark.parametrize("mode", SIGN_MODES)
def test_sign_dash_p_narrows_to_that_platform(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    mode: str,
    signature_format: str,
) -> None:
    """OCX-C-3 (d), (f), (g): `sign -p <platform> <tag>` signs that child
    manifest, and neither the index nor the other child.

    Against a two-platform index, so "narrowed to the platform I named" is
    distinguishable from "narrowed to the only child there was" — which is what
    `test_sign.py::test_sign_with_platform_narrows_into_the_index` cannot say,
    its subject having one child. The unsigned sibling is the assertion that
    carries it: an implementation that ignored `-p` and signed every child, or
    signed the first, fails here and passes there.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "amd64", platform=AMD64)
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "arm64", platform=ARM64)

    children = two_platform_children(ocx, unique_repo, "1.0.0")
    index_digest = fetch_manifest_digest(ocx.registry, unique_repo, "1.0.0")
    assert index_digest not in children.values()

    result = ocx.run(
        "package", "sign",
        *sign_flags(
            mode, sigstore_stack, identity_token,
            signature_format=signature_format, platform=AMD64,
        ),
        f"{unique_repo}:1.0.0",
        check=False,
        env_overrides=sign_env(mode),
    )
    assert result.returncode == 0, (
        f"sign -p {AMD64} failed ({result.returncode})\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    data = json.loads(result.stdout)["data"]
    assert data["platform"] == AMD64
    assert data["key_backend"] == expected_key_backend(mode), (
        f"signed under a backend other than {mode}: {data!r}"
    )
    assert data["subject_digest"] == children[AMD64], (
        f"-p {AMD64} signed {data['subject_digest']}, which is "
        f"{'the index' if data['subject_digest'] == index_digest else 'the other child'}"
    )

    assert_signed(
        ocx.registry, unique_repo, children[AMD64], signature_format,
        f"the {AMD64} manifest",
    )
    assert_unsigned(
        ocx.registry, unique_repo, children[ARM64], f"the unnamed {ARM64} manifest"
    )
    assert_unsigned(ocx.registry, unique_repo, index_digest, "the index")


# ──────────────────────────────────────────────────────────────────────────────
# e — sign -p on a bare (non-index) manifest yields the documented error
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", SIGN_MODES)
def test_sign_dash_p_on_a_bare_manifest_yields_the_documented_error(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    mode: str,
) -> None:
    """OCX-C-3 (e), (f): `sign -p` against a reference that already resolved to
    a bare manifest is refused with the dedicated slug, not `target_not_found`.

    "This package ships no such platform" and "this reference has no platforms
    to choose from" have different remedies, and `target_not_found` would send
    the operator looking for a build that was never missing.

    Parametrized over `mode` because a refusal that fired only in keyless would
    leave the key-mode operator with the wrong slug, and the two paths reach
    resolution through different option plumbing. Not parametrized over
    `signature_format`, and no `--signature-format` is passed: the refusal fires
    during platform resolution, before any wire shape is chosen, so a format
    spelled here would assert a coordinate the code never reads.
    """
    pkg = published_package
    index_digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    platform_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    assert index_digest != platform_digest, (
        f"{pkg.short} must resolve to an image index for this test to mean "
        f"anything, but the tag and its child share {index_digest}"
    )

    result = ocx.run(
        "package", "sign",
        *sign_flags(
            mode, sigstore_stack, identity_token, platform=pkg.platform
        ),
        f"{pkg.repo}@{platform_digest}",
        check=False,
        env_overrides=sign_env(mode),
    )
    assert result.returncode == 79, (
        f"expected NotFound (79), got {result.returncode}\n{result.stderr}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "target_not_an_index", (
        f"expected the dedicated slug, got {envelope['error']}"
    )
    assert_unsigned(
        ocx.registry, pkg.repo, platform_digest, "the refused bare manifest"
    )
