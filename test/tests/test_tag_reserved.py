# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The reserved-tag rule (D7) end to end, in one file.

`adr_oci_index_only_dispatch.md` D7: a reserved tag is not a version. Two
families are reserved — the `__ocx` namespace (prefix-based and
case-insensitive, so `__ocx`, `__ocxfoo` and `__OCX.desc` all match) and the
digest-alias form `<algorithm>.<hex>` — and the rule is one implementation,
`Tag::is_reserved` (`crates/ocx_lib/src/package/tag.rs`).

Two surfaces enforce it, and they fail differently, so both live here rather
than beside their command:

- **`ocx package announce`** DROPS reserved tags from the curated set and
  reports them; it never refuses. Refusing would make announce police how a
  publisher tags their own repository. The drop is applied once, at the
  collapse in `announce::pipeline::resolve_curated_tags`, so it must hold for
  every one of the three curation sources — `--tags`, `--refresh` and a tags
  file. Filtering only `--tags` is the exact three-sources failure D7 exists
  to prevent: `--refresh` and `--tags-file` start from the committed root, so
  neither can introduce a reserved tag but either would re-announce one
  forever once it landed.
- **`ocx index list`** OMITS them from every listing. There are three filter
  sites — `Index::list_tags` (`oci/index.rs`), `LocalIndex::list_tags`
  (`oci/index/local_index.rs`) and `OciIndex::list_tags`
  (`oci/index/oci_index.rs`) — and the third is the one a canonical
  `sha256.<hex>` tag actually enters a derived index through.
"""
from __future__ import annotations

import json
from pathlib import Path

import requests
from announce_helpers import (
    FIXED_CLOCK,
    INDEX_OWNER,
    INDEX_REPO,
    announce,
    announce_json,
    configure_trusted_hosts,
    registry_host,
    seed_empty_root,
)
from fake_forge import FakeForge
from src.helpers import make_package
from src.registry import fetch_manifest_raw, fetch_platform_manifest_digest
from src.runner import OcxRunner

# The five reserved forms the rule has to cover, spelled out rather than
# generated: `__ocx.desc` (a real internal tag), `__ocx` (the bare namespace,
# no separator), `__ocxfoo` (no separator, arbitrary suffix), `__OCX.desc`
# (case-insensitive) and a `sha256.<64 hex>` digest alias. A prefix check of
# `"__ocx."` misses three of them; `Tag::from` alone misses the last.
_DIGEST_ALIAS = "sha256." + "a" * 64
RESERVED_FORMS = ["__ocx.desc", "__ocx", "__ocxfoo", "__OCX.desc", _DIGEST_ALIAS]


def _seed_root_with_tags(fake_forge: FakeForge, package: str, physical: str, tags: dict[str, dict]) -> None:
    """Seeds a committed root carrying `tags` verbatim — the state `--refresh`
    and a tags-file union both start from."""
    fake_forge.seed_root(
        INDEX_OWNER, INDEX_REPO, f"p/{package}.json", {"repository": physical, "tags": tags}
    )


def _committed_entry(content: str = "sha256:" + "0" * 64) -> dict:
    return {"content": content, "observed": FIXED_CLOCK}


# ---------------------------------------------------------------------------
# announce — dropped, never refused, across all three curation sources
# ---------------------------------------------------------------------------


def test_announce_drops_every_reserved_form_from_explicit_tags(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Source 1 of 3 — `--tags`. All five reserved forms are dropped, the run
    still exits 0, only the real version is announced, and the drops are
    reported on BOTH surfaces: a stderr notice and `reserved_tags_dropped` in
    the JSON report.

    The reserved tags are deliberately not published on the registry: the drop
    happens at the curated-set collapse, before the first observe, so a build
    that failed to drop them fails here on the observe (an unresolved tag),
    never silently.
    """
    make_package(ocx, unique_repo, "1.2.3", tmp_path, new=True, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        ",".join([*RESERVED_FORMS, "1.2.3"]),
        "--out",
        str(tmp_path / "out"),
    )

    assert result.returncode == 0, f"a reserved tag is dropped, never refused: {result.stderr}"
    report = json.loads(result.stdout)
    assert report["status"] == "updated"
    assert sorted(report["reserved_tags_dropped"]) == sorted(RESERVED_FORMS), (
        f"every reserved form must ride out in the report, got {report['reserved_tags_dropped']}"
    )
    for form in RESERVED_FORMS:
        assert form in result.stderr, f"the stderr notice must name {form}: {result.stderr}"

    root = json.loads((tmp_path / "out" / "p" / f"{package}.json").read_bytes())
    assert list(root["tags"]) == ["1.2.3"], f"only the real version may be announced, got {list(root['tags'])}"


