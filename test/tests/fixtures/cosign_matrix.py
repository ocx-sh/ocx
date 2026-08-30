# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The shared driver behind the cosign interop matrix (WP6, C-001).

One module every `test_cosign_matrix_*.py` imports. It owns the four things
that vary per matrix coordinate — registry, Referrers-API presence, key model,
wire format — and the two primitives every cell needs in order for its green to
mean anything: a corruption proven to have landed, and a proof that exactly one
signature was ever discoverable.

Everything here is **image-level**: both tools resolve the artifact out of a
registry themselves, so no blob *verification* command appears anywhere and
discovery is always registry-resolved. (:func:`cosign_attach_simplesigning`
does run `cosign sign-blob` — v3.1.1 offers no other route to a simplesigning
signature — but only to mint bytes it then attaches to the registry; nothing is
ever verified out of a local file.) That is the point of the matrix — the
pre-existing suite proved bundle *content* agreement through `verify-blob`;
this proves *discovery plus content* through the commands a user actually runs.

Measured behaviour this module encodes is recorded in
`.claude/artifacts/analysis_cosign_interop_probes.md`; the contract it
implements is C-001 and C-003..C-008 of
`.claude/artifacts/plan_cosign_wp6_matrix.md`. Do not re-derive either.
"""

from __future__ import annotations

import base64
import json
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import requests
from cryptography import x509
from cryptography.hazmat.primitives import serialization

from src import registry as reg
from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo
from tests.fixtures import cosign, cosign_artifacts
from tests.fixtures.adversarial import signature_referrer, subject_of
from tests.fixtures.sigstore_stack import SigstoreStack

# ──────────────────────────────────────────────────────────────────────────────
# C-004 — version drift guard
# ──────────────────────────────────────────────────────────────────────────────

#: Every string constant below, and every behavioural claim in
#: `analysis_cosign_interop_probes.md`, was measured against this exact image.
#: A bump must red here rather than silently re-test a different tool against
#: strings nobody observed on it.
PINNED_COSIGN_IMAGE = "ghcr.io/sigstore/cosign/cosign:v3.1.1"

# An explicit `raise`, not an `assert`: `python -O` strips asserts, and a
# version guard that evaporates under an interpreter flag is a guard whose
# green is indistinguishable from never having run.
if cosign.COSIGN_IMAGE != PINNED_COSIGN_IMAGE:
    raise RuntimeError(
        f"the cosign interop matrix pins {PINNED_COSIGN_IMAGE}; `cosign.COSIGN_IMAGE` is now "
        f"{cosign.COSIGN_IMAGE}. Re-measure the probes in "
        "`.claude/artifacts/analysis_cosign_interop_probes.md` and the stderr constants in "
        "this module before moving the pin — see plan_cosign_wp6_matrix.md C-004."
    )

# ──────────────────────────────────────────────────────────────────────────────
# Fixed material
# ──────────────────────────────────────────────────────────────────────────────

_KEYS = Path(__file__).parent / "golden" / "keys"

#: The cosign key pair `golden/generate.py` minted, and which `key_bundle.json`
#: and `simplesigning_key_manifest.json` already pin. Regenerating it would
#: invalidate those fixtures — see `golden/keys/README.md`.
COSIGN_KEY = _KEYS / "cosign.key"
COSIGN_PUB = _KEYS / "cosign.pub"

#: Reaches ocx as ``OCX_KEY_PASSWORD`` and cosign as ``COSIGN_PASSWORD``.
KEY_PASSWORD = "ocxtest"

#: C-005: `ocx package sign` signs the platform manifest under the package's
#: index, and every cell signs and verifies this one.
PLATFORM = "linux/amd64"

# ──────────────────────────────────────────────────────────────────────────────
# C-006 — what "refuses" asserts, per consumer
#
# ocx: exit 65 and `error.detail == "signature_invalid"`. Never 79
# (`no_signatures_found`) — that would mean the corruption destroyed discovery
# rather than the signature. Asserted by the cells, not here.
#
# cosign: non-zero, and stderr matching a measured cryptographic refusal. There
# is no single such string: cosign reaches a different check first depending on
# the wire format and on whether the artifact has a transparency-log entry, so
# every string below was OBSERVED against the pinned image on the exact shape it
# is named for. :func:`cosign_refusal` is the selector, so no cell has to choose
# between them by hand.
# ──────────────────────────────────────────────────────────────────────────────

#: Keyless bundle, one DSSE signature byte flipped, verified WITHOUT
#: `--insecure-ignore-tlog`. The flipped signature no longer matches the entry
#: the log signed, so cosign fails at inclusion before it ever checks the
#: signature itself — and calls a DSSE candidate an "attestation".
COSIGN_BUNDLE_KEYLESS_REFUSAL = (
    "no matching attestations: failed to verify log inclusion: "
    "transparency log signature does not match"
)

#: Keyless `sha256-<hex>.sig` sidecar, signature annotation flipped. An OCX
#: keyless sidecar carries `dev.sigstore.cosign/bundle`, so cosign compares the
#: annotation against the signature the offline bundle recorded and stops there.
COSIGN_SIDECAR_KEYLESS_REFUSAL = (
    "no matching signatures: error verifying bundle: "
    "signature in bundle does not match signature being verified"
)

#: Key-mode sidecar, signature annotation flipped, verified WITH
#: `--insecure-ignore-tlog`. No offline bundle to cross-check, so the flipped
#: bytes reach the ECDSA verifier as a malformed ASN.1 signature.
COSIGN_SIDECAR_KEY_REFUSAL = "no matching signatures: invalid signature when validating ASN.1 encoded signature"

#: THE TRAP (C-006). A key-mode artifact carries no Rekor entry, so
#: `cosign verify` without `--insecure-ignore-tlog` fails at the transparency-log
#: search — with a *discovery-flavoured* sentence for a non-discovery cause.
#: "the message is not a discovery error" is therefore not a safe filter, which
#: is why every cell pins the exact string it expects instead.
#:
#: SIDECAR SHAPE ONLY. The plan's error table gives one row for "no Rekor entry,
#: key mode"; measurement splits it in two, because cosign reaches a different
#: check first per format — see :data:`COSIGN_NO_TLOG_ENTRY_BUNDLE`.
COSIGN_NO_TLOG_ENTRY = "no matching signatures: signature not found in transparency log"

#: The exit code that accompanies :data:`COSIGN_NO_TLOG_ENTRY`. Sidecar only.
COSIGN_NO_TLOG_EXIT = 12

#: The same missing Rekor entry, reached through a key-mode **bundle**: cosign
#: counts verified log entries rather than searching for one, so both the
#: sentence and the exit code differ from the sidecar's. Measured, not inferred.
#:
#: CONSUMER: `test_cosign_matrix_extras.py`'s M-03/M-04 rework, which pins the
#: bundle half of "a key signature records a Rekor entry only when asked" the
#: way M-01/M-02 already pin the sidecar half through
#: :data:`COSIGN_NO_TLOG_ENTRY` / :data:`COSIGN_NO_TLOG_EXIT`. Until that lands
#: these two are a measurement no test reads — kept, because re-measuring them
#: costs a container run, but not to be mistaken for an asserted pin.
COSIGN_NO_TLOG_ENTRY_BUNDLE = (
    "no matching attestations: failed to verify log inclusion: "
    "not enough verified log entries from transparency log: 0 < 1"
)

#: The exit code that accompanies :data:`COSIGN_NO_TLOG_ENTRY_BUNDLE`.
COSIGN_NO_TLOG_EXIT_BUNDLE = 1

#: Bundle × key, one DSSE signature byte flipped, verified WITH
#: `--insecure-ignore-tlog` (a key-mode signature uploads nothing to Rekor, so
#: without the flag cosign stops at log inclusion and never reaches the
#: signature). Observed against the pinned image on this shape.
#:
#: This sentence only became usable as a refusal once ocx stopped writing
#: `dsseEnvelope.signatures[0].keyid`. cosign's DSSE verifier matches signatures
#: on keyid and `cosign sign --key` omits the member, so while ocx wrote it the
#: *intact* bundle produced this same sentence — a negative half that also
#: matches a valid signature discriminates nothing (C-003). The intact half of
#: M-03/M-04 now measures rc=0, so this is reachable only from a signature that
#: really is bad.
COSIGN_BUNDLE_KEY_REFUSAL = (
    "no matching attestations: failed to verify signature: could not verify envelope: "
    "accepted signatures do not match threshold, Found: 0, Expected 1"
)


# ──────────────────────────────────────────────────────────────────────────────
# The coordinate
# ──────────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class Cell:
    """One matrix coordinate, and the registry it runs against."""

    #: ``"localhost:5000"`` (zot) or ``"localhost:5001"`` (registry:2).
    registry: str
    #: True for zot, False for registry:2. Not derived from ``registry``: the
    #: cells assert the premise (`list_referrers` really does 404 over there)
    #: and a derived flag would make that assertion circular.
    referrers: bool
    #: ``"keyless"`` | ``"key"``.
    key_mode: str
    #: ``"bundle"`` | ``"simplesigning"``.
    fmt: str

    def __post_init__(self) -> None:
        if self.key_mode not in ("keyless", "key"):
            raise ValueError(f"key_mode is keyless or key, not {self.key_mode!r}")
        if self.fmt not in ("bundle", "simplesigning"):
            raise ValueError(f"fmt is bundle or simplesigning, not {self.fmt!r}")


# ──────────────────────────────────────────────────────────────────────────────
# Subject
# ──────────────────────────────────────────────────────────────────────────────


def subject_package(
    ocx: OcxRunner,
    cell: Cell,
    repo: str,
    tmp_path: Path,
    *,
    tag: str = "1.0.0",
) -> tuple[OcxRunner, PackageInfo, str, int]:
    """Publish a fresh ocx package into ``cell.registry``.

    Returns ``(runner, pkg, subject_digest, subject_size)`` where the runner is
    pointed at this cell's registry (the ``ocx`` fixture's own is not, for the
    fallback half of the matrix) and the digest is the **platform manifest**
    under the package's index — the object both tools address (C-005).

    ``subject_size`` comes from the bytes the registry served, never a
    re-encoding: a referrer's ``subject`` descriptor has to match what the
    registry stored.
    """
    runner = ocx if cell.registry == ocx.registry else OcxRunner(ocx.binary, ocx.ocx_home, cell.registry)
    pkg = make_package(runner, repo, tag, tmp_path, platform=PLATFORM)
    subject_digest, subject_size = subject_of(cell.registry, pkg.repo, pkg.tag, PLATFORM)
    return runner, pkg, subject_digest, subject_size


def image_ref(cell: Cell, pkg: PackageInfo, subject_digest: str) -> str:
    """``{registry}/{repo}@{subject_digest}`` — C-005. NEVER a tag.

    A tag resolves to the package's *index*, where no signature lives, so
    `cosign verify <registry>/<repo>:<tag>` would find nothing and fail with a
    discovery error — which a bare non-zero assertion would happily accept as
    the negative half of a cell.
    """
    return f"{cell.registry}/{pkg.repo}@{subject_digest}"


# ──────────────────────────────────────────────────────────────────────────────
# ocx side
# ──────────────────────────────────────────────────────────────────────────────


def ocx_sign(
    runner: OcxRunner,
    cell: Cell,
    pkg: PackageInfo,
    *,
    stack: SigstoreStack,
    identity_token: Path,
    signature_format: str | None = None,
    extra_args: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    """`ocx package sign` for this cell. Returned unchecked — the cell asserts.

    The key model decides the whole trust-material half: keyless names Fulcio,
    Rekor and the identity token; ``key`` names the committed key pair and
    passes its password through the environment, because ocx has no flag for it
    and a password in argv is world-readable in /proc.

    ``signature_format`` overrides ``cell.fmt`` on the *write* side only, for
    the one caller that needs it: X-01 signs `--signature-format both`, which is
    not a matrix coordinate — the format axis has two values — but is a real
    flag value. The cell keeps naming the single shape it then reasons about, so
    :func:`corrupt_signature` and :func:`cosign_refusal` stay unambiguous.
    """
    args = ["package", "sign", "--platform", PLATFORM, "--signature-format", signature_format or cell.fmt]
    env_overrides: dict[str, str] = {}
    if cell.key_mode == "key":
        args += ["--key", str(COSIGN_KEY)]
        env_overrides["OCX_KEY_PASSWORD"] = KEY_PASSWORD
    else:
        args += [
            "--fulcio-url", stack.fulcio_url,
            "--rekor-url", stack.rekor_url,
            "--identity-token-file", str(identity_token),
        ]
    args += [*extra_args, pkg.short]
    return runner.run(*args, check=False, env_overrides=env_overrides)


def ocx_verify_args(cell: Cell, *, stack: SigstoreStack, pin_format: str | None = None) -> list[str]:
    """The `ocx package verify` flags for this cell, minus the identifier.

    **No `--platform`, deliberately.** The identifier every cell passes is
    :func:`image_ref` — the platform manifest's own digest (C-005) — and
    `--platform` against a reference that already resolves to a single manifest
    is refused: measured exit **79**, `target_not_an_index`, "`--platform
    linux/amd64` was given but the reference resolved to a single manifest, not
    an index". Narrowing is for a tag; a digest reference needs none, and adding
    the flag would make every cell's positive half fail for a reason that has
    nothing to do with interop.

    Keyless and key mode are mutually exclusive at the CLI — `--key` conflicts
    with the certificate flags — so exactly one half is ever spelled.
    ``pin_format`` sets `--signature-format`, which on the verify side pins the
    one shape to accept; leave it ``None`` for the unpinned scan.
    """
    args: list[str] = []
    if cell.key_mode == "key":
        # `--key` conflicts with the certificate matchers, but NOT with the
        # trusted root -- and a key-mode **bundle** still needs it. `cosign sign
        # --key` against a signing config uploads to Rekor regardless, so its
        # bundle carries `tlogEntries`, and ocx checks that SET against whatever
        # root it resolved. Without this flag it reaches for the public-good
        # root and refuses a local-stack entry: measured exit 65,
        # `rekor_set_invalid`, "Rekor SET does not verify". With it, exit 0 and
        # `signed_at` populated -- one flag apart on one artifact.
        #
        # A `sha256-<hex>.sig` carries no transparency material at all, so there
        # is no SET to check and the flag is inert. It is withheld there rather
        # than spelled harmlessly, so M-15/M-16's claim stays literally true: a
        # committed public key is the whole trust story for a key-mode sidecar.
        args += ["--key", str(COSIGN_PUB)]
        if cell.fmt == "bundle":
            args += ["--sigstore-trusted-root", str(stack.trust_root)]
    else:
        args += [
            "--rekor-url", stack.rekor_url,
            "--sigstore-trusted-root", str(stack.trust_root),
            "--certificate-identity", stack.identity,
            "--certificate-oidc-issuer", stack.issuer,
        ]
    if pin_format:
        args += ["--signature-format", pin_format]
    return args


