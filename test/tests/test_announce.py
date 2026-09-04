# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package announce` acceptance tests.

Covers the announce initiative's Track-A gate (design register / meta-plan
G-A): the fake-forge harness (`fake_forge.py`), the C6 unchanged/zero-PR
short-circuit, the fork/PR state machine (idempotent create, 422 reuse,
renamed fork, parent mismatch), the explicit target-owner shared-fork path
(S12), token-leak assertions (X6), and SSRF acceptance (X1-X3, ordering, the
`trusted_hosts` escape hatch, and the connect-time pin — see the module docs
on individual tests for exact scope).

Every scenario announces against a real `registry:2` instance (the `ocx`
fixture / `registry` fixture from `test/conftest.py`) and a fresh
per-test `fake_forge` (`test/tests/fake_forge.py`), never real network.
"""
from __future__ import annotations

import hashlib
import json
import subprocess
import time
from pathlib import Path

from announce_helpers import (
    FIXED_CLOCK,
    INDEX_FULL,
    INDEX_OWNER,
    INDEX_REPO,
    TOKEN,
    announce,
    announce_json,
    branch_name,
    committed_root,
    configure_trusted_hosts,
    registry_host,
    seed_empty_root,
)
from fake_forge import FakeForge

from src.helpers import make_package
from src.registry import fetch_manifest_raw, fetch_platform_manifest_digest
from src.runner import OcxRunner

# ── --out mode: byte-exact root + content-addressed CAS objects ────────────


def test_announce_out_writes_canonical_root_and_content_addressed_cas(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--out` writes the rebuilt root in the CONTRACTS §14 byte form (2-space
    indent, trailing newline) and every CAS object as the registry's own image
    index, byte-for-byte.

    Scope, precisely. The ROOT round-trip pins indentation, separators and the
    trailing newline; it does NOT pin field order (`json.loads` preserves
    document order, so re-dumping reproduces whatever order the serializer
    chose). Root field order is pinned by the vendored index fixture parity
    suite, which is where that contract lives.

    The CAS objects are pinned against the REGISTRY, not against a re-encoding
    of themselves (A1/A2, `adr_oci_index_only_dispatch.md` D1): the committed
    bytes must equal what `GET /v2/<repo>/manifests/<tag>` served, and the
    tag's `content` pointer must equal the digest that response was served
    under. A "does it equal its own canonical re-serialization" check cannot
    fail for a writer that re-encodes — it is built from the code under test —
    which is exactly why the earlier `json.dumps(..., sort_keys=True,
    separators=(",", ":"))` assertion is gone rather than adapted.

    Tag `1.0.0` alone cannot carry that claim either, and for the same reason
    one level down: ocx pushed it, so the registry serves ocx's OWN canonical
    serde encoding and a writer that re-serialises the parsed manifest lands
    on byte-identical output. Tag `2.0.0` is therefore PUT by hand as a
    3-space-indented encoding of the same document — bytes no serializer in
    the tree produces. It is the tag that makes this test able to fail.
    """
    import requests

    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    canonical_bytes, _ = fetch_manifest_raw(ocx.registry, unique_repo, "1.0.0")
    odd_bytes = json.dumps(json.loads(canonical_bytes), indent=3).encode()
    assert odd_bytes != canonical_bytes, "the odd encoding must actually differ from ocx's own"
    requests.put(
        f"http://{ocx.registry}/v2/{unique_repo}/manifests/2.0.0",
        data=odd_bytes,
        headers={"Content-Type": "application/vnd.oci.image.index.v1+json"},
        timeout=10,
    ).raise_for_status()
    odd_served_bytes, odd_served_digest = fetch_manifest_raw(ocx.registry, unique_repo, "2.0.0")
    assert odd_served_bytes == odd_bytes, "precondition: the registry must serve the odd bytes verbatim"

    out_dir = tmp_path / "out"
    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0,2.0.0", "--out", str(out_dir)
    )

    assert report["status"] == "updated"
    assert report["pull_request_url"] is None
    assert report["fork"] is None
    written = report["written_paths"]
    assert written, "at least the root must be written"
    for relative in written:
        assert (out_dir / relative).is_file(), f"reported written path {relative} does not exist on disk"

    root_path = out_dir / "p" / f"{package}.json"
    root_bytes = root_path.read_bytes()
    root_obj = json.loads(root_bytes)
    reproduced = (json.dumps(root_obj, indent=2) + "\n").encode()
    assert reproduced == root_bytes, "root bytes must equal their own canonical re-serialization"

    cas_files = [relative for relative in written if "/o/sha256/" in relative]
    assert cas_files, "at least one CAS dispatch object must be written"
    for relative in cas_files:
        cas_bytes = (out_dir / relative).read_bytes()
        expected_hex = Path(relative).stem
        assert hashlib.sha256(cas_bytes).hexdigest() == expected_hex, f"{relative} is not content-addressed"

    tag_content = root_obj["tags"]["1.0.0"]["content"]
    hex_digest = tag_content.split(":", 1)[1]
    assert f"p/{package}/o/sha256/{hex_digest}.json" in written

    # A2 — the tag's `content` is the digest the REGISTRY served the tag under.
    # Against a writer that mints its own digest for a derived document this
    # differs, and the fetch below 404s outright.
    served_bytes, served_digest = fetch_manifest_raw(ocx.registry, unique_repo, "1.0.0")
    assert tag_content == served_digest, (
        "the tag's content pointer must be the registry's own image-index digest, "
        f"got {tag_content} against {served_digest}"
    )

    # A1 — the committed CAS object IS the registry's image index, verbatim.
    # Fetched by the pointer the root itself wrote, so a minted digest fails
    # here at the fetch (404), before any byte comparison.
    cas_bytes = (out_dir / f"p/{package}/o/sha256/{hex_digest}.json").read_bytes()
    by_pointer_bytes, by_pointer_digest = fetch_manifest_raw(ocx.registry, unique_repo, tag_content)
    assert cas_bytes == served_bytes, "the CAS object must be the registry's image-index bytes, verbatim"
    assert cas_bytes == by_pointer_bytes
    assert f"sha256:{hex_digest}" == by_pointer_digest, (
        "the CAS filename hex must equal the Docker-Content-Digest the registry served"
    )

    # A1, the load-bearing half — `2.0.0` was stored 3-space-indented, so a
    # writer that re-serialises the parsed image index produces different
    # bytes, a different digest, and a CAS file under a different name.
    odd_content = root_obj["tags"]["2.0.0"]["content"]
    assert odd_content == odd_served_digest, (
        "the tag's content pointer must be the digest the registry served the ODD bytes under, "
        f"got {odd_content} against {odd_served_digest}"
    )
    odd_relative = f"p/{package}/o/sha256/{odd_served_digest.split(':', 1)[1]}.json"
    assert odd_relative in written
    assert (out_dir / odd_relative).read_bytes() == odd_bytes, (
        "the CAS object must be the registry's stored bytes, not a re-encoding of the same document"
    )

    out_dir_2 = tmp_path / "out2"
    report_2 = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0,2.0.0", "--out", str(out_dir_2)
    )
    assert sorted(report_2["written_paths"]) == sorted(written)
    for relative in written:
        assert (out_dir_2 / relative).read_bytes() == (out_dir / relative).read_bytes()


