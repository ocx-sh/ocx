# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Group B of the cosign interop matrix: cosign signs, ocx verifies (M-09..M-16).

Every cell is image-level. cosign writes its artifact into a real registry and
`ocx package verify` resolves it back out of that same registry by digest — no
blob command, no bundle handed between the two tools as a file. Three axes
vary: the wire format (Sigstore bundle / `sha256-<hex>.sig` simplesigning
sidecar), the key model (keyless Fulcio + Rekor / key pair), and whether the
registry serves the OCI 1.1 Referrers API (zot does, registry:2 does not).

Each cell carries both verdicts in one function (C-002). The acceptance is
evidence only because the same artifact, in the same registry, under the same
trust material, was then corrupted and refused — and because
:func:`assert_single_candidate` proved exactly one signature was ever
discoverable, before *and* after, so neither verdict can have come from a
different artifact that happened to be lying around.

**All eight cells are parity now, and two of them were not.**
:func:`test_ocx_refuses_a_cosign_keyless_sidecar_exactly_as_cosign_does` and
its legacy-registry twin carried `@pytest.mark.divergence` while ocx *accepted*
a keyless sidecar cosign refuses — an artifact with no transparency-log entry,
whose ten-minute Fulcio certificate nothing timestamped. They were pins on what
ocx did, written to go red the day it was fixed; it was, they did, and they now
assert both tools refusing and each accepting only under its own explicit
opt-out. No cell in this module carries the marker.

The shared driver is `tests/fixtures/cosign_matrix.py` (C-001); every constant
it pins was measured against cosign v3.1.1 and recorded in
`.claude/artifacts/analysis_cosign_interop_probes.md`. Do not re-derive either.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from src import registry as reg
from src.runner import OcxRunner, PackageInfo
from tests.fixtures.adversarial import SIGSTORE_BUNDLE_V03
from tests.fixtures.cosign_matrix import (
    COSIGN_NO_TLOG_ENTRY,
    COSIGN_NO_TLOG_EXIT,
    Cell,
    accepted_signature,
    assert_ocx_refusal,
    assert_registry_premise,
    assert_single_candidate,
    corrupt_signature,
    cosign_attach_simplesigning,
    cosign_sign,
    cosign_verify,
    image_ref,
    ocx_verify,
    subject_package,
)
from tests.fixtures.sigstore_stack import SigstoreStack

#: The three annotations cosign writes on a bundle referrer. Its Referrers-API
#: descriptor carries all three; the child descriptor it writes into the
#: `sha256-<hex>` fallback index carries none — measured, probe P5, and the
#: subject of `_assert_fallback_index_annotation_loss`.
BUNDLE_REFERRER_ANNOTATIONS = (
    "dev.sigstore.bundle.content",
    "dev.sigstore.bundle.predicateType",
    "org.opencontainers.image.created",
)


# ──────────────────────────────────────────────────────────────────────────────
# Cell scaffolding — C-002 steps 1, 2 and 4..6
# ──────────────────────────────────────────────────────────────────────────────


def _signed_subject(
    ocx: OcxRunner,
    cell: Cell,
    repo: str,
    tmp_path: Path,
    *,
    stack: SigstoreStack,
    identity_token: Path,
) -> tuple[OcxRunner, PackageInfo, str]:
    """C-002 steps 1–2: publish a subject, have **cosign** sign it, prove it is alone.

    Returns ``(runner, pkg, subject_digest)``. The runner is pointed at this
    cell's registry; the digest is the `linux/amd64` platform manifest under the
    package's index — the object both tools address (C-005).

    The producer is the cell's format: `cosign sign` always writes a bundle and
    never a sidecar on v3.1.1 (probe P2), so the sidecar half has to go the long
    way round through `generate` -> `sign-blob` -> `attach signature`. Which
    door the bundle lands behind is the registry's choice, not a flag's.
    """
    runner, pkg, subject_digest, _ = subject_package(ocx, cell, repo, tmp_path)
    assert_registry_premise(cell, pkg.repo, subject_digest)

    work = tmp_path / "cosign"
    work.mkdir()
    ref = image_ref(cell, pkg, subject_digest)
    if cell.fmt == "bundle":
        signed = cosign_sign(work, cell, ref, stack=stack, identity_token=identity_token)
        assert signed.returncode == 0, (
            f"cosign sign failed for {cell}\nstdout: {signed.stdout}\nstderr: {signed.stderr}"
        )
    else:
        # Raises on any of the three steps; a half-attached sidecar is not a
        # shape worth asserting against.
        cosign_attach_simplesigning(work, cell, ref, stack=stack, identity_token=identity_token)

    assert_single_candidate(cell, cell.registry, pkg.repo, subject_digest)
    return runner, pkg, subject_digest


