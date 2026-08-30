# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package push --sign`` and the signing modifiers
``--signature-format`` / ``--key`` / ``--rekor-upload`` it shares with
``--sbom``.

Contract source: ``.claude/artifacts/design_spec_cosign_parity.md``
§"User-facing surface", ``ocx package push``, plus the division of labour that
bounds what ``--sign`` covers:

    | What               | Signed by | When                            |
    | Platform manifests | push      | inline, per platform            |
    | Index              | sign      | after the last platform lands   |

``push_manifest_and_merge_tags`` rebuilds the index on every platform push, so
an N-platform package walks through N index digests and signing it inline would
leave N-1 dead signatures. Every assertion here that names a digest therefore
names the *platform manifest*, and one of them asserts it is not the index.

Key mode throughout, and deliberately: a key pair contacts neither Fulcio nor
(without ``--rekor-upload``) Rekor, so these tests drive the real signing
pipeline end to end without the ``sigstore`` compose profile. The keyless
rows here are the ones that must refuse *before* the push, which needs no
network either.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
import urllib.error
import urllib.request

from src.helpers import make_package, resolved_metadata_path
from src.registry import get_blob, list_referrers, referrers_fallback_tag
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import attestations

#: A cosign-format private key and its password. The same pair
#: ``test_sign.py`` signs with, so a key-mode push and a key-mode ``sign``
#: exercise one backend.
COSIGN_KEY = Path(__file__).parent / "fixtures" / "golden" / "keys" / "cosign.key"
KEY_PASSWORD = {"OCX_KEY_PASSWORD": "ocxtest"}


# ---------------------------------------------------------------------------
# Shared plumbing
# ---------------------------------------------------------------------------


def push_argv(
    pkg: PackageInfo, tmp_path: Path, extra: list[str], identifier: str | None = None
) -> list[str]:
    """A full ``package push`` argv for a package ``make_package`` built.

    ``-m`` must point at the sidecar ``create`` wrote next to the bundle -- the
    file carrying the resolved dependency pins -- so it is reconstructed from
    what ``make_package`` left behind rather than re-derived. Flags precede the
    positional layers, which is this project's CLI grammar.
    """
    bundles = sorted(tmp_path.glob("bundle-*.tar.xz"))
    assert bundles, f"make_package left no bundle in {tmp_path}"
    return [
        "package", "push",
        "-p", current_platform(),
        "-m", str(resolved_metadata_path(bundles[0])),
        *extra,
        "-i", identifier or pkg.fq,
        str(bundles[0]),
    ]


def push(
    ocx: OcxRunner,
    pkg: PackageInfo,
    tmp_path: Path,
    extra: list[str] | None = None,
    *,
    env_overrides: dict[str, str] | None = None,
    identifier: str | None = None,
):
    """Re-push ``pkg`` with ``extra`` flags, returning the raw result."""
    return ocx.run(
        *push_argv(pkg, tmp_path, extra or [], identifier),
        check=False,
        env_overrides=env_overrides,
    )


def push_report(ocx: OcxRunner, pkg: PackageInfo, tmp_path: Path, extra: list[str], **kwargs) -> dict:
    """Re-push and hand back the parsed ``--format json`` report."""
    result = push(ocx, pkg, tmp_path, extra, **kwargs)
    assert result.returncode == 0, f"push failed ({result.returncode})\nstdout: {result.stdout}\nstderr: {result.stderr}"
    return json.loads(result.stdout)