def ocx_verify(
    runner: OcxRunner,
    cell: Cell,
    ref: str,
    *,
    stack: SigstoreStack,
    pin_format: str | None = None,
    extra_args: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    """`ocx package verify <ref>` for this cell. Returned unchecked — the cell asserts.

    Centralised because three test modules had grown three incompatible private
    spellings of it: one taking ``(pkg, subject_digest)`` and rebuilding the
    ref, one taking ``pin_format``, one hard-coding `--attestation --type`. All
    three assemble the same command, and a flag that landed in only two of them
    would silently make the third test a different test.

    ``pin_format=None`` is the unpinned scan — the command a user runs when they
    have not thought about wire formats. ``extra_args`` carries whatever is not
    a matrix axis, `attest`'s ``("--attestation", "--type", <predicate>)`` being
    the only one today; it goes **before** the flags and the identifier, so the
    command reads flags-then-positional throughout.

    Never `check=True`: every caller here asserts an exact exit code, and a
    raising runner would turn a refusal into an error before the pair from
    :func:`ocx_refusal` could be compared.
    """
    return runner.run(
        "package", "verify",
        *extra_args,
        *ocx_verify_args(cell, stack=stack, pin_format=pin_format),
        ref,
        check=False,
    )


# ──────────────────────────────────────────────────────────────────────────────
# cosign side
# ──────────────────────────────────────────────────────────────────────────────

_TRUSTED_ROOT = "trusted_root.json"
_IDENTITY_TOKEN = "identity-token"
_PUBLIC_KEY = "cosign.pub"
_PRIVATE_KEY = "cosign.key"


def _stage_trust(work: Path, cell: Cell, stack: SigstoreStack, identity_token: Path | None) -> str:
    """Put this cell's trust material in the mounted directory; return the config.

    The container sees only relative paths, so everything cosign reads has to
    live under ``work``. The signing config is the key-model split: cosign 3
    removed `--fulcio-url`/`--rekor-url` from the signing commands, and a
    `--key` signer mints no certificate, so naming a CA and an issuer it will
    never call would only invite cosign to try.
    """
    cosign.stage(work, _TRUSTED_ROOT, stack.trusted_root_json.read_bytes())
    if cell.key_mode == "key":
        cosign.stage(work, _PRIVATE_KEY, COSIGN_KEY.read_bytes())
        cosign.stage(work, _PUBLIC_KEY, COSIGN_PUB.read_bytes())
        return cosign.signing_config(work, rekor_url=stack.rekor_url, name="signing-config-key.json")
    if identity_token is None:
        raise ValueError("a keyless cell needs an identity token")
    cosign.stage(work, _IDENTITY_TOKEN, identity_token.read_bytes())
    return cosign.signing_config(
        work,
        rekor_url=stack.rekor_url,
        fulcio_url=stack.fulcio_url,
        oidc_url=stack.issuer,
        name="signing-config-keyless.json",
    )


def cosign_sign(
    work: Path,
    cell: Cell,
    ref: str,
    *,
    stack: SigstoreStack,
    identity_token: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    """`cosign sign <ref>` — always a bundle, never a sidecar (probe P2).

    **Raises if the signing fails**, the same contract as
    :func:`cosign_attach_simplesigning`: the two are interchangeable at the one
    call site that branches on ``cell.fmt``, so a caller that had to remember
    which of them checks its own exit code would eventually forget. An artifact
    that was never signed is not a shape any cell should go on to assert
    against. The returned process is the `sign` step, for a caller that wants to
    look at what cosign printed.

    `--registry-referrers-mode=legacy` does not select the simplesigning writer;
    that writer no longer exists on `cosign sign` in v3.1.1. Which door the
    bundle lands behind is the registry's choice, not a flag's: the Referrers
    API where one exists, the `sha256-<hex>` fallback index where it does not.
    """
    config = _stage_trust(work, cell, stack, identity_token)
    args = ["sign", "--signing-config", config, "--trusted-root", _TRUSTED_ROOT, "--yes"]
    env: dict[str, str] | None = None
    if cell.key_mode == "key":
        args += ["--key", _PRIVATE_KEY]
        env = {"COSIGN_PASSWORD": KEY_PASSWORD}
    else:
        args += ["--identity-token", _IDENTITY_TOKEN]
    signed = cosign.run_registry(work, *args, ref, env=env)
    if signed.returncode != 0:
        raise RuntimeError(f"cosign sign failed for {cell}:\n{signed.stdout}\n{signed.stderr}")
    return signed


def cosign_attach_simplesigning(
    work: Path,
    cell: Cell,
    ref: str,
    *,
    stack: SigstoreStack,
    identity_token: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    """`generate` -> `sign-blob` -> `attach signature`: the ONLY sidecar route (P2).

    **Raises if any step fails**, the same contract as :func:`cosign_sign`,
    because a half-attached sidecar is not a shape any cell should then go on to
    assert against. The returned process is the `attach` step, so a caller that
    wants to look at what cosign printed can.

    v3.1.1's `sign-blob` has no `--output-signature`/`--output-certificate` and
    prints nothing once `--bundle` is given, so both artifacts are lifted out of
    the bundle it wrote — the same route `golden/generate.py` records.
    """
    config = _stage_trust(work, cell, stack, identity_token)

    generated = cosign.run_registry(work, "generate", ref)
    if generated.returncode != 0:
        raise RuntimeError(f"cosign generate failed:\n{generated.stdout}\n{generated.stderr}")
    claim = cosign.stage(work, "claim.json", generated.stdout.encode())

    blob_bundle = "blob-bundle.json"
    sign_blob = [
        "sign-blob", "--signing-config", config,
        "--trusted-root", _TRUSTED_ROOT, "--bundle", blob_bundle, "--yes",
    ]
    env: dict[str, str] | None = None
    if cell.key_mode == "key":
        sign_blob += ["--key", _PRIVATE_KEY]
        env = {"COSIGN_PASSWORD": KEY_PASSWORD}
    else:
        sign_blob += ["--identity-token", _IDENTITY_TOKEN]
    signed = cosign.run(work, *sign_blob, claim, env=env)
    if signed.returncode != 0:
        raise RuntimeError(f"cosign sign-blob failed:\n{signed.stdout}\n{signed.stderr}")

    bundle = json.loads((work / blob_bundle).read_text())
    signature = cosign.stage(work, "signature.b64", bundle["messageSignature"]["signature"].encode())
    attach = ["attach", "signature", "--payload", claim, "--signature", signature]
    certificate = bundle["verificationMaterial"].get("certificate")
    if certificate:
        pem = _pem(base64.b64decode(certificate["rawBytes"]))
        attach += ["--certificate", cosign.stage(work, "certificate.pem", pem.encode())]
    attached = cosign.run_registry(work, *attach, ref)
    if attached.returncode != 0:
        raise RuntimeError(f"cosign attach signature failed:\n{attached.stdout}\n{attached.stderr}")
    return attached


def cosign_verify(
    work: Path,
    cell: Cell,
    ref: str,
    *,
    stack: SigstoreStack,
    ignore_tlog: bool,
) -> subprocess.CompletedProcess[str]:
    """`cosign verify <ref>`. ``ignore_tlog`` is REQUIRED, never defaulted (C-006).

    The plan decides it per cell and it is load-bearing in both directions: a
    keyless ocx artifact carries `dev.sigstore.cosign/bundle` and clears
    cosign's full transparency-log check **without** the flag, so passing it
    there would hide the strongest property that cell has; a key-mode artifact
    carries no Rekor entry at all and fails :data:`COSIGN_NO_TLOG_EXIT` /
    :data:`COSIGN_NO_TLOG_ENTRY` without it. A default would silently pick one.
    """
    cosign.stage(work, _TRUSTED_ROOT, stack.trusted_root_json.read_bytes())
    args = ["verify"]
    if cell.key_mode == "key":
        cosign.stage(work, _PUBLIC_KEY, COSIGN_PUB.read_bytes())
        args += ["--key", _PUBLIC_KEY]
    else:
        args += [
            "--trusted-root", _TRUSTED_ROOT,
            "--certificate-identity", stack.identity,
            "--certificate-oidc-issuer", stack.issuer,
        ]
    args += [f"--insecure-ignore-tlog={'true' if ignore_tlog else 'false'}"]
    return cosign.run_registry(work, *args, ref)


def cosign_refusal(cell: Cell) -> str:
    """The stderr cosign emits for a corrupted signature in ``cell``.

    A selector rather than one constant because cosign reaches a different check
    first per shape, and C-006 forbids the obvious shortcut: its transparency-log
    failure reads "no matching signatures: signature not found in transparency
    log", a discovery-flavoured sentence for a non-discovery cause, so "assert
    the message is not a discovery error" is not a safe filter. Every string
    returned here was observed on the shape it is returned for.

    Bundle × key answers :data:`COSIGN_BUNDLE_KEY_REFUSAL` like any other cell.
    It could not while ocx wrote a `keyid` — the sentence matched the intact
    artifact too — so that shape used to raise out of here instead.
    """
    if cell.fmt == "simplesigning":
        return COSIGN_SIDECAR_KEY_REFUSAL if cell.key_mode == "key" else COSIGN_SIDECAR_KEYLESS_REFUSAL
    if cell.key_mode == "key":
        return COSIGN_BUNDLE_KEY_REFUSAL
    return COSIGN_BUNDLE_KEYLESS_REFUSAL


def ocx_refusal(cell: Cell) -> tuple[int, str]:
    """The ``(exit code, error.detail)`` `ocx package verify` **must** report.

    One pair for both key models — exit 65, `signature_invalid` — because that
    is what C-006 contracts and what a corrupted *signature* means. Never a
    range, and never 79 (`no_signatures_found`), which would mean the corruption
    destroyed discovery rather than the signature: telling those two apart is
    the whole reason this contract exists.

    Under a key this was **77 / `identity_mismatch`** until `matching_key_policies`
    (`oci/verify/identity.rs`) learned to answer `SignatureInvalid` once at least
    one policy key was tried and none verified — literally "certificate identity
    mismatch", on a path that carries no certificate and so has no identity to
    report on. Writing 77 here would have made that wrong branch load-bearing:
    the fix would have arrived as a test failure, and four cells would have stood
    as evidence that a key-mode refusal is *supposed* to speak about identity.

    The keyless arm still answers `IdentityMismatch`, and correctly: a policy set
    naming only keyless signers never reaches `verify_signature`, so nothing about
    the signature was measured and the verdict is about authorization, not
    corruption.
    """
    return (65, "signature_invalid")


def _pem(der: bytes) -> str:
    """The DER leaf Fulcio issued, re-encoded as the PEM `cosign attach` reads.

    Through `cryptography` rather than a hand-rolled base64-and-armour: PEM is a
    wire format, and a hand-written emitter fails silently past the one fixture
    it was written against. Parsing on the way through is the bonus — a bundle
    whose `rawBytes` are not a certificate reds here, where the cause is
    visible, rather than three steps later as a cosign parse error.
    """
    return x509.load_der_x509_certificate(der).public_bytes(serialization.Encoding.PEM).decode()


# ──────────────────────────────────────────────────────────────────────────────
# C-007 — the corruption, per shape, proven to have landed
# ──────────────────────────────────────────────────────────────────────────────


def corrupt_signature(cell: Cell, registry: str, repo: str, subject_digest: str) -> tuple[bytes, bytes]:
    """Flip one byte of the signature this cell's artifact carries; re-push.

    Returns ``(before, after)`` — the raw signature bytes the registry served
    before and after, both read BACK OFF THE WIRE **through the door a verifier
    walks**, and asserted here to differ (C-002 step 4). The assertion lives in
    this function rather than in each cell: a mutation that did not land is
    indistinguishable from one that did, and five call sites each free to forget
    the check is five chances for a refusal to be credited to a corruption that
    never happened.

    The naive "re-push under the same tag" is wrong for the bundle shapes.
    Flipping a byte inside the bundle changes the blob's digest, so the referrer
    manifest's layer descriptor, the referrer manifest digest, and — on the
    fallback registry — the index child's ``digest`` *and* ``size`` all have to
    be rewritten. The recipes below do exactly that, and leave exactly one
    candidate behind (C-008).
    """
    if cell.fmt == "simplesigning":
        before, after = _corrupt_sidecar(registry, repo, subject_digest)
    elif cell.referrers:
        before, after = _corrupt_bundle_over_referrers(registry, repo, subject_digest)
    else:
        before, after = _corrupt_bundle_in_fallback_index(registry, repo, subject_digest)
    if before == after:
        raise AssertionError(
            f"the {cell.fmt} corruption for {cell} did not land: {repo}@{subject_digest} still "
            f"serves the same signature bytes through its "
            f"{'referrers_api' if cell.referrers else 'fallback'} door after the rewrite. "
            "Any refusal a caller now observes would be about something other than the "
            "signature (C-002 step 4)."
        )
    return before, after


def _corrupt_bundle_over_referrers(registry: str, repo: str, subject_digest: str) -> tuple[bytes, bytes]:
    """Rewrite blob and referrer, then DELETE the original so one candidate remains.

    zot serves manifest DELETE, which is what makes "replace" reachable here and
    not on the fallback registry.

    ``after`` is read through a **freshly re-resolved** referrer listing, not at
    the blob digest the rewrite just pushed: blobs are content-addressed, so
    reading back at that digest can only ever confirm the upload, and would
    return the flipped bytes even if the referrer still pointed at the original
    manifest. Re-resolving makes the pair prove *reachability*.
    """
    descriptor = signature_referrer(registry, repo, subject_digest)
    manifest = reg.get_manifest(registry, repo, descriptor["digest"])
    before = _served_bundle_signature(registry, repo, manifest)
    _rewrite_bundle_blob(registry, repo, manifest)

    new_manifest_digest, _ = reg.push_manifest(registry, repo, manifest)
    if new_manifest_digest == descriptor["digest"]:
        raise RuntimeError("the rewritten referrer hashes to the original; the delete below would remove it")
    reg.delete_manifest(registry, repo, descriptor["digest"])

    reachable = signature_referrer(registry, repo, subject_digest)
    after = _served_bundle_signature(registry, repo, reg.get_manifest(registry, repo, reachable["digest"]))
    return before, after


def _corrupt_bundle_in_fallback_index(registry: str, repo: str, subject_digest: str) -> tuple[bytes, bytes]:
    """Rewrite blob and referrer, then repoint the `sha256-<hex>` index at them.

    Index-tag overwrite only: `mirror-registry` (registry:2) sets no
    ``REGISTRY_STORAGE_DELETE_ENABLED``, so manifest DELETE is not available
    there. The original referrer manifest survives in the repo but nothing names
    it any more, and on this registry the index tag is the only door.

    ``after`` is read by walking that door again from the tag down — index tag,
    its one child, that manifest's layer, the blob — rather than at the digest
    the rewrite pushed. An index whose repoint silently did not land still
    serves the *original* child, so this pair reds where the content-addressed
    read could not: it proves the corrupted bundle is what a verifier now finds.
    """
    tag = reg.referrers_fallback_tag(subject_digest)
    index = _one_child_fallback_index(registry, repo, tag)
    children = index["manifests"]

    manifest = reg.get_manifest(registry, repo, children[0]["digest"])
    before = _served_bundle_signature(registry, repo, manifest)
    _rewrite_bundle_blob(registry, repo, manifest)

    new_manifest_digest, _ = reg.push_manifest(registry, repo, manifest)
    children[0]["digest"] = new_manifest_digest
    # `push_manifest` serialises with `json.dumps`, so this is the byte length
    # the registry stored — the value the index child's `size` must carry.
    children[0]["size"] = len(json.dumps(manifest).encode())
    reg.push_manifest(registry, repo, index, reference=tag)

    reachable = _one_child_fallback_index(registry, repo, tag)["manifests"][0]
    after = _served_bundle_signature(registry, repo, reg.get_manifest(registry, repo, reachable["digest"]))
    return before, after


def _corrupt_sidecar(registry: str, repo: str, subject_digest: str) -> tuple[bytes, bytes]:
    """Flip the signature ANNOTATION on the sidecar's layer descriptor.

    Deliberately not the payload blob: that would trip `verify_layer`'s claim
    check and red as a subject mismatch rather than `signature_invalid`, so the
    cell's negative half would be proving the wrong thing. The layer digest is
    untouched, so both manifests name the same committed, byte-exact signed
    message, and the `.sig` tag is simply overwritten.

    Both reads already go through the door — the `.sig` tag is the door — so no
    re-resolution is needed here: an overwrite that did not land leaves the tag
    serving the original annotation and the pair reds by itself.
    """
    tag = reg.referrers_fallback_tag(subject_digest) + ".sig"
    before_b64 = cosign_artifacts.served_sidecar_signature(registry, repo, tag)

    manifest = reg.get_manifest(registry, repo, tag)
    _one_layer(manifest)["annotations"][cosign_artifacts.SIGNATURE_ANNOTATION] = _flip_base64(before_b64)
    reg.push_manifest(registry, repo, manifest, reference=tag)

    after_b64 = cosign_artifacts.served_sidecar_signature(registry, repo, tag)
    return base64.b64decode(before_b64), base64.b64decode(after_b64)


def _one_child_fallback_index(registry: str, repo: str, tag: str) -> dict[str, Any]:
    """The `sha256-<hex>` fallback index at ``tag``, asserted to hold one child.

    Raises rather than picking on an unexpected count, for the same reason
    :func:`signature_referrer` does: corrupting one of several candidates leaves
    an intact sibling the consumer can pass on, which is the exact false green
    :func:`assert_single_candidate` exists to prevent.
    """
    raw, _ = reg.fetch_manifest_raw(registry, repo, tag)
    index = json.loads(raw)
    children = index["manifests"]
    if len(children) != 1:
        raise RuntimeError(f"expected exactly 1 child in {tag}, found {len(children)}: {children!r}")
    return index


def _one_layer(manifest: dict[str, Any]) -> dict[str, Any]:
    """``manifest``'s single layer descriptor, asserted to be the only one.

    Every other selection in this module is cardinality-guarded — one referrer,
    one index child, one DSSE signature — because picking ``[0]`` off a list
    that grew leaves the sibling intact and reachable. A layer list is no
    different: the corruption would land on one of two signed shapes and the
    verifier could still walk to the other.
    """
    layers = manifest.get("layers") or []
    if len(layers) != 1:
        raise RuntimeError(f"expected exactly 1 layer to corrupt, found {len(layers)}: {layers!r}")
    return layers[0]


def _served_bundle_signature(registry: str, repo: str, manifest: dict[str, Any]) -> bytes:
    """The DSSE signature bytes reachable through ``manifest``'s one layer."""
    return base64.b64decode(
        cosign_artifacts.served_bundle_signature(registry, repo, _one_layer(manifest)["digest"])
    )


def _rewrite_bundle_blob(registry: str, repo: str, manifest: dict[str, Any]) -> None:
    """Flip the DSSE signature in ``manifest``'s bundle blob and push the result.

    Mutates ``manifest``'s layer descriptor in place to name the new blob. The
    caller still has to push the manifest — where it goes differs per door — and
    reads the before/after signature bytes through that door itself, because a
    blob read at the digest this function just pushed proves only the upload.
    """
    layer = _one_layer(manifest)
    bundle = json.loads(reg.get_blob(registry, repo, layer["digest"]))
    signatures = bundle["dsseEnvelope"]["signatures"]
    if len(signatures) != 1:
        raise RuntimeError(f"expected exactly 1 DSSE signature to corrupt, found {len(signatures)}")
    signatures[0]["sig"] = _flip_base64(signatures[0]["sig"])

    mutated = json.dumps(bundle, separators=(",", ":")).encode()
    layer["digest"] = reg.push_blob(registry, repo, mutated)
    layer["size"] = len(mutated)


def _flip_base64(b64: str) -> str:
    """Flip the last byte of a base64 signature, and prove the flip is real.

    Decode-flip-encode rather than editing the base64 text: the result is always
    valid base64, and the decoded bytes are guaranteed to differ. Both are
    asserted rather than assumed — an alphabet-level edit can land on a padding
    bit and decode to the same bytes, which would make every cell's negative
    half pass against an intact signature.

    THIRD COPY, AND THE ONLY GUARDED ONE. `cosign_artifacts._flip_last_byte`
    and `adversarial._flip_last_byte` are the same four lines without the
    check — deliberately left alone, because their callers are out of this work
    package's scope and a shared helper landing in three files at once is how a
    consolidation goes wrong. Whoever consolidates: this body is the one to
    keep, and the other two sites need the guard, not the reverse.
    """
    raw = bytearray(base64.b64decode(b64))
    raw[-1] ^= 0xFF
    flipped = base64.b64encode(bytes(raw)).decode()
    if base64.b64decode(flipped) == base64.b64decode(b64):
        raise RuntimeError("the flip did not change the decoded signature bytes")
    return flipped


# ──────────────────────────────────────────────────────────────────────────────
# C-008 — exactly one candidate, across three doors
# ──────────────────────────────────────────────────────────────────────────────


def discoverable_candidates(registry: str, repo: str, subject_digest: str) -> dict[str, list[str]]:
    """Every signature reachable **on ``subject_digest`` itself**, by door.

    Three doors on this one subject, because verification uses three against a
    subject: the OCI 1.1 Referrers API, the `sha256-<hex>` tag-schema fallback
    index, and the `sha256-<hex>.sig` simplesigning sidecar tag. `pipeline.rs`
    fires the third whenever the bundle match set comes back empty, so the
    sidecar is reachable even in a cell that never meant to publish one.

    **There is a fourth, and it is deliberately not here.** `pipeline.rs` opens
    the `sha256-<hex>.att` tag too, but only for an *attestation* run — the
    `.sig` door and the `.att` door are gated on opposite `VerifyContentMode`
    arms, so no single run ever walks both. A cell that publishes an
    attestation asks the question through
    :func:`discoverable_attestation_candidates`, which is this function plus
    that door.

    **A fourth door exists and is out of range here.** `pipeline.rs`'s
    `scan_with_index_fallback` re-runs the whole three-door scan against the
    *enclosing index* once the subject scan fails and that index lists the
    subject — so an index-level signature can decide a child's verdict. This
    function never looks there, and cannot: it is handed a subject digest, not
    the chain that reached it. No cell is falsely green on that account today —
    every cell signs a platform manifest and none signs an index, and each
    addresses the platform digest directly (:func:`image_ref`), which leaves no
    enclosing index to fall through to. A cell that ever signs an index, or
    verifies through a tag, needs its own assertion; this one would not see it.

    The Referrers API listing is deliberately **unfiltered**: a candidate whose
    ``artifactType`` drifted is still a candidate the scan will fetch, and
    filtering it out here would hide exactly the extra artifact this function
    exists to find.
    """
    doors: dict[str, list[str]] = {"referrers_api": [], "fallback_index": [], "sidecar_tag": []}

    status, index = reg.list_referrers(registry, repo, subject_digest)
    if status == 200 and index is not None:
        doors["referrers_api"] = [entry["digest"] for entry in index.get("manifests") or []]
    elif status != 404:
        raise RuntimeError(f"referrers list for {repo}@{subject_digest} answered HTTP {status}")

    tag = reg.referrers_fallback_tag(subject_digest)
    fallback = _manifest_if_present(registry, repo, tag)
    if fallback is not None:
        doors["fallback_index"] = [entry["digest"] for entry in fallback.get("manifests") or []]

    sidecar = _manifest_if_present(registry, repo, f"{tag}.sig")
    if sidecar is not None:
        doors["sidecar_tag"] = [layer["digest"] for layer in sidecar.get("layers") or []]

    return doors


#: The key :func:`discoverable_attestation_candidates` reports the `.att` door
#: under. Named rather than spelled at each site so a cell asserting "behind
#: THIS door" and the function filling it cannot drift into two strings.
ATTESTATION_SIDECAR_DOOR = "attestation_sidecar_tag"


def attestation_sidecar_tag(subject_digest: str) -> str:
    """`sha256-<hex>.att` — the only address a cosign attestation sidecar has.

    Not a referrer under any spelling: the `.att` manifest cosign writes
    carries neither `artifactType` nor `subject`
    (`golden/attestation_sidecar_key_manifest.json` pins both absences), so no
    listing can reach it and the tag is the whole discovery story.
    """
    return reg.referrers_fallback_tag(subject_digest) + ".att"


def discoverable_attestation_candidates(registry: str, repo: str, subject_digest: str) -> dict[str, list[str]]:
    """:func:`discoverable_candidates` plus the `.att` door an attestation run opens.

    A separate function rather than a fourth key on the other one, for two
    reasons that point the same way. It is the *correct* model — a signature
    run and an attestation run open different door sets, gated on opposite
    `VerifyContentMode` arms in `pipeline.rs`, so one dict cannot answer both
    questions without over-claiming for whichever run is asking. And the
    signature form's key set is pinned by an exact-dict comparison in
    `test_cosign_matrix_extras.py`'s `_assert_open_doors`, where a silently
    added key would red every call site for a door that test never publishes.

    The `.sig` door stays in the result even though an attestation run never
    walks it. Keeping it is strictly conservative: a stray simplesigning
    signature on the same subject is not a candidate for this verdict, but it
    *is* evidence that the cell published something it did not mean to, and
    that is exactly what C-008 exists to catch.
    """
    doors = discoverable_candidates(registry, repo, subject_digest)
    sidecar = _manifest_if_present(registry, repo, attestation_sidecar_tag(subject_digest))
    doors[ATTESTATION_SIDECAR_DOOR] = [layer["digest"] for layer in sidecar.get("layers") or []] if sidecar else []
    return doors


def assert_single_attestation_candidate(cell: Cell, registry: str, repo: str, subject_digest: str) -> None:
    """The C-008 total, counted across the doors an ATTESTATION run opens.

    :func:`assert_single_candidate`'s twin, and it exists because that one
    cannot see the `.att` tag: aimed at a cell whose only artifact is an
    attestation sidecar it would report a total of **zero** and fail every
    cell for the absence of a shape those cells never publish.
    """
    doors = discoverable_attestation_candidates(registry, repo, subject_digest)
    total = sum(len(entries) for entries in doors.values())
    assert total == 1, (
        f"{cell} expects exactly one discoverable attestation for {repo}@{subject_digest}, "
        f"found {total}: {doors}"
    )


def corrupt_attestation_sidecar(registry: str, repo: str, subject_digest: str) -> tuple[bytes, bytes]:
    """Flip the DSSE signature INSIDE a `.att` layer; re-push; prove it landed.

    :func:`corrupt_signature`'s recipe for the one shape it does not reach.
    Its `simplesigning` arm rewrites the `dev.cosignproject.cosign/signature`
    layer annotation, which is where a `.sig` claim's *detached* signature
    lives — a `.att` layer is a DSSE envelope carrying its signatures in
    `signatures[].sig` inside the blob, so that arm aimed here would rewrite an
    annotation nothing reads and leave the signature a verifier checks intact:
    the cell would then credit an acceptance to a corruption that never
    happened.

    So the envelope blob itself is rewritten, which moves its digest, which
    means the layer descriptor's `digest` *and* `size` move with it and the
    `.att` tag is overwritten to name them. The payload half of the envelope is
    untouched, so the signed Statement — and the subject digest it binds — is
    byte-identical on both sides and the refusal is about the signature rather
    than about the claim.

    Returns ``(before, after)`` — the decoded signature bytes read back
    **through the tag door** on each side, asserted here to differ for the
    reason :func:`corrupt_signature` states: a rewrite that did not land is
    otherwise indistinguishable from one that did. The `.att` tag is the door,
    so both reads already walk it and no re-resolution is needed.
    """
    tag = attestation_sidecar_tag(subject_digest)
    before = _served_envelope_signature(registry, repo, tag)

    manifest = reg.get_manifest(registry, repo, tag)
    layer = _one_layer(manifest)
    envelope = json.loads(reg.get_blob(registry, repo, layer["digest"]))
    signatures = envelope["signatures"]
    if len(signatures) != 1:
        raise RuntimeError(f"expected exactly 1 DSSE signature to corrupt in {tag}, found {len(signatures)}")
    signatures[0]["sig"] = _flip_base64(signatures[0]["sig"])

    mutated = json.dumps(envelope, separators=(",", ":")).encode()
    layer["digest"] = reg.push_blob(registry, repo, mutated)
    layer["size"] = len(mutated)
    reg.push_manifest(registry, repo, manifest, reference=tag)

    after = _served_envelope_signature(registry, repo, tag)
    if before == after:
        raise AssertionError(
            f"the attestation-sidecar corruption did not land: {repo}@{subject_digest} still serves "
            f"the same DSSE signature through {tag} after the rewrite. Any refusal a caller now "
            "observes would be about something other than the signature (C-002 step 4)."
        )
    return before, after


def _served_envelope_signature(registry: str, repo: str, tag: str) -> bytes:
    """The DSSE signature bytes reachable by walking ``tag`` down to its blob."""
    manifest = reg.get_manifest(registry, repo, tag)
    envelope = json.loads(reg.get_blob(registry, repo, _one_layer(manifest)["digest"]))
    return base64.b64decode(envelope["signatures"][0]["sig"])


def assert_single_candidate(cell: Cell, registry: str, repo: str, subject_digest: str) -> None:
    """Exactly one signature is discoverable across the three doors ON THIS SUBJECT (C-008).

    Without this, a corruption that *empties* the bundle match set can be
    rescued by an untouched sidecar — `pipeline.rs` falls through to the sidecar
    door precisely then — and the cell's negative half would go green while the
    consumer accepted a different signature entirely. Run before and after the
    corruption: the first call proves nothing else was ever reachable, the
    second proves the corruption replaced the candidate rather than adding one.

    Scoped to ``subject_digest``, not to everything a verifier could ever reach:
    the enclosing index's own signatures are a fourth door this cannot see. See
    :func:`discoverable_candidates` for why no cell is falsely green on that.
    """
    doors = discoverable_candidates(registry, repo, subject_digest)
    total = sum(len(entries) for entries in doors.values())
    assert total == 1, (
        f"{cell} expects exactly one discoverable signature for {repo}@{subject_digest}, "
        f"found {total}: {doors}"
    )


def _manifest_if_present(registry: str, repo: str, reference: str) -> dict[str, Any] | None:
    """The manifest at ``reference``, or ``None`` on a genuine **404**.

    NOT ``try: reg.fetch_manifest_raw(...) except RuntimeError``. That helper
    raises the same exception for every non-200, so a 401, a 429 or a 500 would
    read as "this door is empty" — and :func:`assert_single_candidate` would
    then pass with a second live candidate still sitting behind it, which is
    precisely the false green C-008 exists to prevent. Only 404 means absent;
    every other status propagates.

    Both manifest media types are offered, the way `fetch_manifest_raw` does it:
    the fallback index is an image index and the sidecar an image manifest, and
    a registry may 404 a manifest whose media type the request never accepted.
    """
    url = f"http://{registry}/v2/{repo}/manifests/{reference}"
    statuses: list[int] = []
    for media_type in (reg.IMAGE_INDEX_MEDIA_TYPE, reg.IMAGE_MANIFEST_MEDIA_TYPE):
        response = requests.get(url, headers={"Accept": media_type}, timeout=10)
        if response.status_code == 200:
            return response.json()
        statuses.append(response.status_code)
    if any(status != 404 for status in statuses):
        raise RuntimeError(
            f"GET {url} answered HTTP {statuses}, neither 200 nor 404. Treating that as an "
            "empty discovery door would let assert_single_candidate pass with a candidate it "
            "never saw (C-008)."
        )
    return None


# ──────────────────────────────────────────────────────────────────────────────
# What every cell repeats
#
# Each of the four assertions below existed as three near-identical private
# copies across the `test_cosign_matrix_*.py` modules. Three copies of an
# assertion are three chances for one to drift into a weaker form — a tolerated
# exit-code range, a bare non-zero, a premise nobody re-checked — and the drift
# is invisible, because each copy keeps passing on its own. They live here so a
# cell states WHICH property it wants and never HOW to check it.
# ──────────────────────────────────────────────────────────────────────────────


def assert_registry_premise(cell: Cell, repo: str, subject_digest: str) -> None:
    """The registry really is the one ``cell.referrers`` claims (C-008's precondition).

    ``Cell.referrers`` is declared, never derived from the port, and it picks
    the corruption recipe *and* names the door the signature is expected behind.
    So this is a real two-sample check rather than a tautology: the referrers
    half must see HTTP 200 from `/v2/<repo>/referrers/<digest>` and the fallback
    half must see 404. Without it, a cell whose runner silently pointed at the
    wrong registry would still go green — it would just be exercising the other
    door, and every fallback cell would quietly become a duplicate of its
    referrers twin wearing this test's name.

    Run against the freshly published subject, before anything signs it: zot
    answers 200 with an empty list and registry:2 answers 404, so the difference
    measured is the API's presence rather than a referrer's.
    """
    status, _index = reg.list_referrers(cell.registry, repo, subject_digest)
    expected = 200 if cell.referrers else 404
    assert status == expected, (
        f"{cell} expects the Referrers API to answer HTTP {expected} on {cell.registry}, "
        f"got {status}: this cell ran against the wrong registry, so whichever discovery "
        f"door it exercised is not the one it names"
    )


def accepted_signature(result: subprocess.CompletedProcess[str], what: str) -> dict[str, Any]:
    """The one signature row a successful `ocx package verify` reported.

    Asserts rc 0 first, so a refusal reds with ocx's own stderr rather than as a
    JSON KeyError three lines later. Then **destructures** rather than indexes:
    a second row would mean the verdict came from an ANY-of scan over more than
    one candidate, which is exactly what :func:`assert_single_candidate` exists
    to have already ruled out, and `[0]` would hide it.
    """
    assert result.returncode == 0, (
        f"ocx refused {what}\nstdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    )
    [entry] = json.loads(result.stdout)["data"]["signatures"]
    return entry


def assert_ocx_refusal(result: subprocess.CompletedProcess[str], cell: Cell, what: str) -> None:
    """The exact ``(exit code, error.detail)`` pair :func:`ocx_refusal` names for ``cell``.

    Never a tolerated range and never a bare non-zero (C-003): a range cannot
    tell "ocx rejected the signature" from "ocx could not parse the flags", and
    both are non-zero. In particular never 79 (`no_signatures_found`) — 79 would
    mean the corruption destroyed *discovery* rather than the signature, which
    is the confusion this whole contract exists to tell apart.
    """
    code, detail = ocx_refusal(cell)
    assert result.returncode == code, (
        f"expected exit {code} for {what}, got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == detail, (
        f"expected `{detail}` for {what}, got {envelope['error']}"
    )