def _assert_corruption_is_refused(
    runner: OcxRunner,
    cell: Cell,
    pkg: PackageInfo,
    subject_digest: str,
    *,
    stack: SigstoreStack,
    extra_args: tuple[str, ...] = (),
) -> None:
    """C-002 steps 4–6: flip a byte, prove the flip landed, prove ocx then refuses.

    :func:`corrupt_signature` asserts the flip reached the wire itself — the
    bundle recipes rewrite a blob, a manifest and (on the fallback registry) an
    index child's digest and size, and a rewrite that silently did not land is
    otherwise indistinguishable from one that did, leaving the refusal below to
    be credited to a corruption that never happened.

    The second :func:`assert_single_candidate` is the other half: the corruption
    must have *replaced* the candidate, not added one beside it. And
    :func:`assert_ocx_refusal` pins an exact ``(exit code, error.detail)`` pair,
    never a range and never a bare non-zero — in particular never 79
    (`no_signatures_found`), which would mean the mutation destroyed discovery
    rather than the signature.

    ``extra_args`` exists for the cosign-authored keyless sidecar cells, which
    pass `--allow-unlogged-signature`. Those artifacts carry no transparency-log
    entry, so without the flag they are refused with the *same* pair before the
    corruption is ever reached — and this helper would then be asserting nothing
    about the flipped byte. Lifting the evidence requirement leaves the
    corruption as the only thing left to refuse.
    """
    corrupt_signature(cell, cell.registry, pkg.repo, subject_digest)
    assert_single_candidate(cell, cell.registry, pkg.repo, subject_digest)

    assert_ocx_refusal(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject_digest), stack=stack, extra_args=extra_args),
        cell,
        f"{cell} with a corrupted signature",
    )


