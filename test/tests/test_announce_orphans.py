# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package announce` removes the index objects its new root stops naming.

Every announce used to be purely additive: a digest move left the old dispatch
object behind, and a description edit left the old readme and logo behind, for
the lifetime of the index. The run that stops referencing an object now removes
it in the same commit, so the published tree holds what its roots name and
nothing else.

Scope, and what each row is evidence for:

* **A moved tag** drops its old `o/<algo>/<hex>.json` and writes the new one,
  while a tag that did not move keeps its object. Those two assertions ride the
  same commit on purpose: the surviving object is what says the deletion set is
  a *diff* between two roots rather than a sweep of everything the previous root
  held. No separate "nothing moved" row exists, because an announce that moves
  nothing commits nothing at all (C6) — that row would measure the unchanged
  short-circuit, not this behaviour.
* **A changed readme** drops the markdown blob the old description named.
* **A logo that changes format** drops the `.png` the root could not name an
  extension for — found by probing the parent commit, which is the one read this
  feature adds.

Every scenario announces against the compose registry and a fresh per-test
`fake_forge`, the same as `test_announce.py`, and reads the committed tree
directly out of the fake's git graph: the deletion is only observable in the
tree the commit produced.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from announce_helpers import (
    INDEX_FULL,
    announce_json,
    branch_name,
    committed_root,
    configure_trusted_hosts,
    registry_host,
    seed_empty_root,
)
from fake_forge import FakeForge

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("package_announce")

#: `--fork` plus the index coordinate: every row here commits, so every row
#: needs the branch the commit lands on.
FORK_ARGS = ["--fork", "forkuser/index", "--index-repo", INDEX_FULL]

#: The fork the announce branch lives on — where the committed tree is read.
FORK_OWNER = "forkuser"
FORK_REPO = "index"

#: A 1x1 transparent PNG, and a 1x1 SVG: the two logo media types the
#: description format allows, which is what makes the extension unpredictable
#: from the root alone.
PNG_BYTES = (
    b"\x89PNG\r\n\x1a\n"
    b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06"
    b"\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\nIDATx"
    b"\x9cc\x00\x01\x00\x00\x05\x00\x01\r\n\xb4\x00\x00\x00\x00IEND\xaeB`\x82"
)
SVG_TEXT = '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>'


def object_path(package: str, digest: str, extension: str) -> str:
    """The wire path a CAS object of `digest` occupies under `package`."""
    algorithm, _, hex_digest = digest.partition(":")
    return f"p/{package}/o/{algorithm}/{hex_digest}.{extension}"


def object_committed(fake_forge: FakeForge, package: str, digest: str, extension: str) -> bool:
    """Whether the announce branch's tree carries that CAS object right now."""
    raw = fake_forge.read_file(
        FORK_OWNER, FORK_REPO, object_path(package, digest, extension), branch=branch_name(package)
    )
    return raw is not None