def manifest_status(registry: str, repo: str, ref: str) -> int:
    """The HTTP status a manifest GET returns -- 404 when the ref is absent.

    A bare ``fetch_manifest_raw`` raises on any non-200 and so cannot tell
    "absent" from "the registry is down", which is exactly the distinction an
    assertion that *nothing was written* rests on.
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


def assert_no_signature(ocx: OcxRunner, repo: str, subject_digest: str) -> None:
    """Nothing signs ``subject_digest``, in any of the three places one could.

    The Referrers API is where a bundle lands on a registry that serves it
    (zot does), the ``sha256-<hex>`` fallback index is where it lands on one
    that does not, and ``sha256-<hex>.sig`` is the cosign simplesigning
    sidecar. Asserting only the first would pass for an implementation that
    wrote to either of the others.
    """
    status, index = list_referrers(ocx.registry, repo, subject_digest)
    assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
    assert index["manifests"] == [], f"an unsigned push left referrers behind: {index['manifests']!r}"

    fallback = referrers_fallback_tag(subject_digest)
    assert manifest_status(ocx.registry, repo, fallback) == 404, (
        f"the referrers fallback tag {fallback} exists on an unsigned push"
    )

    algorithm, encoded = subject_digest.split(":", 1)
    sidecar = f"{algorithm}-{encoded}.sig"
    assert manifest_status(ocx.registry, repo, sidecar) == 404, (
        f"the simplesigning sidecar tag {sidecar} exists on an unsigned push"
    )


def sole_platform_digest(report: dict) -> tuple[str, str]:
    """The one ``(platform, digest)`` pair a single-platform push records."""
    digests = report["platform_digests"]
    assert len(digests) == 1, f"a single-platform push records one manifest, got {digests!r}"
    return next(iter(digests.items()))


# ---------------------------------------------------------------------------
# `--sign` is opt-in
# ---------------------------------------------------------------------------


def test_push_without_sign_writes_no_signature(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A push with no ``--sign`` signs nothing, and says nothing about signing.

    The spec makes inline signing opt-in, so the absence has to be provable in
    the registry and not merely absent from the report -- a report key can be
    omitted by a serializer while a referrer sits on the manifest.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    report = push_report(ocx, pkg, tmp_path, [])

    assert "signatures" not in report, f"an unsigned push must not report signatures: {report!r}"
    _, digest = sole_platform_digest(report)
    assert_no_signature(ocx, pkg.repo, digest)


def test_push_sign_writes_one_signature_per_platform_manifest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--sign`` publishes a bundle referrer on each platform manifest.

    The row count is asserted against ``platform_digests`` rather than against
    a literal 1: the loop is over what the push recorded, and a run that signed
    the first platform and stopped would still satisfy "at least one".
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    report = push_report(
        ocx, pkg, tmp_path, ["--sign", "--key", str(COSIGN_KEY)], env_overrides=KEY_PASSWORD
    )

    assert len(report["signatures"]) == len(report["platform_digests"]), (
        f"one row per pushed platform manifest: {report['signatures']!r} vs {report['platform_digests']!r}"
    )
    for row in report["signatures"]:
        assert row["status"] == "completed", row
        subject = row["report"]["subject_digest"]
        assert subject == report["platform_digests"][row["platform"]], (
            f"row {row['platform']} signed {subject}, not the manifest that push landed on"
        )
        status, index = list_referrers(ocx.registry, pkg.repo, subject)
        assert status == 200, f"zot must serve the Referrers API, got HTTP {status}"
        assert row["report"]["legs"][0]["manifest_digest"] in {
            item["digest"] for item in index["manifests"]
        }, f"the bundle the report names is not a referrer of {subject}"


def test_push_sign_signs_the_platform_manifest_never_the_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The signed subject is the platform manifest, not the image index.

    The division of labour rests on this: the index digest is rewritten on
    every platform merge, so a signature naming it would be dead the moment a
    second platform landed. ``manifest_digest`` in the same report *is* the
    index, which is what makes this comparison able to fail.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    report = push_report(
        ocx, pkg, tmp_path, ["--sign", "--key", str(COSIGN_KEY)], env_overrides=KEY_PASSWORD
    )

    platform, platform_digest = sole_platform_digest(report)
    [row] = report["signatures"]
    assert row["platform"] == platform
    assert row["report"]["subject_digest"] == platform_digest
    assert row["report"]["subject_digest"] != report["manifest_digest"], (
        "push signed the image index; every later platform merge would strand that signature"
    )
    # The index itself is left for `ocx package sign --tags-file` to sweep.
    assert_no_signature(ocx, pkg.repo, report["manifest_digest"])


def test_push_sign_reports_the_key_model_and_the_absent_transparency_record(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A key-mode inline signature says it is a key, and says no record was made.

    ``transparency_log_index`` is asserted ``None`` rather than merely absent:
    under a key ``--rekor-upload`` is opt-in, so a missing Rekor entry has to be
    a fact the operator can see rather than one they infer from a missing key.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    report = push_report(
        ocx, pkg, tmp_path, ["--sign", "--key", str(COSIGN_KEY)], env_overrides=KEY_PASSWORD
    )

    [row] = report["signatures"]
    signature = row["report"]
    assert signature["key_backend"] == "file"
    assert signature["signer"] == "file", "signer must not still say keyless-fulcio under a key"
    assert signature["public_key_hint"], "a key-mode signature reports the key's cosign hint"
    assert signature["transparency_log_index"] is None, (
        "no --rekor-upload means no transparency record, reported as null"
    )

    bundle = json.loads(get_blob(ocx.registry, pkg.repo, signature["legs"][0]["payload_digest"]))
    material = bundle["verificationMaterial"]
    assert material["publicKey"]["hint"] == signature["public_key_hint"]
    assert "certificate" not in material, "a key-mode bundle carries no Fulcio leaf"


def test_push_sign_honours_the_signature_format(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--signature-format simplesigning`` writes the sidecar, not a referrer.

    The flag reaching the inline signing is the point: a run that ignored it
    would write a bundle referrer and leave the ``sha256-<hex>.sig`` tag absent,
    which is the exact pair this asserts in both directions.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    report = push_report(
        ocx,
        pkg,
        tmp_path,
        ["--sign", "--key", str(COSIGN_KEY), "--signature-format", "simplesigning"],
        env_overrides=KEY_PASSWORD,
    )

    [row] = report["signatures"]
    assert [entry["format"] for entry in row["report"]["legs"]] == ["simplesigning"]

    subject = row["report"]["subject_digest"]
    algorithm, encoded = subject.split(":", 1)
    assert manifest_status(ocx.registry, pkg.repo, f"{algorithm}-{encoded}.sig") == 200, (
        "the simplesigning leg must write the cosign sidecar tag"
    )
    status, index = list_referrers(ocx.registry, pkg.repo, subject)
    assert status == 200
    assert index["manifests"] == [], (
        "simplesigning writes the sidecar only; a bundle referrer means the format flag was ignored"
    )


# ---------------------------------------------------------------------------
# The cross-flag rule
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "modifier",
    [
        pytest.param(["--key", str(COSIGN_KEY)], id="key"),
        pytest.param(["--signature-format", "simplesigning"], id="signature-format"),
        pytest.param(["--rekor-upload"], id="rekor-upload"),
        pytest.param(["--no-rekor-upload"], id="no-rekor-upload"),
    ],
)
def test_a_signing_modifier_without_sign_or_sbom_is_a_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, modifier: list[str]
) -> None:
    """A flag that does nothing is the failure mode the spec rejects everywhere.

    Exit 64, and the message names both flags that would give the modifier a
    meaning -- not just one of them, which is what a ``requires = "sign"`` would
    have produced.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    result = push(ocx, pkg, tmp_path, modifier, env_overrides=KEY_PASSWORD)

    assert result.returncode == 64, (
        f"expected a usage error, got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "--sign" in result.stderr and "--sbom" in result.stderr, (
        f"the refusal must name both admitting flags, got: {result.stderr!r}"
    )


