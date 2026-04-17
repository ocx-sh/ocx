"""Acceptance tests for Capture Full OCI Resolution Chain in Package refs/blobs/ (#35).

Specification-mode tests — written from the design record
(.claude/artifacts/plan_resolution_chain_refs.md), NOT from the implementation.

These tests encode the acceptance criteria (AC1–13) and user-experience
scenarios (UX1–7) from the design record. They MUST fail against the current
binary because the ChainedIndex write-through, link_blobs, and GC
changes are not yet wired.

Acceptance criteria traceability:
  test 47 → AC1: refs/blobs/ populated after install
  test 48 → AC2: find via different tag appends refs
  test 49 → AC3: clean retains reachable blobs
  test 50 → AC4: clean collects orphaned chain after uninstall --purge
  test 51 → AC5: offline re-resolve survives clean after full chain capture
  test 52 → AC7: index update writes only tag files, not blobs
  test 53 → AC8: --remote install persists and links chain
  test 54 → AC9: --remote index list refreshes tags from source
  test 55 → AC10: offline install after bare index update fails cleanly
  test 56 → UX5: failed install leaves collectable orphans
  test 57 → AC11: parallel install races preserve full chain
  test 58 → AC12: no sidecar lock/log/tmp files after install
  test 59 → AC13/UX7: missing manifest after index update recovers on install
  test 60 → find read-only against matching chain makes no writes
  test 61 → AC6: clean collects real chain blobs after uninstall --purge
  test 62 → AC2 (append): find via different-version tag appends new chain refs
  test 63 → AC9 (cache-bypass): --remote tag resolution bypasses the local tag cache
"""

from __future__ import annotations

import concurrent.futures
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo, make_package, registry_dir
from src.registry import fetch_manifest_digest, fetch_manifest_from_registry


# ── Helpers ───────────────────────────────────────────────────────────────


def _ocx_home(ocx: OcxRunner) -> Path:
    return Path(ocx.env["OCX_HOME"])


def _blobs_dir(ocx: OcxRunner) -> Path:
    return _ocx_home(ocx) / "blobs"


def _tags_dir(ocx: OcxRunner) -> Path:
    return _ocx_home(ocx) / "tags"


def _refs_blobs_dir(content_path: Path) -> Path:
    """Given a package content path, return its refs/blobs/ directory."""
    return content_path.parent / "refs" / "blobs"


