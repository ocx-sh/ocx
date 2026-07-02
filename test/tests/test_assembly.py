# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for hardlink-based package assembly (Sub-plan 6).

Coverage:
  I2 — Versioned shared-library symlink chain: package contains
       libfoo.so -> libfoo.so.1 -> libfoo.so.1.2.3.  After install
       the chain is preserved verbatim in the package content/.
  I5 — Layer dedup via inode sharing: two packages whose OCI layers
       are identical share the same layer extraction.  A file that
       lives in that shared layer has the same inode in both
       packages' content/ directories.
  I6 — Physical dedup: the two packages from I5 together occupy
       roughly one layer's worth of disk space (not two), because
       the hardlinks share inodes.

Deferred:
  I1 — Relative-RPATH binary ($ORIGIN/../lib/libfoo.so): deferred
       because constructing a binary with a custom RPATH requires
       either a C toolchain in the test environment or shipping a
       pre-compiled fixture binary.  This warrants its own test
       fixture authoring effort.
       See .claude/artifacts/plan_hardlink_assembly.md Sub-plan 6 / I1.

Shared-layer construction (Option B):
  make_package() cannot force two different packages to reference the
  same OCI layer because each package has a unique marker baked into
  its binary (different bytes → different layer digest).  To get a
  genuine shared layer we push the *same* tarball to two different
  OCI repositories, with *different* metadata for each.

  The OCI manifest contains:
    - Layer blob: identical (same tarball bytes → same digest)
    - Config blob: different per-repo metadata → different digest
    - Resulting manifest digest: different per repo → distinct packages

  Different manifest digests → two separate entries in packages/.
  Identical layer digest → one entry in layers/ shared by both.
  ocx extracts the layer once and hardlinks from it into both packages.