def test_a_signing_modifier_is_accepted_alongside_sbom(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--sbom`` admits the modifiers on its own -- ``--sign`` is not required.

    The other half of the rule above, and the half a one-flag ``requires``
    would have got wrong. Asserted on a run that actually lands, so "accepted"
    means the flags reached the attestation rather than merely parsing.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = tmp_path / "sbom.cdx.json"
    sbom.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())

    report = push_report(
        ocx,
        pkg,
        tmp_path,
        ["--sbom", str(sbom), "--key", str(COSIGN_KEY)],
        env_overrides=KEY_PASSWORD,
    )
    assert "signatures" not in report, "--sbom alone signs no platform manifest"
    assert report["attestation"]["status"] == "succeeded"


def test_push_sbom_with_a_key_produces_a_key_mode_attestation(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``push --sbom --key`` signs with the key, not keyless.

    ``push --sbom`` used to hard-code ``key: None, rekor_upload: true`` because
    push carried none of the key-mode flags. It carries them now, so the
    published bundle must carry a public key and no Fulcio certificate -- with
    no Sigstore stack running at all, which a keyless run could not manage.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = tmp_path / "sbom.cdx.json"
    sbom.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())

    report = push_report(
        ocx,
        pkg,
        tmp_path,
        ["--sbom", str(sbom), "--key", str(COSIGN_KEY)],
        env_overrides=KEY_PASSWORD,
    )
    attestation = report["attestation"]
    assert attestation["status"] == "succeeded"
    assert attestation["signed"] is True, "a key-mode attestation is signed, not attached raw"

    referrer = json.loads(
        get_blob(
            ocx.registry,
            pkg.repo,
            json.loads(
                urllib.request.urlopen(
                    urllib.request.Request(
                        f"http://{ocx.registry}/v2/{pkg.repo}/manifests/{attestation['referrer_digest']}",
                        headers={"Accept": "application/vnd.oci.image.manifest.v1+json"},
                    ),
                    timeout=10,
                ).read()
            )["layers"][0]["digest"],
        )
    )
    material = referrer["verificationMaterial"]
    assert "publicKey" in material, f"a key-mode attestation carries a public key: {material.keys()}"
    assert "certificate" not in material, (
        "a Fulcio leaf means the attestation ran keyless and the --key flag was dropped"
    )


