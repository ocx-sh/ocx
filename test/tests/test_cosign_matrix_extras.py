# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The extras of the cosign interop matrix (X-01, X-01b, X-02, X-02b, X-03).

Not matrix coordinates. Each one pins a property of `ocx package sign` /
`ocx package verify` that the 16 cells cannot reach, because each needs a
registry state with **two** shapes in it, or two runs one flag apart:

* **X-01** — `--signature-format both` writes a bundle *and* a sidecar, each
  pin verifies its own, and each pin reds when its own shape is corrupted.
* **X-01b** — the divergence that leg uncovered, now closed: a bundle that is
  present, fetched and **cryptographically refused** must fail closed with its
  own exit code rather than let the unpinned scan exit 0 through the sidecar.
* **X-02** — the D9 preference: with only a sidecar, unpinned verify reports
  `simplesigning`; with a bundle beside it, the same command reports `bundle`.
* **X-02b** — the fallback's decided trigger, the one X-01b is not: a bundle
  that is merely *absent*, because a mirror stopped serving its referrer.
* **X-03** — the Rekor-upload default, as a three-sample experiment rather than
  an assertion that two `Option` fields happen to be absent, plus the keyless
  refusal, split so the refusal and its exit code are separately falsifiable.

The shared driver is `tests/fixtures/cosign_matrix.py` (C-001); every constant
it pins was measured against cosign v3.1.1 and recorded in
`.claude/artifacts/analysis_cosign_interop_probes.md`. The contract is
`.claude/artifacts/plan_cosign_wp6_matrix.md`. Do not re-derive either here.

`assert_single_candidate` (C-008) is used by **none** of these tests and is not
imported. It asserts a *total* of one candidate, which is right for a matrix
cell and wrong for every test in this file: these publish a bundle **and** a
sidecar on purpose, and the whole question they ask is which door a verifier
then walks through. :func:`_assert_open_doors` is the stronger form for that
state — it names the exact count behind each of the three doors, so a stray
extra candidate, or a shape a test believed it had published and had not, reds
here rather than silently changing what the verify below is even about.

