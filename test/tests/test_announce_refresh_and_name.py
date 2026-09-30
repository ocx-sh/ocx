# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package announce` — the two refusals ocx#487 and ocx#477 moved.

Split out of `test_announce.py` rather than appended to it: both scenarios need
a committed root seeded in the index's **own byte form** (2-space indent,
trailing newline) so a run that changes nothing reports `unchanged` rather than
reformatting the fixture, and both need roots whose `name` is deliberately
wrong — neither of which `announce_helpers.seed_empty_root` can express without
growing a flag per scenario.

Scope:

* **ocx#487** — `--refresh` over a claimed but unreleased package (`"tags": {}`)
  used to exit 64 (`no curated tags given`) before the description was ever
  observed. It now completes, and a description-only refresh is reported through
  the existing two status words: `status: updated` with `desc_status: updated`.
  The selections that *name* tags keep the refusal.
* **ocx#477** — announce refuses a committed root whose `name` disagrees with
  the identifier on the command line, at exit 65, naming both values. An absent
  `name` is a disagreement too. The check needs no `[registries."<domain>"]`
  entry, which the `--out` rows here are what say.

* **Snapshot removal** — vanished rows, empty tags files and canonical observation.

Every run points at a fresh per-test `fake_forge` and the compose registry, the
same as `test_announce.py`.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
from announce_helpers import (
    INDEX_FULL,
    INDEX_OWNER,
    INDEX_REPO,
    announce,
    announce_json,
    configure_trusted_hosts,
    registry_host,
    root_name,
)
from fake_forge import FakeForge

from src.helpers import make_package
from src.registry import delete_manifest, fetch_manifest_digest
from src.runner import OcxRunner

pytestmark = pytest.mark.command("package_announce")

#: `--fork` plus the index coordinate, the argv prefix every writing row shares.
FORK_ARGS = ["--fork", "forkuser/index", "--index-repo", INDEX_FULL]


def seed_canonical_root(
    fake_forge: FakeForge,
    package: str,
    physical: str,
    *,
    name: str | None = None,
    tags: dict | None = None,
) -> None:
    """Seed `p/<package>.json` on the index base in the index's own byte form.

    `announce_helpers.seed_empty_root` goes through `FakeForge.seed_root`, which
    stores `json.dumps(root)` — compact. Announce re-serializes with a 2-space
    indent and a trailing newline, so a run over a compactly-seeded root reports
    `updated` for the reformatting alone, and a row asserting `unchanged` would
    measure the fixture's encoding instead of the behaviour under test.

    `name` defaults to the value announce expects; a row passes it to seed a
    root that disagrees, or `""` to seed one carrying no `name` at all.
    """
    root: dict = {"repository": physical, "tags": tags or {}}
    if name != "":
        root = {"name": name or root_name(package, physical), **root}
    fake_forge.seed_files(
        INDEX_OWNER,
        INDEX_REPO,
        {f"p/{package}.json": (json.dumps(root, indent=2) + "\n").encode()},
    )


# ── ocx#487: --refresh on a claimed, unreleased package ─────────────────────


