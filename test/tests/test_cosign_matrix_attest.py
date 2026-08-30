# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The attest sub-matrix of the cosign interop suite (A-01..A-07).

Both directions of a DSSE attestation, across every discovery path an
attestation can travel. `test_cosign_interop.py` already proves the two
implementations agree on an attestation handed over as a **file**
(`verify-blob-attestation` / `attest-blob`); these are the image-level
equivalents, where each tool resolves the artifact out of a registry itself —
so a green proves *discovery plus content*, not content alone.

| ID | Direction | Discovery path | Model |
|---|---|---|---|
| A-01 | `ocx package attest` -> `cosign verify-attestation` | zot, Referrers API | keyless bundle |
| A-02 | `ocx package attest` -> `cosign verify-attestation` | registry:2, fallback tag | keyless bundle |
| A-03 | `cosign attest` -> `ocx package verify --attestation` | zot, Referrers API | keyless bundle |
| A-04 | `cosign attest` -> `ocx package verify --attestation` | registry:2, fallback tag | keyless bundle |
| A-05 | `cosign attach attestation` -> `ocx package verify --attestation` | `sha256-<hex>.att` sidecar tag | key, simplesigning |
| A-06 | `ocx package attest --signature-format simplesigning` -> `cosign verify-attestation` | `sha256-<hex>.att` sidecar tag | key, simplesigning |
| A-07 | `cosign attach sbom` -> `ocx package sbom` | `sha256-<hex>.sbom` sidecar tag | unsigned |

**A-01..A-04 are keyless, and that is a scope decision rather than a gap** (plan
§"Attest and SBOM"): for a *bundle* attestation the key model changes nothing
that M-03/M-11 do not already prove, so the shape only differs on the discovery
axis. A-05 and A-06 are key-mode for the opposite reason — it is the only model
the `.att` sidecar has at all, for which see "The sidecar half" below.

Every cell follows C-002 — produce, prove exactly one candidate is
discoverable, accept, corrupt, prove the corruption reached the wire, prove it
did not add a second candidate, refuse — and every refusal is an exact
(exit code, message) pair, never a bare non-zero (C-003).

The driver is `tests/fixtures/cosign_matrix.py`; the corruption recipes and the
single-candidate proof are its, not this file's. An attestation referrer carries
the same `application/vnd.dev.sigstore.bundle.v0.3+json` artifactType as a
signature referrer and wraps the same `dsseEnvelope`, so `corrupt_signature`
applies unchanged to A-01..A-04 and no attestation-specific tamper helper is
needed for them. The two sidecar cells are the exception, and it is a shape
difference rather than a convenience: `corrupt_signature`'s `simplesigning` arm
rewrites the
`dev.cosignproject.cosign/signature` layer annotation, which is where a `.sig`
claim's *detached* signature lives, while a `.att` layer is a DSSE envelope
carrying its signature inside the blob. Aimed here that arm would rewrite an
annotation nothing reads and leave the checked signature intact, so the driver
grew `corrupt_attestation_sidecar` alongside it.

## The sidecar half: what `.att` costs, and what is left unwritten

The spec asks for each cell "also exercised for `sign`, `attest` and SBOM attach
where the shape differs". The `.att` **sidecar tag** is where it differs. Both
of its key-mode directions are written — A-05 and A-06 — and the two keyless
ones are absent by decision rather than by omission.

