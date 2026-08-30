"""Acceptance tests for the `ocx package push --format json` report.

Every test here pushes a real bundle to the live registry fixture and asserts
on the parsed report, so each one needs a registry. The report is a wire
contract: `ocx-mirror pipeline push` keys its go/no-go bookkeeping off
`status` and records `cascade_tags_written` in its run summary, and
`platform_digests` is the signing input a later `push --sign` covers.

No test in this module may gate an assertion on the exit code of the command
under test. Six of them once did — `if result.returncode != 0: pytest.skip(...)`
— and skipped on a malformed command line for as long as they existed, so the
whole report contract went unasserted while reporting green.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from src.helpers import make_package, resolved_metadata_path
from src.registry import (
    fetch_manifest_digest,
    fetch_platform_manifest_digest,
    make_client,
)
from src.runner import OcxRunner, current_platform

# ---------------------------------------------------------------------------
# Shared: build one bundle, push it, hand back the `--format json` report.
# ---------------------------------------------------------------------------


def _push_json(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    *,
    platform: str,
    cascade: bool = False,
    extra_push_args: list[str] | None = None,
) -> dict:
    """Create a one-layer bundle for ``platform`` and push it, returning the
    ``--format json`` report.

    ``make_package`` pushes through ``ocx.plain`` and so discards the report;
    every test in this module asserts on the report itself, which is why the
    create/push pair is spelled out here rather than reused.

    Creation is skipped when the bundle is already on disk, so calling this
    twice with identical arguments pushes the *same* bundle — which is what a
    re-push assertion needs. The guard is required, not merely convenient:
    ``ocx package create`` refuses an existing ``-o`` without ``--force``
    (``package_create.rs``), so a second unguarded call would abort before
    reaching the push.
    """
    stem = f"{repo.replace('/', '_')}-{platform.replace('/', '-')}-{tag}"
    bundle = tmp_path / f"{stem}.tar.xz"

    if not bundle.exists():
        layer_dir = tmp_path / f"content-{stem}"
        (layer_dir / "bin").mkdir(parents=True)
        (layer_dir / "bin" / "hello").write_text(f"#!/bin/sh\necho {stem}\n")

        metadata_path = tmp_path / f"{stem}-metadata-in.json"
        metadata_path.write_text(json.dumps({
            "type": "bundle",
            "version": 1,
            "env": [
                {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
            ],
        }))

        ocx.plain(
            "package", "create", "-m", str(metadata_path), "-o", str(bundle),
            "-p", platform, str(layer_dir),
        )

    args = ["package", "push", "-p", platform, "-m", str(resolved_metadata_path(bundle))]
    if cascade:
        args.append("--cascade")
    args += extra_push_args or []
    args += ["-i", f"{ocx.registry}/{repo}:{tag}", str(bundle)]
    return ocx.json(*args)


# ---------------------------------------------------------------------------
# The `--format json` push report contract.
#
# `identifier`, `status`, `manifest_digest`, `cascade_tags_written` and
# `keep_tags_written` are what `ocx-mirror pipeline push` parses;
# `platform_digests` is the signing input a later `push --sign` covers. Every
# test below pushes for real and asserts against what the registry serves.
# ---------------------------------------------------------------------------

#: The exact key set of a push report with no `--sbom` (`attestation` and an
#: empty `platform_digests` are both `skip_serializing_if`-omitted).
_REPORT_KEYS = {
    "identifier",
    "status",
    "manifest_digest",
    "cascade_tags_written",
    "keep_tags_written",
    "layers",
    "platform_digests",
}


def _expected_platform_digests(ocx: OcxRunner, repo: str, tag: str, platform: str) -> dict:
    """What `platform_digests` must be for a single-platform push of ``tag``:
    the platform manifest the registry actually serves, keyed by platform."""
    return {platform: fetch_platform_manifest_digest(ocx.registry, repo, tag, platform=platform)}


def test_push_report_json_schema_has_required_fields(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The report carries exactly `_REPORT_KEYS` — no key missing, none added.

    A closed set rather than membership checks: `ocx-mirror pipeline push`
    parses this document, so a key appearing is as much a contract change as
    one vanishing, and only the closed form catches the first.
    """
    plat = current_platform()
    report = _push_json(ocx, unique_repo, "1.0.0", tmp_path, platform=plat)

    assert set(report) == _REPORT_KEYS, report
    assert report["platform_digests"] == _expected_platform_digests(
        ocx, unique_repo, "1.0.0", plat
    ), report