def _assert_fallback_index_annotation_loss(cell: Cell, repo: str, subject_digest: str) -> None:
    """cosign's `sha256-<hex>` index child keeps `artifactType` and drops every annotation.

    [sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641), as it
    actually behaves on v3.1.1. `design_spec_cosign_parity.md` reads the issue as
    losing `artifactType` too; probe P5 measured `artifactType` surviving, and
    this assertion is the record of what is true rather than what was expected.

    The absence half is proved non-vacuous in the same breath: the referrer
    manifest the child points at carries all three annotations, so they exist on
    this artifact and their absence from the descriptor is a loss, not a shape
    that was never written. On a Referrers-API registry the descriptor keeps all
    three — which is why a consumer that only reads descriptors sees a different
    artifact depending on which registry served it.
    """
    tag = reg.referrers_fallback_tag(subject_digest)
    raw, _ = reg.fetch_manifest_raw(cell.registry, repo, tag)
    children = json.loads(raw)["manifests"]
    assert len(children) == 1, f"expected exactly 1 child in {tag}, found {len(children)}: {children!r}"
    child = children[0]

    assert child.get("artifactType") == SIGSTORE_BUNDLE_V03, (
        f"cosign's fallback-index child must still carry `artifactType`: {child!r}"
    )

    manifest = reg.get_manifest(cell.registry, repo, child["digest"])
    written = manifest.get("annotations") or {}
    missing = [key for key in BUNDLE_REFERRER_ANNOTATIONS if key not in written]
    assert not missing, (
        f"the referrer manifest itself is missing {missing}, so asserting their absence from the "
        f"index child below would be vacuous: {written!r}"
    )

    carried = child.get("annotations") or {}
    kept = [key for key in BUNDLE_REFERRER_ANNOTATIONS if key in carried]
    assert not kept, (
        f"cosign#4641: the fallback-index child is expected to drop every annotation, but kept "
        f"{kept}: {child!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# M-09..M-12 — cosign writes a bundle, ocx verifies it
# ──────────────────────────────────────────────────────────────────────────────


def test_ocx_verifies_a_cosign_bundle_keyless_over_the_referrers_api(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-09. cosign's keyless bundle, discovered by ocx through the Referrers API.

    The strongest cell in the group: cosign's referrer, cosign's Fulcio leaf and
    cosign's Rekor entry, read out of a live registry by the shipped binary and
    checked against the identity and issuer the local stack mints. `signed_at`
    and `rekor_log_index` are both reported here — the bundle carries the log
    entry, so ocx has a proved signing instant, which is exactly what the two
    sidecar cells below turn out not to have.
    """
    cell = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    entry = accepted_signature(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject), stack=sigstore_stack),
        "a cosign keyless bundle",
    )
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "bundle", "referrers_api", "keyless",
    ), entry
    assert entry.get("certificate_identity") == sigstore_stack.identity, entry
    assert entry.get("certificate_oidc_issuer") == sigstore_stack.issuer, entry
    # The two fields M-13/M-14 prove ocx cannot have. Asserting them present here
    # is what stops that absence from being vacuous: a regression that dropped
    # them for every signature would red this cell rather than quietly making the
    # disclosure cells look correct.
    assert entry.get("signed_at"), f"a bundle carries its log entry, so ocx has a proved instant: {entry!r}"
    assert isinstance(entry.get("rekor_log_index"), int), (
        f"a bundle carries its log entry, so ocx has a log index to report: {entry!r}"
    )

    _assert_corruption_is_refused(runner, cell, pkg, subject, stack=sigstore_stack)


def test_ocx_verifies_a_cosign_bundle_keyless_through_the_fallback_tag(
    ocx: OcxRunner,
    unique_repo: str,
    legacy_registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-10. The same bundle on a registry with no Referrers API — plus cosign#4641.

    registry:2 404s `/v2/<name>/referrers/<digest>`, so cosign falls back to the
    `sha256-<hex>` index tag and ocx has to find it there. This cell also pins
    what that fallback write costs: the index child keeps `artifactType` and
    loses all three annotations — see
    :func:`_assert_fallback_index_annotation_loss`.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="keyless", fmt="bundle")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )
    _assert_fallback_index_annotation_loss(cell, pkg.repo, subject)

    entry = accepted_signature(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject), stack=sigstore_stack),
        "a cosign keyless bundle behind the fallback tag",
    )
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "bundle", "fallback_tag", "keyless",
    ), entry

    _assert_corruption_is_refused(runner, cell, pkg, subject, stack=sigstore_stack)


def test_ocx_verifies_a_cosign_bundle_key_over_the_referrers_api(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-11. cosign's key-mode bundle, discovered through the Referrers API.

    No certificate, so `key_backend` is `file` and there is no identity to pin —
    but there **is** a transparency-log entry: a signing config forces the Rekor
    upload even under `--key`, and this bundle carries the entry to prove it. So
    ocx checks the SET as well as the signature, and needs the local Rekor key to
    do it (see :func:`cosign_matrix.ocx_verify_args`); `signed_at` comes back populated from
    the entry's `integratedTime`.

    The negative half is not here. A corrupted signature on this coordinate is
    refused **65 / `signature_invalid`**, asserted once for all four key-mode
    cells in
    :func:`test_a_corrupted_key_mode_signature_is_refused_as_signature_invalid`.
    """
    cell = Cell(registry=registry, referrers=True, key_mode="key", fmt="bundle")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    entry = accepted_signature(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject), stack=sigstore_stack),
        "a cosign key-mode bundle",
    )
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "bundle", "referrers_api", "file",
    ), entry

    # The negative half lives in
    # `test_a_corrupted_key_mode_signature_is_refused_as_signature_invalid`, which
    # runs this exact coordinate for all four key-mode cells at once. Kept split
    # out now that it passes: one parametrized cell is where a shared refusal
    # contract belongs, and inlining it four times would say the same thing four
    # times while making each cell's positive half harder to read.