**SBOM attach is A-07, and it has no key axis at all.** `cosign attach sbom`
signs nothing — it prints "Attaching SBOMs this way does not sign them" — and no
cosign command signs the `sha256-<hex>.sbom` tag afterwards, so there is no
keyless/key split to write and no reverse direction either: OCX has no writer
for an unsigned sidecar and deliberately does not grow one (a modern SBOM is
attested, which is A-01..A-04's shape). What A-07 asserts instead is the pair
the two `ocx package sbom` modes make on one artifact — listed `verified: false`
under `--no-verify`, refused `unsigned_rejected_by_policy` (77) under
`--verify` — plus cosign's DEFAULT `--type spdx`, whose `text/spdx+json` layer
type OCX reads and never writes.

**There is an OCX reader, and A-05 exercises it.**
`crates/ocx_lib/src/oci/verify/attestation_sidecar.rs` is the `.att` reader;
`verify/pipeline.rs` opens that door on any run whose `--signature-format` is
not pinned to `bundle` and whose content mode is `Attestation`. An earlier
revision of this docstring recorded the opposite, quoting
`verify/simplesigning_read.rs` to the effect that no such reader existed — that
citation is superseded: the same module now says "`.att` is a tag-only shape and
`super::attestation_sidecar` is its reader". A-05 is the cell that premise was
blocking.

**A-06 exists because writing this row found a shipped defect.** The first time
the reverse direction was measured, `cosign verify-attestation` refused every
sidecar `ocx package attest --signature-format simplesigning` had ever written:

    Error: no matching attestations: signature layer sha256:... is missing
    "dev.cosignproject.cosign/signature" annotation

`SidecarLayer::attestation` (`crates/ocx_lib/src/oci/sign/simplesigning_write.rs`)
omitted that annotation, reasoning that an empty value would claim material that
is not there. Measurement contradicted it in both halves: cosign's own `attach
attestation` writes the key **empty** — pinned in
`fixtures/golden/attestation_sidecar_key_manifest.json`, whose one layer carries
`{"dev.cosignproject.cosign/signature": ""}` — and cosign's reader treats the
key's *absence*, never its emptiness, as fatal. On a `.att` layer the key is a
presence marker, not a place to put a signature; the signature is inside the
envelope. The writer now emits it and A-06 is what keeps it emitted. This is the
row a one-directional matrix cannot have: OCX's own reader ignores the
annotation entirely, so every unit test and A-05 itself stayed green across the
whole life of the defect.

**The two keyless `.att` cells are a decision, and they stay unwritten.** No
cosign version emits a keyless `.att` at all, so there is no artifact for a cell
to consume and none for OCX's writer to be judged against. Checked against the
pinned image rather than assumed: `cosign attach attestation --help` lists
exactly `--attestation` plus registry/TLS flags — no `--certificate`, no
`--chain`, no `--rekor-response`, so no Fulcio leaf and no offline Rekor bundle
can be attached; and `cosign attest --help` carries no
`--registry-referrers-mode` (`attach sbom --help` does), so nothing selects the
legacy tag writer, which is why A-01/A-03 find a `SIGSTORE_BUNDLE_V03` referrer
where an `.att` tag would once have been. OCX's own keyless `.att` arm is
therefore covered where a fixture for it exists — unit tests in
`attestation_sidecar.rs` built from the cosign-authored `keyless_bundle.json` —
and not here, because an interop cell with only one implementation on the wire
is not an interop cell.

Measured behaviour is recorded in
`.claude/artifacts/analysis_cosign_interop_probes.md`; the contract is
`.claude/artifacts/plan_cosign_wp6_matrix.md`. Do not re-derive either.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src import registry as reg
from src.runner import OcxRunner, PackageInfo
from tests.fixtures import attestations, cosign
from tests.fixtures import cosign_matrix as matrix
from tests.fixtures.cosign_matrix import Cell
from tests.fixtures.sigstore_stack import SigstoreStack

#: The predicate every cell attests, as the alias both CLIs take and as the URI
#: both must resolve it to. Asserting the pair is what makes `--type` more than
#: a string that travelled: a regression publishing the wrong resolved URI would
#: still satisfy the alias.
PREDICATE_TYPE = "cyclonedx"
PREDICATE_TYPE_URI = "https://cyclonedx.org/bom"

#: The type each cell's narrowing control asks for and must not get.
WRONG_PREDICATE_TYPE = "spdxjson"

# ──────────────────────────────────────────────────────────────────────────────
# C-006 — what "refuses" asserts here. Every string below was OBSERVED against
# `cosign.COSIGN_IMAGE` (pinned by the driver's C-004 guard) on the exact shape
# it is named for, before it was asserted.
# ──────────────────────────────────────────────────────────────────────────────

#: `cosign verify-attestation` on a corrupted keyless DSSE bundle. Exit code and
#: sentence both measured on A-01 and A-02; the sentence is the driver's
#: :data:`cosign_matrix.COSIGN_BUNDLE_KEYLESS_REFUSAL`, re-observed here because
#: it was measured for `verify` and this is a different subcommand. cosign stops
#: at log inclusion before it ever checks the signature, which is why the string
#: names inclusion — the flipped signature no longer matches the entry Rekor
#: signed.
COSIGN_REFUSAL_EXIT = 1

#: `cosign verify-attestation --type spdxjson` against an intact CycloneDX
#: attestation. Measured; it names the type it *did* find, so this control
#: proves cosign read the predicateType rather than merely declining.
COSIGN_WRONG_TYPE_REFUSAL = (
    f"none of the attestations matched the predicate type: {WRONG_PREDICATE_TYPE}, "
    f"found: {PREDICATE_TYPE_URI}"
)

#: Printed by `cosign verify-attestation` only when `--check-claims` (default
#: true) actually ran. A-01/A-02 leave the flag at its default so the subject
#: binding is asserted — the whole reason the attestation is attached to *this*
#: manifest — and assert this line so "left at its default" is a checked fact
#: rather than a comment.
COSIGN_CLAIMS_VALIDATED = "The cosign claims were validated"

#: `ocx package verify --attestation --type spdxjson` against an intact
#: CycloneDX attestation. Measured pair. 79/`attestation_not_found` rather than
#: a type-mismatch slug because ocx narrows by the signed payload: an
#: attestation of another type is not a candidate at all.
OCX_WRONG_TYPE_EXIT = 79
OCX_WRONG_TYPE_DETAIL = "attestation_not_found"

#: `cosign verify-attestation --key` on a corrupted key-mode `.att` sidecar,
#: verified WITH `--insecure-ignore-tlog` (A-06's coordinate). Measured on that
#: exact shape. Deliberately NOT the driver's `cosign_refusal(cell)`: that
#: selector answers :data:`cosign_matrix.COSIGN_SIDECAR_KEY_REFUSAL` for
#: `(simplesigning, key)` — "invalid signature when validating ASN.1 encoded
#: signature", which is what `cosign verify` says about a flipped `.sig`
#: annotation. A `.att` layer's signature lives inside the DSSE envelope, so
#: cosign reaches its envelope verifier instead and says something else
#: entirely. Nor is it the driver's :data:`COSIGN_BUNDLE_KEY_REFUSAL`, which
#: carries a "failed to verify signature: could not verify envelope: " middle
#: this shape does not print.
COSIGN_SIDECAR_KEY_ATTESTATION_REFUSAL = (
    "no matching attestations: accepted signatures do not match threshold, Found: 0, Expected 1"
)

#: THE TRAP (C-006) in its attestation spelling, and why A-06 passes
#: `--insecure-ignore-tlog=true`. A key-mode `.att` uploads nothing to Rekor
#: (D10), so without the flag cosign fails at the transparency-log search — with
#: a *discovery-flavoured* sentence for a non-discovery cause. A-06 asserts this
#: pair rather than just passing the flag, so "the flag is standing in for a
#: missing log entry" is a checked fact and not a comment; the driver pins the
#: `.sig` twin as :data:`cosign_matrix.COSIGN_NO_TLOG_ENTRY`, which differs in
#: both the noun and the exit code.
COSIGN_ATTESTATION_NO_TLOG_ENTRY = "no matching attestations: signature not found in transparency log"

#: The door each cell's single candidate must be behind. `Cell.referrers` picks
#: the corruption recipe; this maps it to the key of
#: `discoverable_candidates` that has to hold the artifact.
_DOOR = {True: "referrers_api", False: "fallback_index"}

#: The same door as `ocx package verify` names it in its report. The two
#: spellings differ for the fallback half — the driver's scan calls it
#: `fallback_index` after the artifact it reads, ocx calls it `fallback_tag`
#: after the tag it resolved — and mapping them separately keeps either side
#: free to be renamed without silently turning this assertion into a tautology.
_OCX_DISCOVERY_METHOD = {True: "referrers_api", False: "fallback_tag"}

#: What A-05's accepted attestation must report, as one tuple. Measured against
#: the cosign-written `.att` sidecar, not derived: `simplesigning` because a
#: `.att` layer is the sidecar wire shape rather than a bundle, `sidecar_tag`
#: because the manifest carries neither `artifactType` nor `subject` and so is
#: reachable through no listing, and `file` because `cosign attach attestation`
#: takes no certificate and the envelope is signed by the committed key pair.
A_05_ACCEPTED = ("simplesigning", "sidecar_tag", "file")


# ──────────────────────────────────────────────────────────────────────────────
# Cell scaffolding
# ──────────────────────────────────────────────────────────────────────────────


def _assert_one_candidate_behind_the_named_door(cell: Cell, repo: str, subject_digest: str) -> None:
    """C-008, plus *which* door held the one candidate.

    `assert_single_candidate` proves the total is one across all three doors —
    the property that stops a corruption which empties one door from being
    rescued by an untouched artifact behind another. It does not say which door,
    and for these four cells the door *is* the axis: A-02 and A-04 exist only to
    exercise the `sha256-<hex>` fallback index. Asserting it here is what keeps
    them from quietly becoming second copies of A-01 and A-03.
    """
    matrix.assert_single_candidate(cell, cell.registry, repo, subject_digest)
    doors = matrix.discoverable_candidates(cell.registry, repo, subject_digest)
    expected = _DOOR[cell.referrers]
    assert len(doors[expected]) == 1, (
        f"{cell} expects its one attestation behind the `{expected}` door, found {doors}"
    )


def _ocx_attest(
    runner: OcxRunner,
    cell: Cell,
    pkg: PackageInfo,
    *,
    stack: SigstoreStack,
    identity_token: Path | None,
    predicate: Path,
) -> subprocess.CompletedProcess[str]:
    """`ocx package attest` for this cell. Returned unchecked — the cell asserts.

    The key model decides the whole trust-material half, exactly as
    `cosign_matrix.ocx_sign` splits it: keyless names Fulcio, Rekor and the
    identity token; `key` names the committed key pair and passes its password
    through the environment, because ocx has no flag for it and a password in
    argv is world-readable in /proc.

    `--signature-format` comes from ``cell.fmt`` rather than being left at its
    default, and the two values are two different artifacts: `bundle` writes the
    OCI 1.1 referrer A-01/A-02 discover (which door it lands behind is then the
    registry's choice, not a flag's — that is what those two split on), and
    `simplesigning` writes the `sha256-<hex>.att` sidecar tag A-06 discovers.
    """
    args = [
        "package", "attest",
        "--platform", matrix.PLATFORM,
        "--predicate", str(predicate),
        "--type", PREDICATE_TYPE,
        "--signature-format", cell.fmt,
    ]
    env_overrides: dict[str, str] = {}
    if cell.key_mode == "key":
        args += ["--key", str(matrix.COSIGN_KEY)]
        env_overrides["OCX_KEY_PASSWORD"] = matrix.KEY_PASSWORD
    else:
        if identity_token is None:
            raise ValueError("a keyless cell needs an identity token")
        args += [
            "--fulcio-url", stack.fulcio_url,
            "--rekor-url", stack.rekor_url,
            "--identity-token-file", str(identity_token),
        ]
    return runner.run(*args, pkg.short, check=False, env_overrides=env_overrides)


def _cosign_attest(
    work: Path,
    ref: str,
    *,
    stack: SigstoreStack,
    identity_token: Path,
    predicate: Path,
) -> subprocess.CompletedProcess[str]:
    """`cosign attest <ref>` — the mirror image of :func:`_ocx_attest`.

    A signing config rather than `--fulcio-url`/`--rekor-url`: cosign 3 removed
    those from the signing commands, so pointing at a self-hosted stack is not
    optional plumbing but the only route (`cosign.signing_config`). Everything
    cosign reads has to live under ``work``, which is what the container sees.
    """
    cosign.stage(work, "trusted_root.json", stack.trusted_root_json.read_bytes())
    cosign.stage(work, "identity-token", identity_token.read_bytes())
    cosign.stage(work, "predicate.json", predicate.read_bytes())
    config = cosign.signing_config(
        work, rekor_url=stack.rekor_url, fulcio_url=stack.fulcio_url, oidc_url=stack.issuer
    )
    return cosign.run_registry(
        work,
        "attest",
        "--signing-config", config,
        "--trusted-root", "trusted_root.json",
        "--identity-token", "identity-token",
        "--predicate", "predicate.json",
        "--type", PREDICATE_TYPE,
        "--yes",
        ref,
    )


def _cosign_verify_attestation(
    work: Path,
    cell: Cell,
    ref: str,
    *,
    stack: SigstoreStack,
    ignore_tlog: bool,
    predicate_type: str = PREDICATE_TYPE,
) -> subprocess.CompletedProcess[str]:
    """`cosign verify-attestation <ref>` — image-level, this cell's key model.

    ``ignore_tlog`` is **required, never defaulted**, the contract
    `cosign_matrix.cosign_verify` states for the signature half and for the same
    reason: it is load-bearing in opposite directions per cell, and a default
    would silently pick one. A keyless ocx attestation carries its Rekor entry
    in the bundle and clears cosign's full transparency-log check offline, so
    A-01/A-02 pass ``False`` and passing ``True`` there would hide the strongest
    property those cells have. A key-mode `.att` uploads nothing to Rekor by
    default (D10), so A-06 passes ``True`` and pins
    :data:`COSIGN_ATTESTATION_NO_TLOG_ENTRY` to show what the flag is standing
    in for rather than leaving it an unexamined convenience.

    **No `--check-claims`**: its default is true, and
    :data:`COSIGN_CLAIMS_VALIDATED` asserts the check actually ran, so the
    subject binding is verified rather than assumed.

    The key model picks the matchers, exactly as it does on the signature side:
    `--key` conflicts with the certificate flags, so exactly one half is ever
    spelled. The trusted root is staged for both — under a key cosign never
    reaches for it, and staging it unconditionally keeps the one mounted
    directory identical across cells.
    """
    cosign.stage(work, "trusted_root.json", stack.trusted_root_json.read_bytes())
    args = ["verify-attestation", "--type", predicate_type]
    if cell.key_mode == "key":
        cosign.stage(work, "cosign.pub", matrix.COSIGN_PUB.read_bytes())
        args += ["--key", "cosign.pub"]
    else:
        args += [
            "--trusted-root", "trusted_root.json",
            "--certificate-identity", stack.identity,
            "--certificate-oidc-issuer", stack.issuer,
        ]
    args += [f"--insecure-ignore-tlog={'true' if ignore_tlog else 'false'}"]
    return cosign.run_registry(work, *args, ref)


def _corrupt(cell: Cell, repo: str, subject_digest: str) -> None:
    """C-002 steps 4-5: flip a DSSE signature byte, prove it reached the wire.

    ``corrupt_signature`` dispatches on the cell's shape, rewrites the bundle
    blob, the referrer manifest and — on the fallback registry — the index
    child's digest *and* size, then reads the signature back off the wire on
    both sides and asserts the two differ itself: a rewrite that silently did
    not land is otherwise indistinguishable from one that did, and the refusal
    that follows would be credited to a corruption that never happened.

    The door check here is the other half: the corruption must have *replaced*
    the candidate, not added one beside it.
    """
    matrix.corrupt_signature(cell, cell.registry, repo, subject_digest)
    _assert_one_candidate_behind_the_named_door(cell, repo, subject_digest)


def _published_subject(
    ocx: OcxRunner, cell: Cell, repo: str, tmp_path: Path
) -> tuple[OcxRunner, PackageInfo, str, str]:
    """A fresh package in this cell's registry, with its premise asserted.

    Returns ``(runner, pkg, subject_digest, ref)``. The digest is the
    `linux/amd64` platform manifest under the package's index and ``ref`` names
    it by digest, never by tag (C-005): a tag resolves to the *index*, where no
    attestation lives, so both tools would fail with a discovery error that a
    bare non-zero assertion would happily accept as a negative half.
    """
    runner, pkg, subject_digest, _size = matrix.subject_package(ocx, cell, repo, tmp_path)
    matrix.assert_registry_premise(cell, pkg.repo, subject_digest)
    return runner, pkg, subject_digest, matrix.image_ref(cell, pkg, subject_digest)


# ──────────────────────────────────────────────────────────────────────────────
# A-01, A-02 — ocx attests, cosign verifies
# ──────────────────────────────────────────────────────────────────────────────


def _ocx_attests_cosign_verifies(
    cell: Cell,
    *,
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    stack: SigstoreStack,
    identity_token: Path,
    narrowing_control: bool,
) -> None:
    """The whole C-002 cycle for a cell ocx produces and cosign consumes."""
    runner, pkg, subject_digest, ref = _published_subject(ocx, cell, repo, tmp_path)

    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attested = _ocx_attest(
        runner, cell, pkg, stack=stack, identity_token=identity_token, predicate=predicate
    )
    assert attested.returncode == 0, (
        f"ocx could not attest {cell}\nstdout: {attested.stdout}\nstderr: {attested.stderr}"
    )
    assert json.loads(attested.stdout)["data"]["predicate_type"] == PREDICATE_TYPE_URI, (
        f"ocx resolved `--type {PREDICATE_TYPE}` to something other than {PREDICATE_TYPE_URI}, "
        f"so cosign's --type assertion below would be about a different document: {attested.stdout}"
    )
    _assert_one_candidate_behind_the_named_door(cell, pkg.repo, subject_digest)

    work = tmp_path / "cosign"
    work.mkdir()
    accepted = _cosign_verify_attestation(work, cell, ref, stack=stack, ignore_tlog=False)
    assert accepted.returncode == 0, (
        f"cosign rejected an intact ocx attestation for {cell}\n"
        f"stdout: {accepted.stdout}\nstderr: {accepted.stderr}"
    )
    assert COSIGN_CLAIMS_VALIDATED in accepted.stderr, (
        f"cosign verified the envelope without validating the claims, so the subject binding — "
        f"that this attestation is of THIS manifest — went unasserted for {cell}\n"
        f"stderr: {accepted.stderr}"
    )

    if narrowing_control:
        # Without this, `--type cyclonedx` passing above proves only that cosign
        # accepted the bundle, not that it read the predicateType ocx wrote — and
        # a regression publishing the wrong resolved URI would sail through both
        # halves. The image-level twin of
        # `test_cosign_rejects_an_ocx_attestation_narrowed_to_the_wrong_type`.
        mismatched = _cosign_verify_attestation(
            work, cell, ref, stack=stack, ignore_tlog=False, predicate_type=WRONG_PREDICATE_TYPE
        )
        assert mismatched.returncode == COSIGN_REFUSAL_EXIT, (
            f"expected exit {COSIGN_REFUSAL_EXIT} for the wrong --type, got "
            f"{mismatched.returncode}\nstdout: {mismatched.stdout}\nstderr: {mismatched.stderr}"
        )
        assert COSIGN_WRONG_TYPE_REFUSAL in mismatched.stderr, (
            f"cosign refused --type {WRONG_PREDICATE_TYPE} for some reason other than the "
            f"predicate type, so this controls for the wrong thing\nstderr: {mismatched.stderr}"
        )

    _corrupt(cell, pkg.repo, subject_digest)

    refused = _cosign_verify_attestation(work, cell, ref, stack=stack, ignore_tlog=False)
    assert refused.returncode == COSIGN_REFUSAL_EXIT, (
        f"expected exit {COSIGN_REFUSAL_EXIT} for a corrupted attestation, got "
        f"{refused.returncode}\nstdout: {refused.stdout}\nstderr: {refused.stderr}"
    )
    expected = matrix.cosign_refusal(cell)
    assert expected in refused.stderr, (
        f"cosign refused {cell} for a different reason than the corrupted signature; expected "
        f"{expected!r}\nstdout: {refused.stdout}\nstderr: {refused.stderr}"
    )


def test_cosign_verifies_an_ocx_attestation_over_the_referrers_api(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A-01: cosign discovers and verifies an ocx attestation through OCI 1.1.

    The image-level counterpart of
    `test_cosign_verifies_an_attestation_ocx_produced`, which handed cosign the
    bundle as a file. On top of the certificate chain, the log entry and the
    in-toto Statement that test already covers, this one adds the thing a file
    hand-over cannot: cosign finding the artifact on its own, through the
    Referrers API, from nothing but a digest reference.

    Carries the `--type` narrowing control, so `--type cyclonedx` passing is
    evidence that cosign read the predicateType rather than merely accepted the
    bundle.

    The corrupted half fails at log inclusion rather than at the signature —
    cosign checks the Rekor entry first, and a flipped DSSE signature no longer
    matches what the log signed — which is why the pinned string names
    inclusion.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="keyless", fmt="bundle")
    _ocx_attests_cosign_verifies(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, narrowing_control=True,
    )


def test_cosign_verifies_an_ocx_attestation_through_the_fallback_tag(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A-02: the same attestation, discovered through the `sha256-<hex>` index.

    `legacy_registry` is registry:2, which 404s the Referrers API — asserted,
    not assumed, and the single candidate is then asserted to be behind the
    fallback door specifically, because a cell that silently ran against zot
    would be A-01 wearing this name.

    The corruption recipe differs here and the difference is worth naming:
    registry:2 sets no `REGISTRY_STORAGE_DELETE_ENABLED`, so the superseded
    referrer manifest cannot be deleted and the index tag is rewritten in place
    to name the new blob's digest *and* size. The door check afterwards is what
    proves the orphan is no longer reachable.

    Carries the `--type` narrowing control too, same as A-01: the door a cell
    discovers its artifact behind has no bearing on whether cosign reads the
    predicateType, so there is no reason for this cell to skip the control.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="keyless", fmt="bundle")
    _ocx_attests_cosign_verifies(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, narrowing_control=True,
    )


# ──────────────────────────────────────────────────────────────────────────────
# A-03, A-04 — cosign attests, ocx verifies
# ──────────────────────────────────────────────────────────────────────────────


def _cosign_attests_ocx_verifies(
    cell: Cell,
    *,
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    stack: SigstoreStack,
    identity_token: Path,
    narrowing_control: bool,
) -> None:
    """The whole C-002 cycle for a cell cosign produces and ocx consumes."""
    runner, pkg, subject_digest, ref = _published_subject(ocx, cell, repo, tmp_path)

    work = tmp_path / "cosign"
    work.mkdir()
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attested = _cosign_attest(
        work, ref, stack=stack, identity_token=identity_token, predicate=predicate
    )
    assert attested.returncode == 0, (
        f"cosign could not attest {cell} against the local stack\n"
        f"stdout: {attested.stdout}\nstderr: {attested.stderr}"
    )
    _assert_one_candidate_behind_the_named_door(cell, pkg.repo, subject_digest)

    accepted = matrix.ocx_verify(
        runner, cell, ref, stack=stack, extra_args=("--attestation", "--type", PREDICATE_TYPE)
    )
    entry = matrix.accepted_signature(accepted, f"an intact cosign attestation for {cell}")
    expected_method = _OCX_DISCOVERY_METHOD[cell.referrers]
    assert entry["discovery_method"] == expected_method, (
        f"{cell} expects ocx to report `{expected_method}`; it reported {entry!r}"
    )
    assert entry["key_backend"] == "keyless", f"{cell} is a keyless cell; ocx reported {entry!r}"

    if narrowing_control:
        # The ocx-side twin of A-01's control. Without it `--type cyclonedx`
        # above proves only that ocx accepted the attestation, not that it
        # narrowed by the signed payload's predicateType.
        mismatched = matrix.ocx_verify(
            runner, cell, ref, stack=stack, extra_args=("--attestation", "--type", WRONG_PREDICATE_TYPE)
        )
        assert mismatched.returncode == OCX_WRONG_TYPE_EXIT, (
            f"expected exit {OCX_WRONG_TYPE_EXIT} for --type {WRONG_PREDICATE_TYPE}, got "
            f"{mismatched.returncode}\nstdout: {mismatched.stdout}\nstderr: {mismatched.stderr}"
        )
        assert json.loads(mismatched.stdout)["error"]["detail"] == OCX_WRONG_TYPE_DETAIL, (
            f"ocx refused the wrong --type for another reason: {mismatched.stdout}"
        )

    _corrupt(cell, pkg.repo, subject_digest)

    refused = matrix.ocx_verify(
        runner, cell, ref, stack=stack, extra_args=("--attestation", "--type", PREDICATE_TYPE)
    )
    # Never 79 (`attestation_not_found`): that would mean the corruption
    # destroyed discovery rather than the signature, which is the failure this
    # contract exists to tell apart — and the narrowing control above shows 79
    # is genuinely reachable on this very artifact.
    matrix.assert_ocx_refusal(refused, cell, f"a corrupted attestation for {cell}")


def test_ocx_verifies_a_cosign_attestation_over_the_referrers_api(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A-03: `ocx package verify --attestation` accepts what `cosign attest` wrote.

    The image-level counterpart of
    `test_ocx_verifies_an_attestation_cosign_produced`, which pushed the bundle
    into the registry by hand after `attest-blob` wrote it to a file. Here cosign
    publishes the artifact itself, so the cell also covers the manifest and
    annotations cosign's own writer emits — and ocx's report is asserted to name
    `referrers_api`, the door it was supposed to come through.

    Carries the ocx-side `--type` narrowing control, which doubles as the proof
    that 79 / `attestation_not_found` is reachable on this artifact — so the
    corrupted half asserting 65 / `signature_invalid` is a real discrimination
    between "the signature is bad" and "nothing was found".
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="keyless", fmt="bundle")
    _cosign_attests_ocx_verifies(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, narrowing_control=True,
    )


def test_ocx_verifies_a_cosign_attestation_through_the_fallback_tag(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A-04: the same, discovered through the `sha256-<hex>` fallback index.

    cosign has no flag for this — it writes the fallback index because
    registry:2 serves no Referrers API, exactly as ocx does in A-02. Both tools
    choosing the same door unprompted, and ocx then reporting `fallback_tag`, is
    the interop property this cell exists for.

    Carries the ocx-side `--type` narrowing control too, same as A-03: which
    door discovered the artifact has no bearing on whether ocx narrows by the
    signed payload's predicateType, so 79 / `attestation_not_found` needs
    proving reachable here exactly as it does there.
    """
    cell = Cell(registry=legacy_registry, referrers=False, key_mode="keyless", fmt="bundle")
    _cosign_attests_ocx_verifies(
        cell, ocx=ocx, repo=unique_repo, tmp_path=tmp_path,
        stack=sigstore_stack, identity_token=identity_token, narrowing_control=True,
    )


# ──────────────────────────────────────────────────────────────────────────────
# A-05 — cosign attaches a `.att` sidecar, ocx verifies it
# ──────────────────────────────────────────────────────────────────────────────


def _assert_one_attestation_behind_the_sidecar_tag(cell: Cell, repo: str, subject_digest: str) -> None:
    """C-008 for A-05, plus the door the one candidate has to be behind.

    :func:`_assert_one_candidate_behind_the_named_door`'s twin, and separate
    because the doors differ by content mode rather than by registry: an
    attestation run never walks the `.sig` tag and a signature run never walks
    the `.att` tag, so the driver answers the two questions with two functions
    (see `cosign_matrix.discoverable_attestation_candidates`).

    Naming the door is what stops A-05 from quietly becoming A-03. Both would
    accept a bundle referrer on zot; only this assertion says the artifact ocx
    verified came out of the tag, which is the whole shape this cell exists for.
    """
    matrix.assert_single_attestation_candidate(cell, cell.registry, repo, subject_digest)
    doors = matrix.discoverable_attestation_candidates(cell.registry, repo, subject_digest)
    assert len(doors[matrix.ATTESTATION_SIDECAR_DOOR]) == 1, (
        f"{cell} expects its one attestation behind the `{matrix.ATTESTATION_SIDECAR_DOOR}` "
        f"door, found {doors}"
    )


def _cosign_attach_attestation(
    work: Path,
    ref: str,
    *,
    stack: SigstoreStack,
    subject_manifest: bytes,
    predicate: Path,
) -> subprocess.CompletedProcess[str]:
    """`attest-blob` -> `attach attestation`: the ONLY `.att` route in v3.1.1.

    **Raises if either step fails**, the contract `cosign_matrix.cosign_sign`
    states: a half-attached sidecar is not a shape a cell should then assert
    against. The returned process is the `attach` step.

    Two things here are load-bearing rather than incidental, and both are
    `golden/generate.py`'s recipe, re-run live instead of replayed:

    The signed blob is the **subject manifest's own bytes**, so the in-toto
    Statement's single subject digest *is* the manifest digest ocx binds it to.
    Signing a stand-in payload would produce a perfectly well-formed envelope
    attached to this subject and bound to nothing, and the cell's positive half
    would pass while proving no binding at all.

    `attach attestation` takes the bare DSSE envelope, which is the
    `dsseEnvelope` half of the bundle `attest-blob` wrote — lifted out, never
    re-encoded, because the signature covers a PAE derived from the `payload`
    field inside it.

    Key mode is not a choice this function makes. `cosign attach attestation`
    accepts only `--attestation` — no `--certificate`, no `--chain`, no
    `--rekor-response` — so keyless material cannot be attached by any spelling,
    which is the module doc's "the two keyless `.att` cells are a decision".
    """
    cosign.stage(work, "trusted_root.json", stack.trusted_root_json.read_bytes())
    cosign.stage(work, "cosign.key", matrix.COSIGN_KEY.read_bytes())
    cosign.stage(work, "subject.manifest", subject_manifest)
    cosign.stage(work, "predicate.json", predicate.read_bytes())
    # No Fulcio and no OIDC provider in the config: a `--key` signer mints no
    # certificate and has no identity to authenticate, so naming a CA it will
    # never call would only invite cosign to try.
    config = cosign.signing_config(work, rekor_url=stack.rekor_url, name="signing-config-key.json")

    attest_blob = [
        "attest-blob", "--key", "cosign.key", "--signing-config", config,
        "--trusted-root", "trusted_root.json", "--predicate", "predicate.json",
        "--type", PREDICATE_TYPE, "--bundle", "att-blob-bundle.json", "--yes",
    ]
    signed = cosign.run(
        work, *attest_blob, "subject.manifest", env={"COSIGN_PASSWORD": matrix.KEY_PASSWORD}
    )
    if signed.returncode != 0:
        raise RuntimeError(f"cosign attest-blob failed:\n{signed.stdout}\n{signed.stderr}")

    bundle = json.loads((work / "att-blob-bundle.json").read_text())
    envelope = cosign.stage(work, "attestation.json", json.dumps(bundle["dsseEnvelope"]).encode())
    attached = cosign.run_registry(work, "attach", "attestation", "--attestation", envelope, ref)
    if attached.returncode != 0:
        raise RuntimeError(f"cosign attach attestation failed:\n{attached.stdout}\n{attached.stderr}")
    return attached


def test_ocx_verifies_a_cosign_attestation_sidecar_tag(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """A-05: `ocx package verify --attestation` reads cosign's `.att` sidecar.

    The third discovery path an attestation can arrive on, and the one A-01..A-04
    cannot reach: the `sha256-<hex>.att` tag carries neither `artifactType` nor
    `subject`, so no listing finds it and the fallback index does not hold it
    either — the tag is the whole discovery story. The reader is
    `crates/ocx_lib/src/oci/verify/attestation_sidecar.rs`, and this cell is
    what proves it agrees with cosign's writer on the wire rather than only with
    the committed capture its own unit tests replay.

    Key mode, because it is the only model this shape has: `attach attestation`
    accepts no certificate and `attest` no longer writes the tag, so a keyless
    `.att` is not producible by any cosign v3.1.1 command (module doc, "The
    sidecar half"). The DSSE envelope carries its own signature, checked against
    the committed public key — no trusted root is spelled, because a `.att` tag
    carries no transparency material for one to be about.

    Carries the `--type` narrowing control, which does double duty exactly as it
    does in A-03: it proves ocx narrowed by the *signed* predicateType rather
    than merely accepting the envelope, and it proves 79 / `attestation_not_found`
    is reachable **on this very artifact** — so the corrupted half asserting 65 /
    `signature_invalid` discriminates "the signature is bad" from "nothing was
    found" instead of assuming it.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="key", fmt="simplesigning")
    runner, pkg, subject_digest, ref = _published_subject(ocx, cell, unique_repo, tmp_path)

    work = tmp_path / "cosign"
    work.mkdir()
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    _cosign_attach_attestation(
        work,
        ref,
        stack=sigstore_stack,
        subject_manifest=reg.fetch_manifest_raw(cell.registry, pkg.repo, subject_digest)[0],
        predicate=predicate,
    )
    _assert_one_attestation_behind_the_sidecar_tag(cell, pkg.repo, subject_digest)

    accepted = matrix.ocx_verify(
        runner, cell, ref, stack=sigstore_stack,
        pin_format="simplesigning",
        extra_args=("--attestation", "--type", PREDICATE_TYPE),
    )
    entry = matrix.accepted_signature(accepted, f"an intact cosign attestation sidecar for {cell}")
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == A_05_ACCEPTED, entry

    mismatched = matrix.ocx_verify(
        runner, cell, ref, stack=sigstore_stack,
        pin_format="simplesigning",
        extra_args=("--attestation", "--type", WRONG_PREDICATE_TYPE),
    )
    assert mismatched.returncode == OCX_WRONG_TYPE_EXIT, (
        f"expected exit {OCX_WRONG_TYPE_EXIT} for --type {WRONG_PREDICATE_TYPE}, got "
        f"{mismatched.returncode}\nstdout: {mismatched.stdout}\nstderr: {mismatched.stderr}"
    )
    assert json.loads(mismatched.stdout)["error"]["detail"] == OCX_WRONG_TYPE_DETAIL, (
        f"ocx refused the wrong --type for another reason: {mismatched.stdout}"
    )

    # C-002 steps 4-5, through the driver's `.att` recipe: the DSSE signature
    # inside the envelope blob is flipped and the pair is read back through the
    # tag on both sides, so a rewrite that did not land reds there rather than
    # being credited as the cause of the refusal below. The door check that
    # follows is the other half — the corruption must have replaced the
    # candidate, not added one beside it.
    matrix.corrupt_attestation_sidecar(cell.registry, pkg.repo, subject_digest)
    _assert_one_attestation_behind_the_sidecar_tag(cell, pkg.repo, subject_digest)

    refused = matrix.ocx_verify(
        runner, cell, ref, stack=sigstore_stack,
        pin_format="simplesigning",
        extra_args=("--attestation", "--type", PREDICATE_TYPE),
    )
    matrix.assert_ocx_refusal(refused, cell, f"a corrupted attestation sidecar for {cell}")


# ──────────────────────────────────────────────────────────────────────────────
# A-06 — ocx writes a `.att` sidecar, cosign verifies it
# ──────────────────────────────────────────────────────────────────────────────


def test_cosign_verifies_an_ocx_attestation_sidecar_tag(
    ocx: OcxRunner,
    unique_repo: str,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """A-06: `cosign verify-attestation` reads the `.att` sidecar ocx wrote.

    A-05's mirror image, and the cell that closes the sidecar row: A-05 proved
    ocx reads what cosign's `attach attestation` writes, this one proves cosign
    reads what `ocx package attest --signature-format simplesigning` writes. Both
    directions on one shape is what makes it interop rather than two independent
    readers agreeing with their own writers.

    It also pins a regression that shipped. The layer descriptor cosign requires
    is not the envelope — it is the presence of a
    `dev.cosignproject.cosign/signature` annotation, which on a `.att` layer
    cosign writes **empty** because the DSSE envelope carries its signature
    inside. `SidecarLayer::attestation` used to omit the key on exactly that
    reasoning, and cosign refuses a layer without it ("signature layer sha256:…
    is missing …"), so every `.att` ocx published was unverifiable by any cosign
    release while the intact-artifact half of this row went untested. This cell
    is what makes re-omitting it red.

    Key mode, because it is the only model this shape has (module doc, "The
    sidecar half"), and `--insecure-ignore-tlog=true` because a key-mode `.att`
    uploads nothing to Rekor by default — asserted rather than assumed, via
    :data:`COSIGN_ATTESTATION_NO_TLOG_ENTRY`, so the flag is not quietly hiding
    a different failure.
    """
    cell = Cell(registry=ocx.registry, referrers=True, key_mode="key", fmt="simplesigning")
    runner, pkg, subject_digest, ref = _published_subject(ocx, cell, unique_repo, tmp_path)

    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    attested = _ocx_attest(
        runner, cell, pkg, stack=sigstore_stack, identity_token=None, predicate=predicate
    )
    assert attested.returncode == 0, (
        f"ocx could not attest {cell}\nstdout: {attested.stdout}\nstderr: {attested.stderr}"
    )
    report = json.loads(attested.stdout)["data"]
    assert report["predicate_type"] == PREDICATE_TYPE_URI, (
        f"ocx resolved `--type {PREDICATE_TYPE}` to something other than {PREDICATE_TYPE_URI}, "
        f"so cosign's --type assertion below would be about a different document: {attested.stdout}"
    )
    # The sidecar leg, and only it. A `--signature-format simplesigning` attest
    # writes no referrer at all, so a report naming one would mean this cell is
    # about to verify a bundle through a door it never meant to open — which the
    # C-008 proof below would then have to catch instead.
    assert report.get("sidecar_digest") and not report.get("referrer_digest"), (
        f"{cell} expects the `.att` leg alone; ocx reported {report}"
    )
    _assert_one_attestation_behind_the_sidecar_tag(cell, pkg.repo, subject_digest)

    work = tmp_path / "cosign"
    work.mkdir()
    accepted = _cosign_verify_attestation(work, cell, ref, stack=sigstore_stack, ignore_tlog=True)
    assert accepted.returncode == 0, (
        f"cosign rejected an intact ocx attestation sidecar for {cell}\n"
        f"stdout: {accepted.stdout}\nstderr: {accepted.stderr}"
    )
    assert COSIGN_CLAIMS_VALIDATED in accepted.stderr, (
        f"cosign verified the envelope without validating the claims, so the subject binding — "
        f"that this attestation is of THIS manifest — went unasserted for {cell}\n"
        f"stderr: {accepted.stderr}"
    )

    # C-006's trap, and what `ignore_tlog=True` above is standing in for. Without
    # the flag cosign fails at the transparency-log search on an artifact that
    # never uploaded one, in a sentence that reads like a discovery error — so
    # "the message is not a discovery error" is not a safe filter anywhere here.
    unlogged = _cosign_verify_attestation(work, cell, ref, stack=sigstore_stack, ignore_tlog=False)
    assert unlogged.returncode == COSIGN_REFUSAL_EXIT, (
        f"expected exit {COSIGN_REFUSAL_EXIT} without --insecure-ignore-tlog, got "
        f"{unlogged.returncode}\nstdout: {unlogged.stdout}\nstderr: {unlogged.stderr}"
    )
    assert COSIGN_ATTESTATION_NO_TLOG_ENTRY in unlogged.stderr, (
        f"cosign refused the unlogged key-mode sidecar for some reason other than the missing "
        f"Rekor entry, so `ignore_tlog=True` is covering for something else\n"
        f"stderr: {unlogged.stderr}"
    )

    # The narrowing control, same as A-01/A-02: without it `--type cyclonedx`
    # passing above proves only that cosign accepted the envelope, not that it
    # read the predicateType ocx signed into the Statement.
    mismatched = _cosign_verify_attestation(
        work, cell, ref, stack=sigstore_stack, ignore_tlog=True, predicate_type=WRONG_PREDICATE_TYPE
    )
    assert mismatched.returncode == COSIGN_REFUSAL_EXIT, (
        f"expected exit {COSIGN_REFUSAL_EXIT} for the wrong --type, got "
        f"{mismatched.returncode}\nstdout: {mismatched.stdout}\nstderr: {mismatched.stderr}"
    )
    assert COSIGN_WRONG_TYPE_REFUSAL in mismatched.stderr, (
        f"cosign refused --type {WRONG_PREDICATE_TYPE} for some reason other than the "
        f"predicate type, so this controls for the wrong thing\nstderr: {mismatched.stderr}"
    )

    matrix.corrupt_attestation_sidecar(cell.registry, pkg.repo, subject_digest)
    _assert_one_attestation_behind_the_sidecar_tag(cell, pkg.repo, subject_digest)

    refused = _cosign_verify_attestation(work, cell, ref, stack=sigstore_stack, ignore_tlog=True)
    assert refused.returncode == COSIGN_REFUSAL_EXIT, (
        f"expected exit {COSIGN_REFUSAL_EXIT} for a corrupted attestation sidecar, got "
        f"{refused.returncode}\nstdout: {refused.stdout}\nstderr: {refused.stderr}"
    )
    assert COSIGN_SIDECAR_KEY_ATTESTATION_REFUSAL in refused.stderr, (
        f"cosign refused {cell} for a different reason than the corrupted DSSE signature; "
        f"expected {COSIGN_SIDECAR_KEY_ATTESTATION_REFUSAL!r}\n"
        f"stdout: {refused.stdout}\nstderr: {refused.stderr}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# A-07 — cosign attaches an SBOM, ocx lists it
# ──────────────────────────────────────────────────────────────────────────────


#: The layer media type `cosign attach sbom --type spdx` writes for a JSON
#: input — and `spdx` is cosign's DEFAULT type, so this is the single most
#: likely `.sbom` layer in the wild.
#:
#: Deliberately NOT `application/spdx+json`, which is the registered spelling
#: and the one OCX itself writes: cosign derives this one by appending `+json`
#: to its own `text/spdx`. Measured on the pinned image over the full `--type` ×
#: `--input-format` cross product; the table is recorded in
#: `crates/ocx_lib/src/oci/referrer/media_types.rs`. Asserting the pair (this
#: string, the URI below) is what makes the SPDX half more than a second
#: CycloneDX run: a reader covering only OCX's own three spellings refuses it.
COSIGN_SPDX_JSON_MEDIA_TYPE = "text/spdx+json"
SPDX_PREDICATE_TYPE_URI = "https://spdx.dev/Document"

#: What `attach sbom --type cyclonedx` writes for the same JSON input, and one
#: of only two rows where cosign's table and OCX's own spellings coincide.
COSIGN_CYCLONEDX_MEDIA_TYPE = "application/vnd.cyclonedx+json"


def _sbom_flags(stack: SigstoreStack, *, verify: bool) -> tuple[str, ...]:
    """`ocx package sbom` flags for one of its two modes.

    The identity flags are what *resolve* demand mode, not what it then checks:
    a `.sbom` sidecar is unsigned by construction, so the refusal lands before
    any certificate is looked at and the stack's material is never exercised.
    They are still the stack's rather than invented strings — `--verify` with no
    identity source is a usage error (64), and a cell asserting 77 must not be
    able to pass by accidentally hitting 64 instead.
    """
    if not verify:
        return ("--no-verify", "--platform", matrix.PLATFORM)
    return ("--verify", *stack.verify_args(matrix.PLATFORM))


def _attach_sbom(work: Path, ref: str, document: bytes, *, sbom_type: str) -> None:
    """`cosign attach sbom`, raising rather than returning a half-attached state.

    Default (legacy) referrers mode, which is what writes the tag at all:
    `--registry-referrers-mode oci-1-1` writes an OCI 1.1 referrer instead — and
    is refused outright without `COSIGN_EXPERIMENTAL=1`, measured on the pinned
    image. That referrer shape is covered by unit tests over a stub registry;
    the tag is what needs a live one, because the tag is invisible to every
    listing and only a real registry can show that.

    Deprecated and still functional in v3.1.1: `cosign attach` prints "attach
    will be removed in v4.0.0" and `attach sbom` adds its own 2024 notice. Both
    are the reason to cover it — the `.sbom` sidecars already sitting in
    registries outlive the command that wrote them.
    """
    name = cosign.stage(work, f"sbom.{sbom_type}.json", document)
    attached = cosign.run_registry(work, "attach", "sbom", "--sbom", name, "--type", sbom_type, ref)
    assert attached.returncode == 0, (
        f"cosign could not attach a {sbom_type} SBOM\n"
        f"stdout: {attached.stdout}\nstderr: {attached.stderr}"
    )


def _assert_the_tag_is_the_only_door(
    cell: Cell, repo: str, subject_digest: str, *, expected_layer_type: str
) -> str:
    """The `.sbom` tag holds the document and no listing can reach it.

    Three claims, and each one is why the reader had to be written where it was:

    * the Referrers API answers 200 with an **empty** list, so the OCI 1.1
      discovery path finds nothing — a reader hung off the listing would see
      this subject as carrying no SBOM at all;
    * the tag's manifest declares no `subject`, which is *why* the listing is
      empty rather than a coincidence beside it;
    * the layer is typed by the DOCUMENT, which is why a simplesigning or DSSE
      reader aimed here returns an empty scan and why the reader had to be a
      document reader on the permissive listing path.

    Returns the digest the registry served the tag's manifest under — the value
    `ocx package sbom` must report as `referrer_digest`.
    """
    status, index = reg.list_referrers(cell.registry, repo, subject_digest)
    assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
    assert index is not None and index["manifests"] == [], (
        "`cosign attach sbom` in legacy mode must leave the Referrers API empty; if it does "
        f"not, this cell is no longer about the tag door: {index}"
    )

    tag = "sha256-" + subject_digest.removeprefix("sha256:") + ".sbom"
    raw, served = reg.fetch_manifest_raw(cell.registry, repo, tag)
    manifest = json.loads(raw)
    assert "subject" not in manifest, (
        "a `.sbom` sidecar is not a referrer — a subject here means cosign started writing "
        f"one and the listing above would have found it: {manifest}"
    )
    assert "artifactType" not in manifest, f"a `.sbom` sidecar declares no artifactType: {manifest}"
    assert len(manifest["layers"]) == 1, f"`attach sbom` writes exactly one layer: {manifest}"
    assert manifest["layers"][0]["mediaType"] == expected_layer_type, (
        f"cosign must type the layer by the document; expected {expected_layer_type}, "
        f"got {manifest['layers'][0]['mediaType']}"
    )
    return served


def test_ocx_lists_a_cosign_sbom_sidecar_tag(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """A-07: `ocx package sbom` reads cosign's `sha256-<hex>.sbom` sidecar.

    The third tag-only cosign shape, and the one that is not a signature. Its
    reader is `pipeline.rs`'s `read_sbom_sidecar_tag` on the permissive listing
    path — not a fourth `SidecarKind` — because an `.sbom` layer keeps the SBOM
    document's own media type, so a simplesigning or DSSE reader aimed at this
    tag returns an empty scan for every sidecar that exists.

    `cell.key_mode` and `cell.fmt` are inert here and named only because `Cell`
    requires them: **nothing in this cell is signed**. `cosign attach sbom`
    signs nothing and says so on stderr, and no cosign command signs the tag
    afterwards — which is exactly why the two `ocx package sbom` modes split on
    it the way they do, and why this is the one interop cell that needs no
    signing material of its own.

    Four properties, in the order that makes each one falsifiable:

    1. **The control.** Before the attach, the same command on the same subject
       exits 79. Without it a green below would be consistent with a reader that
       lists something for every subject, and the cell would prove nothing about
       the attachment.
    2. **The tag is the only door**, asserted on the wire rather than assumed —
       see :func:`_assert_the_tag_is_the_only_door`.
    3. **Permissive lists it**, byte-exact and labelled by the layer's type,
       naming the manifest digest the registry served.
    4. **Demand refuses it**, exit 77, the same code an unsigned *referrer*
       already gets. The pair is the contract: read alone, either half looks
       like a bug — an SBOM that is plainly there and refused, or an unchecked
       row presented as a result — and it is the flag that decides which is
       correct. There is no third mode in which this shape verifies.

    The SPDX half at the end is not a second CycloneDX run. `--type spdx` is
    cosign's default and writes `text/spdx+json`, a spelling OCX never emits and
    did not read before this work; a reader covering only OCX's own three
    spellings passes every assertion above and fails there.
    """
    cell = Cell(registry=registry, referrers=True, key_mode="key", fmt="simplesigning")
    runner, pkg, subject_digest, ref = _published_subject(ocx, cell, unique_repo, tmp_path)

    # ── 1. The control: nothing attached, nothing listed ──────────────────────
    before = runner.run(
        "package", "sbom", *_sbom_flags(sigstore_stack, verify=False), pkg.short, check=False,
    )
    assert before.returncode == 79, (
        "the control must show this subject carrying no SBOM before cosign attaches one; "
        f"got exit {before.returncode}\nstdout: {before.stdout}\nstderr: {before.stderr}"
    )

    work = tmp_path / "cosign"
    work.mkdir()
    document = attestations.PRETTY_CYCLONEDX_PATH.read_bytes()
    _attach_sbom(work, ref, document, sbom_type="cyclonedx")

    # ── 2. The tag is the only door ───────────────────────────────────────────
    manifest_digest = _assert_the_tag_is_the_only_door(
        cell, pkg.repo, subject_digest, expected_layer_type=COSIGN_CYCLONEDX_MEDIA_TYPE,
    )

    # ── 3. Permissive lists it ────────────────────────────────────────────────
    listed = runner.run(
        "package", "sbom", *_sbom_flags(sigstore_stack, verify=False), pkg.short, check=False,
    )
    assert listed.returncode == 0, (
        f"ocx must list a cosign `.sbom` sidecar\nstdout: {listed.stdout}\nstderr: {listed.stderr}"
    )
    data = json.loads(listed.stdout)["data"]
    assert data["summary"]["verification"] == "unverified"
    [entry] = data["entries"]
    assert entry["verified"] is False, f"nothing signed this document: {entry}"
    assert entry["predicate_type"] == PREDICATE_TYPE_URI, entry
    assert entry["subject_digest"] == subject_digest, entry
    assert entry["referrer_digest"] == manifest_digest, (
        "the row must name the manifest the registry answered the tag with, so an operator "
        f"can fetch it: {entry}"
    )

    extracted = runner.run(
        "package", "sbom", *_sbom_flags(sigstore_stack, verify=False),
        "--output", "-", pkg.short, check=False,
    )
    assert extracted.returncode == 0, extracted.stderr
    assert extracted.stdout.encode() == document, (
        "the document must be the bytes cosign uploaded, verbatim — a read path that "
        "re-serialized it would report something the registry never served"
    )

    # ── 4. Demand refuses it ──────────────────────────────────────────────────
    refused = runner.run(
        "package", "sbom", *_sbom_flags(sigstore_stack, verify=True), pkg.short, check=False,
    )
    assert refused.returncode == 77, (
        "an unsigned sidecar must be refused by a demanded scan, not listed and not reported "
        f"as absent; got exit {refused.returncode}\nstdout: {refused.stdout}\nstderr: {refused.stderr}"
    )
    assert json.loads(refused.stdout)["error"]["detail"] == "unsigned_rejected_by_policy", refused.stdout

    # ── The SPDX half: cosign's DEFAULT type, and a spelling OCX never writes ──
    spdx_document = json.dumps(
        {"spdxVersion": "SPDX-2.3", "name": "ocx-a07", "SPDXID": "SPDXRef-DOCUMENT"}, indent=2,
    ).encode()
    _attach_sbom(work, ref, spdx_document, sbom_type="spdx")
    _assert_the_tag_is_the_only_door(
        cell, pkg.repo, subject_digest, expected_layer_type=COSIGN_SPDX_JSON_MEDIA_TYPE,
    )

    spdx_listed = runner.run(
        "package", "sbom", *_sbom_flags(sigstore_stack, verify=False), pkg.short, check=False,
    )
    assert spdx_listed.returncode == 0, (
        "cosign's DEFAULT `--type spdx` writes `text/spdx+json`, which OCX itself never emits; "
        "a read map covering only OCX's spellings refuses it here\n"
        f"stdout: {spdx_listed.stdout}\nstderr: {spdx_listed.stderr}"
    )
    [spdx_entry] = json.loads(spdx_listed.stdout)["data"]["entries"]
    assert spdx_entry["predicate_type"] == SPDX_PREDICATE_TYPE_URI, spdx_entry
    assert spdx_entry["verified"] is False, spdx_entry