def test_a_plain_push_never_resolves_the_sigstore_endpoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A push that asks for no signing must not fail on signing configuration.

    The endpoint ladder is resolved before the push, so a rejected
    `[trust.sigstore]` URL costs no upload -- but only when signing was
    actually asked for. Resolving it unconditionally would make a plain
    `ocx package push` start failing on a config key it never reads, which is
    a regression a signing test could never notice.

    The same hostile config carries both halves: the plain push must land, and
    `--sign` against it must be refused. One of the two passing alone would be
    satisfied by a gate stuck in either position.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    # Plain http is what the SSRF guard refuses, on the config tier exactly as
    # on a flag.
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        '[trust.sigstore]\nfulcio_url = "http://fulcio.corp.example"\n'
    )

    plain = push(ocx, pkg, tmp_path, [])
    assert plain.returncode == 0, (
        f"a plain push must ignore [trust.sigstore]\nstdout: {plain.stdout}\nstderr: {plain.stderr}"
    )

    signed = push(ocx, pkg, tmp_path, ["--sign", "--key", str(COSIGN_KEY)], env_overrides=KEY_PASSWORD)
    assert signed.returncode == 64, (
        f"--sign must reach the endpoint guard, got {signed.returncode}\nstdout: {signed.stdout}"
    )
    assert json.loads(signed.stdout)["error"]["detail"] == "invalid_endpoint_url", signed.stdout