def test_ocx_verifies_a_cosign_bundle_key_through_the_fallback_tag(
    ocx: OcxRunner,
    unique_repo: str,
    legacy_registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-12. The key-mode bundle behind the fallback tag — plus cosign#4641 again.

    Asserted on this cell as well as M-10 rather than once: the annotation loss
    is a property of cosign's fallback *write*, so if it were key-model
    dependent, a single cell would be reporting it for both and neither of us
    would know.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="key", fmt="bundle")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )
    _assert_fallback_index_annotation_loss(cell, pkg.repo, subject)

    entry = accepted_signature(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject), stack=sigstore_stack),
        "a cosign key-mode bundle behind the fallback tag",
    )
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "bundle", "fallback_tag", "file",
    ), entry

    # The negative half lives in
    # `test_a_corrupted_key_mode_signature_is_refused_as_signature_invalid`, which
    # runs this exact coordinate for all four key-mode cells at once. Kept split
    # out now that it passes: one parametrized cell is where a shared refusal
    # contract belongs, and inlining it four times would say the same thing four
    # times while making each cell's positive half harder to read.


# ──────────────────────────────────────────────────────────────────────────────
# M-13, M-14 — the two cells that pass because of an open finding
# ──────────────────────────────────────────────────────────────────────────────

#: What both tools now do with a cosign-authored keyless sidecar, and why they
#: agree. Cited by **symbol** rather than by line number: these citations
#: outlive the code they name, and a line number silently drifts off it while
#: still looking precise.
#:
#: `cosign attach signature --rekor-response` validates its argument and never
#: writes the `dev.sigstore.cosign/bundle` annotation on v3.1.1, so a
#: cosign-authored keyless `.sig` carries a certificate and **no**
#: transparency-log entry. A Fulcio leaf lives about ten minutes, so with no
#: entry there is nothing to say the signature happened while the certificate
#: was live — and both tools refuse: cosign with rc 12
#: ("signature not found in transparency log"), ocx with 65 /
#: `signature_invalid` from `simplesigning_read::verify_keyless`.
#:
#: This is where the matrix's largest open finding used to live. ocx **accepted**
#: this artifact: `sidecar_bundle` set the synthesised log entry's
#: `integrated_time` to the leaf certificate's own `notBefore`, `sigstore`'s
#: `Verifier::verify` anchored both its chain build and its certificate-expiry
#: check on that same instant, and `verify_keyless` re-asserted it a third time
#: as `SigningInstant::CallerSupplied`. Every time check on the path was true by
#: construction and none consulted the clock, so a leaf valid for ten minutes a
#: year ago verified for ever. The durable proof of that was
#: `test_verify.py`'s committed sidecar whose certificate expired at
#: 2026-08-29T02:17:58Z; that cell now asserts the refusal, and its sibling
#: asserts `--allow-unlogged-signature` brings the artifact back.
KEYLESS_SIDECAR_NO_TLOG = (
    "a cosign-authored keyless sidecar carries no `dev.sigstore.cosign/bundle`, so nothing proves "
    "when its ten-minute Fulcio certificate was used. cosign refuses it (rc 12, "
    "\"signature not found in transparency log\") and so does ocx (65, `signature_invalid`); both "
    "accept it only under an explicit opt-out -- `--insecure-ignore-tlog` and "
    "`--allow-unlogged-signature` respectively."
)

#: The ocx opt-out, `--insecure-ignore-tlog`'s counterpart in our grammar.
_ALLOW_UNLOGGED = ("--allow-unlogged-signature",)