def test_refresh_on_a_claimed_unreleased_package_completes_instead_of_exiting_64(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str
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


def test_an_empty_tags_file_changes_nothing_and_exits_0(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--tags-file` lists the tags a run acts on, so an empty file acts on
    none: the run succeeds, writes nothing and never reaches the index. That is
    what lets a pipeline whose upstream step found nothing to announce call
    announce unconditionally.

    The output directory staying absent proves no forge work ran. `--tags ''`
    is a different input (one malformed tag name, exit 64), pinned below.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    empty_tags_file = tmp_path / "tags.txt"
    empty_tags_file.write_text("")
    out_dir = tmp_path / "out"

    result = announce(
        ocx,
        fake_forge,
        "--tags-file",
        str(empty_tags_file),
        "--out",
        str(out_dir),
        package,
        check=False,
    )

    assert result.returncode == 0, (
        f"an empty --tags-file names no version and is a no-op; got {result.returncode}: {result.stderr}"
    )
    assert not out_dir.exists(), f"a no-op must not write --out; found {sorted(out_dir.rglob('*'))}"


# ── ocx#477: the root's name is checked against the identifier ──────────────


def test_announce_refuses_a_root_that_names_another_package(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The root's `name` and the identifier on the command line are two
    statements of the same fact, and nothing compared them — so announcing
    `<registry>/acme/widget` against a root that says `other.example/acme/widget`
    rewrote somebody else's entry.

    Exit 65, not 64: the identifier parses and the root exists, so nothing about
    the command line is malformed — the two sides genuinely disagree and only a
    human decides which moves. The message has to name both values, because an
    operator cannot tell which side is wrong from either one alone.

    No `[registries]` entry is configured, and the refusal still fires: the check
    reads the identifier, never the config, so it cannot refuse an `--out` render
    on a machine that has no entry for the domain.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    foreign = f"other.example/{package}"
    seed_canonical_root(fake_forge, package, physical, name=foreign)

    result = announce(
        ocx, fake_forge, "--refresh", "--out", str(tmp_path / "out"), package, check=False
    )

    assert result.returncode == 65, f"expected DataError (65), got {result.returncode}: {result.stderr}"
    assert foreign in result.stderr, f"the message must name what the root says: {result.stderr}"
    assert f"{ocx.registry}/{package}" in result.stderr, (
        f"the message must name what the run announces: {result.stderr}"
    )


def test_announce_refuses_a_root_carrying_no_name_at_all(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Fail closed. The index root schema requires `name` of every root, so a
    file without one is not the root it is standing in for — treating the
    absence as "no disagreement" would let exactly the roots nobody can vouch
    for through the check that exists to vouch for them."""
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical, name="")

    result = announce(
        ocx, fake_forge, "--refresh", "--out", str(tmp_path / "out"), package, check=False
    )

    assert result.returncode == 65, f"expected DataError (65), got {result.returncode}: {result.stderr}"
    assert f"{ocx.registry}/{package}" in result.stderr, (
        f"the message must still name what the run announces: {result.stderr}"
    )


def test_a_matching_name_needs_no_registries_entry_to_get_past_the_check(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The other side of the same decision: the check must not become a
    requirement that `[registries."<domain>"]` exists.

    With no config entry at all and a root whose `name` agrees, the run gets
    past the name check and is stopped later by the SSRF pre-flight (exit 78,
    the loopback compose registry with no `trusted_hosts` exemption). 78 rather
    than 65 is the assertion: it can only be reached from *after* the name
    check, so it says the check passed without reading any config.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical)
    # Deliberately no `configure_trusted_hosts` call.

    result = announce(
        ocx, fake_forge, "--refresh", "--out", str(tmp_path / "out"), package, check=False
    )

    assert result.returncode == 78, (
        "a matching name must reach the SSRF pre-flight, which is the next refusal on this "
        f"path; got {result.returncode}: {result.stderr}"
    )


def test_an_empty_tags_value_names_one_malformed_tag(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--tags ''` is not the empty `--tags-file` the no-op row above covers.

    clap's comma delimiter splits the value into a list of one *empty tag
    name*, so the selection is non-empty, reaches the observe loop, and is
    refused there as a tag outside the OCI grammar (exit 64) — where an empty
    `--tags-file` names nothing and succeeds, off two inputs that look alike on
    a command line.
    """
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_canonical_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    out_dir = tmp_path / "out"

    result = announce(ocx, fake_forge, "--tags", "", "--out", str(out_dir), package, check=False)

    assert result.returncode == 64, (
        "an empty --tags value names one malformed tag, which is a usage error — not the "
        f"no-op an empty --tags-file gets; got {result.returncode}: {result.stderr}"
    )
    assert "is not a valid OCI tag" in result.stderr, (
        f"the 64 must be the empty tag's refusal, not another one; stderr: {result.stderr}"
    )
    assert not out_dir.exists(), f"a refused run must write nothing; found {sorted(out_dir.rglob('*'))}"


# ── snapshot removal: a tag the registry no longer serves ───────────────────

#: A stamp earlier than the pinned announce clock, so a row the run rewrote would
#: show a different `observed` and a row it carried would not.
EARLIER_STAMP = "2026-07-01T00:00:00Z"


def seed_two_vanished_tags(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> tuple[str, dict[str, dict]]:
    """Publish `1.0.0`, `2.0.0`, `3.0.0`, delete the first two from the registry, and
    seed a committed root holding all three: `1.0.0` ephemeral, `2.0.0` and
    `3.0.0` durable. Returns the package and the seeded rows by tag.

    The deletion is real: the precondition asserts the registry answers not-found
    for both, so a run that keeps or removes them is reacting to the registry and
    not to a fixture that never had them.
    """
    digests: dict[str, str] = {}
    for tag in ("1.0.0", "2.0.0", "3.0.0"):
        make_package(ocx, unique_repo, tag, tmp_path, cascade=False)
        digests[tag] = fetch_manifest_digest(ocx.registry, unique_repo, tag)
    for tag in ("1.0.0", "2.0.0"):
        delete_manifest(ocx.registry, unique_repo, digests[tag])
        with pytest.raises(RuntimeError):
            fetch_manifest_digest(ocx.registry, unique_repo, tag)

    rows = {
        "1.0.0": {"content": digests["1.0.0"], "observed": EARLIER_STAMP, "ephemeral": True},
        "2.0.0": {"content": digests["2.0.0"], "observed": EARLIER_STAMP},
        "3.0.0": {"content": digests["3.0.0"], "observed": EARLIER_STAMP},
    }
    package = f"acme/{unique_repo}"
    seed_canonical_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}", tags=rows)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    return package, rows


def test_refresh_removes_a_vanished_ephemeral_row_and_reports_a_vanished_durable_one(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--refresh` only re-observes: it removes a vanished row when the row was
    marked ephemeral, and for a durable row it says so and leaves the row alone.

    `3.0.0` is the control. It is present, durable and unmoved, so its row must
    come out byte-identical — including the old `observed` stamp — which is what
    separates "removed the two gone rows" from "rewrote the whole tag map".
    """
    package, rows = seed_two_vanished_tags(ocx, fake_forge, unique_repo, tmp_path)
    out_dir = tmp_path / "out"

    report = announce_json(ocx, fake_forge, "--refresh", "--out", str(out_dir), package)

    assert report["status"] == "updated"
    assert report["removed"] == ["1.0.0"], "only the ephemeral row is removed by a refresh"
    assert report["durable_missing"] == ["2.0.0"], "a durable row a refresh finds gone is reported, not removed"
    written = json.loads((out_dir / "p" / f"{package}.json").read_bytes())
    assert written["tags"] == {"2.0.0": rows["2.0.0"], "3.0.0": rows["3.0.0"]}


def test_naming_a_vanished_durable_tag_removes_it_and_touches_no_other_row(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Naming a tag is the one gesture that removes a durable row. The ephemeral
    `1.0.0` is just as gone but the file does not list it, so it stays: a
    `--tags-file` acts on exactly the tags it names.
    """
    package, rows = seed_two_vanished_tags(ocx, fake_forge, unique_repo, tmp_path)
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("2.0.0\n")
    out_dir = tmp_path / "out"

    report = announce_json(ocx, fake_forge, "--tags-file", str(tags_file), "--out", str(out_dir), package)

    assert report["removed"] == ["2.0.0"]
    assert report["durable_missing"] == []
    written = json.loads((out_dir / "p" / f"{package}.json").read_bytes())
    assert written["tags"] == {"1.0.0": rows["1.0.0"], "3.0.0": rows["3.0.0"]}


def test_ephemeral_beside_refresh_is_a_usage_error(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--ephemeral` marks the rows a run adds and a refresh adds none, so the
    pair says nothing coherent. Exit 64, before any registry or forge work."""
    result = announce(
        ocx,
        fake_forge,
        "--ephemeral",
        "--refresh",
        "--out",
        str(tmp_path / "out"),
        f"acme/{unique_repo}",
        check=False,
    )

    assert result.returncode == 64, f"expected a usage error (64), got {result.returncode}: {result.stderr}"
    assert not (tmp_path / "out").exists()


def test_a_tags_file_naming_a_tag_neither_registry_nor_index_holds_writes_nothing(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A typo in a tags file must not pass for a vanished tag: with no row to
    remove and nothing at the registry, the run has nothing it could do with the
    name, so it refuses (exit 79, not-found) and leaves the output untouched."""
    package = f"acme/{unique_repo}"
    seed_canonical_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("9.9.9\n")
    out_dir = tmp_path / "out"

    result = announce(ocx, fake_forge, "--tags-file", str(tags_file), "--out", str(out_dir), package, check=False)

    assert result.returncode == 79, f"expected NotFound (79), got {result.returncode}: {result.stderr}"
    assert "9.9.9" in result.stderr, f"the refusal must name the tag it could not resolve: {result.stderr}"
    assert not out_dir.exists(), f"a refused run must write nothing; found {sorted(out_dir.rglob('*'))}"


def test_a_tag_outside_the_oci_grammar_exits_64_and_writes_nothing(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A malformed tag in a tags file never reaches a manifest URL: the registry
    read refuses it before any request, so the run exits 64 and writes no root."""
    package = f"acme/{unique_repo}"
    seed_canonical_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("9.9.9#x\n")
    out_dir = tmp_path / "out"

    result = announce(ocx, fake_forge, "--tags-file", str(tags_file), "--out", str(out_dir), package, check=False)

    assert result.returncode == 64, f"expected a usage error (64), got {result.returncode}: {result.stderr}"
    assert "is not a valid OCI tag" in result.stderr, f"the 64 must be the tag refusal: {result.stderr}"
    assert not out_dir.exists(), f"a refused run must write nothing; found {sorted(out_dir.rglob('*'))}"


def test_a_fragment_on_a_published_tag_exits_64_instead_of_recording_that_tag(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A URL drops its `#fragment`, so `1.0#x` would fetch `manifests/1.0` and record
    the published `1.0` under the malformed name; the read refuses it first."""
    make_package(ocx, unique_repo, "1.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    seed_canonical_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("1.0#x\n")
    out_dir = tmp_path / "out"

    result = announce(ocx, fake_forge, "--tags-file", str(tags_file), "--out", str(out_dir), package, check=False)

    assert result.returncode == 64, f"expected a usage error (64), got {result.returncode}: {result.stderr}"
    assert "is not a valid OCI tag" in result.stderr, f"the 64 must be the tag refusal: {result.stderr}"
    assert not out_dir.exists(), f"a refused run must write nothing; found {sorted(out_dir.rglob('*'))}"


def test_tags_are_observed_at_the_canonical_registry_when_a_mirror_is_configured(
    ocx: OcxRunner, mirror_registry: str, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The registry that says whether a tag exists is the one the index's root
    names, never a registry-role mirror standing in for it.

    The mirror here is a live registry that holds nothing for this repository, so
    a run that read through it would find `1.0.0` gone, then see it present on the
    canonical follow-up probe and fail as a race (exit 75); the canonical registry
    holds `1.0.0`, and the row carries the digest it served. The package is pushed before the mirror entry is written,
    or the push itself would follow the mirror.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    served = fetch_manifest_digest(ocx.registry, unique_repo, "1.0.0")
    package = f"acme/{unique_repo}"
    seed_canonical_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    hosts = [registry_host(ocx.registry), registry_host(mirror_registry), "127.0.0.1"]
    configure_trusted_hosts(ocx, ocx.registry, hosts)
    with (Path(ocx.env["OCX_HOME"]) / "config.toml").open("a") as config:
        config.write(f'\n[mirrors."{ocx.registry}"]\nregistry = "http://{mirror_registry}"\n')
    out_dir = tmp_path / "out"

    result = announce(
        ocx,
        fake_forge,
        "--tags",
        "1.0.0",
        "--out",
        str(out_dir),
        package,
        check=False,
        # The mirror is plain HTTP, like the canonical registry; without it the config is refused (78).
        extra_env={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{mirror_registry}"},
    )

    assert result.returncode == 0, (
        f"the tag lives at the canonical registry, so the run must succeed; got {result.returncode}: {result.stderr}"
    )
    written = json.loads((out_dir / "p" / f"{package}.json").read_bytes())
    assert written["tags"]["1.0.0"]["content"] == served
