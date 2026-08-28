# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package push --announce-file` -> `ocx package announce --tags-from-file`
integration (design register C2, cross-track contract #2).

`push --announce-file` writes a comma-joined, `indexbot`-compatible tag file
(the pushed primary tag plus any cascade tags, deduped on append); this
proves that file can be fed straight into `announce --tags-from-file` and unions
correctly with whatever is already committed.
"""
from __future__ import annotations

import json
from pathlib import Path

from announce_helpers import (
    INDEX_FULL,
    announce_json,
    committed_root,
    configure_trusted_hosts,
    registry_host,
    seed_empty_root,
)
from fake_forge import FakeForge
from src.helpers import make_package, resolved_metadata_path
from src.runner import OcxRunner


def test_push_announce_file_feeds_announce_tags_file_union(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    announce_file = tmp_path / "announce-tags.txt"

    # A pre-existing committed tag on a DIFFERENT, real version — proves the
    # union keeps it (deletion only ever happens via --tags, not --tags-from-file).
    make_package(ocx, unique_repo, "0.9.0", tmp_path, cascade=False)

    # The cascading push under test: writes the pushed tag + cascade tags
    # (e.g. "1.2.3,1.2,1,latest") to `announce_file`.
    make_package(
        ocx,
        unique_repo,
        "1.2.3",
        tmp_path,
        cascade=True,
        extra_push_args=["--announce-file", str(announce_file)],
    )

    file_tags = [tag for tag in announce_file.read_text().split(",") if tag]
    assert "1.2.3" in file_tags, f"the pushed primary tag must be in the announce file: {file_tags}"
    assert "latest" in file_tags, f"a cascading push must record the latest tag: {file_tags}"
    assert "\n" not in announce_file.read_text(), "the format is comma-joined, not newline-joined"

    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "0.9.0")
    report = announce_json(ocx, fake_forge, *args, "--tags-from-file", str(announce_file))

    assert report["status"] == "updated"
    final_tags = set(committed_root(fake_forge, package)["tags"])
    assert final_tags == {"0.9.0", *file_tags}, "--tags-from-file must union with the committed root, dropping nothing"


def test_push_reports_canonical_tags_and_keeps_them_out_of_the_announce_file(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`PushReport.canonical_tags_written` names every `sha256.<hex>` tag the
    push wrote, and the `--announce-file` it wrote alongside contains none of
    them.

    Two halves of one rule (D7). A canonical tag is a digest alias, not a
    version: the push genuinely writes it (so the report must say so — the
    report states what reached the registry), and announce would drop it (so
    the file that feeds `announce --tags-from-file` must not carry it in the first
    place). Reporting is not publishing.

    Non-vacuity: the report's canonical list must be non-empty. `push` writes
    one canonical tag per distinct platform manifest unless
    `--no-canonical-tag` is given, so an empty list here means the push path
    changed and the file assertion below has nothing to exclude.
    """
    announce_file = tmp_path / "announce-tags.txt"
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    bundle = tmp_path / f"bundle-{unique_repo}-1.0.0.tar.xz"

    report = json.loads(
        ocx.run(
            "package",
            "push",
            "-p",
            pkg.platform,
            "-m",
            str(resolved_metadata_path(bundle)),
            "--announce-file",
            str(announce_file),
            "-i",
            pkg.fq,
            str(bundle),
            format="json",
        ).stdout
    )

    canonical = report["canonical_tags_written"]
    assert canonical, "a default push must write at least one canonical tag, or this test excludes nothing"
    for tag in canonical:
        assert tag.startswith("sha256."), f"a canonical tag is a digest alias, got {tag}"

    file_tags = [tag for tag in announce_file.read_text().split(",") if tag]
    assert "1.0.0" in file_tags, f"the pushed primary tag must be in the announce file: {file_tags}"
    for tag in canonical:
        assert tag not in file_tags, (
            f"the announce file feeds `announce --tags-from-file`; a canonical tag is not a version "
            f"and must never enter it: {tag} in {file_tags}"
        )


def test_push_still_reports_when_the_announce_file_cannot_be_written(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The announce file is a scratch artifact written AFTER the registry push
    has already landed — and a push is not undoable. So an unwritable path must
    still fail the command (the caller asked for the file) while the push report
    is emitted regardless: without it, a pipeline whose push genuinely succeeded
    has no way to learn the digest and tags it just published."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    bundle = tmp_path / f"bundle-{unique_repo}-1.0.0.tar.xz"
    # A path under a directory that does not exist: the create fails at write
    # time, on every platform, with no permission games.
    unwritable = tmp_path / "no-such-directory" / "announce-tags.txt"

    result = ocx.run(
        "package",
        "push",
        "-p",
        pkg.platform,
        "-m",
        str(resolved_metadata_path(bundle)),
        "--announce-file",
        str(unwritable),
        "-i",
        pkg.fq,
        str(bundle),
        format="json",
        check=False,
    )

    assert result.returncode != 0, "an announce file the caller asked for and did not get is a failure"
    report = json.loads(result.stdout)
    assert report["identifier"] == pkg.fq
    assert report["manifest_digest"].startswith("sha256:")
    assert not unwritable.exists()