def _assert_the_keyless_sidecar_parity(
    runner: OcxRunner,
    cell: Cell,
    pkg: PackageInfo,
    subject_digest: str,
    work: Path,
    *,
    stack: SigstoreStack,
) -> None:
    """One artifact, one verdict from each tool, and the same verdict.

    Four assertions, in the order that makes the last two mean something:

    1. `cosign verify --insecure-ignore-tlog=true` **accepts**. cosign can read
       this sidecar and its cryptography checks out, so nothing below is either
       tool failing to find or parse the artifact.
    2. `cosign verify` **without** the flag is refused, exit
       :data:`COSIGN_NO_TLOG_EXIT` with :data:`COSIGN_NO_TLOG_ENTRY`. That
       sentence — "signature not found in transparency log" — reads like a
       discovery error and is not one (C-006's trap); with (1) already green,
       cosign's *only* objection is that it searched Rekor online and found no
       entry for this signature.
    3. `ocx package verify` refuses too, and with the exact pair
       :func:`ocx_refusal` names — never a bare non-zero, and in particular
       never 79, which would mean ocx never found the sidecar and the agreement
       would be an accident.
    4. `ocx package verify --allow-unlogged-signature` **accepts**, and the row
       it accepts carries neither `signed_at` nor `rekor_log_index`. This is
       (1)'s mirror: it proves (3) is a deliberate refusal about the missing
       entry rather than an artifact neither tool can read, and it proves the
       opt-out is reachable. The two absences are the flag's contract — it buys
       acceptance of a signature nothing timestamps, never an invented instant.

    See :data:`KEYLESS_SIDECAR_NO_TLOG` for what each tool is objecting to.
    """
    ref = image_ref(cell, pkg, subject_digest)

    readable = cosign_verify(work, cell, ref, stack=stack, ignore_tlog=True)
    assert readable.returncode == 0, (
        "cosign must be able to read and cryptographically accept this sidecar, or the refusals "
        f"below would just be cosign failing to find it\nstdout: {readable.stdout}\n"
        f"stderr: {readable.stderr}"
    )

    refused = cosign_verify(work, cell, ref, stack=stack, ignore_tlog=False)
    assert refused.returncode == COSIGN_NO_TLOG_EXIT, (
        f"expected cosign to refuse with exit {COSIGN_NO_TLOG_EXIT}, got {refused.returncode}\n"
        f"stdout: {refused.stdout}\nstderr: {refused.stderr}"
    )
    assert COSIGN_NO_TLOG_ENTRY in refused.stderr, (
        f"expected cosign's refusal to be {COSIGN_NO_TLOG_ENTRY!r}, got: {refused.stderr!r}"
    )

    assert_ocx_refusal(
        ocx_verify(runner, cell, ref, stack=stack),
        cell,
        f"a cosign keyless sidecar with no transparency-log entry\n{KEYLESS_SIDECAR_NO_TLOG}",
    )

    lifted = accepted_signature(
        ocx_verify(runner, cell, ref, stack=stack, extra_args=_ALLOW_UNLOGGED),
        "the same sidecar under --allow-unlogged-signature",
    )
    assert (lifted["signature_format"], lifted["discovery_method"], lifted["key_backend"]) == (
        "simplesigning", "sidecar_tag", "keyless",
    ), lifted
    assert "signed_at" not in lifted, (
        f"the opt-out accepts a signature nothing timestamps; it must not report an instant: {lifted!r}"
    )
    assert "rekor_log_index" not in lifted, (
        f"the opt-out accepts a signature no log holds; it must not report a log position: {lifted!r}"
    )


