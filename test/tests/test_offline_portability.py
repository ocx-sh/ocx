"""Store-portability acceptance suite: copying ONLY ``blobs/`` + ``layers/`` +
``index/`` out of a warmed ``OCX_HOME`` into a brand-new, empty home suffices
to run installs, ``package exec``, and project toolchain execution fully
offline (``OCX_OFFLINE=1``).

This complements ``test_offline.py`` (offline resolution semantics with
transitive deps) and ``test_pinned_offline.py`` (pinned-digest offline exec)
by proving the *storage* contract instead: ``packages/``, ``symlinks/``,
``temp/``, ``projects/``, ``state/`` are all either GC-derived assembly
caches or install-time bookkeeping — a fresh home reconstructs them locally
from the three portable stores without any network access. The mechanism is
the layer-cache fast path (``pull.rs::extract_layer_atomic``): a package
whose layers are already on disk re-assembles into ``packages/`` even when
the manager holds no OCI client at all.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

from src import OcxRunner, assert_symlink_exists, make_package, registry_dir

# ---------------------------------------------------------------------------
# Store-copy helpers
# ---------------------------------------------------------------------------

_PORTABLE_DIRS = ("blobs", "layers", "index")


def _copy_store(warm_home: Path, fresh_home: Path, *, exclude: str | None = None) -> None:
    """Copy the portable store subdirectories from a warm home into a fresh one.

    Copies ``blobs/``, ``layers/``, ``index/`` (minus ``exclude`` when given).
    Deliberately never copies ``packages/``, ``symlinks/``, ``temp/``,
    ``projects/``, ``state/`` — those are exactly what the tests below prove
    unnecessary to carry across. ``symlinks=True`` mirrors the relocation
    pattern used by ``test_patches.py``'s warm-home copies: preserve internal
    symlinks verbatim rather than dereferencing them.
    """
    fresh_home.mkdir(parents=True, exist_ok=True)
    for name in _PORTABLE_DIRS:
        if name == exclude:
            continue
        src = warm_home / name
        if src.is_dir():
            shutil.copytree(src, fresh_home / name, symlinks=True)


def _fresh_runner(ocx: OcxRunner, home: Path) -> OcxRunner:
    """A second ``OcxRunner`` sharing the binary and registry but pointed at
    ``home`` — the "brand-new empty home" side of the portability contract.
    """
    return OcxRunner(ocx.binary, home, ocx.registry)


def _candidate_current_path(home: Path, registry: str, repo: str) -> Path:
    return home / "symlinks" / registry_dir(registry) / repo / "current"


# ---------------------------------------------------------------------------
# (a) `ocx package install` succeeds fully offline against a copied store
# ---------------------------------------------------------------------------


def test_offline_install_succeeds_from_copied_store(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Warm home installs a package; a fresh home built from only its
    ``blobs/`` + ``layers/`` + ``index/`` installs the same package fully
    offline (the package re-assembles locally — no candidate symlink was
    copied, so this also proves install is not relying on stale symlinks).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)

    fresh_home = tmp_path / "fresh_home"
    _copy_store(ocx.ocx_home, fresh_home)
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.run(
        "package", "install", "--select", pkg.short,
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 0, (
        f"offline install against a blobs+layers+index-only copy must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert_symlink_exists(
        _candidate_current_path(fresh_home, ocx.registry, unique_repo),
        "offline install must create the current symlink in the fresh home",
    )


# ---------------------------------------------------------------------------
# (b) `ocx package exec` runs the binary fully offline, no prior install
# ---------------------------------------------------------------------------


def test_offline_package_exec_runs_binary_from_copied_store(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx package exec`` re-assembles on demand and runs the binary fully
    offline against a copied store — no install/symlink step in the fresh
    home first; ``exec`` auto-installs on miss.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)  # warm blobs+layers in home A

    fresh_home = tmp_path / "fresh_home"
    _copy_store(ocx.ocx_home, fresh_home)
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.plain(
        "package", "exec", pkg.short, "--", "hello",
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 0, (
        f"offline package exec against a blobs+layers+index-only copy must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"expected marker {pkg.marker!r} in offline exec output; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# (c) Project toolchain (`ocx run --`) works fully offline
# ---------------------------------------------------------------------------


def test_offline_project_toolchain_run_succeeds_from_copied_store(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project's ``ocx run -- <bin>`` executes fully offline against a home
    whose store was reconstructed from ``blobs/`` + ``layers/`` + ``index/``
    only. ``ocx.lock`` resolution is index-free by design (locks store the
    platform-leaf digest directly), so this also proves the lock-driven path
    shares the same layer-cache re-assembly as tag-driven install/exec.
    """
    short_id = uuid4().hex[:8]
    repo = f"t_{short_id}_offline_toolchain"
    tag = "1.0.0"
    bin_name = "hello"
    pkg = make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, bins=[bin_name])

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=ocx.env
    )
    assert lock.returncode == 0, f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    pull = subprocess.run(
        [str(ocx.binary), "pull"], cwd=project, capture_output=True, text=True, env=ocx.env
    )
    assert pull.returncode == 0, f"ocx pull failed: rc={pull.returncode}\nstderr:\n{pull.stderr}"

    fresh_home = tmp_path / "fresh_home"
    _copy_store(ocx.ocx_home, fresh_home)
    fresh_env = {**ocx.env, "OCX_HOME": str(fresh_home), "OCX_OFFLINE": "1"}

    result = subprocess.run(
        [str(ocx.binary), "run", "--", bin_name],
        cwd=project,
        capture_output=True,
        text=True,
        env=fresh_env,
    )
    assert result.returncode == 0, (
        f"offline `ocx run` against a blobs+layers+index-only copy must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"expected marker {pkg.marker!r} in offline run output; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# (d) Negative: a home missing one of the three stores fails cleanly offline
# ---------------------------------------------------------------------------


def test_offline_install_missing_index_exits_policy_blocked(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A home carrying ``blobs/`` + ``layers/`` but no ``index/`` cannot
    resolve the unpinned tag offline — exits ``PolicyBlocked`` (81), the same
    documented code as an entirely un-indexed package (see
    ``test_offline.py::test_exit_code_on_offline_blocks_fetch``).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)

    fresh_home = tmp_path / "fresh_home_no_index"
    _copy_store(ocx.ocx_home, fresh_home, exclude="index")
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.run(
        "package", "install", "--select", pkg.short,
        check=False,
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 81, (
        f"offline install with no local index/ must exit PolicyBlocked (81); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # Pins `oci::index::error::Error::PolicyResolutionBlocked`
    # (crates/ocx_lib/src/oci/index/error.rs): "{policy} mode refused to
    # resolve unpinned reference '{identifier}'; ...". "unpinned reference" is
    # unique to this variant's message.
    assert "unpinned reference" in result.stderr.lower(), (
        f"stderr must describe the unresolved-tag policy block; got:\n{result.stderr}"
    )


def test_offline_install_missing_blobs_exits_policy_blocked(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A home carrying ``layers/`` + ``index/`` but no ``blobs/`` resolves the
    tag locally (the index is present) but has no cached manifest content —
    exits ``PolicyBlocked`` (81) with a distinct "not in the local cache"
    message, never falling back to the network under ``--offline``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)

    fresh_home = tmp_path / "fresh_home_no_blobs"
    _copy_store(ocx.ocx_home, fresh_home, exclude="blobs")
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.run(
        "package", "install", "--select", pkg.short,
        check=False,
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 81, (
        f"offline install with no local blobs/ must exit PolicyBlocked (81); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # Pins `PackageErrorKind::OfflineManifestMissing`
    # (crates/ocx_lib/src/package_manager/error.rs): "manifest {digest} is not
    # in the local cache; run `ocx install {identifier}` online to populate
    # it". "populate" is unique to this variant's message and distinct from
    # the missing-index PolicyResolutionBlocked message above.
    assert "populate" in result.stderr.lower(), (
        f"stderr must mention the missing local cache — distinct from the "
        f"missing-index unpinned-reference message; got:\n{result.stderr}"
    )
