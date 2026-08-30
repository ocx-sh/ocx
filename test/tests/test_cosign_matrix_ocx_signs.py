# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Group A of the cosign interop matrix: ocx signs, cosign verifies (M-01..M-08).

Eight cells, the full cross-product of the three axes that vary once the
direction is fixed: wire format (Sigstore bundle · simplesigning sidecar), key
model (keyless Fulcio+Rekor · committed key pair) and registry (zot, which
serves the OCI 1.1 Referrers API · registry:2, which does not).

Every cell is **image-level**. `cosign verify <registry>/<repo>@<digest>`
resolves the artifact out of the registry itself, so a green proves discovery
*and* content — the property the pre-existing `verify-blob` suite could not
reach, because it handed cosign the bundle as a file.

All eight cells follow C-002's six steps (`_accept` then `_refuse` below), and
the two halves are deliberately one test: the acceptance in step 3 is evidence
only because step 6 proved a red was reachable on the same artifact, in the
same registry, under the same trust material, and steps 2/4/5 are what stop
that red from being reachable for the wrong reason.

M-03 and M-04 were the exception until the `keyid` fix landed: `ocx package
sign --key` wrote `dsseEnvelope.signatures[0].keyid`, cosign's DSSE verifier
matches candidate signatures on that member, and cosign therefore accepted 0
of 1 signatures on an intact ocx key-mode bundle. They carried the
`divergence` marker and asserted the break. The write side now omits the
member — as `cosign sign` does in both key and keyless mode — so they are
ordinary parity cells again, and no cell in this module is a divergence.