def test_announce_out_writes_the_whole_entry_even_when_nothing_changed(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 is scoped to "no commit, no pull request" — a local write is neither,
    so `--out` materializes the whole entry on every run. A pipeline shaped
    `announce --out dir && publish dir` must never find an empty directory just
    because nothing moved; only `status` reports that. The byte contract makes
    the repeated write idempotent.

    "The whole entry" includes the description's CAS objects, which is why this
    package publishes an `__ocx.desc`: the curated tags' CAS objects are written
    unconditionally, and a `desc.readme` the second run alone omitted would leave
    the published directory carrying a dangling reference the index refuses. The
    readme is deliberately pushed with no title (no frontmatter, no `--title`),
    the state that produced a schema-invalid `desc.title: ""`.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    readme = tmp_path / "README.md"
    readme.write_text("# widget\n\nDoes widget things.\n")
    ocx.plain("package", "description", "push", "--readme", str(readme), f"{ocx.registry}/{unique_repo}")
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    first_dir = tmp_path / "first"
    first = announce_json(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--out", str(first_dir))
    assert first["status"] == "updated"
    assert first["desc_status"] == "updated", "the description moved from null to an object"

    root_obj = json.loads((first_dir / "p" / f"{package}.json").read_bytes())
    desc = root_obj["desc"]
    assert desc["title"], f"the index schema types desc.title minLength 1, got {desc['title']!r}"
    readme_relative = f"p/{package}/o/sha256/{desc['readme'].split(':', 1)[1]}.md"
    assert readme_relative in first["written_paths"], (
        f"the root points at {readme_relative}: {first['written_paths']}"
    )

    # Seed the canonical bytes as the index-main root, so the next run rebuilds
    # something byte-identical to what is already committed.
    root_bytes = (first_dir / "p" / f"{package}.json").read_bytes()
    fake_forge.seed_files("ocx-sh", "index", {f"p/{package}.json": root_bytes})

    second_dir = tmp_path / "second"
    second = announce_json(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--out", str(second_dir))
    assert second["status"] == "unchanged", "nothing moved, so the status must say so"
    assert second["desc_status"] == "unchanged", "the description did not move either"
    assert sorted(second["written_paths"]) == sorted(first["written_paths"]), (
        "an unchanged --out run must still write the whole entry, not an empty directory"
    )
    assert readme_relative in second["written_paths"], (
        "the unchanged run's root still points at the readme, so it must write it too"
    )
    for relative in first["written_paths"]:
        assert (second_dir / relative).read_bytes() == (first_dir / relative).read_bytes()


# ── D4(a): the index records image indices only ────────────────────────────


def test_announce_refuses_a_tag_resolving_to_a_bare_manifest(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A curated tag whose registry target is a bare
    `application/vnd.oci.image.manifest.v1+json` is refused with `DataError`
    (65), and the message names BOTH the tag and the physical repository
    (`adr_oci_index_only_dispatch.md` D4(a), `AnnounceError::
    TagIsNotAnImageIndex`).

    65 rather than 79: the tag resolved and the artifact exists — its *shape*
    is wrong, which is the malformed-input category, not an absent one. And
    rather than exit 1: an unclassified failure is indistinguishable from a
    crash to a release wrapper.

    The exit code alone is too weak an assertion — several announce failures
    could be made to exit 65 — so the message content carries the test.
    """
    import requests

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    # Publish a tag that points DIRECTLY at the leaf platform manifest, the
    # one shape `ocx package push` never writes under a version tag.
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "1.0.0")
    leaf_bytes, _ = fetch_manifest_raw(ocx.registry, unique_repo, leaf_digest)
    requests.put(
        f"http://{ocx.registry}/v2/{unique_repo}/manifests/9.9.9",
        data=leaf_bytes,
        headers={"Content-Type": "application/vnd.oci.image.manifest.v1+json"},
        timeout=10,
    ).raise_for_status()

    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    result = announce(
        ocx, fake_forge, "--package", package, "--tags", "9.9.9", "--out", str(tmp_path / "out"), check=False
    )

    assert result.returncode == 65, f"expected DataError (65), got {result.returncode}: {result.stderr}"
    assert "9.9.9" in result.stderr, f"the refusal must name the offending tag: {result.stderr}"
    assert physical in result.stderr, f"the refusal must name the physical repository: {result.stderr}"
    assert not (tmp_path / "out").exists(), "a refused announce must write nothing"


# ── fork mode: happy path + C6 unchanged (G-A / G-D gate) ──────────────────


def test_announce_fork_happy_path_opens_pull_request(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert report["status"] == "updated"
    assert report["fork"] == "forkuser/index"
    assert report["pull_request_url"]
    assert report["pull_request_number"] is not None
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/pulls") == 1
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/forks") == 1
    # C8: the first announce (no pre-existing announce branch) must resolve
    # its base SHA from the UPSTREAM index repo, not the fork's own main —
    # forks share GitHub's object store, so the upstream SHA is a valid base
    # to branch from even when the fork's main has diverged or is stale.
    assert fake_forge.request_count("GET", "/repos/ocx-sh/index/git/ref/heads/main") == 1, (
        "the first announce must fetch the base ref from the upstream index repo"
    )
    assert fake_forge.request_count("GET", "/repos/forkuser/index/git/ref/heads/main") == 0, (
        "the first announce must not fetch the base ref from the fork's own main"
    )


def test_announce_fork_second_identical_run_is_unchanged_and_reuses_the_pull_request(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 (F1 amendment) / meta-plan gate G-A / G-D: an identical re-run whose
    branch already diverges from the upstream base reports `status: "unchanged"`
    and makes ZERO new commit or pull-request *create* calls, while still
    reusing (never duplicating) the open PR."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    first = announce_json(ocx, fake_forge, *args)
    assert first["status"] == "updated"
    assert first["pull_request_url"]

    blobs_before = fake_forge.request_count("POST", "/repos/forkuser/index/git/blobs")
    commits_before = fake_forge.request_count("POST", "/repos/forkuser/index/git/commits")

    second = announce_json(ocx, fake_forge, *args)
    assert second["status"] == "unchanged"
    # C6 amendment (F1): the branch diverges from the upstream base, so an
    # unchanged re-run ensures the PR still exists — it reuses the open one
    # (same URL, no duplicate) rather than leaving the update stranded.
    assert second["pull_request_url"] == first["pull_request_url"]
    assert second["fork"] == "forkuser/index"
    assert second["written_paths"] == []
    # Load-bearing: ZERO new commit / blob work — the root is byte-identical.
    assert fake_forge.request_count("POST", "/repos/forkuser/index/git/blobs") == blobs_before
    assert fake_forge.request_count("POST", "/repos/forkuser/index/git/commits") == commits_before
    # The PR is ensured (F1) but never DUPLICATED — still exactly one open PR.
    assert len(fake_forge.open_prs.get(("ocx-sh", "index"), {})) == 1


def test_announce_fork_unchanged_with_no_branch_is_a_pure_noop(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 (F1 amendment), the other half of the branch-ahead distinction: when
    the committed root already matches AND no announce branch exists yet, the
    run is a pure no-op — no fork, no commit, and (unlike the branch-ahead
    case) NO pull-request call at all."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    # Materialize the canonical committed root via `--out`, then seed those exact
    # bytes as the index-main root so the fork run reads an already-matching
    # state. Seeding the RAW bytes (not a re-serialized dict) is essential: the
    # unchanged short-circuit compares bytes, and only the canonical `--out`
    # form is byte-identical to what a fork run would regenerate.
    out_dir = tmp_path / "out"
    announce_json(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--out", str(out_dir))
    root_bytes = (out_dir / "p" / f"{package}.json").read_bytes()
    fake_forge.seed_files("ocx-sh", "index", {f"p/{package}.json": root_bytes})

    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL
    )
    assert report["status"] == "unchanged"
    assert report["pull_request_url"] is None
    assert report["fork"] is None
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/forks") == 0
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/pulls") == 0
    assert fake_forge.request_count("POST", "/repos/forkuser/index/git/commits") == 0


def test_announce_fork_unchanged_with_a_branch_not_ahead_is_a_pure_noop(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 (F1 amendment): "branch exists" is NOT the ensure-PR predicate —
    "branch is AHEAD of the upstream base" is. An announce branch sitting at the
    index base carries nothing unmerged (the shape a merged-but-undeleted branch
    leaves behind), so an unchanged run against it must stay a pure no-op rather
    than ensuring a pull request for work that is already in the index."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    # Same setup as the no-branch no-op: materialize the canonical root via
    # `--out` and seed those raw bytes as the index-main root (the byte compare
    # only matches the canonical form).
    out_dir = tmp_path / "out"
    announce_json(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--out", str(out_dir))
    root_bytes = (out_dir / "p" / f"{package}.json").read_bytes()
    fake_forge.seed_files("ocx-sh", "index", {f"p/{package}.json": root_bytes})
    # ...then park the announce branch exactly ON that base: it exists, and it
    # is not ahead.
    fake_forge.seed_branch_at(
        "forkuser", "index", branch_name(package), source_owner="ocx-sh", source_repo="index"
    )

    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL
    )
    assert report["status"] == "unchanged"
    assert report["pull_request_url"] is None
    assert report["fork"] is None
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/forks") == 0
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/pulls") == 0
    assert fake_forge.request_count("POST", "/repos/forkuser/index/git/commits") == 0


def test_announce_fork_recovers_stranded_pull_request_on_a_later_unchanged_run(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 (F1 amendment): if a run commits to the branch but its pull-request
    open fails, the update is stranded (committed, no PR). A later identical run
    reports `unchanged` yet — because the branch diverges from the upstream
    base — ensures the PR is opened, so the update is never lost."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    # Run 1: the commit lands, but the pull-request open fails.
    fake_forge.pull_fail_once = True
    failed = announce(ocx, fake_forge, *args, check=False)
    assert failed.returncode != 0, "the pull-request open failure must surface as a non-zero exit"
    assert "1.0.0" in committed_root(fake_forge, package)["tags"], "the commit must have landed on the branch"

    # Run 2: identical content ⇒ unchanged, but the diverged branch recovers the PR.
    recovered = announce_json(ocx, fake_forge, *args)
    assert recovered["status"] == "unchanged"
    assert recovered["pull_request_url"], "a stranded update's PR must be recovered on the next run"
    assert recovered["fork"] == "forkuser/index"


def test_announce_fork_retries_once_on_non_fast_forward_preserving_concurrent_change(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C4 (F2 amendment): the branch ref update is fast-forward-only (CAS). When
    a concurrent announce advances the branch between our read and our commit —
    here yanking `1.0.0`, a change CI does NOT re-derive — our update is rejected
    non-fast-forward, NOT force-overwritten. Announce re-reads the advanced head,
    regenerates against it, and retries once. The result preserves BOTH changes:
    the concurrent yank on `1.0.0` survives verbatim, and our new `2.0.0` lands —
    exactly the lost-update the amendment closes."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    # A first announce commits `1.0.0` onto the branch.
    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    # Script a racing announce that yanks `1.0.0` and advances the branch head.
    # It reuses the real observed content, so on the retry `1.0.0` is an unmoved
    # digest and its yank is carried verbatim. `1.0.0` must be in our curated set
    # for the verbatim carry to fire, so our run curates both `1.0.0` and `2.0.0`.
    concurrent = committed_root(fake_forge, package)
    concurrent["tags"]["1.0.0"]["yanked"] = {"reason": "concurrent security yank", "at": FIXED_CLOCK}
    branch = branch_name(package)
    fake_forge.concurrent_ref_advance[f"forkuser/index/{branch}"] = {
        f"p/{package}.json": json.dumps(concurrent).encode()
    }

    report = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0,2.0.0")

    assert report["status"] == "updated"
    # The scripted advance fired (the non-FF was hit and the retry engaged).
    assert not fake_forge.concurrent_ref_advance, "the non-fast-forward must have been triggered"
    final_tags = committed_root(fake_forge, package)["tags"]
    # BOTH preserved: our new tag AND the concurrent yank, not an overwrite.
    assert set(final_tags) == {"1.0.0", "2.0.0"}
    assert final_tags["1.0.0"].get("yanked", {}).get("reason") == "concurrent security yank", (
        "the concurrent announce's yank on 1.0.0 must survive the fast-forward-only retry"
    )


def test_announce_tags_file_race_retry_unions_against_the_winning_head(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C3/C4: two concurrent `--tags-file` announces must UNION, never clobber.
    The loser's fast-forward-only update is rejected, and its retry has to
    re-resolve its curated universe against the WINNING head — `regenerate`
    replaces the root's `tags` object wholesale, so a retry that replays the tag
    set resolved from the pre-race root silently deletes whatever the winner
    added.

    The existing non-fast-forward regression test cannot catch this: it races a
    yank on `1.0.0`, a tag already inside the loser's curated set, so a stale
    replay still happens to cover the winning head. Here the winner adds
    `2.0.0` — a tag the loser never resolved — which is exactly what a stale
    replay drops.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "3.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    # Shared starting state both racers read: the branch carries `1.0.0`.
    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    # The winner adds `2.0.0`, advancing the branch head between our read and
    # our commit. Its `content` is a placeholder — the retry re-observes every
    # curated tag, so the value the racer wrote must not survive verbatim.
    placeholder = f"sha256:{'e' * 64}"
    concurrent = committed_root(fake_forge, package)
    concurrent["tags"]["2.0.0"] = {"content": placeholder, "observed": FIXED_CLOCK}
    branch = branch_name(package)
    fake_forge.concurrent_ref_advance[f"forkuser/index/{branch}"] = {
        f"p/{package}.json": json.dumps(concurrent).encode()
    }

    # The loser announces `3.0.0` by file — additive union (C3).
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("3.0.0")
    report = announce_json(ocx, fake_forge, *args, "--tags-file", str(tags_file))

    assert report["status"] == "updated"
    assert not fake_forge.concurrent_ref_advance, "the non-fast-forward must have been triggered"
    final_tags = committed_root(fake_forge, package)["tags"]
    assert set(final_tags) == {"1.0.0", "2.0.0", "3.0.0"}, (
        "the retry must union against the winning head, never delete its 2.0.0"
    )
    assert final_tags["2.0.0"]["content"] != placeholder, (
        "the concurrently added tag must be genuinely re-observed on the retry"
    )


def test_announce_identical_race_retry_makes_no_empty_diff_commit(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C4 + C6 (X7 empty-diff threat class): when the announce that WINS the
    race committed exactly the bytes we were about to commit — two identical
    concurrent announces — our retry must not commit again. The second commit's
    tree would equal its base: an empty-diff commit on the PR branch, one of the
    governance threat classes the index bot tests for. The retry re-applies the
    unchanged predicate against the WINNING head (not the pre-race root it was
    first evaluated against), skips the commit, and still ensures the pull
    request so nothing is stranded."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    # Shared starting state both racers read: the branch carries `1.0.0`.
    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    # Materialize the exact entry an announce of `1.0.0,2.0.0` produces. The
    # clock is pinned (`FIXED_CLOCK`) and `regenerate` carries an unmoved digest
    # verbatim, so these bytes are what the fork run below regenerates too.
    out_dir = tmp_path / "winner"
    announce_json(ocx, fake_forge, "--package", package, "--tags", "1.0.0,2.0.0", "--out", str(out_dir))
    winning_files = {
        path.relative_to(out_dir).as_posix(): path.read_bytes() for path in out_dir.rglob("*") if path.is_file()
    }
    assert f"p/{package}.json" in winning_files

    # Script the racer to commit precisely those bytes, advancing the branch
    # head between our read and our commit.
    branch = branch_name(package)
    fake_forge.concurrent_ref_advance[f"forkuser/index/{branch}"] = winning_files

    ref_update_path = f"/repos/forkuser/index/git/refs/heads/{branch}"
    ref_updates_before = fake_forge.request_count("PATCH", ref_update_path)

    report = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0,2.0.0")

    assert not fake_forge.concurrent_ref_advance, "the non-fast-forward must have been triggered"
    assert report["status"] == "unchanged", "the winning head already carries our exact bytes"
    assert report["pull_request_url"], "the pull request must still be ensured, never stranded"
    # Load-bearing: exactly ONE ref update was attempted — the pre-race one that
    # was rejected 422. A second one would be the empty-diff commit.
    assert fake_forge.request_count("PATCH", ref_update_path) == ref_updates_before + 1, (
        "the retry must push no second commit when the winning head already matches"
    )
    assert set(committed_root(fake_forge, package)["tags"]) == {"1.0.0", "2.0.0"}


# ── C6 ensure-PR gate: the compare verdict is never guessed ────────────────


def test_announce_unchanged_indeterminate_compare_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 amendment: the ensure-PR gate asks whether the announce branch carries
    commits the upstream base does not. A compare 404 does not answer that — the
    branch demonstrably exists (this very run just read its head), so a 404 is an
    indeterminate compare, not "not ahead". Guessing "not ahead" would report a
    clean unchanged no-op while a committed update sits on the branch with no
    pull request — the stranded-commit window C6 exists to close."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    assert announce_json(ocx, fake_forge, *args)["status"] == "updated"

    fake_forge.compare_404_once = True
    result = announce(ocx, fake_forge, *args, check=False)
    assert result.returncode != 0, "an indeterminate compare must not resolve to a clean unchanged no-op"
    assert not fake_forge.compare_404_once, "the scripted compare 404 must have fired"


def test_announce_unchanged_unmodelled_compare_status_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6 amendment, the other half: the compare `status` classification is
    exhaustive over GitHub's four documented values. A value the client does not
    model must surface, never fall through to "not ahead" — same stranded-commit
    consequence as an indeterminate compare."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    assert announce_json(ocx, fake_forge, *args)["status"] == "updated"

    fake_forge.compare_status_once = "sideways"
    result = announce(ocx, fake_forge, *args, check=False)
    assert result.returncode != 0, "an unmodelled compare status must not resolve to a clean unchanged no-op"
    assert fake_forge.compare_status_once is None, "the scripted compare status must have fired"


# ── curation: --tags, --tags-file, --tags-from-registry, --refresh ────


def test_announce_tags_replace_drops_omitted_committed_tag(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0,2.0.0")
    assert set(committed_root(fake_forge, package)["tags"]) == {"1.0.0", "2.0.0"}

    announce_json(ocx, fake_forge, *args, "--tags", "2.0.0")
    assert set(committed_root(fake_forge, package)["tags"]) == {
        "2.0.0"
    }, "a committed tag absent from --tags must be dropped"


def test_announce_tags_file_union_adds_without_dropping(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    first_head = fake_forge.branch_head("forkuser", "index", branch_name(package))
    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("2.0.0")
    announce_json(ocx, fake_forge, *args, "--tags-file", str(tags_file))

    assert set(committed_root(fake_forge, package)["tags"]) == {"1.0.0", "2.0.0"}
    # An Ahead (not Diverged) branch fast-forwards onto its OWN prior head —
    # the counterpart to the Stale/Reset contract pinned elsewhere in this file.
    assert fake_forge.commit_parent("forkuser", "index", branch_name(package)) == first_head


def _seed_cascade_and_curate_one(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> tuple[str, list[str]]:
    """Publish `1.2.3` **with its cascade** and curate only the exact version.

    Leaves the registry holding strictly more tags than the index root carries,
    which is the state `--tags-from-registry` exists to close and the one every
    other announce test deliberately avoids by passing ``cascade=False``.

    Returns the package id and the argv prefix shared by the announce calls.
    """
    make_package(ocx, unique_repo, "1.2.3", tmp_path, cascade=True)
    package = f"acme/{unique_repo}"
    seed_empty_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.2.3")
    assert set(committed_root(fake_forge, package)["tags"]) == {
        "1.2.3"
    }, "precondition: the root starts behind the registry"
    return package, args


def test_announce_tags_from_registry_discovers_unannounced_tags(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The registry supplies the curated set, so a cascade published before the
    package reached the index gets announced without anyone naming the tags."""
    package, args = _seed_cascade_and_curate_one(ocx, fake_forge, unique_repo, tmp_path)

    announce_json(ocx, fake_forge, *args, "--tags-from-registry")

    assert set(committed_root(fake_forge, package)["tags"]) == {
        "1.2.3",
        "1.2",
        "1",
        "latest",
    }, "every rolling tag the cascade wrote is discovered"


def test_announce_tags_from_registry_drops_no_keep_tag_into_the_root(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`push` writes an `__ocx.keep.<algorithm>-<hex>` tag per platform by default,
    so a registry listing carries one per published version. They are not versions and
    must never reach the index — and, unlike a caller-named reserved tag, they are
    filtered silently rather than reported, because the caller named nothing."""
    package, args = _seed_cascade_and_curate_one(ocx, fake_forge, unique_repo, tmp_path)

    report = announce_json(ocx, fake_forge, *args, "--tags-from-registry")

    tags = committed_root(fake_forge, package)["tags"]
    assert not [tag for tag in tags if tag.startswith("__ocx.keep.")], (
        f"keep tags leaked into the root: {sorted(tags)}"
    )
    assert report["reserved_tags_dropped"] == [], "a registry listing names nothing, so it reports no drops"


def test_announce_tags_from_registry_never_drops_a_committed_tag(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Additive only. A committed tag the registry no longer serves survives: the
    index treats a vanished non-yanked tag as an anomaly for a human to look at,
    so announce silently dropping it would pre-empt that decision."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    seed_empty_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]
    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    # A second package pushed into the same repo moves the registry on without
    # removing 1.0.0 from the committed root's claim.
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    announce_json(ocx, fake_forge, *args, "--tags-from-registry")

    tags = committed_root(fake_forge, package)["tags"]
    assert "1.0.0" in tags, "the already-committed tag is never dropped by a registry-sourced run"
    assert "2.0.0" in tags, "and the newly published one is picked up"


def test_announce_tags_from_registry_keeps_a_yank_marker(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Retirement is yank, not delete: a yanked tag re-observed from the registry
    keeps its marker. This is what makes the additive union safe to re-run — a
    dropped tag would come back, a yanked one stays yanked."""
    package, args = _seed_cascade_and_curate_one(ocx, fake_forge, unique_repo, tmp_path)
    announce_json(ocx, fake_forge, *args, "--yank", "1.2.3", "--yank-reason", "bad build", "--refresh")
    assert committed_root(fake_forge, package)["tags"]["1.2.3"]["yanked"]["reason"] == "bad build"

    announce_json(ocx, fake_forge, *args, "--tags-from-registry")

    yanked = committed_root(fake_forge, package)["tags"]["1.2.3"].get("yanked")
    assert yanked is not None, "a registry-sourced re-observe must not clear the yank marker"
    assert yanked["reason"] == "bad build"


def _squash_merge_the_announce(fake_forge: FakeForge, package: str) -> None:
    """Land the fork branch's root on the index base the way `ocx-sh/index`
    actually merges — a **squash**: the content arrives under a brand-new commit
    and none of the branch's own commits become ancestors of `main`. Then close
    the pull request, leaving the per-package branch behind, because that is what
    GitHub does."""
    root_path = f"p/{package}.json"
    merged = committed_root(fake_forge, package)
    fake_forge.seed_root(INDEX_OWNER, INDEX_REPO, root_path, merged)
    fake_forge.close_pull_request(INDEX_OWNER, INDEX_REPO, f"forkuser:{branch_name(package)}")


def test_announce_after_its_pull_request_squash_merges_rebuilds_from_the_base(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The second announce for a package must not stack on the spent branch.

    Regression for ocx-sh/ocx#228. The announce branch is named from the package
    alone, so it outlives every pull request opened from it. `ocx-sh/index`
    squash-merges, which puts the branch's content on `main` under a new commit
    while leaving none of the branch's own commits in `main`'s history. Basing
    the next announce on that branch re-proposed the already-merged commits and
    the pull request conflicted on the one file every announce edits — measured
    live as 6 commits / 2 changed files / `mergeable_state: dirty`, against
    1 commit / 1 file / clean once the branch was rebuilt from the base.

    The parent assertion is what discriminates: the tag set alone passes either
    way, because the merged root is read back in both worlds."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    _squash_merge_the_announce(fake_forge, package)

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0,2.0.0")

    branch = branch_name(package)
    assert fake_forge.commit_parent("forkuser", "index", branch) == fake_forge.branch_head(
        INDEX_OWNER, INDEX_REPO, "main"
    ), "the announce must sit directly on the index base, not on the spent branch"
    assert set(committed_root(fake_forge, package)["tags"]) == {"1.0.0", "2.0.0"}


# ── #399: a Diverged branch with an open pull request rebuilds on the base ─


def test_announce_rebuilds_a_stale_branch_when_the_base_changes_its_own_root(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """#399: the index base migrating THIS package's root shape must not
    freeze its open pull request forever.

    `resolve_branch_state` used to read `Diverged` + open-PR as `Live`, so the
    committed root came from the branch head and every later run reproduced
    the branch's pre-migration bytes — reported `unchanged`, committed nothing,
    left the pull request CONFLICTING (the ocx-sh/index#740 `owners[]`
    respelling, 34 packages frozen up to 21 days). `BranchState::Stale` (D1)
    fixes it: the root's SHAPE is read from the current index base, the
    branch's own tags are carried onto it, and the branch is reset to sit
    directly on the base. The parent assertion is what discriminates a real
    rebuild from a read that merely tolerates the new shape; the shape and tag
    order assertions are what discriminate a rebuild from one that regenerates
    a fresh document instead of the base's own."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    first = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    # Migrate THIS package's root shape on main, the way ocx-sh/index#740 added
    # `owners[]`: every non-`tags` key stays, `owners` is inserted ahead of
    # `tags`, and `tags` itself stays whatever main already carries.
    base_root = json.loads(fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch="main"))
    shaped_root = {
        **{key: value for key, value in base_root.items() if key != "tags"},
        "owners": [{"login": "acme", "id": 1}],
        "tags": base_root["tags"],
    }
    fake_forge.seed_root(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", shaped_root)

    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("2.0.0")
    second = announce_json(ocx, fake_forge, *args, "--tags-file", str(tags_file))

    assert second["status"] == "updated"
    assert second["pull_request_number"] == first["pull_request_number"], "the open pull request must be reused"
    branch = branch_name(package)
    assert fake_forge.commit_parent("forkuser", "index", branch) == fake_forge.branch_head(
        INDEX_OWNER, INDEX_REPO, "main"
    ), "the rebuild must sit directly on the current index base"
    root = committed_root(fake_forge, package)
    assert root["owners"] == [{"login": "acme", "id": 1}], "the base's shape must be carried, not dropped"
    assert list(root["tags"]) == [
        "1.0.0",
        "2.0.0",
    ], "base order first, branch-only tags appended after — order is contract, not cosmetics"
    assert list(root.keys())[-1] == "tags", "tags stays the last key of the root (wire_writer sort_keys=False)"
    pull_path = f"/repos/{INDEX_OWNER}/{INDEX_REPO}/pulls/{first['pull_request_number']}"
    assert fake_forge.request_count("GET", pull_path) == 0, (
        "the mergeability tripwire (D2) is for the unchanged path only — this run committed something new"
    )


def test_announce_refuses_an_unchanged_run_whose_open_pull_request_conflicts(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """D2's tripwire: an unchanged `Stale` run whose open pull request cannot
    merge must fail loudly, not report a clean `unchanged` while the pull
    request stays CONFLICTING forever — the #399 freeze pattern in the one
    corner the rebuild itself cannot reach. `Stale`'s rebuild only fixes a
    conflict when there is something new to commit; here nothing is (the base
    already carries the branch's only tag), so the rebuild step is a no-op and
    the underlying same-file divergence leaves the open pull request
    CONFLICTING regardless. Exit 65 (`DataError`, C13), naming the branch so a
    human can close the pull request or delete the branch — the tool has no
    close-PR / delete-ref primitive to clear this corner itself
    (`adr_announce_diverged_branch_rebuild.md`)."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    first = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    branch = branch_name(package)
    head_before = fake_forge.branch_head("forkuser", "index", branch)

    # Main already carries the branch's exact tag entry (nothing new to carry)
    # PLUS an added key — the same file diverged on both sides since the merge
    # base, which is what makes the pull request genuinely CONFLICTING rather
    # than merely behind. Serialized canonically (indent=2 + trailing newline,
    # CONTRACTS §14) so this is what ocx itself would regenerate byte-for-byte
    # if nothing had diverged — otherwise C6 reads "changed" off formatting
    # alone and the run rebuilds instead of hitting the unchanged path.
    committed_bytes = fake_forge.read_file("forkuser", "index", f"p/{package}.json", branch=branch)
    assert committed_bytes is not None
    branch_root = json.loads(committed_bytes)
    diverged_root = {
        **{key: value for key, value in branch_root.items() if key != "tags"},
        "owners": [{"login": "acme", "id": 1}],
        "tags": branch_root["tags"],
    }
    fake_forge.seed_files(
        INDEX_OWNER, INDEX_REPO, {f"p/{package}.json": (json.dumps(diverged_root, indent=2) + "\n").encode()}
    )

    result = announce(ocx, fake_forge, *args, "--refresh", check=False)

    assert result.returncode == 65, f"expected a data error, got {result.returncode}"
    assert branch in result.stderr, "the stderr must name the branch a human has to clear"
    assert str(first["pull_request_number"]) in result.stderr or first["pull_request_url"] in result.stderr, (
        "the stderr must name the stuck pull request"
    )
    assert fake_forge.branch_head("forkuser", "index", branch) == head_before, "a refused run must not move the branch"


def test_announce_rebuilds_a_stale_branch_once_then_reports_unchanged(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A `Stale` rebuild is self-stabilizing, not churn. Read literally, C6
    compares the regenerated candidate against index main's own raw bytes
    (`adr_announce_diverged_branch_rebuild.md`): a `Stale` branch whose tag is
    not yet on main therefore reports `updated` on THIS run, rebuilding onto
    the moved base as one commit. That rebuild leaves the branch `Ahead` of
    main (no longer `Diverged`), so the run after it takes the ordinary
    pre-#399 C6-amendment path — compares against the branch's own
    just-rebuilt bytes — and reports `unchanged`, reusing the pull request."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    first = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    branch = branch_name(package)

    # An unrelated commit lands on the index base, so the branch is `Diverged`
    # from it — with its pull request still open and untouched by the move.
    fake_forge.seed_root(INDEX_OWNER, INDEX_REPO, "p/other/package.json", {"tags": {}})

    second = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    assert second["status"] == "updated", "the Stale branch must rebuild onto the moved base"
    assert second["pull_request_number"] == first["pull_request_number"]
    assert fake_forge.commit_parent("forkuser", "index", branch) == fake_forge.branch_head(
        INDEX_OWNER, INDEX_REPO, "main"
    ), "the rebuild must sit directly on the current index base"
    assert list(committed_root(fake_forge, package)["tags"]) == ["1.0.0"]

    head_after_rebuild = fake_forge.branch_head("forkuser", "index", branch)
    third = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    assert third["status"] == "unchanged", "once ahead of main, a repeat run must not rebuild again"
    assert third["pull_request_number"] == first["pull_request_number"]
    assert fake_forge.branch_head("forkuser", "index", branch) == head_after_rebuild, (
        "an unchanged run must not advance the branch"
    )


def test_announce_unchanged_stale_branch_with_a_mergeable_pull_request_reports_it(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """D2's other branch: a `Stale` unchanged run whose open pull request CAN
    still merge must report `unchanged` and reuse the pull request, never the
    hard error D2 raises only for a genuinely conflicting one. Main gains the
    branch's EXACT committed bytes for this package's root under a new,
    diverging commit — without closing the pull request — using raw bytes
    (`seed_files`, not `seed_root`'s `json.dumps` round-trip, which would
    change the blob and falsely register as a conflict). Both sides land the
    path on the identical blob, so the fake's three-way (`_conflicting_locked`,
    the shared oracle for both forges) answers `Mergeable`."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    first = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    branch = branch_name(package)
    head_before = fake_forge.branch_head("forkuser", "index", branch)

    committed_bytes = fake_forge.read_file("forkuser", "index", f"p/{package}.json", branch=branch)
    assert committed_bytes is not None
    fake_forge.seed_files(INDEX_OWNER, INDEX_REPO, {f"p/{package}.json": committed_bytes})

    second = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    assert second["status"] == "unchanged"
    assert second["pull_request_number"] == first["pull_request_number"]
    assert second["pull_request_url"] == first["pull_request_url"]
    assert fake_forge.branch_head("forkuser", "index", branch) == head_before, "an unchanged run must not advance the branch"


def test_announce_stale_branch_reset_is_not_subject_to_the_fast_forward_race_knob(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A `Stale` branch commits with `Reset` (D1), and `fake_forge`'s only race
    knob (`concurrent_ref_advance`) fires exclusively inside
    `handle_patch_ref`'s fast-forward-only branch (`force: false`) — its own
    comment says a `force: true` update is "deliberately NOT consulted, since
    a reset is not racing anyone for a fast-forward". So arming that knob on a
    `Stale` run cannot inject a race; this test pins exactly that (the knob
    stays unconsumed) and, incidentally, that the ordinary rebuild still lands
    correctly regardless. It does NOT exercise a Stale-branch race — see
    `test/tests/test_announce_gitlab.py::test_a_stale_branch_retry_rebuilds_from_the_upstream_head`
    for the real one, driven through GitLab's per-file compare-and-swap
    instead, which GitHub's ref-level PATCH has no equivalent knob for."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    base_root = json.loads(fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch="main"))
    shaped_root = {
        **{key: value for key, value in base_root.items() if key != "tags"},
        "owners": [{"login": "acme", "id": 1}],
        "tags": base_root["tags"],
    }
    fake_forge.seed_root(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", shaped_root)

    branch = branch_name(package)
    racer = committed_root(fake_forge, package)
    racer["tags"]["1.0.0"]["yanked"] = {"reason": "unreachable racer", "at": FIXED_CLOCK}
    fake_forge.concurrent_ref_advance[f"forkuser/index/{branch}"] = {f"p/{package}.json": json.dumps(racer).encode()}

    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("2.0.0")
    report = announce_json(ocx, fake_forge, *args, "--tags-file", str(tags_file))

    assert f"forkuser/index/{branch}" in fake_forge.concurrent_ref_advance, (
        "documents the known limitation: a Reset commit never consumes this knob"
    )
    assert report["status"] == "updated"
    assert fake_forge.commit_parent("forkuser", "index", branch) == fake_forge.branch_head(
        INDEX_OWNER, INDEX_REPO, "main"
    )
    root = committed_root(fake_forge, package)
    assert root["owners"] == [{"login": "acme", "id": 1}]
    assert list(root["tags"]) == ["1.0.0", "2.0.0"]


def test_announce_keeps_accumulated_tags_when_the_base_moves_under_its_open_pull_request(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """#228's invariant, restated for #399: no tag announced into an open pull
    request is ever lost — NOT that the branch's own commit chain is never
    rewritten. That second wording was only ever a proxy for the first, and
    `BranchState::Stale` (D1) rebuilds the branch on the current index base on
    every run, so once the base has moved the branch's new parent is the
    base's OWN head rather than the branch's own prior head — on purpose. What
    must still hold, and is what this test guards, is that BOTH tags land in
    the one open pull request regardless: resetting the branch whenever it
    diverges from the base must never silently drop announce #1's
    still-unmerged tag the moment anything else lands on the index base — the
    failure C4's "update, don't overwrite" rule exists to prevent."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")

    # An unrelated commit lands on the index base, so the branch is now
    # `diverged` from it — with its pull request still open.
    fake_forge.seed_root(INDEX_OWNER, INDEX_REPO, "p/other/package.json", {"tags": {}})

    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("2.0.0")
    announce_json(ocx, fake_forge, *args, "--tags-file", str(tags_file))

    assert fake_forge.commit_parent("forkuser", "index", branch_name(package)) == fake_forge.branch_head(
        INDEX_OWNER, INDEX_REPO, "main"
    ), "a Stale branch's rebuild must sit directly on the current index base"
    assert set(committed_root(fake_forge, package)["tags"]) == {"1.0.0", "2.0.0"}


def test_announce_refresh_reobserves_and_updates_a_moved_digest(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    content_before = committed_root(fake_forge, package)["tags"]["1.0.0"]["content"]

    # Move the tag: re-push it with a new build (a fresh random marker, per
    # `make_package`), so the tag's image index — and its digest — must change. A
    # distinct tmp_path subdir avoids colliding with the first build's
    # deterministic `pkg-<repo>-<tag>` bundle directory.
    second_build = tmp_path / "second-build"
    second_build.mkdir()
    make_package(ocx, unique_repo, "1.0.0", second_build, cascade=False)

    report = announce_json(ocx, fake_forge, *args, "--refresh")
    assert report["status"] == "updated", "a moved digest must not short-circuit as unchanged"
    content_after = committed_root(fake_forge, package)["tags"]["1.0.0"]["content"]
    assert content_after != content_before


# ── yank / unyank (C7) ──────────────────────────────────────────────────────


def test_announce_yank_and_unyank_apply_to_curated_tags(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--tags", "1.0.0", "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args)
    announce_json(ocx, fake_forge, *args, "--yank", "1.0.0", "--yank-reason", "critical security issue")
    yanked = committed_root(fake_forge, package)["tags"]["1.0.0"]["yanked"]
    assert yanked["reason"] == "critical security issue"

    announce_json(ocx, fake_forge, *args, "--unyank", "1.0.0")
    assert "yanked" not in committed_root(fake_forge, package)["tags"]["1.0.0"]


def test_announce_yank_and_unyank_same_tag_errors(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
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
        "1.0.0",
        "--yank",
        "1.0.0",
        "--unyank",
        "1.0.0",
        "--yank-reason",
        "x",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
        check=False,
    )
    assert result.returncode != 0


# ── forge state machine: idempotent fork, PR 422 reuse, renamed fork,
#    parent mismatch, explicit target-owner org fork (S12) ─────────────────


def test_announce_fork_reused_across_runs_without_duplicate_creation(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Design register X5 "fork idempotent": `ensure_fork`'s GET-reuse-first
    check means a fork already known never gets a duplicate create — proven
    across two runs whose curated sets genuinely differ (both take the
    Updated path, both call `ensure_fork`)."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    announce_json(ocx, fake_forge, *args, "--tags", "1.0.0,2.0.0")

    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/forks") == 1


def test_announce_pull_request_422_reuses_existing_pr_without_duplicate(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    args = ["--package", package, "--fork", "forkuser/index", "--index-repo", INDEX_FULL]

    first = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0")
    second = announce_json(ocx, fake_forge, *args, "--tags", "1.0.0,2.0.0")

    assert second["status"] == "updated"
    assert first["pull_request_number"] == second["pull_request_number"], "the same PR must be reused, not duplicated"
    assert fake_forge.request_count("POST", "/repos/ocx-sh/index/pulls") == 2
    assert len(fake_forge.open_prs.get(("ocx-sh", "index"), {})) == 1


def test_announce_renamed_fork_resolves_via_response_body(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    fake_forge.rename_fork_to = "forkuser/grimoire-index"

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert report["status"] == "updated"
    assert report["fork"] == "forkuser/grimoire-index", "endpoints must rebuild from the response full_name"
    assert fake_forge.request_count("POST", "/repos/forkuser/grimoire-index/git/blobs") >= 1


def test_announce_parent_mismatch_fork_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    fake_forge.fork_parent_override = "someone-else/index"

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
        check=False,
    )
    assert result.returncode != 0
    assert (
        fake_forge.request_count("POST", "/repos/forkuser/index/git/blobs") == 0
    ), "no commit may happen after a parent mismatch"


def test_announce_explicit_target_owner_fork_threads_organization(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """S12 shared-fork path: `--fork ocx-contrib/index` threads
    `organization: "ocx-contrib"` into the fork-create body and opens the PR
    against that fork, not a personal one."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "ocx-contrib/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert report["status"] == "updated"
    assert report["fork"] == "ocx-contrib/index"

    fork_bodies = [body for path, body in fake_forge.bodies if path == "/repos/ocx-sh/index/forks"]
    assert fork_bodies and fork_bodies[0].get("organization") == "ocx-contrib"


# ── fork readiness poll ──────────────────────────────────────────────────


def test_announce_fork_readiness_pending_then_ready_succeeds(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    # One not-ready GET before the fork reports ready — costs one 2s backoff
    # sleep (`PollSchedule::default().initial_interval`).
    fake_forge.not_ready["forkuser/index"] = 1

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert report["status"] == "updated"
    # Load-bearing: success alone would also be reported if the poll never ran,
    # so pin that the not-ready probe was genuinely followed by a retry.
    assert fake_forge.request_count("GET", "/repos/forkuser/index") >= 2, (
        "the not-ready probe and at least one retry must have fired"
    )


def test_announce_fork_readiness_unresolvable_keeps_retrying(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`GitHubForge::wait_fork_ready` hardcodes `PollSchedule::default()`
    (2s->30s doubling, 300s deadline) with no test seam to shorten it — the
    exact backoff math is already pinned, deterministically and without
    sleeping, by the Rust unit test `forge::poll::backoff_doubles_caps_at_30s_
    and_bounds_the_deadline` (A4). Exhausting the real 300s deadline here to
    additionally prove the terminal `ForkNotReady` outcome would cost ~5
    minutes of wall time on every suite run for one already-covered
    assertion. This test instead proves the cheaply-bounded half at the
    acceptance level: the retry loop is genuinely engaged end-to-end against
    a real HTTP round trip — still retrying past the first backoff window,
    not failing or succeeding early.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    fake_forge.not_ready["forkuser/index"] = -1  # never ready

    env = {
        **ocx.env,
        "__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url,
        "OCX_ANNOUNCE_TOKEN": TOKEN,
    }
    process = subprocess.Popen(
        [
            str(ocx.binary),
            "--format",
            "json",
            "package",
            "announce",
            "--package",
            package,
            "--tags",
            "1.0.0",
            "--fork",
            "forkuser/index",
            "--index-repo",
            INDEX_FULL,
        ],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        time.sleep(7)  # past the immediate probe + the first 2s backoff sleep
        assert process.poll() is None, "the readiness poll must still be retrying, not have already returned"
        assert (
            fake_forge.request_count("GET", "/repos/forkuser/index") >= 2
        ), "the immediate probe and at least one retry must have fired"
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


# ── A4 deferred runtime proofs: no-redirect client, fresh-fork 404 retry ───


def test_announce_forge_redirect_is_not_followed(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """X5: the forge client is built with `redirect::Policy::none()` — a 3xx
    response must not be chased. Proven with a single server: the redirect
    `Location` points at an otherwise-never-legitimately-requested path on
    the SAME fake forge, and the assertion is that path was never hit."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    trap = f"{fake_forge.base_url}/__unreachable_redirect_trap__"
    fake_forge.redirect_next_contents = trap

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
        check=False,
    )
    assert result.returncode != 0, "a 3xx from the forge must not resolve to success"
    assert ("GET", "/__unreachable_redirect_trap__") not in fake_forge.requests, "the client must not chase the redirect"


def test_announce_fresh_fork_first_request_404_race_retry_fires(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """X5, at the request the race actually hits first: the readiness poll asks
    `GET /repos/{fork}` — repository METADATA, which GitHub makes ready BEFORE
    the fork's git object store. So the first request of the commit sequence,
    `GET .../git/commits/<base-sha>` (which reads the base tree), can still 404
    after the poll passed. `commit_files` retries the WHOLE sequence once after
    a 3s delay; scripted here as a 404-then-200 on that GET.

    Guarding only a later write (the tree POST) would leave this request — and
    the blob POSTs after it — unprotected, and a 404 there surfaced as a
    `missing field tree.sha` error that gives the publisher no hint a retry
    would fix it."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    fake_forge.base_commit_fail_once.add("forkuser/index")

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert report["status"] == "updated"
    base_tree_reads = [
        path
        for method, path in fake_forge.requests
        if method == "GET" and path.startswith("/repos/forkuser/index/git/commits/")
    ]
    assert len(base_tree_reads) == 2, f"exactly one retry must fire, saw {base_tree_reads}"


def test_announce_fresh_fork_tree_404_race_retry_fires(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """X5, the later-write half: the provisioning window can also close after
    the base-tree read succeeds, so a 404 on `POST .../git/trees` must be
    absorbed by the same single retry of the whole sequence."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    fake_forge.tree_fail_once.add("forkuser/index")

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert report["status"] == "updated"
    assert fake_forge.request_count("POST", "/repos/forkuser/index/git/trees") == 2, "exactly one retry must fire"


# ── token-leak (X6, first-class) ─────────────────────────────────────────


def test_announce_token_never_leaks_to_stdout_stderr_or_forge_request_log(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The token is env-only by construction (never placed on argv) — this
    additionally proves it never reaches stdout, stderr, or any logged forge
    request URL (headers legitimately carry it as a bearer value; a URL
    never should)."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
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
        "1.0.0",
        "--fork",
        "forkuser/index",
        "--index-repo",
        INDEX_FULL,
    )
    assert TOKEN not in result.stdout
    assert TOKEN not in result.stderr
    for _method, path in fake_forge.requests:
        assert TOKEN not in path, f"the token leaked into a logged forge URL: {path}"


# ── SSRF acceptance (X1-X3) ─────────────────────────────────────────────


def test_announce_ssrf_forbidden_repository_refused_before_any_registry_call(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A root whose `repository` points at a private/loopback host outside
    `trusted_hosts` is refused (`ConfigError`, exit 78) — and refused
    *before* any registry request. Ordering is distinguished from a
    "genuinely tried and failed to connect" outcome (`Unavailable`, exit 69)
    by pointing at a fast-failing, nothing-listening loopback port (the
    project's established deterministic-failure fixture host, `127.0.0.1:1`
    — see `test_index.py`). A skipped pre-flight would have the real oci
    client ECONNREFUSED against that port and surface `Unavailable` instead
    — a different, distinguishable exit code from the one asserted here.
    Every OTHER happy-path test in this module exercises the flip side (the
    X2 `trusted_hosts` escape hatch letting the real registry:2 instance
    through) as part of its normal setup.
    """
    package = f"acme/{unique_repo}"
    seed_empty_root(fake_forge, package, "oci://127.0.0.1:1/x")
    # No trusted_hosts entry at all for this namespace.

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--out",
        str(tmp_path / "out"),
        check=False,
    )
    assert result.returncode == 78, f"expected ConfigError (78), got {result.returncode}: {result.stderr}"


def test_announce_ssrf_guard_active_permits_cidr_trusted_ip_literal_registry(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Ruling 5: `trusted_hosts` exempts by exact host string OR, for an
    IP-literal host, CIDR membership computed against the resolved address.
    A CIDR entry — rather than an exact-hostname match — forces the pre-flight
    (`resolve_and_validate`) to compute genuine IP-based validation, and the
    observe succeeds only if it answers "trusted".

    Scope, precisely: a green run here does NOT distinguish an engaged
    connect-time `GuardedResolver` from an unwired one — both let a trusted
    address through, so this test cannot fail if the resolver were dropped from
    the observe client. That wiring is stated in `package_announce.rs::
    announce_client` (`ClientBuilder::ssrf_guard`), and the resolver's refusal
    behavior is pinned in isolation by the unit test
    `oci::ssrf::guarded_resolver_refuses_forbidden_host_at_connect` (A2). A true
    DNS-rebinding proof needs a controllable authoritative resolver, out of this
    stdlib-only suite's scope.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    port = ocx.registry.split(":", 1)[1] if ":" in ocx.registry else "443"
    physical = f"oci://127.0.0.1:{port}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, ["127.0.0.0/8"])

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--out",
        str(tmp_path / "out"),
        extra_env={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},127.0.0.1:{port}"},
    )
    assert report["status"] == "updated"


# ── fork-free path: the announce branch lives on the index repository ──────


def test_announce_direct_commits_the_branch_to_the_index_repo_and_opens_a_pull_request(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """With neither `--out` nor `--fork`, the announce branch is committed onto
    the INDEX repository itself and the pull request is opened from there — no
    fork is looked up, created, or written to anywhere.

    Two assertions carry the contract and neither is redundant. That the root
    landed on `ocx-sh/index@<announce branch>` proves the commit reached the
    index repo; that `ocx-sh/index@main` is byte-for-byte the SHA it was before
    the run proves the change still goes through a pull request rather than
    straight onto the default branch, which is what the index's governance gate
    and its `refresh`/`new-package` labelling run on.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    main_before = fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, "main")
    assert main_before is not None, "the seeded index root must give main a head to compare against"

    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--index-repo", INDEX_FULL
    )

    assert report["status"] == "updated"
    assert report["pull_request_url"]
    assert report["fork"] is None, "the fork-free path has no fork to report"
    # The rebuilt root is on the index repo's announce branch...
    branch = branch_name(package)
    committed = fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch=branch)
    assert committed is not None, f"no root committed to {INDEX_FULL}@{branch}"
    assert "1.0.0" in json.loads(committed)["tags"]
    # ...and NOT on the index's default branch.
    assert fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, "main") == main_before, (
        "the direct path must open a pull request, never commit to the index default branch"
    )
    # No fork was consulted or created at any point.
    assert fake_forge.request_count("POST", f"/repos/{INDEX_FULL}/forks") == 0
    assert not [path for _, path in fake_forge.requests if "/forkuser/" in path], (
        f"the direct path must touch no fork: {fake_forge.requests}"
    )
    assert fake_forge.request_count("POST", f"/repos/{INDEX_FULL}/pulls") == 1


def test_announce_direct_without_push_access_fails_closed_naming_repo_and_permission(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A credential that can read the index but not push to it must be refused
    with exit 80 (`AuthError`) BEFORE anything is written, and the message must
    name the repository and the missing permission.

    The probe exists because GitHub answers an unauthorised write with 404 as
    readily as 403, and a mid-sequence 404 is indistinguishable from the
    fresh-fork provisioning race `commit_files` sleeps and retries for — so
    without it this failure is a delayed, bare status code naming a URL.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    fake_forge.no_push_access.add(INDEX_FULL)

    result = announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--index-repo", INDEX_FULL, check=False
    )

    assert result.returncode == 80, f"expected AuthError (80), got {result.returncode}: {result.stderr}"
    assert INDEX_FULL in result.stderr, f"the error must name the repository: {result.stderr}"
    assert "push" in result.stderr, f"the error must name the missing permission: {result.stderr}"
    # Fail-closed: refused before any write, so no branch and no pull request.
    assert fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, branch_name(package)) is None
    assert fake_forge.request_count("POST", f"/repos/{INDEX_FULL}/git/blobs") == 0
    assert fake_forge.request_count("POST", f"/repos/{INDEX_FULL}/pulls") == 0


def test_announce_requires_the_credential_for_every_mode_that_writes(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`OCX_ANNOUNCE_TOKEN` gates writing, not forking: `--out` is the one mode
    that runs without it, and the fork-free path — which has no `--fork` to key
    a credential check off — is refused just like `--fork` is."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    package = f"acme/{unique_repo}"
    physical = f"oci://{ocx.registry}/{unique_repo}"
    seed_empty_root(fake_forge, package, physical)
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    shared = ["--package", package, "--tags", "1.0.0", "--index-repo", INDEX_FULL]

    tokenless_direct = announce(ocx, fake_forge, *shared, token=None, check=False)
    assert tokenless_direct.returncode == 80, (
        f"the fork-free path writes, so it needs the credential: {tokenless_direct.stderr}"
    )
    assert "OCX_ANNOUNCE_TOKEN" in tokenless_direct.stderr

    tokenless_out = announce(ocx, fake_forge, *shared, "--out", str(tmp_path / "out"), token=None, check=False)
    assert tokenless_out.returncode == 0, f"--out writes nothing remote and must still run: {tokenless_out.stderr}"