def test_push_report_cascade_tags_written_is_array(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`cascade_tags_written` is the list of rolling tags the push really wrote.

    Checked against the registry's own tag list, not merely type-checked: a
    report that named tags it never wrote, or omitted ones it did, is what a
    downstream announce would act on.
    """
    plat = current_platform()
    report = _push_json(ocx, unique_repo, "3.28.1", tmp_path, platform=plat, cascade=True)

    written = report["cascade_tags_written"]
    assert isinstance(written, list), f"cascade_tags_written must be an array, got {written!r}"

    in_registry = set(make_client(ocx.registry).get_tags(f"{ocx.registry}/{unique_repo}"))
    rolling = {tag for tag in in_registry if not tag.startswith("__ocx")} - {"3.28.1"}
    assert rolling, f"a cascade push of 3.28.1 must write rolling tags; registry has {in_registry}"
    # Element type is pinned by the comparison above: every member of `rolling`
    # is a `str` from `get_tags`, and no other JSON scalar compares equal to one.
    assert set(written) == rolling, f"reported {written}, registry holds {sorted(in_registry)}"

    assert report["platform_digests"] == _expected_platform_digests(
        ocx, unique_repo, "3.28.1", plat
    ), report


def test_push_report_platform_digest_is_no_tag_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The reported platform digest equals the index digest of no tag this push
    wrote — not the version tag's, not any rolling tag's.

    Every one of those indexes is rewritten by the next platform merge, which
    is exactly what a signature must not be pinned to. `status` is asserted
    here too: it is the constant `"pushed"` that `ocx-mirror` keys its go/no-go
    off, and the command has no `skipped_existing` state.
    """
    plat = current_platform()
    report = _push_json(ocx, unique_repo, "3.28.1", tmp_path, platform=plat, cascade=True)

    assert report["status"] == "pushed", report

    platform_digest = report["platform_digests"][plat]
    assert report["cascade_tags_written"], (
        f"a cascade push of 3.28.1 must write rolling tags; report={report}"
    )
    tag_indexes = {
        tag: fetch_manifest_digest(ocx.registry, unique_repo, tag)
        for tag in ["3.28.1", *report["cascade_tags_written"]]
    }
    assert platform_digest not in tag_indexes.values(), (
        f"platform digest {platform_digest} is a tag's index digest: {tag_indexes}"
    )


def test_push_report_non_cascade_has_empty_cascade_tags(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A push without `--cascade` writes no rolling tag, still writes its keep
    tag, and still reports its platform digest — the three are independent."""
    plat = current_platform()
    report = _push_json(ocx, unique_repo, "1.0.0", tmp_path, platform=plat)

    assert report["cascade_tags_written"] == [], (
        "Non-cascade push must emit cascade_tags_written: [] (empty array)"
    )
    assert report["keep_tags_written"], "the keep tag is on by default"
    assert report["platform_digests"] == _expected_platform_digests(
        ocx, unique_repo, "1.0.0", plat
    ), report


def test_push_report_repush_reports_the_same_platform_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Pushing the identical bundle twice reports `pushed` both times and the
    same platform digest.

    The manifest is content-addressed, so nothing about the second run may move
    it. Both sides are anchored to what the registry serves — comparing the two
    reports to each other alone would survive any uniform defect.
    """
    plat = current_platform()
    first = _push_json(ocx, unique_repo, "1.0.0", tmp_path, platform=plat)
    second = _push_json(ocx, unique_repo, "1.0.0", tmp_path, platform=plat)
    expected = _expected_platform_digests(ocx, unique_repo, "1.0.0", plat)

    assert first["status"] == "pushed" and second["status"] == "pushed", (first, second)
    assert first["platform_digests"] == expected, first
    assert second["platform_digests"] == expected, second


def test_push_report_cascade_with_no_keep_tag_still_reports_platform_digests(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--cascade` and `--no-keep-tag` in one push: rolling tags written, no
    keep tag written, platform digests reported in full.

    All three are independent, and `platform_digests` in particular cannot be
    derived from the keep tags this push deliberately does not write.
    """
    plat = current_platform()
    report = _push_json(
        ocx, unique_repo, "3.28.1", tmp_path,
        platform=plat, cascade=True, extra_push_args=["--no-keep-tag"],
    )

    assert report["keep_tags_written"] == [], report
    assert report["cascade_tags_written"], "a cascade push must still write rolling tags"
    assert report["platform_digests"] == _expected_platform_digests(
        ocx, unique_repo, "3.28.1", plat
    ), report


# ---------------------------------------------------------------------------
# Keep tag — adr_index_indirection.md Decision E
#
# `--[no-]keep-tag`, default ON: after each platform manifest is pushed,
# additionally push an `__ocx.keep.<algorithm>-<hex>` tag pointing directly at
# it (registry-side deletion safety net — a stray rolling/cascade tag delete
# can never orphan a digest a lock still pins).
# ---------------------------------------------------------------------------


def test_push_default_creates_keep_tag(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """Default `ocx package push` (no flag) pushes the keep tag."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=pkg.platform
    )
    keep_tag = "__ocx.keep." + platform_digest.replace(":", "-")

    keep_digest = fetch_manifest_digest(ocx.registry, pkg.repo, keep_tag)
    assert keep_digest == platform_digest, (
        f"keep tag {keep_tag!r} must point at the platform manifest "
        f"digest {platform_digest!r}, got {keep_digest!r}"
    )


def test_push_no_keep_tag_suppresses_the_extra_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--no-keep-tag` must not push the keep tag."""
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        cascade=False,
        extra_push_args=["--no-keep-tag"],
    )

    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=pkg.platform
    )
    keep_tag = "__ocx.keep." + platform_digest.replace(":", "-")

    with pytest.raises(RuntimeError):
        fetch_manifest_digest(ocx.registry, pkg.repo, keep_tag)


def test_push_cascade_tags_only_the_pushed_platform_once(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--cascade` merges the platform into every rolling tag, but the
    keep tag is pushed exactly once — for the platform manifest this
    invocation actually pushed, never retroactively for tags or platforms
    already sitting in the registry.
    """
    pkg = make_package(ocx, unique_repo, "3.28.1", tmp_path, cascade=True)

    platform_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag, platform=pkg.platform
    )
    keep_tag = "__ocx.keep." + platform_digest.replace(":", "-")

    tags = make_client(ocx.registry).get_tags(f"{ocx.registry}/{pkg.repo}")
    keep_tags = [t for t in tags if t.startswith("__ocx.keep.")]
    assert keep_tags == [keep_tag], f"expected exactly one keep tag {keep_tag!r}, got {keep_tags}"


# ---------------------------------------------------------------------------
# `platform_digests` — the per-platform manifest digests a push landed on.
#
# `manifest_digest` names the tag's image index, which is rewritten by the next
# platform merge. `platform_digests` names the immutable platform manifests, so
# it is what a signature can cover — and it is independent of `--keep-tag`.
# ---------------------------------------------------------------------------


def test_push_report_platform_digest_is_the_manifest_not_the_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-001: a single-platform push reports one `platform_digests` entry, keyed
    by the canonical platform string, whose value is the platform manifest's
    digest — not the index digest `manifest_digest` already carries."""
    plat = current_platform()
    report = _push_json(ocx, unique_repo, "1.0.0", tmp_path, platform=plat)

    expected = fetch_platform_manifest_digest(
        ocx.registry, unique_repo, "1.0.0", platform=plat
    )
    assert report["platform_digests"] == {plat: expected}, report
    assert report["platform_digests"][plat] != report["manifest_digest"], (
        "the platform manifest digest must differ from the index digest"
    )


def test_push_report_platform_digests_distinguish_two_platforms_on_one_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-002: two `--cascade` pushes onto one tag each report their own platform
    manifest, never the shared index.

    Two platforms are what makes this discriminating: under one platform the
    index and the manifest are still distinct values, but nothing distinguishes
    "the manifest of the platform this push landed" from "some manifest".
    """
    amd = _push_json(
        ocx, unique_repo, "3.28.1", tmp_path, platform="linux/amd64", cascade=True
    )
    arm = _push_json(
        ocx, unique_repo, "3.28.1", tmp_path, platform="linux/arm64", cascade=True
    )

    assert list(amd["platform_digests"]) == ["linux/amd64"], amd
    assert list(arm["platform_digests"]) == ["linux/arm64"], arm

    amd_digest = amd["platform_digests"]["linux/amd64"]
    arm_digest = arm["platform_digests"]["linux/arm64"]
    assert amd_digest != arm_digest, "distinct platforms have distinct manifests"
    assert arm_digest != arm["manifest_digest"], (
        "the second push must report its platform manifest, not the merged index"
    )

    for plat, reported in (("linux/amd64", amd_digest), ("linux/arm64", arm_digest)):
        assert reported == fetch_platform_manifest_digest(
            ocx.registry, unique_repo, "3.28.1", platform=plat
        ), f"{plat} digest must match what the registry serves"


def test_push_report_platform_digests_survive_no_keep_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-003: `--no-keep-tag` writes no keep tag and still reports every
    platform digest — the two are independent, so a report derived from the
    keep tags would be empty here."""
    plat = current_platform()
    report = _push_json(
        ocx, unique_repo, "1.0.0", tmp_path,
        platform=plat, extra_push_args=["--no-keep-tag"],
    )

    assert report["keep_tags_written"] == [], report
    expected = fetch_platform_manifest_digest(
        ocx.registry, unique_repo, "1.0.0", platform=plat
    )
    assert report["platform_digests"] == {plat: expected}, report