The driver is `tests/fixtures/cosign_matrix.py` (C-001), which owns the
corruption recipes, the single-candidate proof and the pinned refusal strings.
Measured behaviour is recorded in
`.claude/artifacts/analysis_cosign_interop_probes.md`; the contract is
`.claude/artifacts/plan_cosign_wp6_matrix.md`. Do not re-derive either here.
"""

from __future__ import annotations

from pathlib import Path

from src.runner import OcxRunner, PackageInfo
from tests.fixtures import cosign_matrix as matrix
from tests.fixtures.cosign_matrix import Cell
from tests.fixtures.sigstore_stack import SigstoreStack


def _refusal_exit(cell: Cell) -> int:
    """cosign's exit code when it refuses a signature this module corrupted.

    Pinned per shape rather than asserted "non-zero" (C-003), because the code
    is a measurable fact and a bare non-zero cannot tell "cosign rejected the
    signature" from "cosign rejected the flags". Measured on all three shapes
    `_refuse` reaches, against the image `cosign_matrix` pins (C-004).

    The split follows the door, not the key model: a sidecar refusal is a "no
    matching **signatures**" failure and exits 12 on both key models, while a
    DSSE bundle is a "no matching **attestations**" failure and exits 1. The
    driver's own measurements agree — :data:`COSIGN_NO_TLOG_EXIT` is 12 beside a
    "signatures" sentence, :data:`COSIGN_NO_TLOG_EXIT_BUNDLE` is 1 beside an
    "attestations" one — which is why this is a selector and not one constant.
    """
    return 12 if cell.fmt == "simplesigning" else 1


def _accept(
    cell: Cell,
    *,
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    stack: SigstoreStack,
    identity_token: Path,
    ignore_tlog: bool,
) -> tuple[PackageInfo, str, str]:
    """C-002 steps 1-3: publish, sign, prove one candidate, assert cosign accepts.

    Returns ``(pkg, subject_digest, ref)`` so the caller can hand the same three
    to :func:`_refuse` — the negative half has to run against *this* artifact in
    *this* registry, not a freshly published look-alike.
    """
    runner, pkg, subject_digest, _size = matrix.subject_package(ocx, cell, repo, tmp_path)
    matrix.assert_registry_premise(cell, pkg.repo, subject_digest)

    signed = matrix.ocx_sign(runner, cell, pkg, stack=stack, identity_token=identity_token)
    assert signed.returncode == 0, (
        f"ocx could not sign {cell}\nstdout: {signed.stdout}\nstderr: {signed.stderr}"
    )
    matrix.assert_single_candidate(cell, cell.registry, pkg.repo, subject_digest)

    ref = matrix.image_ref(cell, pkg, subject_digest)
    accepted = matrix.cosign_verify(tmp_path, cell, ref, stack=stack, ignore_tlog=ignore_tlog)
    assert accepted.returncode == 0, (
        f"cosign rejected an intact ocx signature for {cell}\n"
        f"stdout: {accepted.stdout}\nstderr: {accepted.stderr}"
    )
    return pkg, subject_digest, ref


def _refuse(
    cell: Cell,
    pkg: PackageInfo,
    subject_digest: str,
    ref: str,
    *,
    tmp_path: Path,
    stack: SigstoreStack,
    ignore_tlog: bool,
) -> None:
    """C-002 steps 4-6: corrupt, prove the corruption landed, assert cosign refuses.

    `corrupt_signature` reads the signature bytes back off the wire on both
    sides of the mutation and asserts they differ itself, so there is no
    caller-side copy of that check here: a mutation that did not land is
    otherwise indistinguishable from one that did, and the refusal below would
    then be about something else entirely.

    The second `assert_single_candidate` is the other half of that: it proves
    the corruption *replaced* the candidate rather than adding one, so cosign
    cannot have refused the mutated artifact while an intact sibling sat beside
    it (or, worse, accepted the sibling in the positive half).

    Both halves of the refusal are exact. The exit code is pinned by
    :func:`_refusal_exit` rather than asserted non-zero (C-003), and the message
    per shape by `cosign_refusal` rather than "not a discovery error" (C-006):
    cosign's transparency-log failure is itself a discovery-flavoured sentence,
    so only the exact measured string discriminates.
    """
    matrix.corrupt_signature(cell, cell.registry, pkg.repo, subject_digest)
    matrix.assert_single_candidate(cell, cell.registry, pkg.repo, subject_digest)

    refused = matrix.cosign_verify(tmp_path, cell, ref, stack=stack, ignore_tlog=ignore_tlog)
    assert refused.returncode == _refusal_exit(cell), (
        f"expected cosign to refuse a corrupted signature for {cell} with exit "
        f"{_refusal_exit(cell)}, got {refused.returncode}\n"
        f"stdout: {refused.stdout}\nstderr: {refused.stderr}"
    )
    expected = matrix.cosign_refusal(cell)
    assert expected in refused.stderr, (
        f"cosign refused {cell} for a different reason than the corrupted signature; "
        f"expected {expected!r}\nstdout: {refused.stdout}\nstderr: {refused.stderr}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Bundle × keyless — the strongest shape ocx produces
# ──────────────────────────────────────────────────────────────────────────────


def test_cosign_verifies_an_ocx_bundle_keyless_over_the_referrers_api(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-01: cosign discovers and verifies an ocx keyless bundle through OCI 1.1.

    The reference is the platform manifest's own digest (C-005). A tag would
    resolve to the package's *index*, where no signature lives, and cosign would
    fail with a discovery error that a bare non-zero assertion would happily
    accept as the negative half.

    `ignore_tlog=False` is measured, not stylistic: an ocx keyless bundle
    carries `dev.sigstore.cosign/bundle`, so cosign clears its full
    transparency-log check offline. Passing `--insecure-ignore-tlog` here would
    hide the strongest property this cell has, and C-003 forbids adding it to
    make a cell pass. The corrupted half then fails at log inclusion before the
    signature check is ever reached — which is why the pinned string names
    inclusion rather than the signature.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="keyless", fmt="bundle")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=False,
    )
    _refuse(cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack, ignore_tlog=False)


def test_cosign_verifies_an_ocx_bundle_keyless_through_the_fallback_tag(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-02: the same bundle, discovered through the `sha256-<hex>` fallback index.

    `legacy_registry` is registry:2, which 404s the Referrers API — asserted by
    `assert_registry_premise` rather than assumed, because a cell that silently
    ran against zot would be a duplicate of M-01 wearing this test's name.

    The corruption recipe differs here for a reason worth naming: registry:2
    sets no `REGISTRY_STORAGE_DELETE_ENABLED`, so the original referrer manifest
    cannot be deleted and the fallback index tag is rewritten in place to name
    the new blob's digest *and* size instead. The single-candidate assertion
    afterwards is what proves the orphaned original is no longer reachable.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="keyless", fmt="bundle")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=False,
    )
    _refuse(cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack, ignore_tlog=False)


# ──────────────────────────────────────────────────────────────────────────────
# Bundle × key — parity cells since the `keyid` fix
# ──────────────────────────────────────────────────────────────────────────────


def _assert_the_tlog_flag_is_load_bearing(
    cell: Cell,
    ref: str,
    *,
    tmp_path: Path,
    stack: SigstoreStack,
) -> None:
    """Why both halves of a bundle × key cell pass `ignore_tlog=True` (C-003).

    A key-mode signature uploads nothing to Rekor, so without the flag cosign
    stops at the transparency-log check and never reaches the signature at all.
    C-003 forbids adding the flag to make a cell pass, so the reason is
    asserted here rather than asserted in prose: were ocx ever to record a
    key-mode log entry, this reds and the flag has to be re-justified.

    It is also the only consumer of the driver's measured *bundle* half of that
    pair — `COSIGN_NO_TLOG_ENTRY_BUNDLE` / `_EXIT_BUNDLE`, where
    `COSIGN_NO_TLOG_ENTRY` / `_EXIT` cover the sidecar half.
    """
    no_tlog = matrix.cosign_verify(tmp_path, cell, ref, stack=stack, ignore_tlog=False)
    assert no_tlog.returncode == matrix.COSIGN_NO_TLOG_EXIT_BUNDLE, (
        f"expected exit {matrix.COSIGN_NO_TLOG_EXIT_BUNDLE} without --insecure-ignore-tlog for "
        f"{cell}, got {no_tlog.returncode}\nstdout: {no_tlog.stdout}\nstderr: {no_tlog.stderr}"
    )
    assert matrix.COSIGN_NO_TLOG_ENTRY_BUNDLE in no_tlog.stderr, (
        f"expected {matrix.COSIGN_NO_TLOG_ENTRY_BUNDLE!r} without --insecure-ignore-tlog for "
        f"{cell}\nstdout: {no_tlog.stdout}\nstderr: {no_tlog.stderr}"
    )


def test_cosign_verifies_an_ocx_bundle_key_over_the_referrers_api(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-03: cosign verifies an ocx key-mode bundle through OCI 1.1.

    A parity cell again. It was inverted — a `divergence` cell asserting that
    cosign could **not** verify this shape — for as long as `ocx package sign
    --key` wrote `dsseEnvelope.signatures[0].keyid`. cosign's DSSE verifier
    matches candidate signatures on that member and `cosign sign` omits it in
    both key and keyless mode, so every ocx key-mode signature was filtered out
    before any cryptography ran: `cosign verify --key
    --insecure-ignore-tlog=true` returned 1, "accepted signatures do not match
    threshold, Found: 0, Expected 1", for an intact signature exactly as for a
    corrupted one. The write side now omits the member, and the key is
    identified where cosign identifies it — `verificationMaterial.publicKey.hint`.

    That history is why the refusal half is an exact string again: while the
    defect stood, the refusal was indistinguishable from acceptance and no
    constant could honestly express it, so `cosign_refusal` raised for this shape
    instead. `cosign_matrix.COSIGN_BUNDLE_KEY_REFUSAL` is the measurement that
    became possible with the fix.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="key", fmt="bundle")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=True,
    )
    _assert_the_tlog_flag_is_load_bearing(cell, ref, tmp_path=tmp_path, stack=sigstore_stack)
    _refuse(
        cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack,
        ignore_tlog=True,
    )


def test_cosign_verifies_an_ocx_bundle_key_through_the_fallback_tag(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-04: the same key-mode bundle, discovered through the `sha256-<hex>` index.

    Not redundant with its referrers-API twin: `legacy_registry` is registry:2,
    whose Referrers API 404s — asserted by `assert_registry_premise`, not
    assumed — so cosign reaching the signature at all is proof it found the
    bundle through the fallback index. That is what separates "the envelope ocx
    writes is now cosign-readable" from "zot serves OCI 1.1 well", and it is
    the same separation that made this cell worth keeping while it was
    inverted alongside M-03.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="key", fmt="bundle")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=True,
    )
    _assert_the_tlog_flag_is_load_bearing(cell, ref, tmp_path=tmp_path, stack=sigstore_stack)
    _refuse(
        cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack,
        ignore_tlog=True,
    )


# ──────────────────────────────────────────────────────────────────────────────
# Simplesigning × keyless — the sidecar ocx writes is stronger than cosign's own
# ──────────────────────────────────────────────────────────────────────────────


def test_cosign_verifies_an_ocx_sidecar_keyless_on_a_referrers_registry(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-05: cosign verifies an ocx keyless `sha256-<hex>.sig` sidecar, tlog and all.

    `ignore_tlog=False` is the whole point of this cell. ocx writes
    `dev.sigstore.cosign/bundle` onto the sidecar's layer, which cosign's own
    `attach signature` cannot (its `--rekor-response` is inert on v3.1.1), so
    cosign clears the transparency-log check offline against an ocx sidecar and
    needs `--insecure-ignore-tlog` against its own. Passing the flag here would
    erase exactly that difference.

    The corruption edits the signature **annotation**, never the payload blob:
    flipping the payload would trip `verify_layer`'s claim check and red as a
    subject mismatch, so the negative half would be proving the wrong thing.
    With the layer digest untouched, both manifests name the same byte-exact
    signed message and only the signature differs — which is why the pinned
    refusal names the offline bundle's signature comparison.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="keyless", fmt="simplesigning")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=False,
    )
    _refuse(cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack, ignore_tlog=False)


def test_cosign_verifies_an_ocx_sidecar_keyless_on_a_legacy_registry(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-06: the same sidecar on a registry with no Referrers API at all.

    The sidecar is a plain tag, so nothing about writing or reading it should
    depend on OCI 1.1 — this cell is what turns "should" into a measurement.
    The premise is asserted (registry:2 404s `/v2/<repo>/referrers/<digest>`)
    rather than assumed, so a cell that silently ran against zot cannot pass
    under this name.

    `ignore_tlog=False` for M-05's reason, and the same annotation-only
    corruption for M-05's reason.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="keyless", fmt="simplesigning")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=False,
    )
    _refuse(cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack, ignore_tlog=False)


# ──────────────────────────────────────────────────────────────────────────────
# Simplesigning × key — the one shape that needs --insecure-ignore-tlog
# ──────────────────────────────────────────────────────────────────────────────


def test_cosign_verifies_an_ocx_sidecar_key_on_a_referrers_registry(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-07: cosign verifies an ocx key-mode sidecar against the committed public key.

    `ignore_tlog=True` here is measured and load-bearing in the opposite
    direction from M-05: a key-mode signature defaults to no Rekor upload, so
    the sidecar carries no `dev.sigstore.cosign/bundle` and `cosign verify`
    without the flag stops at `COSIGN_NO_TLOG_ENTRY` — rc=12, "no matching
    signatures: signature not found in transparency log". That sentence reads
    like a discovery failure for a cause that is not discovery, which is exactly
    why C-006 forbids "assert the message is not a discovery error" as a filter
    and why the refusal below is pinned to an exact measured string instead.

    With no offline bundle to cross-check the annotation against, the flipped
    bytes reach the ECDSA verifier directly, so the pinned refusal names ASN.1
    decoding rather than a bundle mismatch — a different check from M-05's, on
    the same corruption recipe.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="key", fmt="simplesigning")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=True,
    )
    _refuse(cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack, ignore_tlog=True)


def test_cosign_verifies_an_ocx_sidecar_key_on_a_legacy_registry(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """M-08: the key-mode sidecar on the registry with no Referrers API.

    The last cell of Group A, and the one that closes the axis: with the
    Referrers API 404ing (asserted, not assumed), a green here means the
    sidecar-tag door carries a key-mode signature end to end on a plain OCI
    Distribution v2 registry — no OCI 1.1, no Fulcio, no Rekor.

    `ignore_tlog=True` for M-07's reason; the ASN.1 refusal is pinned for
    M-07's reason.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="key", fmt="simplesigning")
    pkg, digest, ref = _accept(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, ignore_tlog=True,
    )
    _refuse(cell, pkg, digest, ref, tmp_path=tmp_path, stack=sigstore_stack, ignore_tlog=True)