def arrange(ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str) -> tuple[str, str]:
    """Seed the claimed-but-empty committed root and trust the compose registry.

    Returns the logical package and the `oci://` pointer its root carries.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    return package, physical


def push_description(ocx: OcxRunner, unique_repo: str, readme_text: str, directory: Path, logo: Path | None) -> None:
    """Publish a `__ocx.desc` artifact carrying `readme_text` and maybe a logo."""
    readme = directory / "README.md"
    readme.write_text(readme_text)
    arguments = ["package", "description", "push", "--readme", str(readme)]
    if logo is not None:
        arguments += ["--logo", str(logo)]
    ocx.plain(*arguments, f"{ocx.registry}/{unique_repo}")


def test_a_moved_tag_removes_the_object_the_new_root_stopped_naming(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The mandate's core case. `2.0.0` is here as the control: it rides the very
    same commit as the deletion, so a sweep that took everything the previous
    root referenced — rather than the difference between the two roots — would
    take its object too and fail this row.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package, _ = arrange(ocx, fake_forge, unique_repo)

    announce_json(ocx, fake_forge, *FORK_ARGS, "--tags", "1.0.0,2.0.0", package)
    first = committed_root(fake_forge, package)["tags"]
    moved_before = first["1.0.0"]["content"]
    unmoved = first["2.0.0"]["content"]
    assert object_committed(fake_forge, package, moved_before, "json"), "the first announce wrote the old object"

    # Re-push `1.0.0` from a fresh build directory: `make_package` bakes a random
    # marker, so the tag's image index — and therefore its digest — moves.
    second_build = tmp_path / "second-build"
    second_build.mkdir()
    make_package(ocx, unique_repo, "1.0.0", second_build, cascade=False)

    report = announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    assert report["status"] == "updated"

    moved_after = committed_root(fake_forge, package)["tags"]["1.0.0"]["content"]
    assert moved_after != moved_before, "the fixture must actually move the digest"
    assert object_committed(fake_forge, package, moved_after, "json"), "the new object is in the tree"
    assert not object_committed(fake_forge, package, moved_before, "json"), (
        "the object the new root no longer names must leave the tree in the same commit"
    )
    assert object_committed(fake_forge, package, unmoved, "json"), (
        "a still-referenced object must survive: the deletion set is a diff, not a sweep"
    )


def test_a_changed_readme_removes_the_markdown_blob_the_old_description_named(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A description edit is a digest move like any other. The readme blob is
    named by `desc.readme`, so the old one is unreachable the moment the new
    root lands.
    """
    push_description(ocx, unique_repo, "# widget\n\nFirst edition.\n", tmp_path, logo=None)
    package, _ = arrange(ocx, fake_forge, unique_repo)

    announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    first_readme = committed_root(fake_forge, package)["desc"]["readme"]
    assert object_committed(fake_forge, package, first_readme, "md"), "the first announce wrote the readme blob"

    second_source = tmp_path / "second-description"
    second_source.mkdir()
    push_description(ocx, unique_repo, "# widget\n\nSecond edition, rewritten.\n", second_source, logo=None)

    report = announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    assert report["desc_status"] == "updated"

    second_readme = committed_root(fake_forge, package)["desc"]["readme"]
    assert second_readme != first_readme, "the fixture must actually move the readme digest"
    assert object_committed(fake_forge, package, second_readme, "md"), "the new readme blob is in the tree"
    assert not object_committed(fake_forge, package, first_readme, "md"), (
        "the readme nobody references any more must leave the tree"
    )


def test_a_logo_that_changes_format_removes_the_old_png(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The one reference whose path the root cannot spell. `desc.logo` records a
    digest and no media type, so the old object is found by probing `<hex>.png`
    and then `<hex>.svg` at the commit the new one is parented on — a read that
    has to hit the right ref in the right repository to answer at all.
    """
    png_logo = tmp_path / "logo.png"
    png_logo.write_bytes(PNG_BYTES)
    push_description(ocx, unique_repo, "# widget\n", tmp_path, logo=png_logo)
    package, _ = arrange(ocx, fake_forge, unique_repo)

    announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    png_digest = committed_root(fake_forge, package)["desc"]["logo"]
    assert object_committed(fake_forge, package, png_digest, "png"), "the first announce wrote the PNG logo"

    svg_source = tmp_path / "svg-description"
    svg_source.mkdir()
    svg_logo = svg_source / "logo.svg"
    svg_logo.write_text(SVG_TEXT)
    push_description(ocx, unique_repo, "# widget\n", svg_source, logo=svg_logo)

    report = announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    assert report["desc_status"] == "updated"

    svg_digest = committed_root(fake_forge, package)["desc"]["logo"]
    assert svg_digest != png_digest, "the fixture must actually replace the logo"
    assert object_committed(fake_forge, package, svg_digest, "svg"), "the new logo is in the tree"
    assert not object_committed(fake_forge, package, png_digest, "png"), (
        "the PNG the new root stopped naming must leave the tree, extension and all"
    )