def test_push_sign_keyless_with_no_rekor_upload_is_refused(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Keyless plus ``--no-rekor-upload`` is an error, never a silent no-op.

    A Fulcio certificate is valid for about ten minutes, and the Rekor
    timestamp is the only lasting proof the signature was made while it was.

    Two things are pinned. The refusal reaches the caller under the frozen
    ``rekor_upload_required_for_keyless`` slug, and it costs no upload: the
    identifier names a closed port, so a run that pushed first would have
    surfaced a connection failure under some other slug instead.

    The exit code is asserted against the literal 64 **and** against what
    ``ocx package sign`` returns for the same refusal: the two commands share
    one resolver and must not diverge, and parity alone was satisfied while
    both answered 1. A tolerated ``!= 0`` cannot tell 1 from 64, which is
    exactly how the wrong code stayed pinned -- a bare ``SignErrorKind``
    reached ``anyhow`` unwrapped, so ``classify_error`` never saw it and the
    variant's own ``exit_code()`` of 64 was unreachable from every command that
    writes a signature.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    result = push(
        ocx, pkg, tmp_path, ["--sign", "--no-rekor-upload"],
        identifier="localhost:1/refused/pkg:1.0.0",
    )

    assert result.returncode == 64, f"the refusal must exit 64\nstdout: {result.stdout}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "rekor_upload_required_for_keyless", envelope
    assert envelope["error"]["kind"] == "usage_error", envelope
    assert envelope["error"]["context"]["identifier"] == "localhost:1/refused/pkg:1.0.0", envelope

    signed = ocx.run(
        "package", "sign", "--no-rekor-upload", "localhost:1/refused/pkg:1.0.0", check=False
    )
    assert json.loads(signed.stdout)["error"]["detail"] == "rekor_upload_required_for_keyless"
    assert signed.returncode == 64, f"sign must exit 64 too\nstdout: {signed.stdout}"
    assert result.returncode == signed.returncode, (
        f"push exits {result.returncode} where sign exits {signed.returncode} for one shared refusal"
    )


def test_an_unimplemented_key_backend_exits_85_from_sign_attest_and_push(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ExitCode::UnsupportedKeyBackend = 85`` is reachable from every writer.

    85 and its ``unsupported_key_backend`` category exist only so a script can
    ``case $?`` on "this backend is recognised but not built yet". They were
    reachable from ``verify`` alone: ``sign``, ``attest`` and ``push`` returned
    the refusal as a bare ``SignErrorKind``, which ``classify_error`` cannot
    downcast, so all three exited 1 as ``internal`` and dropped
    ``context.identifier`` with it.

    ``awskms://alias/foo`` is a recognised scheme with no implementation, so it
    is refused while parsing the reference -- before any registry contact,
    which is why the identifier can name a closed port and the three rows still
    compare. ``verify`` rides along as the control: it was already correct, so
    a regression that flattened the whole taxonomy would take it down too.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    unimplemented = "awskms://alias/foo"
    reference = "localhost:1/refused/pkg:1.0.0"

    runs = {
        "sign": ocx.run("package", "sign", "--key", unimplemented, reference, check=False),
        "attest": ocx.run(
            "package", "attest",
            "--key", unimplemented,
            "--predicate", str(predicate),
            "--type", "cyclonedx",
            reference,
            check=False,
        ),
        "verify": ocx.run("package", "verify", "--key", unimplemented, reference, check=False),
        "push": push(
            ocx, pkg, tmp_path,
            ["--sign", "--key", unimplemented],
            identifier=reference,
        ),
    }

    for command, result in runs.items():
        envelope = json.loads(result.stdout)
        assert result.returncode == 85, (
            f"`{command}` exits {result.returncode} for an unimplemented key backend\n{result.stdout}"
        )
        assert envelope["exit_code"] == 85, f"`{command}` envelope disagrees with $?: {envelope}"
        assert envelope["error"]["kind"] == "unsupported_key_backend", f"`{command}`: {envelope}"
        assert envelope["error"]["detail"] == "unsupported_key_backend", f"`{command}`: {envelope}"
        assert envelope["error"]["context"]["identifier"] == reference, (
            f"`{command}` lost the identifier, which only a bare kind can do: {envelope}"
        )


def test_push_sbom_under_simplesigning_reports_the_sidecar_and_succeeds(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--sbom --signature-format simplesigning`` publishes the ``.att`` sidecar.

    That format writes the ``sha256-<hex>.att`` sidecar and **no** referrer
    manifest, which is a published attestation and not a failure. ``push`` read
    the absent referrer as one: it reported ``attestation.status = failed``,
    logged the miss to stderr, and folded a non-zero code into the run -- while
    the sidecar sat in the registry. The combination is admitted by the flag
    grammar and had no test.

    The default half runs beside it against the same package, so neither a
    permanently-absent ``referrer_digest`` nor a permanently-absent
    ``sidecar_digest`` can satisfy this on its own.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sbom = tmp_path / "sbom.cdx.json"
    sbom.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())
    key_args = ["--sbom", str(sbom), "--key", str(COSIGN_KEY)]

    # Half 1 — the default format: a referrer, no sidecar.
    default = push_report(ocx, pkg, tmp_path, key_args, env_overrides=KEY_PASSWORD)["attestation"]
    assert default["status"] == "succeeded", default
    assert "sidecar_digest" not in default, f"the default wrote a sidecar: {default}"
    assert manifest_status(ocx.registry, pkg.repo, default["referrer_digest"]) == 200, default

    # Half 2 — the same package under `--signature-format simplesigning`.
    sidecar = push_report(
        ocx, pkg, tmp_path,
        [*key_args, "--signature-format", "simplesigning"],
        env_overrides=KEY_PASSWORD,
    )["attestation"]
    assert sidecar["status"] == "succeeded", (
        f"the sidecar is the published attestation, not a failed referrer: {sidecar}"
    )
    assert "referrer_digest" not in sidecar, (
        f"simplesigning publishes no referrer bundle: {sidecar}"
    )
    assert manifest_status(ocx.registry, pkg.repo, sidecar["sidecar_digest"]) == 200, (
        f"the reported sidecar digest names no manifest in the registry: {sidecar}"
    )