"""
from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src.runner import OcxRunner, current_platform

# ---------------------------------------------------------------------------
# Internal helpers (prefixed _ to signal test-file scope)
#
# NOTE: `_make_two_packages_sharing_layer` is imported by `test_purge.py` —
# cross-test-file imports are idiomatic pytest (tests/ is on rootdir's path).
# Keep the canonical helper here; `test_purge.py` reuses it to guarantee
# both test files exercise the same shared-layer construction.
# ---------------------------------------------------------------------------


def _find_content_path(ocx: OcxRunner, short: str) -> Path:
    """Return the content/ path for an installed package via `ocx package which`.

    `ocx package which` reports the package root; this helper traverses into `content/`
    so call sites that exercise installed files keep their natural shape.
    """
    result = ocx.json("package", "which", short)
    return Path(result[short]) / "content"


def _build_fixed_pkg_dir(tmp_path: Path, subdir: str) -> Path:
    """Build a package directory with a deterministic binary (no random marker).

    All bytes are fixed so two packages built from the same subdir produce
    an identical tarball and therefore the same OCI layer digest.

    Tests using this helper are Unix-only (shared-layer tests require inode
    semantics); no Windows fallback is provided.
    """
    pkg_dir = tmp_path / subdir
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    hello = bin_dir / "hello"
    # Write a deterministic 128 KB file so the shared layer dominates over
    # per-directory metadata overhead in the du-based dedup test (I6).
    # A tiny file (~40 bytes) leaves the ratio dominated by directory blocks,
    # causing spurious failures on filesystems with large block sizes.
    line = "#!/bin/sh\necho shared-layer-binary\n"
    padding = "# padding " + "X" * 117 + "\n"  # 128 chars per line
    hello.write_text(line + padding * 1000)  # ~128 KB
    hello.chmod(hello.stat().st_mode | stat.S_IEXEC)
    return pkg_dir


def _write_minimal_metadata(path: Path) -> None:
    """Write a minimal but valid package metadata file."""
    path.write_text(
        json.dumps(
            {
                "type": "bundle", "version": 1, "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": "${installPath}/bin",
                    }
                ],
            }
        )
    )


def _push_bundle(
    ocx: OcxRunner,
    pkg_dir: Path,
    metadata_path: Path,
    bundle_path: Path,
    fq: str,
    *,
    new: bool = True,
) -> None:
    """Bundle the pkg_dir and push it to the registry under fq."""
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata_path),
        "-o",
        str(bundle_path),
        str(pkg_dir),
    )
    push_args = [
        "package",
        "push",
        "-p",
        current_platform(),
        "-m",
        str(metadata_path),
    ]
    if new:
        push_args.append("-n")
    push_args += ["-i", fq, str(bundle_path)]
    ocx.plain(*push_args)


def _write_repo_metadata(path: Path, repo: str) -> None:
    """Write metadata with a repo-specific HOME key so each package has a unique config blob.

    Same binary content (layer blob) + different metadata (config blob) = different
    manifest digest = distinct entries in packages/ while sharing the same layer.
    """
    home_key = repo.upper().replace("-", "_").replace(".", "_") + "_HOME"
    path.write_text(
        json.dumps(
            {
                "type": "bundle", "version": 1, "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": "${installPath}/bin",
                    },
                    {
                        "key": home_key,
                        "type": "constant",
                        "value": "${installPath}",
                    },
                ],
            }
        )
    )


def _make_two_packages_sharing_layer(
    ocx: OcxRunner,
    tmp_path: Path,
    repo_base: str,
) -> tuple[str, str, str]:
    """Push the same binary layer to two OCI repositories with distinct metadata.

    Canonical shared-layer construction, reused by `test_assembly.py` and
    `test_purge.py`.

    Layer construction:
    - Same tarball bytes → same layer digest → shared layers/ entry
    - Different metadata per repo → different config blob → different manifest digest
    - Result: two distinct entries in packages/ that hardlink from one layers/ entry

    Args:
      ocx:        test runner
      tmp_path:   per-test temp dir (for pkg_dir, bundle, metadata files)
      repo_base:  unique repo prefix; `{repo_base}_a` and `{repo_base}_b`
                  are created in the registry

    Returns:
      (short_a, short_b, shared_file_rel_path) — the `repo:tag` identifiers
      for both packages and the path (relative to a package's content/ root)
      of the file that lives in the shared layer and should therefore be
      hardlinked across both installs.
    """
    repo_a = f"{repo_base}_a"
    repo_b = f"{repo_base}_b"
    tag = "1.0.0"
    shared_file_rel_path = "bin/hello"

    # Build a single pkg_dir with deterministic content (no random markers)
    pkg_dir = _build_fixed_pkg_dir(tmp_path, "shared-pkg")

    # Create bundle once — reuse the same tarball for both pushes.
    # The bundle is just the binary content; metadata is provided separately at push time.
    metadata_path = tmp_path / "shared-bundle-metadata.json"
    _write_minimal_metadata(metadata_path)
    bundle_path = tmp_path / "shared-bundle.tar.xz"
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata_path),
        "-o",
        str(bundle_path),
        str(pkg_dir),
    )

    # Push the same tarball to each repo with repo-specific metadata.
    # Different metadata → different config blob → different manifest digest →
    # distinct packages/ entries that both share the same layer.
    for repo in (repo_a, repo_b):
        repo_metadata_path = tmp_path / f"metadata-{repo}.json"
        _write_repo_metadata(repo_metadata_path, repo)
        fq = f"{ocx.registry}/{repo}:{tag}"
        ocx.plain(
            "package",
            "push",
            "-n",
            "-p",
            current_platform(),
            "-m",
            str(repo_metadata_path),
            "-i",
            fq,
            str(bundle_path),
        )
        short = f"{repo}:{tag}"
        ocx.plain("index", "update", short)

    return f"{repo_a}:{tag}", f"{repo_b}:{tag}", shared_file_rel_path


# ---------------------------------------------------------------------------
# I2: Versioned shared-library symlink chain
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks require Unix or Developer Mode")
def test_versioned_symlink_chain_preserved_after_install(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """I2: A versioned .so chain is preserved verbatim in the package content/.

    The package contains:
      lib/libfoo.so       -> libfoo.so.1          (relative, same dir)
      lib/libfoo.so.1     -> libfoo.so.1.2.3       (relative, same dir)
      lib/libfoo.so.1.2.3 (regular file — the real library)

    After install, all three entries must exist in the package content/
    and the symlinks must have identical target strings to what was
    packaged (verbatim preservation).  Resolving the top of the chain
    must also reach the actual library bytes — the hardlink walker must
    copy content, not just metadata.
    """
    tag = "1.0.0"
    marker = f"marker-{uuid4().hex[:12]}"

    # Build package directory with a versioned symlink chain in lib/
    pkg_dir = tmp_path / "pkg-symchain"
    bin_dir = pkg_dir / "bin"
    lib_dir = pkg_dir / "lib"
    bin_dir.mkdir(parents=True)
    lib_dir.mkdir(parents=True)

    # Real binary
    hello = bin_dir / "hello"
    hello.write_text(f"#!/bin/sh\necho {marker}\n")
    hello.chmod(hello.stat().st_mode | stat.S_IEXEC)

    # Versioned library chain (real file + two relative symlinks).
    # The payload is 8 KiB so it is guaranteed to allocate at least one
    # filesystem block on all common filesystems (ext4, xfs, apfs, tmpfs).
    # Subsequent dedup/inode tests rely on non-zero block counts.
    real_lib = lib_dir / "libfoo.so.1.2.3"
    source_bytes = b"x" * 8192
    real_lib.write_bytes(source_bytes)

    link_v1 = lib_dir / "libfoo.so.1"
    link_v1.symlink_to("libfoo.so.1.2.3")

    link_major = lib_dir / "libfoo.so"
    link_major.symlink_to("libfoo.so.1")

    # Metadata
    metadata_path = tmp_path / "metadata-symchain.json"
    _write_minimal_metadata(metadata_path)

    # Bundle + push
    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    bundle_path = tmp_path / "bundle-symchain.tar.xz"
    _push_bundle(ocx, pkg_dir, metadata_path, bundle_path, fq)
    ocx.plain("index", "update", f"{unique_repo}:{tag}")

    # Install
    short = f"{unique_repo}:{tag}"
    ocx.json("package", "install", short)
    content_path = _find_content_path(ocx, short)

    # Verify: real library file exists
    installed_real = content_path / "lib" / "libfoo.so.1.2.3"
    assert installed_real.exists(), f"Real library not found at {installed_real}"

    # Verify: symlinks are preserved with verbatim target strings
    installed_v1 = content_path / "lib" / "libfoo.so.1"
    assert installed_v1.is_symlink(), f"libfoo.so.1 is not a symlink in installed package"
    assert os.readlink(str(installed_v1)) == "libfoo.so.1.2.3", (
        f"libfoo.so.1 target mismatch: got {os.readlink(str(installed_v1))!r}"
    )

    installed_major = content_path / "lib" / "libfoo.so"
    assert installed_major.is_symlink(), f"libfoo.so is not a symlink in installed package"
    assert os.readlink(str(installed_major)) == "libfoo.so.1", (
        f"libfoo.so target mismatch: got {os.readlink(str(installed_major))!r}"
    )

    # Verify: chain resolves — following links reaches the real file
    assert installed_major.resolve().name == "libfoo.so.1.2.3", (
        "libfoo.so chain does not resolve to libfoo.so.1.2.3"
    )

    # Verify: chain resolves to the actual packaged bytes end-to-end.
    # This catches regressions where the walker would create the symlink
    # targets but copy a wrong/empty file, or where resolution somehow
    # lands on a different library with the same name.
    resolved_bytes = installed_major.resolve().read_bytes()
    assert resolved_bytes == source_bytes, (
        "libfoo.so chain must resolve to the original packaged content "
        f"({len(source_bytes)} bytes), got {len(resolved_bytes)} bytes"
    )


# ---------------------------------------------------------------------------
# I5: Layer dedup verified via inode sharing
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="inode comparison not meaningful on Windows")
def test_shared_layer_files_have_same_inode(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """I5: Two packages sharing a layer have files with the same inode.

    When two OCI packages reference the same layer digest, ocx extracts
    the layer once and hardlinks files from that single layer into both
    packages' content/ directories.  Files that live in the shared layer
    must therefore share an inode.
    """
    short_a, short_b, shared_file_rel = _make_two_packages_sharing_layer(ocx, tmp_path, unique_repo)

    # Install both packages
    ocx.json("package", "install", short_a)
    ocx.json("package", "install", short_b)

    content_a = _find_content_path(ocx, short_a)
    content_b = _find_content_path(ocx, short_b)

    file_a = content_a / shared_file_rel
    file_b = content_b / shared_file_rel

    assert file_a.exists(), f"File not found in package A: {file_a}"
    assert file_b.exists(), f"File not found in package B: {file_b}"

    inode_a = os.stat(str(file_a)).st_ino
    inode_b = os.stat(str(file_b)).st_ino

    assert inode_a == inode_b, (
        f"Files from packages sharing a layer should have the same inode, "
        f"but got {inode_a} (A) vs {inode_b} (B). "
        f"Paths: {file_a}, {file_b}"
    )


# ---------------------------------------------------------------------------
# I6: Physical dedup via du — shared layer counts once
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="du not available on Windows")
@pytest.mark.skipif(shutil.which("du") is None, reason="du utility not available")
def test_shared_layer_disk_usage_is_not_doubled(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """I6: Two packages sharing a layer occupy roughly one layer's worth of space.

    Physical disk usage (hardlinks counted once, as reported by du without -l)
    should be close to a single package's size.  We allow a 2x slack factor to
    account for metadata files (manifest.json, metadata.json, refs/, etc.) but
    assert it is significantly less than 3x, proving the layer is NOT duplicated.

    Note: du reports in 512-byte or 1KB blocks depending on OS/implementation.
    We compare relative sizes rather than absolute byte counts to stay portable.
    """
    short_a, short_b, _shared_file_rel = _make_two_packages_sharing_layer(ocx, tmp_path, unique_repo)

    # Install both packages
    ocx.json("package", "install", short_a)
    ocx.json("package", "install", short_b)

    content_a = _find_content_path(ocx, short_a)
    content_b = _find_content_path(ocx, short_b)

    # Measure physical size of each content/ individually (hardlinks counted once each
    # when measured in isolation — this gives the "real" size of a single package content/)
    def _du_blocks(path: Path) -> int:
        """Return total allocated blocks for a directory tree (hardlinks counted once)."""
        result = subprocess.run(
            ["du", "-s", str(path)],
            capture_output=True,
            text=True,
            check=True,
        )
        # du -s output: "<blocks>\t<path>"
        return int(result.stdout.split()[0])

    blocks_a = _du_blocks(content_a)
    blocks_b = _du_blocks(content_b)

    # Measure physical size of both content/ directories together
    # du counts each inode once, so shared hardlinks are counted once total
    result_combined = subprocess.run(
        ["du", "-s", str(content_a), str(content_b)],
        capture_output=True,
        text=True,
        check=True,
    )
    # du -s <dir1> <dir2> prints two lines; sum them up
    combined_blocks = sum(
        int(line.split()[0]) for line in result_combined.stdout.strip().splitlines() if line.strip()
    )

    # When du measures each dir separately it must account for shared inodes
    # in each dir's own traversal.  Combined, shared inodes are counted once.
    # For our single-layer packages: combined should be ≤ max(a, b) * 2
    # (the shared binary + per-package metadata overhead).
    # We assert combined < (blocks_a + blocks_b) * 0.75 to prove dedup.
    # If there's no sharing, combined ≈ blocks_a + blocks_b.
    # If there's perfect sharing, combined ≈ max(blocks_a, blocks_b) (metadata varies).
    #
    # Use a lenient threshold (75%) to handle filesystem-level granularity noise.
    sum_individual = blocks_a + blocks_b

    # Zero allocated blocks means the walker produced nothing — that is a real
    # assembly regression, not a reason to skip. Fail loudly.
    if sum_individual == 0:
        pytest.fail(
            "Package content has zero allocated blocks — assembly walker did not copy files"
        )

    dedup_ratio = combined_blocks / sum_individual
    assert dedup_ratio < 0.75, (
        f"Expected shared-layer dedup: combined={combined_blocks} blocks should be "
        f"< 75% of individual sum ({sum_individual} blocks), "
        f"but got ratio={dedup_ratio:.2f}. "
        f"This suggests the layer was NOT deduplicated via hardlinks."
    )


# ---------------------------------------------------------------------------
# Part 1: strip applied at assemble time, never at extraction (reuse-corruption)
#
# The package-wide strip moved out of BOTH shared-layer-store population paths
# (registry pull + local materialization) and into assemble time. These tests
# prove:
#   A1  registry path: the SAME tarball reused across two packages with
#       different metadata strip_components assembles into differently-stripped
#       content/ trees (strip is per-package, applied at assemble).
#   A1b local path: `ocx package test --output` (pull_local) applies strip at
#       assemble too — no double-strip.
#   A2  the shared layers/{registry}/{digest}/content/ store stays VERBATIM
#       (un-stripped) via BOTH paths — the whole reason strip left extraction.
#   A3  a single-layer package with strip_components still installs with the
#       leading component stripped (end-to-end regression, unchanged behavior).
#
# Against the P1.1 stub `assemble_from_layers_stripped` is unimplemented, so the
# install / materialize step panics — these acceptance rows go green once P1.7
# lands. Runners skip on Windows (Unix shell scripts + shared-layer semantics).
# ---------------------------------------------------------------------------


def _build_topdir_pkg_dir(tmp_path: Path, subdir: str) -> Path:
    """Build a package layout with a single top-level directory over bin/hello.

    The leading ``topdir/`` component is what a package-wide ``strip_components=1``
    removes at assemble time, so its presence/absence is the observable that
    distinguishes stripped from verbatim trees.
    """
    pkg_dir = tmp_path / subdir
    bin_dir = pkg_dir / "topdir" / "bin"
    bin_dir.mkdir(parents=True)
    hello = bin_dir / "hello"
    hello.write_text("#!/bin/sh\necho topdir-shared\n")
    hello.chmod(hello.stat().st_mode | stat.S_IEXEC)
    return pkg_dir


def _strip_metadata(path: Path, strip: int) -> None:
    """Write bundle metadata with an explicit ``strip_components`` value.

    The ``required`` PATH entry must point at where ``bin`` actually lands after
    the package-wide strip is applied at assemble: the source layout is
    ``topdir/bin/hello``, so ``strip=1`` hoists ``bin`` to the content root while
    ``strip=0`` keeps it under ``topdir/``. Tracking the post-strip location
    keeps ``required: True`` a genuine validation of the materialized tree (a
    fixed ``${installPath}/bin`` would spuriously fail the verbatim ``strip=0``
    layout during ``ocx package test`` env composition).
    """
    bin_rel = "bin" if strip >= 1 else "topdir/bin"
    path.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "strip_components": strip,
                "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": f"${{installPath}}/{bin_rel}",
                        "visibility": "public",
                    }
                ],
            }
        )
    )


def _layer_content_dirs(ocx: OcxRunner) -> list[Path]:
    """Return every extracted-layer ``content/`` directory in the layer store."""
    layers_root = Path(ocx.ocx_home) / "layers"
    if not layers_root.exists():
        return []
    return [p for p in layers_root.rglob("content") if p.is_dir()]


@pytest.mark.skipif(sys.platform == "win32", reason="Unix shell scripts + shared-layer inode semantics")
def test_strip_applied_at_assemble_registry_path(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """A1 + A2 (registry path): one shared tarball, two packages, per-package strip.

    Push the SAME tarball (with a top-level dir) to two repos whose metadata
    differ only in ``strip_components`` (1 vs 0). Different metadata → different
    config blob → distinct packages that share one layer digest. After install:

      - strip=1 package content/ = ``bin/hello``       (top dir stripped)
      - strip=0 package content/ = ``topdir/bin/hello`` (verbatim)

    The shared layer store (A2) must be VERBATIM in both cases — extraction never
    bakes strip into the content-addressed store.
    """
    repo_strip1 = f"{unique_repo}_strip1"
    repo_strip0 = f"{unique_repo}_strip0"
    tag = "1.0.0"

    pkg_dir = _build_topdir_pkg_dir(tmp_path, "registry-topdir-pkg")

    # Create the bundle once and reuse it for both pushes → identical layer digest.
    base_meta = tmp_path / "registry-base-meta.json"
    base_meta.write_text(json.dumps({"type": "bundle", "version": 1}))
    bundle = tmp_path / "registry-topdir-bundle.tar.xz"
    ocx.plain("package", "create", "-m", str(base_meta), "-o", str(bundle), str(pkg_dir))

    for repo, strip in ((repo_strip1, 1), (repo_strip0, 0)):
        meta = tmp_path / f"registry-meta-{repo}.json"
        _strip_metadata(meta, strip)
        fq = f"{ocx.registry}/{repo}:{tag}"
        ocx.plain(
            "package", "push", "-n", "-p", current_platform(), "-m", str(meta), "-i", fq, str(bundle)
        )
        ocx.plain("index", "update", f"{repo}:{tag}")

    ocx.json("package", "install", f"{repo_strip1}:{tag}")
    ocx.json("package", "install", f"{repo_strip0}:{tag}")

    content_strip1 = _find_content_path(ocx, f"{repo_strip1}:{tag}")
    content_strip0 = _find_content_path(ocx, f"{repo_strip0}:{tag}")

    # strip=1: the leading directory is dropped, bin/hello sits at content root.
    assert (content_strip1 / "bin" / "hello").exists(), (
        f"strip=1 package must have bin/hello at content root: {content_strip1}"
    )
    assert not (content_strip1 / "topdir").exists(), (
        f"strip=1 package must NOT keep the top dir: {content_strip1}"
    )

    # strip=0: verbatim, the top dir is preserved.
    assert (content_strip0 / "topdir" / "bin" / "hello").exists(), (
        f"strip=0 package must keep topdir/bin/hello: {content_strip0}"
    )
    assert not (content_strip0 / "bin").exists(), (
        f"strip=0 package must NOT strip the top dir: {content_strip0}"
    )

    # A2: the shared layer store is verbatim regardless of either package's strip.
    layer_dirs = _layer_content_dirs(ocx)
    assert layer_dirs, "a shared layer content/ directory must exist after install"
    for layer_content in layer_dirs:
        assert (layer_content / "topdir" / "bin" / "hello").exists(), (
            f"layer store must be verbatim (topdir preserved): {layer_content}"
        )
        assert not (layer_content / "bin").exists(), (
            f"layer store must NOT be stripped: {layer_content}"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix shell scripts + shared-layer inode semantics")
def test_strip_applied_at_assemble_local_path(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """A1b + A2 (local path): ``ocx package test`` materializes via pull_local.

    Reuse one bundle (top-level dir) across two ``package test --output`` runs
    with strip=1 and strip=0 metadata. strip is applied at assemble, so the two
    output trees differ; the shared layer store stays verbatim (no double-strip).
    """
    tag = "1.0.0"
    pkg_dir = _build_topdir_pkg_dir(tmp_path, "local-topdir-pkg")

    # One bundle reused for both materializations → one shared layer digest.
    base_meta = tmp_path / "local-base-meta.json"
    base_meta.write_text(json.dumps({"type": "bundle", "version": 1}))
    bundle = tmp_path / "local-topdir-bundle.tar.xz"
    ocx.plain("package", "create", "-m", str(base_meta), "-o", str(bundle), str(pkg_dir))

    # Push once so the identifier resolves cleanly; the local metadata (-m) is
    # what drives assemble strip, not the pushed config.
    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain("package", "push", "-n", "-p", current_platform(), "-m", str(base_meta), "-i", fq, str(bundle))
    ocx.plain("index", "update", f"{unique_repo}:{tag}")

    def _materialize(strip: int, out_name: str) -> Path:
        meta = tmp_path / f"local-meta-strip{strip}.json"
        _strip_metadata(meta, strip)
        out = Path(ocx.ocx_home) / out_name
        out.mkdir(parents=True, exist_ok=True)
        result = ocx.plain(
            "package", "test",
            "-p", current_platform(),
            "-m", str(meta),
            "-o", str(out),
            "-i", f"{unique_repo}:{tag}",
            str(bundle),
            "--",
            "sh", "-c", "true",
            check=False,
        )
        assert result.returncode == 0, (
            f"package test (strip={strip}) must succeed, got {result.returncode}\nstderr: {result.stderr}"
        )
        return out / "content"

    content_strip1 = _materialize(1, "local-out-strip1")
    content_strip0 = _materialize(0, "local-out-strip0")

    # strip=1: top dir stripped at assemble.
    assert (content_strip1 / "bin" / "hello").exists(), (
        f"local strip=1 output must have bin/hello at content root: {content_strip1}"
    )
    assert not (content_strip1 / "topdir").exists(), (
        f"local strip=1 output must NOT keep the top dir: {content_strip1}"
    )

    # strip=0: verbatim, no double-strip.
    assert (content_strip0 / "topdir" / "bin" / "hello").exists(), (
        f"local strip=0 output must keep topdir/bin/hello: {content_strip0}"
    )
    assert not (content_strip0 / "bin").exists(), (
        f"local strip=0 output must NOT strip the top dir: {content_strip0}"
    )

    # A2: shared layer store verbatim on the local path too.
    layer_dirs = _layer_content_dirs(ocx)
    assert layer_dirs, "a shared layer content/ directory must exist after package test"
    for layer_content in layer_dirs:
        assert (layer_content / "topdir" / "bin" / "hello").exists(), (
            f"layer store must be verbatim on the local path (topdir preserved): {layer_content}"
        )
        assert not (layer_content / "bin").exists(), (
            f"layer store must NOT be stripped on the local path: {layer_content}"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix shell scripts")
def test_single_package_strip_components_regression(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """A3 (no-regression · BC3): a single-layer package with strip_components in
    metadata still installs with the leading component stripped from content/.

    Proves strip still works end-to-end after moving from extraction to assemble.
    """
    tag = "1.0.0"
    pkg_dir = _build_topdir_pkg_dir(tmp_path, "regress-topdir-pkg")

    meta = tmp_path / "regress-meta.json"
    _strip_metadata(meta, 1)
    bundle = tmp_path / "regress-bundle.tar.xz"
    ocx.plain("package", "create", "-m", str(meta), "-o", str(bundle), str(pkg_dir))

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain("package", "push", "-n", "-p", current_platform(), "-m", str(meta), "-i", fq, str(bundle))
    ocx.plain("index", "update", f"{unique_repo}:{tag}")

    ocx.json("package", "install", f"{unique_repo}:{tag}")
    content = _find_content_path(ocx, f"{unique_repo}:{tag}")

    assert (content / "bin" / "hello").exists(), (
        f"strip_components=1 must place bin/hello at content root: {content}"
    )
    assert not (content / "topdir").exists(), (
        f"strip_components=1 must strip the leading top dir: {content}"
    )