def test_announce_refresh_drops_a_reserved_tag_already_in_the_committed_root(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Source 2 of 3 — `--refresh`, which re-observes the COMMITTED set.

    `--refresh` cannot introduce a reserved tag, but it would re-announce one
    forever once it landed. Because the filter lives at the collapse rather
    than at the flag, a committed reserved entry is dropped on the next
    refresh — the committed root heals itself.
    """
    make_package(ocx, unique_repo, "1.2.3", tmp_path, new=True, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    _seed_root_with_tags(
        fake_forge,
        package,
        physical,
        {"1.2.3": _committed_entry(), "__ocx.desc": _committed_entry(), _DIGEST_ALIAS: _committed_entry()},
    )
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    report = announce_json(
        ocx, fake_forge, "--package", package, "--refresh", "--out", str(tmp_path / "out")
    )

    assert sorted(report["reserved_tags_dropped"]) == sorted(["__ocx.desc", _DIGEST_ALIAS])
    root = json.loads((tmp_path / "out" / "p" / f"{package}.json").read_bytes())
    assert list(root["tags"]) == ["1.2.3"], (
        f"a refresh must drop the committed reserved entries, got {list(root['tags'])}"
    )


def test_announce_tags_file_drops_a_reserved_tag_from_the_file(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Source 3 of 3 — a tags file (`--tags-file`, the file
    `ocx package push --announce-file` writes). The union is committed ∪ file;
    the drop applies to the union, so a reserved name is filtered whichever
    side it entered from.
    """
    make_package(ocx, unique_repo, "1.2.3", tmp_path, new=True, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    _seed_root_with_tags(
        fake_forge, package, physical, {"1.2.3": _committed_entry(), "__ocxfoo": _committed_entry()}
    )
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    tags_file = tmp_path / "announce-tags.txt"
    tags_file.write_text(f"2.0.0,{_DIGEST_ALIAS}")

    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags-file", str(tags_file), "--out", str(tmp_path / "out")
    )

    assert sorted(report["reserved_tags_dropped"]) == sorted(["__ocxfoo", _DIGEST_ALIAS]), (
        "a reserved name must be dropped whether it came from the committed root or the file"
    )
    root = json.loads((tmp_path / "out" / "p" / f"{package}.json").read_bytes())
    assert sorted(root["tags"]) == ["1.2.3", "2.0.0"]


def test_announce_entirely_reserved_selection_is_the_existing_empty_set_error(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A selection that is ENTIRELY reserved collapses into the existing
    empty-curated-set error (D6: no new variant for it) — a non-zero exit, not
    a silent success that announces nothing.

    Two things beyond "non-zero" are pinned here, because this is the one D7
    path that produces no report and therefore no `reserved_tags_dropped`:

    - **exit 64.** The tag selection is operator input, so `EX_USAGE` — not the
      unclassified 1 that a release wrapper cannot tell apart from a crash.
    - **the dropped names on stderr.** Every other D7 path names what it
      dropped; the error message is the only channel left on this one, and
      "no curated tags given" would be a false statement about an invocation
      that gave five.
    """
    make_package(ocx, unique_repo, "1.2.3", tmp_path, new=True, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        ",".join(RESERVED_FORMS),
        "--out",
        str(tmp_path / "out"),
        check=False,
    )

    assert result.returncode == 64, (
        f"the tag selection is operator input — EX_USAGE, not an unclassified 1: {result.stderr}"
    )
    assert "no curated tags" in result.stderr.lower(), result.stderr
    for form in RESERVED_FORMS:
        assert form in result.stderr, f"the refusal must name the dropped tag {form}: {result.stderr}"
    assert not (tmp_path / "out").exists()


# ---------------------------------------------------------------------------
# index list — omitted from every listing, at all three filter sites
# ---------------------------------------------------------------------------


def _publish_reserved_tags(ocx: OcxRunner, repo: str, source_tag: str) -> None:
    """Re-tags `source_tag`'s image index under every reserved form, straight
    through the registry HTTP API.

    `ocx` itself will not write these (that is the rule under test), and the
    listing filters can only be exercised against tags a registry genuinely
    serves — a listing that omits a tag nobody published proves nothing.
    """
    body, _digest = fetch_manifest_raw(ocx.registry, repo, source_tag)
    for form in RESERVED_FORMS:
        requests.put(
            f"http://{ocx.registry}/v2/{repo}/manifests/{form}",
            data=body,
            headers={"Content-Type": "application/vnd.oci.image.index.v1+json"},
            timeout=10,
        ).raise_for_status()


def test_index_list_omits_every_reserved_form_local_and_remote(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Every reserved form is omitted from `ocx index list`, both from the
    LOCAL index (`Index::list_tags` over `LocalIndex::list_tags`) and from a
    `--remote` read (`Index::list_tags` over `OciIndex::list_tags`).

    `OciIndex::list_tags` is the load-bearing one: it is where a canonical
    `sha256.<hex>` tag actually enters a derived index, and it seeds the tag
    cache — a missed filter there poisons the cache rather than merely
    printing an extra line.

    The non-vacuity anchor is the real version tag: it must be listed by both
    paths, so an empty listing (or a broken command) cannot pass this test.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    _publish_reserved_tags(ocx, pkg.repo, "1.0.0")

    # The canonical tag `ocx package push` writes by default is itself a
    # reserved form and is genuinely on the registry — assert it is there so
    # the omission below is about a tag that exists.
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "1.0.0")
    canonical_tag = leaf_digest.replace(":", ".")
    published = requests.get(f"http://{ocx.registry}/v2/{pkg.repo}/tags/list", timeout=10).json()["tags"]
    for form in [*RESERVED_FORMS, canonical_tag]:
        assert form in published, f"precondition: {form} must be published, got {published}"

    remote = ocx.plain("--remote", "index", "list", pkg.repo)
    ocx.plain("index", "update", pkg.repo)
    local = ocx.plain("index", "list", pkg.repo)

    for listing, label in ((remote.stdout, "--remote index list"), (local.stdout, "index list")):
        assert "1.0.0" in listing, f"{label} must list the real version — otherwise this proves nothing"
        for form in [*RESERVED_FORMS, canonical_tag]:
            assert form not in listing, f"{label} must omit the reserved tag {form}; got:\n{listing}"
        # `__OCX.desc` differs from `__ocx.desc` only by case; assert the
        # case-insensitive family is gone wholesale rather than form by form.
        assert "__ocx" not in listing.lower(), f"{label} must omit the whole __ocx namespace; got:\n{listing}"