def test_ocx_refuses_a_cosign_keyless_sidecar_exactly_as_cosign_does(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-13. Parity, on the cell that used to be the matrix's largest divergence.

    On one artifact in one registry: **cosign refuses it** (exit 12, "signature
    not found in transparency log" — it searches Rekor online and finds
    nothing) and **ocx refuses it** (65, `signature_invalid`). Each accepts it
    only under its own explicit opt-out. Same bytes, same verdict, and each
    tool's acceptance flag proves its refusal was a decision rather than an
    inability to read the artifact.

    It was `divergence`-marked and green for the opposite reason: ocx exited 0,
    reporting neither `signed_at` nor `rekor_log_index`, its acceptance resting
    on no signing-time evidence at all. See :data:`KEYLESS_SIDECAR_NO_TLOG`.

    **What this cell proves and what it does not.** The artifact is signed here
    and verified seconds later, well inside the leaf's ten-minute Fulcio window,
    so this cell would read the same on an implementation that enforced expiry
    and one that did not — it measures the *tlog requirement*, not the window.
    The claim that a once-valid leaf must not stay acceptable indefinitely is
    carried by `test_verify.py`'s committed sidecar whose certificate expired at
    2026-08-29T02:17:58Z, and by
    `test_cosign_matrix_extras.py::test_a_withheld_bundle_must_not_expose_an_expired_certificate_sidecar`,
    which composes it with a withheld bundle.

    The negative half still runs underneath, so the refusal above is not
    vacuous in the other direction either: corrupt the signature and ocx refuses
    65 / `signature_invalid` — the same pair, reached through the corruption
    rather than through the missing entry, which is why the opt-out assertion is
    what tells the two apart.
    """
    cell = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="simplesigning")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    _assert_the_keyless_sidecar_parity(
        runner, cell, pkg, subject, tmp_path / "cosign", stack=sigstore_stack
    )
    _assert_corruption_is_refused(
        runner, cell, pkg, subject, stack=sigstore_stack, extra_args=_ALLOW_UNLOGGED
    )


def test_ocx_refuses_a_cosign_keyless_sidecar_on_a_legacy_registry_as_cosign_does(
    ocx: OcxRunner,
    unique_repo: str,
    legacy_registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-14. The same parity on registry:2, where the sidecar tag is the only door.

    Identical to M-13 in substance: :data:`KEYLESS_SIDECAR_NO_TLOG`, both tools
    refusing, both accepting only under their own opt-out.

    Run separately rather than folded into M-13 because the sidecar door is the
    *only* door on a registry with no Referrers API, so this is where a
    verification that silently landed on the weaker path is indistinguishable
    from one that chose it. Asserting the premise (`/referrers/` 404s) is part
    of the cell for that reason.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="keyless", fmt="simplesigning")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    _assert_the_keyless_sidecar_parity(
        runner, cell, pkg, subject, tmp_path / "cosign", stack=sigstore_stack
    )
    _assert_corruption_is_refused(
        runner, cell, pkg, subject, stack=sigstore_stack, extra_args=_ALLOW_UNLOGGED
    )


# ──────────────────────────────────────────────────────────────────────────────
# M-15, M-16 — cosign's key-mode sidecar, ocx verifies
# ──────────────────────────────────────────────────────────────────────────────


def test_ocx_verifies_a_cosign_sidecar_key_on_a_referrers_registry(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-15. The sparest artifact in the matrix, and ocx accepts it correctly.

    A key-mode `sha256-<hex>.sig` carries no certificate, no chain and no
    `dev.sigstore.cosign/bundle` — one base64 signature annotation over the
    payload layer is the whole thing. Refusing it would be refusing a legal
    cosign output; accepting it is right, and unlike M-13 the acceptance claims
    nothing it cannot back, because a committed public key is the entire trust
    story and there is no signing instant for it to get wrong.

    The negative half is the discriminator: flip the annotation and ocx refuses
    65 / `signature_invalid`.
    """
    cell = Cell(registry=registry, referrers=True, key_mode="key", fmt="simplesigning")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    entry = accepted_signature(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject), stack=sigstore_stack),
        "a cosign key-mode sidecar",
    )
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "simplesigning", "sidecar_tag", "file",
    ), entry
    # The same two absences M-13/M-14 assert, on the same evidence-free shape —
    # asserted here too so the pair means "no transparency material was found"
    # rather than "this cell happened to check one field and that one happened
    # to check the other".
    assert "signed_at" not in entry, (
        f"a key-mode sidecar carries no transparency material, so no signing instant is proved: {entry!r}"
    )
    assert "rekor_log_index" not in entry, (
        f"a key-mode sidecar carries no transparency material, so there is no log index to report: {entry!r}"
    )

    # The negative half lives in
    # `test_a_corrupted_key_mode_signature_is_refused_as_signature_invalid`, which
    # runs this exact coordinate for all four key-mode cells at once. Kept split
    # out now that it passes: one parametrized cell is where a shared refusal
    # contract belongs, and inlining it four times would say the same thing four
    # times while making each cell's positive half harder to read.