No test here carries the `divergence` marker any more: X-01b, X-02b and X-04
all pinned open findings and all three are now assertions of the decided
contract. The marker stays registered for the next disclosure — a green count
that cannot separate parity from a pinned defect is what it exists to prevent.
"""

from __future__ import annotations

import datetime
import json
import subprocess
from pathlib import Path

from cryptography import x509

from src import registry as reg
from src.runner import OcxRunner, PackageInfo
from tests.fixtures import cosign_artifacts
from tests.fixtures.adversarial import SIGSTORE_BUNDLE_V03
from tests.fixtures.cosign_matrix import (
    Cell,
    accepted_signature,
    assert_ocx_refusal,
    corrupt_signature,
    cosign_verify,
    discoverable_candidates,
    image_ref,
    ocx_sign,
    ocx_verify,
    subject_package,
)
from tests.fixtures.sigstore_stack import SigstoreStack

#: What `discoverable_candidates` returns for a subject nothing has signed.
_NO_DOORS = {"referrers_api": 0, "fallback_index": 0, "sidecar_tag": 0}

#: Every member `ocx package verify` puts in `data` on a successful run, as
#: `api/data/verification.rs::VerificationReport` declares them.
#:
#: Pinned as an exact set by X-02b, the one cell where an unpinned scan still
#: succeeds on the weaker shape: the report says which shape it landed on and
#: nothing about the bundle it did not find. An equality on the key set makes
#: the day the report grows a member — a `refused`, a "bundle expected" signal —
#: a red rather than a silent change nobody notices. `signatures` is
#: `skip_serializing_if` empty, and X-02b has already asserted one verified.
_REPORT_KEYS = {
    "subject_digest",
    "referrer_digest",
    "certificate_identity",
    "certificate_oidc_issuer",
    "signed_at",
    "signatures",
}

#: `critical.type` in the claim `cosign verify` prints, per carrier — the one
#: field that says WHICH of two shapes cosign read on a `--signature-format
#: both` subject, where both are discoverable and cosign picks by its own order.
#:
#: An ocx bundle is a DSSE in-toto statement whose predicate type is
#: `oci::attest::COSIGN_SIGN_PREDICATE_TYPE`, and cosign echoes that as the
#: claim type; an ocx sidecar carries a real simplesigning claim, whose type is
#: `oci::simplesigning::SIMPLESIGNING_CLAIM_TYPE`. Measured on the pinned
#: cosign image, exactly like every constant in the driver (C-004): a bump
#: re-measures these before it moves the pin.
_COSIGN_BUNDLE_CLAIM_TYPE = "https://sigstore.dev/cosign/sign/v1"
_COSIGN_SIDECAR_CLAIM_TYPE = "cosign container image signature"


# ──────────────────────────────────────────────────────────────────────────────
# Shared assertions
# ──────────────────────────────────────────────────────────────────────────────


def _assert_open_doors(cell: Cell, repo: str, subject_digest: str, expected: dict[str, int], why: str) -> None:
    """Exactly ``expected`` signatures are reachable behind each of the three doors.

    The two-shape form of C-008. `assert_single_candidate` asserts a total of
    one, which is right for a matrix cell and wrong for every test here: these
    publish a bundle *and* a sidecar deliberately, and the whole question they
    ask is which door a verifier then walks through. Asserting the per-door
    counts instead keeps the same protection — a stray extra candidate, or a
    shape a test believed it had published and had not, reds here rather than
    silently changing what the verify below is even about.
    """
    doors = discoverable_candidates(cell.registry, repo, subject_digest)
    counts = {door: len(entries) for door, entries in doors.items()}
    assert counts == expected, f"{why}: expected {expected}, found {doors}"


def _sign(
    runner: OcxRunner,
    cell: Cell,
    pkg: PackageInfo,
    *,
    stack: SigstoreStack,
    identity_token: Path,
    signature_format: str | None = None,
    extra_args: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    """`ocx package sign`, asserted to have succeeded, report parsed by the caller."""
    signed = ocx_sign(
        runner, cell, pkg,
        stack=stack,
        identity_token=identity_token,
        signature_format=signature_format,
        extra_args=extra_args,
    )
    assert signed.returncode == 0, (
        f"ocx could not sign {cell} as {signature_format or cell.fmt}\n"
        f"stdout: {signed.stdout}\nstderr: {signed.stderr}"
    )
    return signed


# ──────────────────────────────────────────────────────────────────────────────
# X-01 — `--signature-format both`
# ──────────────────────────────────────────────────────────────────────────────


def _publish_both(
    ocx: OcxRunner,
    cell: Cell,
    repo: str,
    tmp_path: Path,
    *,
    stack: SigstoreStack,
    identity_token: Path,
) -> tuple[OcxRunner, PackageInfo, str, str]:
    """Publish a subject and sign it once with `--signature-format both`.

    Returns ``(runner, pkg, subject_digest, ref)``. ``cell`` names the shape the
    caller then goes on to reason about — `corrupt_signature` dispatches on
    ``cell.fmt`` — while the *write* is `both`, which is a flag value and not a
    matrix coordinate. The driver's ``signature_format`` override exists for
    exactly this caller.
    """
    runner, pkg, subject_digest, _size = subject_package(ocx, cell, repo, tmp_path)
    _assert_open_doors(cell, pkg.repo, subject_digest, _NO_DOORS, "a freshly published, unsigned subject")

    _sign(runner, cell, pkg, stack=stack, identity_token=identity_token, signature_format="both")
    _assert_open_doors(
        cell, pkg.repo, subject_digest,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "`--signature-format both` on a Referrers-API registry",
    )
    return runner, pkg, subject_digest, image_ref(cell, pkg, subject_digest)


def test_signature_format_both_writes_two_shapes_each_pinned_verify_reaching_its_own(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-01. One `both` signature: cosign verifies it, and each `--signature-format` pin
    reaches its own shape — proved by corrupting one shape at a time.

    Two subjects, because the two negatives are mutually destructive: corrupting
    the sidecar and then the bundle on one subject would leave nothing intact to
    ask the third question of.

    **Subject A — corrupt the sidecar only.** The bundle pin stays green and the
    sidecar pin reds. That is what makes each pin's green mean "this pin read
    *this* shape" rather than "something on this subject verified".

    **Subject B — corrupt the bundle only.** The bundle pin reds, which is the
    mirror image of subject A and completes the claim that each pin reaches its
    own shape.

    **cosign's half names its carrier.** `cosign verify` is asked once, against
    the intact `both` artifact, and a bare exit 0 would not say which of the two
    discoverable shapes it accepted — so the claim cosign prints is destructured
    and its `critical.type` asserted to be :data:`_COSIGN_BUNDLE_CLAIM_TYPE`.
    Measured: cosign reads the **bundle**, not the sidecar
    (:data:`_COSIGN_SIDECAR_CLAIM_TYPE` is what the other answer would look
    like), which makes this leg a statement about bundle interop rather than
    "something on this subject satisfied cosign".

    The *unpinned* scan over a corrupted-bundle subject is deliberately **not**
    asked here: what it does with a refused bundle is a property of the fallback
    rule rather than of `--signature-format both`, and it is pinned in
    :func:`test_a_cryptographically_refused_bundle_fails_closed_instead_of_downgrading`.

    **The sidecar pin is also this file's positive control for transparency-log
    evidence.** ocx writes `dev.sigstore.cosign/bundle` onto the layer, so the
    keyless sidecar here carries a real Rekor entry and must verify *and* report
    it — the counterweight to every cell that asserts an evidence-free keyless
    sidecar is refused.
    """
    bundle = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle")
    sidecar = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="simplesigning")

    # ── Subject A: both shapes intact, then the sidecar alone corrupted ───────
    runner, pkg, subject, ref = _publish_both(
        ocx, bundle, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    accepted = cosign_verify(tmp_path, bundle, ref, stack=sigstore_stack, ignore_tlog=False)
    assert accepted.returncode == 0, (
        f"cosign rejected an intact `--signature-format both` artifact\n"
        f"stdout: {accepted.stdout}\nstderr: {accepted.stderr}"
    )
    # Destructured, not indexed: a second claim would mean cosign accepted both
    # shapes, and `[0]` would pick one of them and call it the answer.
    [claim] = json.loads(accepted.stdout)
    assert claim["critical"]["type"] == _COSIGN_BUNDLE_CLAIM_TYPE, (
        f"cosign accepted the `both` artifact through a shape other than the bundle "
        f"(the sidecar's claim type is {_COSIGN_SIDECAR_CLAIM_TYPE!r}): {claim!r}"
    )

    on_bundle = accepted_signature(
        ocx_verify(runner, bundle, ref, stack=sigstore_stack, pin_format="bundle"),
        "the bundle pin against an intact `both` artifact",
    )
    assert (on_bundle["signature_format"], on_bundle["discovery_method"]) == ("bundle", "referrers_api"), on_bundle

    on_sidecar = accepted_signature(
        ocx_verify(runner, sidecar, ref, stack=sigstore_stack, pin_format="simplesigning"),
        "the simplesigning pin against an intact `both` artifact",
    )
    assert (on_sidecar["signature_format"], on_sidecar["discovery_method"]) == (
        "simplesigning", "sidecar_tag",
    ), on_sidecar
    # The evidence half, and the reason this cell is the positive control for
    # the keyless sidecar's transparency requirement: ocx writes
    # `dev.sigstore.cosign/bundle` onto the layer it signs, so unlike cosign's
    # own `attach signature` output this sidecar carries a real Rekor entry —
    # and verify now checks its SET, binds its body to this signature, and
    # reports what it proved. Both fields were absent before that landed, so a
    # regression to "accept without reading the annotation" reds here rather
    # than passing quietly.
    assert isinstance(on_sidecar.get("rekor_log_index"), int), (
        "an ocx-written keyless sidecar carries a verified Rekor entry, and its log index "
        f"must be reported: {on_sidecar!r}"
    )
    assert on_sidecar.get("signed_at"), (
        "the signing instant comes from that entry's integratedTime, and a keyless sidecar "
        f"that verified must report one: {on_sidecar!r}"
    )
    assert on_bundle["referrer_digest"] != on_sidecar["referrer_digest"], (
        "the two pins reported the same carrier digest, so `both` wrote one shape twice "
        f"rather than two shapes: {on_bundle!r} / {on_sidecar!r}"
    )

    # `corrupt_signature` reads the served signature bytes back on both sides of
    # the mutation and asserts they differ itself, so there is no caller-side
    # copy of that check here or below.
    corrupt_signature(sidecar, registry, pkg.repo, subject)
    _assert_open_doors(
        sidecar, pkg.repo, subject,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "a corrupted sidecar replaces the sidecar candidate rather than adding one",
    )

    still_green = accepted_signature(
        ocx_verify(runner, bundle, ref, stack=sigstore_stack, pin_format="bundle"),
        "the bundle pin after only the sidecar was corrupted",
    )
    assert (still_green["signature_format"], still_green["discovery_method"]) == (
        "bundle", "referrers_api",
    ), still_green
    assert_ocx_refusal(
        ocx_verify(runner, sidecar, ref, stack=sigstore_stack, pin_format="simplesigning"),
        sidecar,
        "the simplesigning pin after the sidecar was corrupted",
    )

    # ── Subject B: both shapes intact, then the bundle alone corrupted ────────
    runner_b, pkg_b, subject_b, ref_b = _publish_both(
        ocx, bundle, f"{unique_repo}_b", tmp_path / "b", stack=sigstore_stack, identity_token=identity_token
    )

    corrupt_signature(bundle, registry, pkg_b.repo, subject_b)
    _assert_open_doors(
        bundle, pkg_b.repo, subject_b,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "a corrupted bundle replaces the bundle candidate rather than adding one",
    )

    assert_ocx_refusal(
        ocx_verify(runner_b, bundle, ref_b, stack=sigstore_stack, pin_format="bundle"),
        bundle,
        "the bundle pin after the bundle was corrupted",
    )


# ──────────────────────────────────────────────────────────────────────────────
# X-01b — a refused bundle must fail closed, never downgrade onto the sidecar
# ──────────────────────────────────────────────────────────────────────────────


def test_a_cryptographically_refused_bundle_fails_closed_instead_of_downgrading(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-01b. A bundle fetched and **cryptographically refused** must fail
    closed with its own exit code — never be walked past onto the sidecar.

    X-02b is the other trigger: a bundle that is merely *absent*, where the
    fallback is the decided behaviour and the scan does land on the sidecar.
    The two triggers were once one branch — `pipeline.rs`'s
    `VerifyPipeline::scan` fired the simplesigning door on `matches.is_empty()`,
    which a *refused* candidate satisfies exactly as a missing one does — and
    unpinned `ocx package verify` exited **0** here, reporting `simplesigning` /
    `sidecar_tag`, with `found.refused` dropped on the floor so the JSON named
    nothing that had failed. A signature that fails verification must never end
    in exit 0 by silently taking a weaker path.

    **Registry-triggerable *and* attacker-triggerable**, which is why absence
    and refusal had to be split. The old trigger set was {withheld, corrupted,
    replaced}: withheld needs no privilege beyond serving the image, corrupted
    is this test's one flipped byte from anyone who can write the repository,
    and replaced is the same writer substituting a bundle signed by a key the
    verifier does not trust. Only the first is an absence; the other two are the
    verifier having looked at a signature and rejected it.

    Three assertions, and the third is what makes the second mean something:

    1. the pinned control refuses with the exact pair `ocx_refusal` names, so
       the refusal is real and exact — never a bare non-zero, and in particular
       never 79, which would mean discovery failed rather than crypto;
    2. the **unpinned** scan refuses with that same pair, over a subject whose
       sidecar is still standing and still discoverable;
    3. that sidecar, asked for by name, still verifies — so (2) is a deliberate
       fail-closed and not a subject on which nothing works any more. Without
       it a broken sidecar would produce the same red and this test could not
       tell the two apart.
    """
    bundle = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle")
    sidecar = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="simplesigning")

    runner, pkg, subject, ref = _publish_both(
        ocx, bundle, unique_repo, tmp_path, stack=sigstore_stack, identity_token=identity_token
    )

    # (3) first, while the bundle is still intact: the sidecar verifies on its
    # own. Asserted before the corruption so it cannot be read as an artefact of
    # it, and it is the control that keeps (2) from passing vacuously.
    accepted_signature(
        ocx_verify(runner, sidecar, ref, stack=sigstore_stack, pin_format="simplesigning"),
        "the simplesigning pin before anything was corrupted",
    )

    # `corrupt_signature` asserts the served bytes changed; without that the
    # refusal below could be about anything.
    corrupt_signature(bundle, registry, pkg.repo, subject)
    _assert_open_doors(
        bundle, pkg.repo, subject,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "a corrupted bundle replaces the bundle candidate rather than adding one, and the "
        "sidecar door is still open — which is the whole point: the scan can reach it and must not",
    )

    # (1) the refusal is real, and exact.
    assert_ocx_refusal(
        ocx_verify(runner, bundle, ref, stack=sigstore_stack, pin_format="bundle"),
        bundle,
        "the bundle pin over the subject whose bundle was corrupted",
    )

    # (2) the same subject, the same registry state, the command a user runs
    # when they have not thought about wire formats. It must answer what the
    # pin answered.
    assert_ocx_refusal(
        ocx_verify(runner, bundle, ref, stack=sigstore_stack),
        bundle,
        "the UNPINNED scan after the bundle was corrupted, with the sidecar still standing",
    )


# ──────────────────────────────────────────────────────────────────────────────
# X-02 — the D9 preference
# ──────────────────────────────────────────────────────────────────────────────


def test_unpinned_verify_prefers_the_bundle_once_one_is_published_beside_the_sidecar(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-02. One subject, one command, two registry states: the sidecar wins when it
    is alone, and loses the moment a bundle exists.

    The pair is the falsifiable form. "Unpinned verify reports `bundle`" on its
    own is satisfied by an implementation that can only ever report `bundle`;
    the preference is demonstrated only when the *same* command reports
    something else for a different registry state. So the sidecar is published
    first and asked, then the bundle is added and the identical command asked
    again — the only thing that changed between the two answers is what the
    registry serves.

    **Key mode, deliberately.** The keyless sidecar path passes through the open
    finding in `simplesigning_read.rs:438` (a Fulcio certificate checked against
    its own `notBefore`, so satisfied by construction); a keyless green here
    would depend on that as well as on the preference, and the two would not be
    separable. Under a key there is no certificate window to be wrong about.
    """
    sidecar = Cell(registry=registry, referrers=True, key_mode="key", fmt="simplesigning")
    bundle = Cell(registry=registry, referrers=True, key_mode="key", fmt="bundle")

    runner, pkg, subject, _size = subject_package(ocx, sidecar, unique_repo, tmp_path)
    ref = image_ref(sidecar, pkg, subject)

    _sign(runner, sidecar, pkg, stack=sigstore_stack, identity_token=identity_token)
    _assert_open_doors(
        sidecar, pkg.repo, subject,
        {"referrers_api": 0, "fallback_index": 0, "sidecar_tag": 1},
        "state 1: the sidecar is the only signature published",
    )

    alone = accepted_signature(
        ocx_verify(runner, sidecar, ref, stack=sigstore_stack),
        "the unpinned scan with only a sidecar published",
    )
    assert (alone["signature_format"], alone["discovery_method"]) == ("simplesigning", "sidecar_tag"), alone

    _sign(runner, bundle, pkg, stack=sigstore_stack, identity_token=identity_token)
    _assert_open_doors(
        bundle, pkg.repo, subject,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "state 2: a bundle now sits beside the untouched sidecar",
    )

    preferred = accepted_signature(
        ocx_verify(runner, bundle, ref, stack=sigstore_stack),
        "the unpinned scan with both shapes published",
    )
    assert (preferred["signature_format"], preferred["discovery_method"]) == ("bundle", "referrers_api"), preferred
    assert preferred["referrer_digest"] != alone["referrer_digest"], (
        "the second answer names the same carrier as the first, so the state change this test "
        f"rests on did not happen: {alone!r} / {preferred!r}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# X-02b — the downgrade, disclosed as an open finding
# ──────────────────────────────────────────────────────────────────────────────


def test_an_unreachable_bundle_downgrades_the_unpinned_scan_onto_the_sidecar(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-02b. The fallback's one decided trigger: a bundle that is **absent**.

    `pipeline.rs`'s `VerifyPipeline::scan` fires the sidecar door when the bundle
    shape produced neither a match nor a refusal, and absence is whatever the
    registry served. Nothing distinguishes "this subject has no bundle" from
    "this registry did not hand me the one it has", so a mirror that 404s the
    bundle referrer downgrades verification onto the weaker path and the
    operator sees exit 0. That is the **owner's decision**, not an open finding:
    the fallback is automatic on absence, and is deliberately not additionally
    gated behind an explicit `--signature-format` pin.

    Its sibling is where the line was drawn. A bundle fetched and
    *cryptographically refused* is not an absence, and
    :func:`test_a_cryptographically_refused_bundle_fails_closed_instead_of_downgrading`
    asserts it fails closed with its own exit code. This cell exists to keep the
    two apart: it must stay green as that one goes red on a refusal, or the fix
    has swallowed the legitimate trigger along with the illegitimate one.

    The setup is a valid bundle **and** a valid sidecar, both verifying, then
    the bundle removed with the sidecar untouched. Asserted below: unpinned
    `ocx package verify` **exits 0 reporting `simplesigning` / `sidecar_tag`**,
    and the pinned control `--signature-format bundle` refuses **79 /
    `no_signatures_found`** — the pair is what makes the unpinned green a
    downgrade rather than a disagreement about whether the bundle is gone.

    **Key mode throughout**, and not incidentally: a keyless sidecar carrying no
    transparency-log evidence is refused (see
    :func:`test_a_withheld_bundle_must_not_expose_an_expired_certificate_sidecar`),
    so the exit 0 this cell is about is only reachable under a key.
    """
    bundle = Cell(registry=registry, referrers=True, key_mode="key", fmt="bundle")
    sidecar = Cell(registry=registry, referrers=True, key_mode="key", fmt="simplesigning")

    runner, pkg, subject, _size = subject_package(ocx, bundle, unique_repo, tmp_path)
    ref = image_ref(bundle, pkg, subject)
    _sign(runner, bundle, pkg, stack=sigstore_stack, identity_token=identity_token, signature_format="both")
    _assert_open_doors(
        bundle, pkg.repo, subject,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "both shapes published and reachable",
    )

    # Both halves verify on their own before anything is removed. Without this,
    # the downgrade below could be read as "the bundle never worked".
    intact = accepted_signature(
        ocx_verify(runner, bundle, ref, stack=sigstore_stack),
        "the unpinned scan while the bundle is still reachable",
    )
    assert (intact["signature_format"], intact["discovery_method"]) == ("bundle", "referrers_api"), intact
    accepted_signature(
        ocx_verify(runner, sidecar, ref, stack=sigstore_stack, pin_format="simplesigning"),
        "the simplesigning pin while the bundle is still reachable",
    )

    # The registry-side event: the bundle referrer stops being served. Its blob
    # and its manifest content are untouched — this is a mirror that 404s, not a
    # corruption, which is exactly what makes it something an operator cannot
    # detect from the verify report.
    status, index = reg.list_referrers(registry, pkg.repo, subject, artifact_type=SIGSTORE_BUNDLE_V03)
    assert status == 200 and index is not None, f"referrers list failed (HTTP {status}) for {pkg.repo}@{subject}"
    [referrer] = index.get("manifests") or []
    reg.delete_manifest(registry, pkg.repo, referrer["digest"])
    _assert_open_doors(
        bundle, pkg.repo, subject,
        {"referrers_api": 0, "fallback_index": 0, "sidecar_tag": 1},
        "the bundle referrer is gone and the sidecar is the only door left",
    )

    downgraded_result = ocx_verify(runner, bundle, ref, stack=sigstore_stack)
    downgraded = accepted_signature(
        downgraded_result,
        "the UNPINNED scan after the bundle was made unreachable (see this test's docstring)",
    )
    assert (downgraded["signature_format"], downgraded["discovery_method"]) == (
        "simplesigning", "sidecar_tag",
    ), (
        "unpinned verify no longer downgrades onto the sidecar when the bundle stops being "
        f"served. If that is a deliberate change, this whole test is the record to update: {downgraded!r}"
    )
    assert set(json.loads(downgraded_result.stdout)["data"]) == _REPORT_KEYS, (
        "the successful `data` object gained or lost a member. If a `refused` or "
        "\"bundle expected\" member arrived, update _REPORT_KEYS and assert what it now says "
        f"about the shape this scan did not find.\n{downgraded_result.stdout}"
    )

    pinned = ocx_verify(runner, bundle, ref, stack=sigstore_stack, pin_format="bundle")
    assert pinned.returncode == 79, (
        "the pinned control must refuse: a bundle pin over a subject whose bundle the registry "
        f"no longer serves has nothing to verify\nstdout: {pinned.stdout}\nstderr: {pinned.stderr.strip()}"
    )
    assert json.loads(pinned.stdout)["error"]["detail"] == "no_signatures_found", pinned.stdout


# ──────────────────────────────────────────────────────────────────────────────
# X-03 — the Rekor-upload default, as a three-sample experiment
# ──────────────────────────────────────────────────────────────────────────────


def test_a_key_signature_records_a_rekor_entry_only_when_asked(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-03. `--rekor-upload` / `--no-rekor-upload` / neither, on one shape, three
    samples that differ only in that flag.

    Asserting only that `signed_at` and `rekor_log_index` are absent would be
    vacuous: both are `Option`, both are `skip_serializing_if = "Option::is_none"`,
    so a regression that dropped them for **every** signature would pass such a
    test unchanged. The experiment is the contrast — the same command on the
    same package shape, differing in one flag, producing opposite answers:

    * `--key --rekor-upload` → the sign report carries `transparency_log_index`,
      and `ocx package verify` reports both `signed_at` and `rekor_log_index`.
    * `--key --no-rekor-upload` → `transparency_log_index` is `null` and neither
      verify field is emitted at all.
    * `--key` and **neither flag** → `transparency_log_index` is `null` too. This
      is the sample that earns the "only when asked" in this test's name: the
      first two prove the flags are *honoured*, and only the third proves the
      default is off. Without it the name would claim something asserted
      nowhere.

    Any sample alone proves nothing about the default. Together they prove the
    fields track the flag rather than the code path's mood.

    The keyless half of X-03 is not a fourth sample but a refusal, and it lives
    in :func:`test_a_keyless_signature_cannot_opt_out_of_the_transparency_log`
    and its exit-code sibling below it.
    """
    key = Cell(registry=registry, referrers=True, key_mode="key", fmt="bundle")

    # ── Sample 1: --rekor-upload ─────────────────────────────────────────────
    runner, pkg, subject, _size = subject_package(ocx, key, unique_repo, tmp_path)
    ref = image_ref(key, pkg, subject)
    # `--rekor-url` because key mode names no Sigstore endpoint of its own: the
    # upload has to reach the local Rekor rather than the public-good default.
    signed = _sign(
        runner, key, pkg,
        stack=sigstore_stack,
        identity_token=identity_token,
        extra_args=("--rekor-upload", "--rekor-url", sigstore_stack.rekor_url),
    )
    uploaded_index = json.loads(signed.stdout)["data"]["transparency_log_index"]
    assert isinstance(uploaded_index, int), (
        f"`--rekor-upload` must report the log index it created, got {uploaded_index!r}"
    )

    with_entry = accepted_signature(
        ocx_verify(runner, key, ref, stack=sigstore_stack),
        "a key-mode signature signed with --rekor-upload",
    )
    assert with_entry.get("signed_at"), (
        f"a Rekor entry gives ocx a proved signing instant to report: {with_entry!r}"
    )
    assert with_entry.get("rekor_log_index") == uploaded_index, (
        f"verify must report the very log index sign created ({uploaded_index}): {with_entry!r}"
    )

    # ── Sample 2: --no-rekor-upload, same shape, one flag apart ──────────────
    runner_b, pkg_b, subject_b, _size_b = subject_package(ocx, key, f"{unique_repo}_b", tmp_path / "b")
    ref_b = image_ref(key, pkg_b, subject_b)
    signed_b = _sign(
        runner_b, key, pkg_b,
        stack=sigstore_stack,
        identity_token=identity_token,
        extra_args=("--no-rekor-upload", "--rekor-url", sigstore_stack.rekor_url),
    )
    assert json.loads(signed_b.stdout)["data"]["transparency_log_index"] is None, (
        f"`--no-rekor-upload` must report no log index at all: {signed_b.stdout}"
    )

    without_entry = accepted_signature(
        ocx_verify(runner_b, key, ref_b, stack=sigstore_stack),
        "a key-mode signature signed with --no-rekor-upload",
    )
    assert "signed_at" not in without_entry, (
        f"no Rekor entry means no proved instant, so the field must be absent: {without_entry!r}"
    )
    assert "rekor_log_index" not in without_entry, (
        f"no Rekor entry means no log index, so the field must be absent: {without_entry!r}"
    )

    # ── Sample 3: neither flag — the default, which is what "only when asked"
    #    actually claims. No `--rekor-url` either: naming an endpoint would be a
    #    second difference from sample 2, and the default reaches for none.
    runner_c, pkg_c, _subject_c, _size_c = subject_package(ocx, key, f"{unique_repo}_c", tmp_path / "c")
    signed_c = _sign(runner_c, key, pkg_c, stack=sigstore_stack, identity_token=identity_token)
    assert json.loads(signed_c.stdout)["data"]["transparency_log_index"] is None, (
        "with neither flag, `ocx package sign --key` must not upload: a default that quietly "
        f"created a log entry is what this sample exists to catch\n{signed_c.stdout}"
    )


def test_a_keyless_signature_cannot_opt_out_of_the_transparency_log(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-03, keyless half: `--no-rekor-upload` is refused, and nothing is written.

    Keyless identity *is* the Rekor entry — a Fulcio certificate outlives its own
    signature by minutes, so a keyless signature with nothing in the transparency
    log has no provable signing instant and is unverifiable by construction.
    Refusing rather than writing an unverifiable artifact is the contract.

    Two assertions, kept apart from the exit-code one its sibling below carries.
    The split outlived the strict xfail that forced it — the refusal and the code
    it exits with are independent claims, and a marker or a `KeyError` on one must
    never absorb the other: if the refusal vanished entirely and keyless
    `--no-rekor-upload` started signing, `json.loads(...)["error"]` would raise
    here, where it names the right cause.

    `rekor_upload_required_for_keyless` is asserted by name and deliberately not
    clap's "required argument --key was not provided", which would invert the
    reason for the refusal.
    """
    keyless = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle")
    runner, pkg, subject, _size = subject_package(ocx, keyless, unique_repo, tmp_path)
    refused = ocx_sign(
        runner, keyless, pkg,
        stack=sigstore_stack,
        identity_token=identity_token,
        extra_args=("--no-rekor-upload",),
    )
    assert json.loads(refused.stdout)["error"]["detail"] == "rekor_upload_required_for_keyless", refused.stdout
    _assert_open_doors(
        keyless, pkg.repo, subject, _NO_DOORS,
        "a refused sign writes nothing to the registry",
    )


def test_the_keyless_no_rekor_upload_refusal_carries_its_usage_exit_code(
    ocx: OcxRunner,
    registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """X-03, keyless half: the exit code, and **only** the exit code.

    One assertion, on purpose. The refusal itself and its `detail` are its
    sibling above; the sole thing this cell measures is the code, contracted at
    **64 / `usage_error`** by `SignErrorKind::exit_code` and unit-tested in
    `options/rekor_upload.rs`.

    It was a strict xfail while the CLI answered 1 / `internal`: the bare
    `SignErrorKind` propagated out of `RekorUploadOpt::enabled` and
    `cli/classify.rs::try_classify` had a `try_downcast!(SignError)` arm but
    none for the bare kind, so the chain walk fell through to
    `ExitCode::Failure`. `40bddb87` taught the classifier the bare kind and the
    marker came off — the assertion below is the contract, asserted plainly,
    and it reds if the classifier ever forgets again.
    """
    keyless = Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle")
    runner, pkg, _subject, _size = subject_package(ocx, keyless, unique_repo, tmp_path)
    refused = ocx_sign(
        runner, keyless, pkg,
        stack=sigstore_stack,
        identity_token=identity_token,
        extra_args=("--no-rekor-upload",),
    )
    assert refused.returncode == 64, (
        "a keyless signature cannot opt out of the transparency log, and the refusal must carry "
        "the usage exit code its own taxonomy assigns it\n"
        f"stdout: {refused.stdout}\nstderr: {refused.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# X-04 — the composition: an expired-cert sidecar reached because the bundle
# was withheld. Neither half is new; the two together are the actual attack.
# ──────────────────────────────────────────────────────────────────────────────


def test_a_withheld_bundle_must_not_expose_an_expired_certificate_sidecar(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
) -> None:
    """X-04. The attack the matrix otherwise pins only in halves — must fail closed.

    Finding (a) says a keyless sidecar's certificate window is checked against
    the certificate's own `notBefore`, so it is satisfied by construction and an
    expired Fulcio leaf stays acceptable for ever. Finding (b) says the sidecar
    door opens whenever the bundle match set comes back empty, and emptiness is
    whatever the registry served. Each is pinned elsewhere, and each pin passes.

    Two passing halves say nothing about the composition. This is it: an
    attacker with push access parks a sidecar bearing a **historically issued**
    certificate for the pinned identity, and a mirror stops serving the bundle.
    (a) makes the stale certificate acceptable, (b) routes verification to it,
    and the operator gets a clean exit 0 over a signature they never intended to
    trust. That is a different claim from either ingredient, so it gets its own
    assertion rather than being inferred from two greens.

    **Written against the end state before it existed**, as a strict xfail, so
    the assertion carried the contract and the marker carried the defect —
    rather than pinning exit 0 and inverting later, the mistake this file made
    once with the key-mode refusal code. Both halves have landed and the marker
    is gone: a keyless simplesigning sidecar now verifies only with
    transparency-log evidence (signing instant from Rekor's `integratedTime`,
    reversing the G1 `CallerSupplied(notBefore)` contract), and the
    bundle→simplesigning fallback fires on **absence** only.

    It stays here as the composition's own regression test. Its two halves are
    pinned separately — X-01b for the fallback, `test_verify.py` for the
    expired-certificate sidecar — and either could be relaxed without the other
    noticing; this is the cell that reds if the two together ever yield exit 0
    again.

    Both ingredients are asserted present, because a composed-attack test that
    quietly degenerates into one of its halves is worse than no test: the
    certificate is checked to have genuinely expired, and the bundle is
    published, checked discoverable, and only then withdrawn — so "withheld"
    is a state this test created, never an artifact that was never there.
    """
    subject, subject_size = cosign_artifacts.push_subject(registry, unique_repo)

    # ── Ingredient A: a sidecar whose certificate expired long ago ───────────
    sidecar_manifest = cosign_artifacts.GOLDEN / "simplesigning_keyless_manifest.json"
    _tag, layer_digest = cosign_artifacts.push_sidecar(
        registry, unique_repo, subject, sidecar_manifest
    )
    annotations = json.loads(sidecar_manifest.read_text())["layers"][0]["annotations"]
    certificate = x509.load_pem_x509_certificate(
        annotations["dev.sigstore.cosign/certificate"].encode()
    )
    now = datetime.datetime.now(datetime.UTC)
    assert certificate.not_valid_after_utc < now, (
        "this cell is only an attack if the certificate is genuinely expired; the golden "
        f"leaf is valid until {certificate.not_valid_after_utc} and it is now {now}. "
        "Regenerating the fixture with a live certificate would turn this into a test of "
        "nothing — mint a short-window leaf rather than widening this assertion"
    )
    assert "dev.sigstore.cosign/bundle" not in annotations, (
        "the sidecar must carry no transparency-log material, or the end-state rule this "
        f"test is written against would be satisfied by it: {sorted(annotations)}"
    )

    # ── Ingredient B: a real bundle, discoverable, and then withheld ─────────
    referrer = cosign_artifacts.push_bundle_referrer(
        registry, unique_repo, "keyless",
        {"mediaType": reg.IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": subject_size},
    )
    _assert_open_doors(
        Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle"),
        unique_repo, subject,
        {"referrers_api": 1, "fallback_index": 0, "sidecar_tag": 1},
        "both shapes must be live before one is withheld, or 'withheld' means 'never pushed'",
    )

    reg.delete_manifest(registry, unique_repo, referrer.digest)
    _assert_open_doors(
        Cell(registry=registry, referrers=True, key_mode="keyless", fmt="bundle"),
        unique_repo, subject,
        {"referrers_api": 0, "fallback_index": 0, "sidecar_tag": 1},
        "the bundle is gone and the sidecar remains — the exact state a hostile mirror serves",
    )

    # ── The composition must fail closed ─────────────────────────────────────
    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")
    verified = ocx.run(
        "package", "verify",
        # A Rekor that cannot be reached, deliberately: the end-state rule requires
        # transparency-log *evidence*, and evidence this sidecar does not carry
        # must not be substitutable by a live lookup that happens to succeed.
        "--rekor-url", cosign_artifacts.DEAD_REKOR_URL,
        "--sigstore-trusted-root", str(cosign_artifacts.TRUST_ROOT),
        "--certificate-identity", identity,
        "--certificate-oidc-issuer", issuer,
        f"{registry}/{unique_repo}@{subject}",
        check=False,
    )
    assert verified.returncode == 65, (
        "the composed attack must fail closed: an expired certificate carries no verifiable "
        "signing instant, and a withheld bundle must not silently promote the sidecar that "
        f"bears it. Got exit {verified.returncode}\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr.strip()}"
    )
    assert json.loads(verified.stdout)["error"]["detail"] == "signature_invalid", verified.stdout
    assert layer_digest, "the sidecar layer is what the refusal must be about"
