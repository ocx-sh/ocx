# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package announce` — the refusal ocx#487 moved.

Split out of `test_announce.py` rather than appended to it: these rows need a
committed root seeded in the index's **own byte form** (2-space indent, trailing
newline) so a run that changes nothing reports `unchanged` rather than
reformatting the fixture, which `announce_helpers.seed_empty_root` cannot
express.

`--refresh` over a claimed but unreleased package (`"tags": {}`) used to exit 64
(`no curated tags given`) before the description was ever observed. It now
completes, and a description-only refresh is reported through the existing two
status words: `status: updated` with `desc_status: updated`. The selections that
*name* tags keep the refusal.

Every run points at a fresh per-test `fake_forge` and the compose registry, the
same as `test_announce.py`.
"""
from __future__ import annotations

import json
from pathlib import Path

from announce_helpers import (
    INDEX_FULL,
    INDEX_OWNER,
    INDEX_REPO,
    announce,
    announce_json,
    configure_trusted_hosts,
    registry_host,
)
from fake_forge import FakeForge

from src.runner import OcxRunner

#: `--fork` plus the index coordinate, the argv prefix every writing row shares.
FORK_ARGS = ["--fork", "forkuser/index", "--index-repo", INDEX_FULL]


def seed_canonical_root(
    fake_forge: FakeForge,
    package: str,
    physical: str,
    *,
    tags: dict | None = None,
) -> None:
    """Seed `p/<package>.json` on the index base in the index's own byte form.

    `announce_helpers.seed_empty_root` goes through `FakeForge.seed_root`, which
    stores `json.dumps(root)` — compact. Announce re-serializes with a 2-space
    indent and a trailing newline, so a run over a compactly-seeded root reports
    `updated` for the reformatting alone, and a row asserting `unchanged` would
    measure the fixture's encoding instead of the behaviour under test.
    """
    registry = physical.removeprefix("oci://").split("/", 1)[0]
    root = {"name": f"{registry}/{package}", "repository": physical, "tags": tags or {}}
    fake_forge.seed_files(
        INDEX_OWNER,
        INDEX_REPO,
        {f"p/{package}.json": (json.dumps(root, indent=2) + "\n").encode()},
    )


def test_refresh_on_a_claimed_unreleased_package_completes_instead_of_exiting_64(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A root freshly written by `ocx package claim` carries `"tags": {}`, and
    `--refresh` derives its universe from exactly that — so it collapsed to the
    empty set and hit the `no curated tags given` refusal (exit 64) before
    reaching the description observation, which is the only work such a run has
    to do.

    Nothing has moved here, so the run reports `unchanged` on both status words
    and commits nothing. Exit 0 is the whole claim; the status words are what
    say the run actually ran rather than short-circuiting somewhere earlier.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    report = announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)

    assert report["status"] == "unchanged"
    assert report["desc_status"] == "unchanged"
    assert report["pull_request_url"] is None, "nothing moved, so nothing is proposed"


def test_refresh_publishes_a_description_for_a_package_with_no_versions(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The reason the refusal had to move: a claimed package can carry a README
    long before it carries a version, and `--refresh` is the command that brings
    it into the index.

    `AnnounceStatus` stays two-valued — no third outcome word was minted for
    this. `desc_status` is what discriminates a description-only pass, and the
    second run proves the first was not simply reporting `updated`
    unconditionally.
    """
    readme = tmp_path / "README.md"
    readme.write_text("# widget\n\nA claimed package with no versions yet.\n")
    ocx.plain("package", "description", "push", "--readme", str(readme), f"{ocx.registry}/{unique_repo}")

    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    first = announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    assert first["status"] == "updated"
    assert first["desc_status"] == "updated", "the description moved from null to an object"
    assert first["pull_request_url"] is not None, "a description-only change is still a proposal"

    second = announce_json(ocx, fake_forge, *FORK_ARGS, "--refresh", package)
    assert second["status"] == "unchanged", "the description did not move again"
    assert second["desc_status"] == "unchanged"


def test_a_selection_that_names_no_tag_at_all_still_exits_64(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The half of the old refusal that stays: `--tags-file` is the publisher
    naming which versions the index should carry, so resolving to nothing means
    the invocation asked for nothing — and accepting it would retract the whole
    curated set on the strength of an empty variable.

    This row and `--refresh` above start from the same committed root and differ
    only in which selection they carry, which is exactly the distinction
    ocx#487 draws.

    Scope: the empty **file** is the reachable empty set. `--tags ''` is not the
    same input — clap's comma delimiter turns it into a list of one empty tag
    name, which the observe loop refuses at exit 79 (`tag  does not resolve`),
    both before this change and after. `TagSelection::Replace`'s own empty-set
    refusal is pinned by the unit test `empty_curated_set_is_an_error`, which is
    where it can be reached at all.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    empty_tags_file = tmp_path / "tags.txt"
    empty_tags_file.write_text("")

    result = announce(
        ocx,
        fake_forge,
        "--tags-file",
        str(empty_tags_file),
        "--out",
        str(tmp_path / "out"),
        package,
        check=False,
    )

    assert result.returncode == 64, (
        "an empty --tags-file over an empty committed root names no version, which is a "
        f"usage error; got {result.returncode}: {result.stderr}"
    )