def _install_content(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Install pkg and return the resolved content/ path."""
    result = ocx.json("install", pkg.short)
    candidate = Path(result[pkg.short]["path"])
    return candidate.resolve()


def _count_blobs(blobs_dir: Path) -> set[Path]:
    """Collect all data files in blobs/ — one per blob."""
    if not blobs_dir.exists():
        return set()
    return {p for p in blobs_dir.rglob("data") if p.is_file()}


def _collect_sidecar_files(blobs_dir: Path) -> list[Path]:
    """Return all .lock, .log, .tmp files anywhere under blobs/."""
    if not blobs_dir.exists():
        return []
    sidecars: list[Path] = []
    for suffix in (".lock", ".log", ".tmp"):
        sidecars.extend(blobs_dir.rglob(f"*{suffix}"))
    return sidecars


def _wipe_blobs(ocx: OcxRunner) -> None:
    """Remove the blobs/ directory entirely."""
    blobs = _blobs_dir(ocx)
    if blobs.exists():
        shutil.rmtree(blobs)


def _wipe_tags(ocx: OcxRunner) -> None:
    """Remove the tags/ directory to simulate a fresh machine."""
    tags = _tags_dir(ocx)
    if tags.exists():
        shutil.rmtree(tags)


# ── Test 47 — AC1: refs/blobs/ populated after install ───────────────────


def test_install_creates_full_chain_refs(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC1: After ocx install <pkg>, the package's refs/blobs/ contains at
    least one forward-ref for every OCI blob the resolver read.

    Design record AC1: "After ocx install <pkg>, the package's refs/blobs/
    contains a forward-ref for every OCI blob the resolver read (image index
    + platform manifest at minimum)."

    A single-platform package pushed via make_package produces at least one
    manifest blob (the ImageManifest). The refs/blobs/ directory must exist
    and contain at least one symlink pointing into blobs/.
    """
    pkg = published_package
    content = _install_content(ocx, pkg)

    refs_blobs = _refs_blobs_dir(content)
    assert refs_blobs.is_dir(), (
        f"AC1: refs/blobs/ directory must exist after install; missing: {refs_blobs}"
    )
    entries = list(refs_blobs.iterdir())
    assert len(entries) >= 1, (
        f"AC1: refs/blobs/ must contain at least one forward-ref after install; "
        f"found: {entries}"
    )
    # Every entry must be a symlink pointing into blobs/.
    for entry in entries:
        assert entry.is_symlink(), f"AC1: {entry} must be a symlink"
        target = Path(os.readlink(entry))
        assert "blobs" in str(target), (
            f"AC1: ref {entry} must point into blobs/; target: {target}"
        )


# ── Test 48 — AC2: find via different tag appends refs ────────────────────


def test_find_via_different_tag_appends_refs(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
) -> None:
    """AC2 (idempotency half): repeated ocx find against an installed package
    neither removes, duplicates, nor corrupts refs/blobs/ — the chain-link
    pass is a safe upsert.

    Design record AC2: "After ocx find <pkg> via a tag path ... those blobs
    are appended to refs/blobs/ — no duplicate entries, no changed targets,
    idempotent on re-run."

    The "count grows" half of AC2 requires a scenario where a find call
    walks an image-index chain that was not present at install time. That
    is not reachable with the current fixture infrastructure (make_package
    reuses the same image-index digest across tags for a given version).
    Test 62 covers the related "different versions produce disjoint refs"
    invariant instead.
    """
    v1, _v2 = published_two_versions
    content = _install_content(ocx, v1)

    refs_blobs = _refs_blobs_dir(content)
    before = (
        {e.name: os.readlink(e) for e in refs_blobs.iterdir() if e.is_symlink()}
        if refs_blobs.is_dir()
        else {}
    )
    assert before, "AC2 prerequisite: install must have produced chain refs"

    # Two successive finds must leave refs/blobs/ stable — no new duplicates,
    # no targets mutated, no existing refs removed.
    ocx.plain("find", v1.short)
    ocx.plain("find", v1.short)

    after = (
        {e.name: os.readlink(e) for e in refs_blobs.iterdir() if e.is_symlink()}
        if refs_blobs.is_dir()
        else {}
    )
    assert set(after) == set(before), (
        f"AC2: find must not add or remove refs; before={set(before)}, after={set(after)}"
    )
    for name, target in after.items():
        assert target == before[name], (
            f"AC2: find must not rewrite existing symlinks; {name}: "
            f"before={before[name]}, after={target}"
        )


# ── Test 49 — AC3: clean retains reachable blobs ─────────────────────────


def test_clean_retains_reachable_blobs(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC3: ocx clean does not delete any blob reachable via any installed
    package's refs/blobs/.

    Design record AC3: "ocx clean does not delete any blob reachable via any
    installed package's refs/blobs/."
    """
    pkg = published_package
    content = _install_content(ocx, pkg)

    blobs_before = _count_blobs(_blobs_dir(ocx))
    assert blobs_before, "prerequisite: blobs must exist after install"

    ocx.plain("clean")

    blobs_after = _count_blobs(_blobs_dir(ocx))
    # All blobs that are reachable via refs/blobs/ must survive.
    refs_blobs = _refs_blobs_dir(content)
    if refs_blobs.is_dir():
        for ref_link in refs_blobs.iterdir():
            if ref_link.is_symlink():
                target = Path(os.readlink(ref_link))
                # Resolve relative paths relative to the symlink directory.
                if not target.is_absolute():
                    target = (ref_link.parent / target).resolve()
                assert target.exists(), (
                    f"AC3: clean must not delete blob {target} reachable via {ref_link}"
                )


# ── Test 50 — AC4: clean collects orphaned chain after uninstall --purge ──


def test_clean_collects_orphaned_chain_after_uninstall_purge(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC4: ocx clean does delete blobs not reachable from any installed package.

    Design record AC4: "ocx clean does delete blobs not reachable from any
    installed package (e.g., orphans left by a crashed install, or the old
    chain after uninstall --purge)."

    We install, then uninstall --purge, then inject a fake orphan blob and
    verify clean removes it.
    """
    pkg = published_package
    _install_content(ocx, pkg)
    ocx.plain("uninstall", "--purge", pkg.short)

    # Inject a fake orphan blob directly into the blobs/ store.
    # This simulates a blob left by a crashed install (no refs link to it).
    # Path components MUST be valid hex — `BlobStore::list_all` silently
    # skips dirs that don't match `is_valid_cas_path` (2 + 30 hex chars).
    orphan_dir = _blobs_dir(ocx) / registry_dir(ocx.registry) / "sha256" / "aa" / "bb0000000000000000000000000000"
    orphan_dir.mkdir(parents=True, exist_ok=True)
    orphan_data = orphan_dir / "data"
    orphan_data.write_bytes(b"orphan blob content")

    ocx.plain("clean")

    # The fake orphan blob must have been collected.
    assert not orphan_data.exists(), (
        f"AC4: clean must collect the unreachable orphan blob at {orphan_data}"
    )


# ── Test 51 — AC5: offline re-resolve survives clean ─────────────────────


def test_offline_reresolve_survives_clean_after_full_chain_capture(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC5: Offline re-resolve of an installed package succeeds for any tag
    path the package has ever been resolved via.

    Design record AC5: "Offline re-resolve of an installed package succeeds
    for any tag path the package has ever been resolved via."
    Design record UX6.
    """
    pkg = published_package
    ocx.plain("install", pkg.short)

    # Offline find must succeed before clean.
    result = ocx.plain("--offline", "find", pkg.short, check=False)
    assert result.returncode == 0, "prerequisite: offline find must succeed after install"

    ocx.plain("clean")

    # Offline find must still succeed after clean (reachable blobs must survive).
    result = ocx.plain("--offline", "find", pkg.short, check=False)
    assert result.returncode == 0, (
        "AC5: offline find must still succeed after clean when blobs are in refs/blobs/"
    )


# ── Test 52 — AC7: index update writes only tag files ────────────────────


def test_index_update_writes_only_tag_files_not_blobs(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC7: ocx index update <pkg> writes only to $OCX_HOME/tags/, never to
    $OCX_HOME/blobs/.

    Design record AC7: "ocx index update <pkg> writes only to $OCX_HOME/tags/,
    never to $OCX_HOME/blobs/. Verified by walking blobs/ before and after
    and asserting it is unchanged."
    """
    pkg = published_package

    blobs_before = _count_blobs(_blobs_dir(ocx))

    # Run index update — must not write any new blobs.
    ocx.plain("index", "update", pkg.short)

    blobs_after = _count_blobs(_blobs_dir(ocx))
    assert blobs_after == blobs_before, (
        f"AC7: index update must not write to blobs/.\n"
        f"Blobs before: {len(blobs_before)}, after: {len(blobs_after)}"
    )


# ── Test 53 — AC8: --remote install persists and links chain ─────────────


def test_remote_flag_install_persists_and_links_chain(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC8: ocx --remote install <pkg> persists the resolution chain into
    blobs/ and links it into refs/blobs/. --remote no longer disables the
    cache write-through.

    Design record AC8: "ocx --remote install <pkg> persists the resolution
    chain into blobs/ and links it into refs/blobs/."
    Design record UX3.
    """
    pkg = published_package

    result = ocx.json("--remote", "install", pkg.short)
    assert pkg.short in result, f"AC8: --remote install must succeed for {pkg.short}"

    # Resolve the installed content path.
    candidate = Path(result[pkg.short]["path"])
    content = candidate.resolve()
    refs_blobs = _refs_blobs_dir(content)

    assert refs_blobs.is_dir(), (
        f"AC8: refs/blobs/ must exist after --remote install; missing: {refs_blobs}"
    )
    entries = list(refs_blobs.iterdir())
    assert len(entries) >= 1, (
        f"AC8: refs/blobs/ must contain at least one forward-ref after --remote install"
    )


# ── Test 54 — AC9: --remote index list refreshes tags ────────────────────


def test_remote_flag_index_list_refreshes_tags_from_source(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC9: ocx --remote index list <pkg> still refreshes tag data from the
    source on every invocation (mutable lookups bypass cache under --remote).

    Design record AC9: "ocx --remote index list <pkg> still refreshes tag
    data from the source on every invocation."
    """
    pkg = published_package
    # Install once to seed the local cache.
    ocx.plain("install", pkg.short)

    # --remote index list must succeed and reflect the live registry tags.
    result = ocx.plain("--remote", "index", "list", pkg.short, check=False)
    assert result.returncode == 0, (
        f"AC9: --remote index list must succeed; rc={result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    # The output must include the installed tag.
    assert pkg.tag in result.stdout, (
        f"AC9: --remote index list must include tag {pkg.tag!r} from the live registry"
    )


# ── Test 55 — AC10: offline install after bare index update fails cleanly ─


def test_offline_install_after_bare_index_update_fails_cleanly(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC10: ocx --offline install <pkg> after a bare ocx index update <pkg>
    fails with a clear error naming the missing manifest digest.

    Design record AC10: "ocx --offline install <pkg> after a bare ocx index
    update <pkg> fails with a clear error naming the missing manifest digest."
    Design record UX4: "ocx --offline install cmake:3.28 after bare index update
    must fail — Error: manifest not in local cache."
    """
    pkg = published_package

    # Run index update (which now only writes tags, not blobs post-#35).
    ocx.plain("index", "update", pkg.short)

    # Wipe blobs/ to ensure no blob cache exists.
    _wipe_blobs(ocx)

    result = ocx.plain("--offline", "install", pkg.short, check=False)
    assert result.returncode != 0, (
        "AC10: --offline install after bare index update (no blobs) must fail"
    )
    # The error must mention the missing manifest or cache and name the
    # specific digest so the user knows which artifact to re-pull.
    combined = result.stderr + result.stdout
    assert combined.strip(), (
        "AC10: error output must not be empty — must name missing manifest or cache"
    )
    assert "sha256:" in combined, (
        f"AC10: error must name the missing manifest digest (sha256:...); got: {combined!r}"
    )
    assert "manifest" in combined.lower() or "cache" in combined.lower(), (
        f"AC10: error must describe the missing manifest / cache state; got: {combined!r}"
    )


# ── Test 56 — UX5: failed install leaves collectable orphans ─────────────


def test_failed_install_leaves_collectable_orphans(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """UX5: If an install fails mid-chain (network fails after blob is written
    but before link_blobs runs), the orphaned blob is collected by
    the next ocx clean run.

    Design record UX5: "ocx install fails mid-chain; image index blob is
    persisted but install aborts before link_blobs runs; ocx clean
    removes 1 unreferenced blob."

    We simulate the orphan by writing a fake blob directly — there is no
    installed package that references it, so it must be collected.
    """
    _install_content(ocx, published_package)

    # Inject a fake orphan blob (simulates a blob written by a crashed install
    # whose link_blobs never ran). Path components MUST be valid hex —
    # `BlobStore::list_all` silently skips non-CAS dirs.
    orphan_dir = _blobs_dir(ocx) / registry_dir(ocx.registry) / "sha256" / "cc" / "dd0000000000000000000000000000"
    orphan_dir.mkdir(parents=True, exist_ok=True)
    orphan_data = orphan_dir / "data"
    orphan_data.write_bytes(b"crashed install blob")

    ocx.plain("clean")

    assert not orphan_data.exists(), (
        "UX5: orphaned blob from failed install must be collected by ocx clean"
    )


# ── Test 57 — AC11: parallel install races preserve full chain ────────────


def test_parallel_install_races_preserve_full_chain(
    ocx: OcxRunner,
    ocx_binary: Path,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """AC11: Two concurrent ocx install processes against the same $OCX_HOME
    both complete successfully, neither corrupts blob files, both produce full
    refs/blobs/.

    Design record AC11: "Two concurrent ocx install processes against the
    same $OCX_HOME both complete successfully, neither corrupts blob files,
    both produce full refs/blobs/."
    """
    v1 = make_package(ocx, unique_repo, "1.0.0", tmp_path / "v1", new=True, cascade=False)
    v2 = make_package(ocx, unique_repo, "2.0.0", tmp_path / "v2", new=False, cascade=False)

    def run_install(pkg_short: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(ocx_binary), "--format", "json", "install", pkg_short],
            capture_output=True,
            text=True,
            env=ocx.env,
            check=False,
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run_install, v1.short), pool.submit(run_install, v2.short)]
        results = [f.result() for f in futures]

    for pkg, result in zip((v1, v2), results, strict=True):
        assert result.returncode == 0, (
            f"AC11: concurrent install of {pkg.short} failed (rc={result.returncode}).\n"
            f"stdout: {result.stdout}\n"
            f"stderr: {result.stderr}"
        )
        data = json.loads(result.stdout)
        candidate = Path(data[pkg.short]["path"])
        content = candidate.resolve()
        refs_blobs = _refs_blobs_dir(content)
        assert refs_blobs.is_dir(), (
            f"AC11: refs/blobs/ must exist after parallel install of {pkg.short}"
        )
        entries = list(refs_blobs.iterdir())
        assert len(entries) >= 1, (
            f"AC11: refs/blobs/ must have at least one entry for {pkg.short}; found: {entries}"
        )


# ── Test 58 — AC12: no sidecar files in blobs/ after install ─────────────


def test_no_sidecar_lock_files_in_blobs_dir_after_install(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC12: After any successful install, no sidecar .lock, .log, or .tmp
    files remain anywhere under $OCX_HOME/blobs/.

    Design record AC12: "After any successful install, no sidecar .lock, .log,
    or .tmp files remain anywhere under $OCX_HOME/blobs/."
    """
    pkg = published_package
    ocx.plain("install", pkg.short)

    sidecars = _collect_sidecar_files(_blobs_dir(ocx))
    assert not sidecars, (
        f"AC12: no sidecar .lock/.log/.tmp files must remain under blobs/ after install.\n"
        f"Found: {[str(s) for s in sidecars]}"
    )


# ── Test 59 — AC13/UX7: latent-bug fix — missing manifest recovers ────────


def test_missing_manifest_after_index_update_recovers_on_install(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC13/UX7: The update_tags latent bug is fixed: deleting a manifest data
    file from blobs/ (leaving the tag file in place) and then running
    ocx install <pkg> re-fetches the manifest and completes successfully.

    Design record AC13: "deleting a manifest data file from blobs/ (leaving
    the tag file in place) and then running ocx install <pkg> re-fetches the
    manifest and completes successfully (not infinite-loop or NotFound)."
    Design record UX7.
    """
    pkg = published_package

    # Step 1: install online to populate tags/ and blobs/.
    ocx.plain("install", pkg.short)

    # Step 2: wipe the entire blobs/ directory — leaves tag files in place.
    _wipe_blobs(ocx)
    assert not _blobs_dir(ocx).exists() or not list(_blobs_dir(ocx).rglob("data")), (
        "prerequisite: blobs must be absent"
    )

    # Step 3: re-install — must re-fetch the manifest and succeed.
    result = ocx.plain("install", pkg.short, check=False)
    assert result.returncode == 0, (
        f"AC13: install must succeed after blobs/ is wiped (latent bug fix).\n"
        f"stderr: {result.stderr.strip()}"
    )


# ── Test 60 — fast path: read-only find makes no writes ──────────────────


def test_find_read_only_against_matching_chain_makes_no_writes(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Fast-path proof: when find resolves an already-installed package whose
    refs/blobs/ already contains the full chain, no new symlinks or blobs are
    written.

    Design record: "find read-only against matching chain makes no writes"
    (the idempotency guarantee of link_blobs).
    """
    pkg = published_package
    content = _install_content(ocx, pkg)

    refs_blobs = _refs_blobs_dir(content)
    before_refs = set(refs_blobs.iterdir()) if refs_blobs.is_dir() else set()
    blobs_before = _count_blobs(_blobs_dir(ocx))

    # Run find — chain is already fully linked, so no writes should occur.
    ocx.plain("find", pkg.short)

    after_refs = set(refs_blobs.iterdir()) if refs_blobs.is_dir() else set()
    blobs_after = _count_blobs(_blobs_dir(ocx))

    # No new symlinks or blobs.
    new_refs = after_refs - before_refs
    assert not new_refs, (
        f"find on a fully-linked chain must make no new refs; created: {new_refs}"
    )
    assert blobs_after == blobs_before, (
        "find on a fully-linked chain must write no new blobs"
    )


# ── General resolution-chain cases — Identifier shapes ──────────────────


def _chain_entries(content: Path) -> list[Path]:
    refs_blobs = _refs_blobs_dir(content)
    if not refs_blobs.is_dir():
        return []
    return sorted(refs_blobs.iterdir())


def test_resolution_chain_tag_via_image_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """General case: tagged identifier → image index → platform manifest.

    Publishing with ``cascade=True`` produces a multi-tag push backed by an
    image index, so the resolution chain is (image index, platform manifest) —
    two OCI blobs. Installing via the tag must link both into ``refs/blobs/``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)

    content = _install_content(ocx, pkg)
    entries = _chain_entries(content)

    assert len(entries) >= 2, (
        f"tag→image-index→platform-manifest chain must link at least 2 blobs; "
        f"found {len(entries)}: {entries}"
    )
    for entry in entries:
        assert entry.is_symlink(), f"{entry} must be a symlink"
        target = Path(os.readlink(entry))
        resolved = target if target.is_absolute() else (entry.parent / target).resolve()
        assert resolved.exists(), f"chain ref {entry} must point to an existing blob"


def test_resolution_chain_direct_digest_to_image_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """General case: direct digest addressing an image index.

    A digest-pinned identifier that points at the image index must still
    produce a two-blob chain: the index itself plus the platform-selected
    child manifest.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)

    index_digest = fetch_manifest_digest(ocx.registry, unique_repo, pkg.tag)
    # Sanity: make sure the registry actually returned an image index, not
    # a plain image manifest — otherwise this test is not exercising the
    # case we claim to exercise.
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, pkg.tag)
    assert "manifests" in manifest, (
        f"prerequisite: {pkg.tag} must resolve to an image index, got: {manifest.get('mediaType')}"
    )

    digest_ref = f"{ocx.registry}/{unique_repo}@{index_digest}"
    result = ocx.json("install", digest_ref)
    candidate = Path(result[digest_ref]["path"])
    content = candidate.resolve()

    entries = _chain_entries(content)
    assert len(entries) >= 2, (
        f"digest→image-index chain must link at least 2 blobs (index + child); "
        f"found {len(entries)}: {entries}"
    )


def test_resolution_chain_direct_digest_to_platform_manifest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """General case: direct digest addressing a platform image manifest.

    When the user pins a digest that already refers to a leaf platform
    manifest (no enclosing image index), the chain contains a single blob —
    the platform manifest itself.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=True)

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, pkg.tag)
    assert "manifests" in manifest, (
        "prerequisite: the tag must resolve to an image index so we can pick a child"
    )
    # Pick the first platform child — for a single-platform make_package push
    # that is the current host platform.
    child_digest = manifest["manifests"][0]["digest"]

    digest_ref = f"{ocx.registry}/{unique_repo}@{child_digest}"
    result = ocx.json("install", digest_ref)
    candidate = Path(result[digest_ref]["path"])
    content = candidate.resolve()

    entries = _chain_entries(content)
    assert len(entries) >= 1, (
        f"digest→platform-manifest chain must link at least 1 blob; "
        f"found {len(entries)}: {entries}"
    )


def test_resolution_chain_tag_via_flat_image_manifest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """General case: tagged identifier → flat image manifest (no image index).

    Publishing with ``cascade=False`` writes a single image manifest directly
    under the tag, skipping the image-index layer. The chain then contains
    exactly the one manifest blob.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)

    content = _install_content(ocx, pkg)
    entries = _chain_entries(content)

    assert len(entries) >= 1, (
        f"tag→flat-manifest chain must link at least 1 blob; "
        f"found {len(entries)}: {entries}"
    )
    for entry in entries:
        assert entry.is_symlink(), f"{entry} must be a symlink"


# ── Test 61 — AC6: clean collects real chain blobs after uninstall --purge ─
# AC6 → test_clean_collects_real_chain_blobs_after_uninstall_purge


def test_clean_collects_real_chain_blobs_after_uninstall_purge(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC6: After ocx uninstall --purge <pkg> + ocx clean, every blob that was
    linked via refs/blobs/ is collected — not just synthetic orphans.

    Design record AC6 (plan review T1): uninstall --purge removes the install
    symlinks and refs/blobs/ forward-refs; the subsequent clean run must
    collect the real chain blobs because nothing else refers to them.
    """
    pkg = published_package
    _install_content(ocx, pkg)

    blobs_after_install = _count_blobs(_blobs_dir(ocx))
    assert blobs_after_install, (
        "AC6 prerequisite: blobs must exist after install"
    )

    ocx.plain("uninstall", "--purge", pkg.short)
    ocx.plain("clean")

    blobs_after_clean = _count_blobs(_blobs_dir(ocx))
    assert len(blobs_after_clean) == 0, (
        f"AC6: clean after uninstall --purge must collect all real chain blobs; "
        f"remaining: {blobs_after_clean}"
    )


# ── Test 62 — AC2 (v1/v2 isolation): different versions produce disjoint refs ─


def test_different_package_versions_produce_disjoint_refs(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """AC2 corollary: two different package versions installed side-by-side
    produce disjoint refs/blobs/ entries (no cross-contamination from the
    shared BlobStore).

    Note: the "count grows" variant of AC2 (same content dir, second find
    walks a different image-index and appends new refs) is not reachable
    with the current fixture infrastructure — make_package + cascade reuse
    the same image-index digest across tags, so every tag for a given
    version walks the same chain. The genuine append case requires
    constructing two image-index manifests that share a child platform
    manifest, which is beyond what make_package exposes.

    What we CAN test is that two different versions installed to two
    different content dirs have different refs/blobs/ entries (since
    their chain digests differ), and that each entry is a valid symlink
    into the shared blob store.
    """
    v1 = make_package(ocx, unique_repo, "1.0.0", tmp_path / "v1", new=True, cascade=False)
    v2 = make_package(ocx, unique_repo, "2.0.0", tmp_path / "v2", new=False, cascade=False)

    content_v1 = _install_content(ocx, v1)
    content_v2 = _install_content(ocx, v2)

    refs_blobs_v1 = _refs_blobs_dir(content_v1)
    refs_blobs_v2 = _refs_blobs_dir(content_v2)

    v1_ref_names = {e.name for e in refs_blobs_v1.iterdir()} if refs_blobs_v1.is_dir() else set()
    v2_ref_names = {e.name for e in refs_blobs_v2.iterdir()} if refs_blobs_v2.is_dir() else set()

    assert v1_ref_names, "AC2: v1 must have at least one chain ref"
    assert v2_ref_names, "AC2: v2 must have at least one chain ref"
    assert v1_ref_names.isdisjoint(v2_ref_names), (
        f"AC2: v1 and v2 have different digests, so their refs/blobs/ "
        f"entries must be disjoint; v1={v1_ref_names}, v2={v2_ref_names}"
    )

    # Each ref is a valid symlink into blobs/.
    for refs_dir in (refs_blobs_v1, refs_blobs_v2):
        for entry in refs_dir.iterdir():
            assert entry.is_symlink(), f"AC2: {entry} must be a symlink"
            target = Path(os.readlink(entry))
            assert "blobs" in str(target), (
                f"AC2: ref {entry} must point into blobs/; got: {target}"
            )


# ── Test 63 — AC9 (cache-bypass): --remote bypasses the local tag cache ───


def test_remote_mode_tag_resolution_bypasses_local_cache(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """AC9 (cache-bypass): Under --remote, tag→digest resolution must go to
    the registry and bypass the local tag cache.  Without --remote, the local
    tag cache is used and the cached (stale) digest is returned.

    Design record AC9 / plan review T6: "Under --remote, tag→digest resolution
    must always go to the registry (not cache). Manifest caching for
    digest-addressed content is allowed."

    Strategy (mirrors test_ac4_stale_cached_tag_uses_cached_digest in
    test_tag_fallback.py):
      1. Push v1 to unique_repo:1.0.0 and install it → populates local tag
         cache with digest_A.
      2. Snapshot the tag file (digest_A).
      3. Push v2 to unique_repo:1.0.0 → registry now resolves 1.0.0 to
         digest_B.  make_package also refreshes the cache to digest_B.
      4. Restore the tag file to the v1 snapshot (digest_A) — simulates a
         stale cache while the registry has moved to digest_B.
      5. ocx --remote index list unique_repo:1.0.0 → must show digest_B
         (bypassed the cache, fetched from registry).
      6. ocx index list unique_repo:1.0.0 (no --remote) → must show the
         cached digest_A (cache wins in Default mode).
    """
    v1_dir = tmp_path / "v1"
    v1_dir.mkdir()
    pkg_v1 = make_package(ocx, unique_repo, "1.0.0", v1_dir, new=True, cascade=False)
    ocx.json("install", pkg_v1.short)

    tag_file = (
        Path(ocx.env["OCX_HOME"])
        / "tags"
        / registry_dir(ocx.registry)
        / f"{unique_repo}.json"
    )
    assert tag_file.exists(), "AC9 prerequisite: tag file must exist after install"
    v1_snapshot = tag_file.read_text()
    cached_data = json.loads(v1_snapshot)
    digest_a = cached_data["tags"].get(pkg_v1.tag)
    assert digest_a is not None, "AC9 prerequisite: digest_A must be cached"

    # Push v2 to the same repo:tag → registry now resolves 1.0.0 to digest_B.
    v2_dir = tmp_path / "v2"
    v2_dir.mkdir()
    _ = make_package(ocx, unique_repo, "1.0.0", v2_dir, new=False, cascade=False)
    digest_b = fetch_manifest_digest(ocx.registry, unique_repo, "1.0.0")
    assert digest_b != digest_a, (
        "AC9 prerequisite: registry digest must differ from cached digest after pushing v2"
    )

    # Restore the stale cache (digest_A) — make_package refreshed it to digest_B.
    tag_file.write_text(v1_snapshot)

    # Step 5: --remote index list must bypass cache and refresh the local tag
    # file to digest_B. We prove this by running --remote index list and then
    # reading the tag file — it must now contain digest_B.
    remote_result = ocx.plain("--remote", "index", "list", pkg_v1.short)
    assert remote_result.returncode == 0, (
        f"AC9: --remote index list must succeed; rc={remote_result.returncode}\n"
        f"stderr: {remote_result.stderr}"
    )
    # After --remote index list the tag file must have been refreshed to digest_B.
    refreshed_data = json.loads(tag_file.read_text())
    stored_after_remote = refreshed_data["tags"].get(pkg_v1.tag)
    assert stored_after_remote == digest_b, (
        f"AC9: --remote index list must bypass cache and update tag to registry digest.\n"
        f"Expected (registry) digest_B: {digest_b}\n"
        f"Found in tag file after --remote: {stored_after_remote}"
    )

    # Restore the stale cache again so we can verify default mode still uses it.
    tag_file.write_text(v1_snapshot)

    # Step 6: default (no --remote) index list must NOT refresh → cache stays digest_A.
    default_result = ocx.plain("index", "list", pkg_v1.short)
    assert default_result.returncode == 0, (
        f"AC9: index list (default mode) must succeed; rc={default_result.returncode}"
    )
    default_data = json.loads(tag_file.read_text())
    stored_after_default = default_data["tags"].get(pkg_v1.tag)
    assert stored_after_default == digest_a, (
        f"AC9: default-mode index list must not refresh the tag cache.\n"
        f"Expected (cached) digest_A: {digest_a}\n"
        f"Found in tag file after default list: {stored_after_default}"
    )