def test_ocx_verifies_a_cosign_sidecar_key_on_a_legacy_registry(
    ocx: OcxRunner,
    unique_repo: str,
    legacy_registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-16. The same key-mode sidecar on registry:2, where the tag is the only door.

    `cosign attach signature` writes a plain tag, so it works against a registry
    with no Referrers API at all (probe P6) — and the premise is asserted rather
    than assumed, because a registry that had grown the API would make this cell
    a duplicate of M-15 without saying so.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="key", fmt="simplesigning")
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    entry = accepted_signature(
        ocx_verify(runner, cell, image_ref(cell, pkg, subject), stack=sigstore_stack),
        "a cosign key-mode sidecar on a legacy registry",
    )
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "simplesigning", "sidecar_tag", "file",
    ), entry
    assert "signed_at" not in entry, (
        f"a key-mode sidecar carries no transparency material, so no signing instant is proved: {entry!r}"
    )
    assert "rekor_log_index" not in entry, (
        f"a key-mode sidecar carries no transparency material, so there is no log index to report: {entry!r}"
    )

    # The negative half lives in
    # `test_a_corrupted_key_mode_signature_is_refused_as_signature_invalid`, which
    # runs this exact coordinate for all four key-mode cells at once. Kept split
    # out now that it passes: one parametrized cell is where a shared refusal
    # contract belongs, and inlining it four times would say the same thing four
    # times while making each cell's positive half harder to read.


# ──────────────────────────────────────────────────────────────────────────────
# The key-mode negative half — split out, because ocx answers the wrong code
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("referrers", "fmt"),
    [(True, "bundle"), (False, "bundle"), (True, "simplesigning"), (False, "simplesigning")],
    ids=["M-11", "M-12", "M-15", "M-16"],
)
def test_a_corrupted_key_mode_signature_is_refused_as_signature_invalid(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    legacy_registry: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
    referrers: bool,
    fmt: str,
) -> None:
    """The negative half of M-11, M-12, M-15 and M-16, asserting the contract.

    A corrupted *signature* refuses as **65 / `signature_invalid`** on all four
    key-mode coordinates. It answered **77 / `identity_mismatch`** until
    `identity::matching_key_policies` was fixed — "certificate identity
    mismatch" on a path that carries no certificate and reads no identity, and
    a permissions code where a caller scripting 65 for "this did not verify"
    would see nothing. Loop D reported it before this branch merged and deferred
    it; this matrix reproduced it independently at acceptance level, on both
    registries and both wire formats, and it was a strict xfail asserting the
    contract until the fix landed.

    The one refusal that is still 77 is the case where nothing about the
    signature was measured: a policy set naming only keyless signers never
    reaches `verify_signature`, so it answers on identity. That arm is pinned in
    `identity.rs`, not here — this cell always names a key.

    Split out rather than inlined into those four cells so one shared refusal
    contract lives in one place: their positive halves — ocx verifying a cosign
    key-mode artifact through each discovery door — stay unmarked beside it,
    and the setup below is identical to theirs, so a break in publishing or
    signing reds there rather than being absorbed here.
    """
    cell = Cell(
        registry=registry if referrers else legacy_registry,
        referrers=referrers,
        key_mode="key",
        fmt=fmt,
    )
    runner, pkg, subject = _signed_subject(
        ocx, cell, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )
    _assert_corruption_is_refused(runner, cell, pkg, subject, stack=sigstore_stack)
