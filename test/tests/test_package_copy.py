# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package copy` — promoting a published package.

The property the whole feature exists for is digest preservation: the leaf
platform manifest that lands at the target must be byte-identical to the one at
the source, because a Sigstore bundle's subject *is* that digest and an
`ocx.lock` pins it. A promotion that rebuilt the manifest would orphan every
signature and invalidate every pin while reporting success, so the byte-identity
and signature-survival tests below are the load-bearing ones; the rest bound the
ways a copy can be wrong about *which* platforms the target ends up offering.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src.helpers import make_package
from src.registry import (
    fetch_blob,
    fetch_manifest_from_registry,
    fetch_manifest_raw,
    fetch_platform_manifest_digest,
    get_manifest,
    index_platforms,
    push_blob,
    put_manifest,
)
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import cosign_artifacts
from tests.fixtures.sigstore_stack import SigstoreStack


def _copy(
    ocx: OcxRunner,
    target_registry: str,
    *args: str,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx package copy` with both registries marked plain-HTTP.

    The runner's own `OCX_INSECURE_REGISTRIES` names one registry; a copy dials
    two, and the target would otherwise be attempted over HTTPS.
    """
    return ocx.run(
        "package",
        "copy",
        *args,
        check=check,
        env_overlay={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{target_registry}"},
    )


def _dispositions(result: subprocess.CompletedProcess[str]) -> dict[str, str]:
    report = json.loads(result.stdout)
    return {row["platform"]: row["disposition"] for row in report["platforms"]}


def _target_has_tag(target_registry: str, repo: str, tag: str) -> bool:
    """True when `tag` resolves at `target_registry`, false on a 404.

    Narrowed to `ValueError` deliberately: `oras.client.OrasClient.get_manifest`
    raises a plain `ValueError` for any non-2xx response (`_check_200_response`),
    so that is the one exception a missing tag can actually produce here. A
    broader catch would also swallow a connection failure or a bug in the
    fetch helper and silently report "absent" for either.
    """
    try:
        fetch_manifest_from_registry(target_registry, repo, tag)
    except ValueError:
        return False
    return True


# ---------------------------------------------------------------------------
# The load-bearing property: the digest does not move
# ---------------------------------------------------------------------------


def test_copy_across_registries_preserves_the_leaf_bytes_and_digest(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """The target's platform manifest must be the source's bytes, unchanged.

    Compared as raw response bytes and as the digest the registry itself serves
    them under. Comparing parsed documents would pass against a copy that
    re-serialised an equal manifest — which is exactly the defect that would
    orphan every signature.
    """
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert result.returncode == 0, result.stderr

    source_digest = fetch_platform_manifest_digest(ocx.registry, package.repo, package.tag)
    target_digest = fetch_platform_manifest_digest(target_registry, package.repo, package.tag)
    assert target_digest == source_digest

    source_bytes, source_served = fetch_manifest_raw(ocx.registry, package.repo, source_digest)
    target_bytes, target_served = fetch_manifest_raw(target_registry, package.repo, target_digest)
    assert target_bytes == source_bytes, "the leaf manifest must be copied verbatim"
    assert target_served == source_served


def test_a_non_canonical_manifest_is_copied_byte_for_byte(
    ocx: OcxRunner, target_registry: str, unique_repo: str
) -> None:
    """The discriminating byte-identity test: a manifest whose JSON is NOT what
    our own serializer would emit.

    The test above compares an ocx-authored manifest, and a parse-then-reserialize
    round-trip of one of those is byte-stable — so it passes against a copy that
    rebuilds the manifest, and cannot tell the two apart. This one publishes a
    pretty-printed manifest with a foreign key order, which only survives if the
    bytes were genuinely carried rather than re-emitted. That is the property a
    Sigstore subject and an `ocx.lock` pin actually depend on.
    """
    config_digest = push_blob(ocx.registry, unique_repo, b"{}")
    layer_digest = push_blob(ocx.registry, unique_repo, b"a layer, pretend")
    # Indented, and with `layers` written before `config` — neither is what
    # serde_json emits for our manifest type.
    body = json.dumps(
        {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "layers": [
                {
                    "mediaType": "application/octet-stream",
                    "digest": layer_digest,
                    "size": len(b"a layer, pretend"),
                }
            ],
            "config": {
                "mediaType": "application/vnd.oci.empty.v1+json",
                "digest": config_digest,
                "size": 2,
            },
        },
        indent=2,
    ).encode()
    source_digest = put_manifest(
        ocx.registry,
        unique_repo,
        "9.9.9",
        body,
        "application/vnd.oci.image.manifest.v1+json",
    )

    result = _copy(
        ocx,
        target_registry,
        "-i",
        f"{target_registry}/{unique_repo}:9.9.9",
        "--platform",
        current_platform(),
        "--no-referrers",
        f"{unique_repo}:9.9.9",
    )
    assert result.returncode == 0, result.stderr

    copied, served = fetch_manifest_raw(target_registry, unique_repo, source_digest)
    assert copied == body, "the manifest must arrive as the exact bytes the source served"
    assert served == source_digest


def test_a_copied_package_is_installable_from_the_target(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """Digest identity is necessary, not sufficient: the blobs have to arrive
    too. Installing from the target proves the whole chain landed."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    _copy(ocx, target_registry, "--to", target_registry, package.short)

    result = ocx.run(
        "package",
        "install",
        f"{target_registry}/{package.repo}:{package.tag}",
        env_overlay={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{target_registry}"},
    )
    assert result.returncode == 0, result.stderr


# ---------------------------------------------------------------------------
# What the target ends up offering
# ---------------------------------------------------------------------------


def test_copy_merges_into_the_target_index_instead_of_replacing_it(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """A promotion of one platform must not delete a platform the target
    already offers — byte-copying the source index would, and the report has to
    say so out loud rather than leaving a silently mixed index behind."""
    host = current_platform()
    other = "windows/amd64" if not host.startswith("windows") else "linux/amd64"

    package = make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=host)
    # The target already publishes the same tag for a different platform. A
    # separate build directory: the two packages share a repo and tag, so one
    # tmp_path would collide on the bundle directory name.
    target_build = tmp_path / "target-build"
    target_build.mkdir()
    target_runner = OcxRunner(ocx.binary, ocx.ocx_home, target_registry)
    make_package(target_runner, unique_repo, "1.0.0", target_build, platform=other)

    result = _copy(ocx, target_registry, "--to", target_registry, "--platform", host, package.short)
    assert result.returncode == 0, result.stderr

    manifest = fetch_manifest_from_registry(target_registry, package.repo, package.tag)
    assert index_platforms(manifest) == {host, other}

    dispositions = _dispositions(result)
    # `host` is fresh at the target (only `other` was pre-populated there), so
    # the disposition is unambiguously "added" — a tolerance band admitting
    # "replaced" too would pass just as well against a build that always
    # reported "replaced", which is not the contract this test names.
    assert dispositions[host] == "added"
    # WP-B/WP-D are switching `disposition` to a `#[serde(rename_all =
    # "kebab-case")]` enum (subsystem-cli-api.md "Typed Enums Over
    # Strings"); RED until that lands. Spelling per team-lead's message,
    # unconfirmed against a published WP-B artifact at write time.
    assert dispositions[other] == "kept-not-in-source"


def test_copy_within_the_same_registry_to_a_different_repository(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Convergence gap #1 (`plan_package_copy.md`'s convergence check): `--to`
    only ever rewrites the registry host, so landing under a new repository
    name on the *same* host needs `--identifier` instead — a shape distinct
    from every other test here, which all promote across two registries."""
    host = current_platform()
    other = "windows/amd64" if not host.startswith("windows") else "linux/amd64"
    dest_repo = f"{unique_repo}_renamed"

    make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=host)
    other_build = tmp_path / "other-build"
    other_build.mkdir()
    make_package(ocx, unique_repo, "1.0.0", other_build, platform=other)

    result = ocx.run(
        "package",
        "copy",
        "-i",
        f"{ocx.registry}/{dest_repo}:1.0.0",
        f"{unique_repo}:1.0.0",
    )
    assert result.returncode == 0, result.stderr

    manifest = fetch_manifest_from_registry(ocx.registry, dest_repo, "1.0.0")
    assert index_platforms(manifest) == {host, other}


def test_a_second_copy_after_the_source_gains_a_platform_adds_only_the_new_one(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """Convergence gap #6 (`plan_package_copy.md`'s convergence check): a
    source that grows a new platform between two copies must not force the
    target to re-fetch a platform it already has — the first copy's platform
    stays `unchanged` on the second pass, only the newly published one is
    `added`."""
    host = current_platform()
    other = "windows/amd64" if not host.startswith("windows") else "linux/amd64"

    package = make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=host)
    first = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert first.returncode == 0, first.stderr
    assert _dispositions(first) == {host: "added"}

    other_build = tmp_path / "other-build"
    other_build.mkdir()
    make_package(ocx, unique_repo, "1.0.0", other_build, platform=other)

    second = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert second.returncode == 0, second.stderr
    assert _dispositions(second) == {host: "unchanged", other: "added"}

    manifest = fetch_manifest_from_registry(target_registry, package.repo, package.tag)
    assert index_platforms(manifest) == {host, other}


def test_a_repeated_copy_reports_unchanged(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """Promotion has to be safe to re-run: the second pass must recognise the
    target already points at this digest rather than re-uploading it."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    first = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert set(_dispositions(first).values()) == {"added"}
    # Positive control: the first pass must actually have uploaded something,
    # or the second pass's `uploaded == 0` would pass just as well against a
    # copy that never uploads at all.
    assert json.loads(first.stdout)["blobs"]["uploaded"] > 0

    second = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert second.returncode == 0, second.stderr
    assert set(_dispositions(second).values()) == {"unchanged"}
    assert json.loads(second.stdout)["blobs"]["uploaded"] == 0


def test_cascade_is_computed_against_the_target_not_the_source(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """The target already publishes a newer patch, so promoting the older one
    must not drag the rolling tag backwards. Reading the source's tag list
    instead would move `1.0` to the older release."""
    make_package(ocx, unique_repo, "1.0.1", tmp_path)
    older = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    target_build = tmp_path / "target-build"
    target_build.mkdir()
    target_runner = OcxRunner(ocx.binary, ocx.ocx_home, target_registry)
    newer_at_target = make_package(target_runner, unique_repo, "1.0.1", target_build)

    result = _copy(ocx, target_registry, "--to", target_registry, "--cascade", older.short)
    assert result.returncode == 0, result.stderr

    rolling = fetch_platform_manifest_digest(target_registry, older.repo, "1.0")
    newer = fetch_platform_manifest_digest(target_registry, newer_at_target.repo, "1.0.1")
    assert rolling == newer, "the rolling tag must keep pointing at the newer release"


def test_cascade_into_a_target_repository_that_does_not_exist_yet(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """The first promotion of a package must cascade, not fail.

    A repository nobody has pushed to answers the tag listing with a 404, and
    that is the empty tag list, not a failure: none of the rolling tags are
    taken. Propagating it made every *first* `--cascade` promotion exit 79 and
    every second one succeed (#366). Every other copy test pre-seeds the
    target, which is why this never reded.
    """
    package = make_package(ocx, unique_repo, "1.0.1", tmp_path)
    assert not _target_has_tag(target_registry, package.repo, package.tag), (
        "the target repository must be untouched for this to test a first publish"
    )

    result = _copy(ocx, target_registry, "--to", target_registry, "--cascade", package.short)
    assert result.returncode == 0, result.stderr

    pinned = fetch_platform_manifest_digest(target_registry, package.repo, "1.0.1")
    for rolling in ("1.0", "1", "latest"):
        assert fetch_platform_manifest_digest(target_registry, package.repo, rolling) == pinned, (
            f"a first promotion must write the rolling tag {rolling}"
        )


def test_dry_run_reports_the_plan_and_writes_nothing(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    # Positive control for `_target_has_tag`: prove it can observe a tag that
    # genuinely exists, at the source, before trusting its negative below.
    assert _target_has_tag(ocx.registry, package.repo, package.tag)

    result = _copy(ocx, target_registry, "--to", target_registry, "--dry-run", package.short)
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["status"] == "planned"
    assert set(_dispositions(result).values()) == {"added"}
    assert not _target_has_tag(target_registry, package.repo, package.tag), (
        "a dry run must leave the target untouched"
    )


def test_dry_run_leaves_the_targets_existing_index_byte_for_byte_unchanged(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """`--dry-run` skips both write phases (content, then tags/index), not just
    the tag pointer the test above checks. Pre-populate the target with an
    unrelated platform under the same tag, snapshot its raw index bytes, and
    prove a dry-run copy of a *different* platform leaves those bytes and that
    digest completely untouched — the cheap, direct observable for the
    two-phase write order the publisher module doc describes."""
    host = current_platform()
    other = "windows/amd64" if not host.startswith("windows") else "linux/amd64"

    package = make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=host)
    target_build = tmp_path / "target-build"
    target_build.mkdir()
    target_runner = OcxRunner(ocx.binary, ocx.ocx_home, target_registry)
    make_package(target_runner, unique_repo, "1.0.0", target_build, platform=other)

    before_bytes, before_digest = fetch_manifest_raw(target_registry, package.repo, package.tag)

    result = _copy(
        ocx, target_registry, "--to", target_registry, "--dry-run", "--platform", host, package.short
    )
    assert result.returncode == 0, result.stderr

    after_bytes, after_digest = fetch_manifest_raw(target_registry, package.repo, package.tag)
    assert after_bytes == before_bytes, "a dry run must not rewrite the target's existing index at all"
    assert after_digest == before_digest


# ---------------------------------------------------------------------------
# Keep tags
# ---------------------------------------------------------------------------


def test_keep_tag_default_writes_a_digest_named_tag(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """A plain copy also writes an `__ocx.keep.<algorithm>-<hex>` tag pointing
    at the copied leaf digest by default (`--keep-tag` is the affirmative
    spelling of the default), so a pin can still resolve after the mutable tag
    moves on. Paired with the `--no-keep-tag` suppression test below, which
    proves the tag comes from this flag rather than merely from copying at
    all."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert result.returncode == 0, result.stderr

    target_digest = fetch_platform_manifest_digest(target_registry, package.repo, package.tag)
    keep_tag = "__ocx.keep." + target_digest.replace(":", "-")
    assert json.loads(result.stdout)["keep_tags_written"] == [keep_tag]
    assert _target_has_tag(target_registry, package.repo, keep_tag)


def test_no_keep_tag_suppresses_it(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = _copy(ocx, target_registry, "--to", target_registry, "--no-keep-tag", package.short)
    assert result.returncode == 0, result.stderr

    target_digest = fetch_platform_manifest_digest(target_registry, package.repo, package.tag)
    keep_tag = "__ocx.keep." + target_digest.replace(":", "-")
    assert json.loads(result.stdout)["keep_tags_written"] == []
    assert not _target_has_tag(target_registry, package.repo, keep_tag), (
        "--no-keep-tag must suppress the write itself, not just its mention in the report"
    )


# ---------------------------------------------------------------------------
# Signatures
# ---------------------------------------------------------------------------


def test_a_signature_survives_the_promotion(
    ocx: OcxRunner,
    target_registry: str,
    unique_repo: str,
    tmp_path: Path,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Sign at the source, promote, verify at the target.

    This is the whole point of copying rather than rebuilding: the signature's
    subject is the leaf digest, so it stays valid only because the digest did
    not move and the referrer travelled with it.
    """
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    both = f"{ocx.registry},{target_registry}"

    signed = subprocess.run(
        [str(ocx.binary), "package", "sign", *sigstore_stack.sign_args(identity_token), package.short],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_INSECURE_REGISTRIES": both}, check=False,
    )
    assert signed.returncode == 0, signed.stderr

    copied = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert copied.returncode == 0, copied.stderr
    assert json.loads(copied.stdout)["referrers_copied"] >= 1

    verified = subprocess.run(
        [
            str(ocx.binary),
            "package",
            "verify",
            *sigstore_stack.verify_args(),
            f"{target_registry}/{package.repo}:{package.tag}",
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_INSECURE_REGISTRIES": both}, check=False,
    )
    assert verified.returncode == 0, verified.stderr


def test_referrers_against_a_registry_without_the_api_exits_84(
    ocx: OcxRunner, legacy_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """A registry with no Referrers API cannot hold the signature, so a copy
    that was asked to carry referrers refuses (84) rather than silently
    promoting an artifact whose provenance stayed behind.

    Paired with the `--no-referrers` run below, which proves the 84 comes from
    the capability probe and not merely from the target being another host.
    """
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    refused = _copy(ocx, legacy_registry, "--to", legacy_registry, package.short, check=False)
    assert refused.returncode == 84, refused.stderr

    allowed = _copy(
        ocx, legacy_registry, "--to", legacy_registry, "--no-referrers", package.short, check=False
    )
    assert allowed.returncode == 0, allowed.stderr


# ---------------------------------------------------------------------------
# cosign sidecar tags (#376)
# ---------------------------------------------------------------------------


def _push_signed_subject(registry: str, repo: str) -> tuple[str, str, str]:
    """Put cosign's own key-mode simplesigning artifact into ``registry``.

    Returns ``(subject digest, sidecar tag, payload layer digest)``. Every
    signature-bearing byte is the committed capture's — nothing here signs,
    re-canonicalises or re-derives anything, so a promotion that altered one
    byte is caught by the verify at the far end rather than by a fixture that
    was regenerated to agree with it.
    """
    subject, _ = cosign_artifacts.push_subject(registry, repo)
    tag, layer_digest = cosign_artifacts.push_sidecar(
        registry, repo, subject, cosign_artifacts.GOLDEN / "simplesigning_key_manifest.json"
    )
    return subject, tag, layer_digest


def _copy_by_digest(
    ocx: OcxRunner,
    destination: str,
    repo: str,
    subject: str,
    *flags: str,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Promote one leaf manifest by digest. A leaf carries no platform and a
    digest carries no tag, so both have to be declared."""
    return _copy(
        ocx,
        destination,
        *flags,
        "--platform",
        current_platform(),
        "-i",
        f"{destination}/{repo}:promoted",
        f"{repo}@{subject}",
        check=check,
    )


def test_a_cosign_sidecar_signature_survives_the_promotion(
    ocx: OcxRunner, target_registry: str, unique_repo: str
) -> None:
    """S-016 / S-019 — the whole point of #376.

    A cosign `sha256-<hex>.sig` attachment is not a referrer: its manifest
    declares neither ``subject`` nor ``artifactType``, so nothing lists it and
    the referrer walk cannot see it. Before this, promoting such a package
    dropped the signature and exited 0.

    Three things are asserted, and the third is the one an earlier reading of
    this feature would have missed: the sidecar manifest arrives byte-identical,
    its **payload blob** arrives too, and `ocx package verify` against the
    destination alone succeeds. Copying the manifest without the blob would
    publish a signature at the target that resolves to nothing behind it.
    """
    subject, tag, layer_digest = _push_signed_subject(ocx.registry, unique_repo)

    result = _copy_by_digest(ocx, target_registry, unique_repo, subject)
    assert result.returncode == 0, result.stderr

    source_bytes, _ = fetch_manifest_raw(ocx.registry, unique_repo, tag)
    target_bytes, _ = fetch_manifest_raw(target_registry, unique_repo, tag)
    assert target_bytes == source_bytes, (
        "the sidecar must land under the same tag, byte for byte — a re-serialised "
        "manifest changes the digest cosign's own tooling addresses it by"
    )
    assert fetch_blob(target_registry, unique_repo, layer_digest), (
        "the signed payload is a layer blob, not an annotation; without it the "
        "sidecar at the target names bytes nobody transferred"
    )

    verified = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "verify",
            "--rekor-url", cosign_artifacts.DEAD_REKOR_URL,
            "--sigstore-trusted-root", str(cosign_artifacts.TRUST_ROOT),
            "--key", str(cosign_artifacts.GOLDEN / "keys" / "cosign.pub"),
            f"{target_registry}/{unique_repo}@{subject}",
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_INSECURE_REGISTRIES": f"{ocx.registry},{target_registry}"},
        check=False,
    )
    assert verified.returncode == 0, f"stdout: {verified.stdout}\nstderr: {verified.stderr.strip()}"
    [entry] = json.loads(verified.stdout)["data"]["signatures"]
    assert entry["discovery_method"] == "sidecar_tag", entry
    assert entry["signature_format"] == "simplesigning", entry


def test_sidecar_tags_land_on_a_registry_without_the_referrers_api(
    ocx: OcxRunner, legacy_registry: str, unique_repo: str
) -> None:
    """S-017 — and the reason the *position* of the sidecar step is the contract.

    `ensure_target_serves_referrers` refuses a destination with no OCI 1.1
    Referrers API, which is backwards for a mechanism that exists precisely for
    registries lacking one. It also *returns*, so a sidecar step placed after it
    could never run against `registry:2` — and this test would pass by never
    executing.

    So the assertion is deliberately two-sided: the copy still exits 84, proving
    the referrers verdict is unchanged, **and** the sidecar is already at the
    destination, proving the sweep ran before it. The `.att` tag, which the
    source never had, is the control that keeps the positive from being vacuous.
    """
    subject, tag, layer_digest = _push_signed_subject(ocx.registry, unique_repo)

    refused = _copy_by_digest(ocx, legacy_registry, unique_repo, subject, check=False)
    assert refused.returncode == 84, refused.stderr

    assert _target_has_tag(legacy_registry, unique_repo, tag), (
        "the sidecar must have landed before the referrers gate refused the target"
    )
    assert get_manifest(legacy_registry, unique_repo, tag) == get_manifest(ocx.registry, unique_repo, tag)
    assert fetch_blob(legacy_registry, unique_repo, layer_digest)
    assert not _target_has_tag(legacy_registry, unique_repo, tag.replace(".sig", ".att")), (
        "a tag the source never carried must not appear at the destination, or the "
        "assertion above is not observing what it claims to"
    )


def test_no_referrers_copies_no_sidecar_tags(
    ocx: OcxRunner, target_registry: str, unique_repo: str
) -> None:
    """S-024 — one flag governs everything anchored to the leaf.

    `--no-referrers` is how a caller says "content only". A sidecar tag is
    signature material by another name, so it is skipped on the same word.
    """
    subject, tag, _ = _push_signed_subject(ocx.registry, unique_repo)

    result = _copy_by_digest(ocx, target_registry, unique_repo, subject, "--no-referrers")
    assert result.returncode == 0, result.stderr

    assert not _target_has_tag(target_registry, unique_repo, tag)
    assert _target_has_tag(ocx.registry, unique_repo, tag), (
        "control: the sidecar the copy was asked to skip does exist at the source"
    )


# ---------------------------------------------------------------------------
# Usage errors, each caught before the target is contacted
# ---------------------------------------------------------------------------


def test_a_digest_source_without_a_platform_is_a_usage_error(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """A leaf manifest carries no platform, so it has to be declared. The
    refusal must land before the target is contacted — a broken invocation must
    not first authenticate against a production registry."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    digest = fetch_platform_manifest_digest(ocx.registry, package.repo, package.tag)
    source = f"{package.repo}@{digest}"

    result = _copy(
        ocx,
        target_registry,
        "-i",
        f"{target_registry}/{package.repo}:1.0.0",
        source,
        check=False,
    )
    assert result.returncode == 64, result.stderr
    assert "carries no platform" in result.stderr, (
        "the message must name what is wrong, not just the exit code"
    )
    # Positive control for `_target_has_tag`'s negative below: prove it can
    # observe a tag that genuinely exists before trusting that it reports one
    # that should not.
    assert _target_has_tag(ocx.registry, package.repo, package.tag)
    assert not _target_has_tag(target_registry, package.repo, "1.0.0"), (
        "the target must not be written before the arguments are accepted"
    )


def test_a_digest_source_without_an_identifier_is_a_usage_error(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """`--to` preserves the source's tag, and a digest has none."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    digest = fetch_platform_manifest_digest(ocx.registry, package.repo, package.tag)

    result = _copy(
        ocx,
        target_registry,
        "--to",
        target_registry,
        "--platform",
        current_platform(),
        f"{package.repo}@{digest}",
        check=False,
    )
    assert result.returncode == 64, result.stderr
    assert "carries no tag" in result.stderr


def test_a_platform_the_source_does_not_offer_is_a_usage_error(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """RED until WP-D reclassifies
    `crates/ocx_lib/src/publisher/copy.rs::resolve_source_leaves`'s
    no-matching-platform refusal from `ClientError::InvalidManifest` (exit 65)
    to a usage-shaped error. team-lead's decision (relayed after
    `review_r1_spec_package_copy.md` finding A7 /
    `review_r1_testcov_package_copy.md` finding 4): 64 is correct, the code is
    wrong — a caller naming a platform the source does not carry is an
    invocation fault, not malformed registry data. The doc
    (`website/src/docs/reference/command-line.md`'s copy exit-code table)
    already reads 64 and stays there."""
    host = current_platform()
    # A platform a single-platform `make_package(..., platform=host)` build
    # cannot possibly offer. Fixed, deterministic candidate list — never an
    # arbitrary pick, since every entry but the excluded one would do equally.
    candidates = ["linux/amd64", "linux/arm64", "windows/amd64", "darwin/arm64"]
    absent = next(candidate for candidate in candidates if candidate != host)

    package = make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=host)

    result = _copy(
        ocx, target_registry, "--to", target_registry, "--platform", absent, package.short, check=False
    )
    assert result.returncode == 64, result.stderr
    assert not _target_has_tag(target_registry, package.repo, package.tag), (
        "the target must not be written before the platform selection is validated"
    )


def test_a_platform_typo_names_the_platform_not_the_manifest(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """UX review `review_r1_ux_package_copy.md` finding A2: the same defect as
    the previous test, seen from the user's side. Today the error message
    reads as a broken source manifest ("offers no platform matching the
    request") when the actual mistake is a `--platform` typo in the
    invocation — the manifest is fine, the argv is not. Pins the message on
    the value actually typed, and that it does not blame the manifest.
    Exit code and stream asserted separately per TEST-10. RED alongside the
    exit-code test above, same WP-D fix."""
    host = current_platform()
    typo = f"{host}-typo"

    package = make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=host)

    result = _copy(
        ocx, target_registry, "--to", target_registry, "--platform", typo, package.short, check=False
    )
    assert result.returncode == 64, result.stderr
    assert typo in result.stderr, result.stderr
    assert "manifest" not in result.stderr.lower(), (
        f"message must not blame the manifest for a caller-side platform typo: {result.stderr!r}"
    )


def test_an_image_index_named_by_digest_is_a_usage_error(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """An index digest is a snapshot of a mutable set; there is no honest merge
    of "the platform list as it was" into a target that has moved on."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    _, index_digest = fetch_manifest_raw(ocx.registry, package.repo, package.tag)

    result = _copy(
        ocx,
        target_registry,
        "-i",
        f"{target_registry}/{package.repo}:1.0.0",
        "--platform",
        current_platform(),
        f"{package.repo}@{index_digest}",
        check=False,
    )
    assert result.returncode == 64, result.stderr
    assert "names an image index by digest" in result.stderr


def test_to_and_identifier_together_are_a_usage_error(
    ocx: OcxRunner, target_registry: str, published_package: PackageInfo
) -> None:
    result = _copy(
        ocx,
        target_registry,
        "--to",
        target_registry,
        "-i",
        f"{target_registry}/{published_package.repo}:1.0.0",
        published_package.short,
        check=False,
    )
    assert result.returncode == 64, result.stderr
    # clap's own conflict message, not a project string — assert on the two
    # flag spellings it must echo rather than the exact sentence, so this
    # does not pin clap's wording across a version bump.
    assert "--to" in result.stderr
    assert "--identifier" in result.stderr


def test_an_absent_source_tag_is_not_found(
    ocx: OcxRunner, target_registry: str, published_package: PackageInfo
) -> None:
    result = _copy(
        ocx,
        target_registry,
        "--to",
        target_registry,
        f"{published_package.repo}:9.9.9",
        check=False,
    )
    assert result.returncode == 79, result.stderr
    assert "manifest not found" in result.stderr
    assert "9.9.9" in result.stderr


def test_offline_refuses_before_touching_the_network(
    ocx: OcxRunner, target_registry: str, unique_repo: str
) -> None:
    """Convergence gap #9 (`plan_package_copy.md`'s convergence check):
    `--offline` is a deliberate local policy, not a fault — the CLI-contract
    table reserves 81 (`PolicyBlocked`) for exactly this, distinct from the
    64/79 usage/not-found refusals above. The refusal fires before any
    resolution: the source need not even exist at the registry for it to
    trigger, because `--offline` means no remote client is ever constructed."""
    result = ocx.run(
        "--offline",
        "package",
        "copy",
        "--to",
        target_registry,
        f"{unique_repo}:1.0.0",
        check=False,
    )
    assert result.returncode == 81, result.stderr


# ---------------------------------------------------------------------------
# Descriptions
# ---------------------------------------------------------------------------


def test_the_description_travels_only_when_asked(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    """A description is repository-level prose, not part of the version being
    promoted, so a plain copy leaves the target's alone."""
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    readme = tmp_path / "README.md"
    readme.write_text("# Staging catalog page\n")
    described = ocx.run("package", "description", "push", "--readme", str(readme), package.repo)
    assert described.returncode == 0, described.stderr

    plain = _copy(ocx, target_registry, "--to", target_registry, package.short)
    assert plain.returncode == 0, plain.stderr
    assert not _target_has_tag(target_registry, package.repo, "__ocx.desc")
    # A flag that was not passed reports null, so the key is always there to
    # branch on rather than absent on the path a consumer cares about.
    assert json.loads(plain.stdout)["description"] is None

    with_description = _copy(
        ocx, target_registry, "--to", target_registry, "--description", package.short
    )
    assert with_description.returncode == 0, with_description.stderr
    assert _target_has_tag(target_registry, package.repo, "__ocx.desc")
    assert json.loads(with_description.stdout)["description"] == "copied"


def test_describe_from_copies_the_description_alone(
    ocx: OcxRunner, target_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    readme = tmp_path / "README.md"
    readme.write_text("# Staging catalog page\n")
    ocx.run("package", "description", "push", "--readme", str(readme), package.repo)

    result = ocx.run(
        "package",
        "description",
        "push",
        "--from",
        package.repo,
        f"{target_registry}/{package.repo}",
        env_overlay={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{target_registry}"},
    )
    assert result.returncode == 0, result.stderr
    assert _target_has_tag(target_registry, package.repo, "__ocx.desc")
    # The package itself was never promoted — this verb copies prose only.
    assert not _target_has_tag(target_registry, package.repo, package.tag)
