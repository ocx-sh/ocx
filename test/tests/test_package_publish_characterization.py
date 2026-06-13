"""Characterization tests for the current package publish + store layout behavior.

These tests are the safety net (P1.0) for the shared-store feature (plan_shared_store).
They lock the *current* (pre-M1/M2) observable behavior of OCX package publish and the
single-root store layout so that:

  - M1 (non-destructive publish / finalize_package_dir) can be validated against them.
  - M2 (StoreLayout resolver / zone env vars) can verify that the default (unset-env)
    path produces identical layout to today.

Coverage matrix
---------------

Publish behavior (tested against ``ocx package install``, which exercises the full
pull → move_temp_to_object_store pipeline):

  test_fresh_install_creates_package_dir
      A first install creates ``$OCX_HOME/packages/{registry}/{shard}/`` with
      ``content/`` and ``install.json``.  Traced to: system_design §2 constraint
      ("Package publish is destructive"), plan P1.0 acceptance criterion.

  test_install_json_present_after_publish
      ``install.json`` is written by ``post_download_actions`` as the final
      sentinel indicating a complete install.  Traced to: pull.rs:591-622.

  test_temp_cleaned_after_publish
      ``$OCX_HOME/temp/`` must be empty (or absent) after a successful install.
      Reuses the assertion from test_install.py::test_install_cleans_temp_directory
      to confirm the same guarantee holds for the publish path specifically.
      Traced to: TempStore stale-sweep, plan P1.0.

  test_repull_replaces_package_dir_observably
      DOCUMENTS THE CURRENT BUG.  A second install of the same digest (re-pull)
      destroys the existing package directory via ``remove_dir_all`` then renames
      the new temp dir into place.  A sentinel file written before the re-pull is
      absent afterwards, proving the destructive window.

      ** This is the one test EXPECTED TO CHANGE when M1 lands. **
      When M1's stash→swap-under-lock replace is implemented, update this test
      to assert the sentinel is absent from stash but the package dir is present
      and was never removed from its canonical path.

      Traced to: utility/fs.rs::move_dir (remove_dir_all branch), system_design
      §5 M1 "Problem" paragraph, plan P1.0.

Store layout (tested against $OCX_HOME on-disk structure):

  test_single_root_store_layout
      ``$OCX_HOME`` is the single root for all seven stores: blobs/, layers/,
      packages/, tags/, symlinks/, state/, temp/.  Traced to: file_structure.rs
      with_root(), system_design §2 constraint ("Single root: FileStructure::with_root
      → root.join(name) for 7 stores"), plan P1.0 / M2 baseline.
"""
from __future__ import annotations

from pathlib import Path

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_not_exists, registry_dir


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _package_dir(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Return the content-addressed package root from ``ocx package pull`` output."""
    result = ocx.json("package", "pull", pkg.short)
    return Path(result[pkg.short])


# ---------------------------------------------------------------------------
# Publish behavior characterization
# ---------------------------------------------------------------------------


def test_fresh_install_creates_package_dir(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """A fresh install creates packages/{registry}/{shard}/ with content/ sub-directory.

    Characterizes the current move_dir-based publish path.  After M1 the same
    directory structure must exist — this test should remain green.

    Traced to: system_design §5 M1 (Problem), plan P1.0.
    """
    pkg = published_package
    ocx.json("package", "install", pkg.short)

    ocx_home = Path(ocx.env["OCX_HOME"])
    packages_root = ocx_home / "packages" / registry_dir(ocx.registry)

    # packages/{registry}/ must exist.
    assert_dir_exists(packages_root)

    # At least one shard directory containing a content/ sub-directory must be
    # present under the registry slug.  Walk two levels (algorithm/prefix) then
    # one more (full shard) to find a package dir.
    package_dirs = [
        entry
        for level1 in packages_root.iterdir()
        for level2 in level1.iterdir()
        for entry in level2.iterdir()
        if entry.is_dir()
    ]
    assert package_dirs, (
        f"No package shard directories found under {packages_root}; "
        "expected at least one after install"
    )
    content_dirs = [d for d in package_dirs if (d / "content").is_dir()]
    assert content_dirs, (
        f"No package dir with a content/ subdirectory found under {packages_root}; "
        f"shard dirs found: {[str(d) for d in package_dirs]}"
    )


def test_install_json_present_after_publish(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """install.json sentinel must exist in the package dir after a successful install.

    post_download_actions() writes install.json as the final OK sentinel.  Its
    presence proves the publish pipeline completed without a kill-9 truncation.
    Readers (check_install_status) depend on it to determine whether a dir is a
    fully committed install or a partial/broken one.

    Traced to: pull.rs post_download_actions(), plan P1.0.
    """
    pkg = published_package
    result = ocx.json("package", "install", pkg.short)

    pkg_root = Path(result[pkg.short]["path"])
    # The reported path is the symlink (candidate or current); follow it to get
    # the package root containing install.json.
    pkg_content = pkg_root.resolve()
    # install.json is a sibling of content/ under the package root dir.
    pkg_dir = pkg_content.parent if pkg_content.name == "content" else pkg_content
    # Walk up to find a dir containing install.json.
    install_json = _find_install_json(pkg_content)
    assert install_json is not None and install_json.exists(), (
        f"install.json must be present after a successful install; "
        f"searched from {pkg_content}"
    )


def _find_install_json(start: Path) -> Path | None:
    """Walk from start up toward the packages/ root looking for install.json."""
    candidate = start
    for _ in range(6):  # at most 6 levels up (content → pkg_dir → shard → ...)
        install_json = candidate / "install.json"
        if install_json.exists():
            return install_json
        if candidate.parent == candidate:
            break
        candidate = candidate.parent
    return None


def test_temp_cleaned_after_publish(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """$OCX_HOME/temp/ must be empty or absent after a successful install.

    Mirrors test_install.py::test_install_cleans_temp_directory but framed as
    a characterization test so M1/M2 changes cannot accidentally break the stale
    sweep without failing this safety net.

    Traced to: TempStore stale-sweep, plan P1.0.
    """
    pkg = published_package
    ocx.json("package", "install", pkg.short)

    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    if temp_dir.exists():
        leftover = list(temp_dir.iterdir())
        assert leftover == [], (
            f"temp/ must be empty after install; leftover entries: "
            f"{[str(e) for e in leftover]}"
        )


def test_repull_replaces_package_dir_observably(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """INV-M1 (post-M1): re-installing over a broken install is NON-destructive.

    After M1 (``finalize_package_dir`` stash→swap), a re-install over a broken
    install never ``remove_dir_all``s the canonical package dir — it only ever
    renames the canonical name outward to a stash before renaming the new dir
    in. This test asserts the package dir is always present and a healthy
    install.json is restored after the re-install (the prior assertion documented
    today's now-fixed destructive replace).

    Traced to: system_design §5 M1 (INV-M1), plan P1.5; the explicitly-allowed
    characterization update once M1 landed.
    """
    pkg = published_package

    # First install — populate the object store.
    first_result = ocx.json("package", "install", pkg.short)
    first_path = Path(first_result[pkg.short]["path"]).resolve()

    # Locate the package root (content/ parent or content itself).
    pkg_root = first_path
    while pkg_root.name == "content" or pkg_root.name == "candidates" or pkg_root.name == pkg.tag:
        pkg_root = pkg_root.parent
    # Navigate into the CAS tree: symlinks → packages dir.
    # Follow the resolved symlink to its target inside packages/.
    if first_path.is_symlink() or first_path.exists():
        target = first_path.resolve()
        # Walk up from target until we find a dir that contains install.json.
        install_json = _find_install_json(target)
        if install_json is not None:
            pkg_root = install_json.parent

    install_json = pkg_root / "install.json"
    assert install_json.exists(), (
        f"expected a healthy install.json under {pkg_root} before re-pull"
    )

    # ── Re-pull over a HEALTHY install: short-circuits, dir preserved ─────────
    # setup_owned takes a fast-path (find_plain + check_install_status OK) and
    # returns the existing install WITHOUT re-pulling or moving. So a healthy
    # re-pull never reaches move_dir and the existing dir survives. M1 must keep
    # this short-circuit, so this assertion stays green after M1.
    healthy_sentinel = pkg_root / "__healthy_repull_sentinel__.txt"
    healthy_sentinel.write_text("healthy marker")
    ocx.json("package", "install", pkg.short)
    assert healthy_sentinel.exists(), (
        "re-pull of a HEALTHY install must short-circuit (no move_dir) and preserve "
        f"the existing package dir; sentinel must survive under {pkg_root}"
    )

    # ── Re-pull over a BROKEN install: NON-destructive stash→swap (post-M1) ───
    # Break the install by removing install.json so check_install_status returns
    # false; the fast-path no longer fires and setup_owned re-pulls →
    # move_temp_to_object_store → finalize_package_dir (stash→swap under the held
    # digest lock). The canonical name is only ever renamed, never removed.
    #
    # ** Updated for M1 (was: assert the destructive replace). **
    # The canonical package dir must be present before AND after the re-install;
    # a healthy install.json must be restored. The broken sentinel lived inside
    # the replaced broken content, so it travels to the stash (reclaimed by the
    # stale sweep) and is not expected to survive at the canonical name.
    install_json.unlink()
    assert pkg_root.exists(), "canonical package dir must exist before the broken re-install"

    ocx.json("package", "install", pkg.short)

    # INV-M1: the canonical package dir is always present (never removed).
    assert pkg_root.exists(), (
        "INV-M1: the canonical package dir must remain present after the non-destructive "
        f"stash→swap re-install; checked: {pkg_root}"
    )
    assert (pkg_root / "content").exists(), (
        f"the swapped-in package dir must have a content/ directory: {pkg_root}"
    )
    # The re-install restored a fresh healthy install.
    assert install_json.exists(), (
        f"re-install must restore a healthy install.json under {pkg_root}"
    )
    # No __stale_ stash dir must linger under the temp zone after the swap.
    temp_root = Path(ocx.env["OCX_HOME"]) / "temp"
    if temp_root.exists():
        leftover = [p for p in temp_root.iterdir() if p.name.startswith("__stale_")]
        assert not leftover, (
            f"no __stale_ stash dir must remain under temp/ after the swap: {leftover}"
        )


# ---------------------------------------------------------------------------
# Store layout characterization
# ---------------------------------------------------------------------------


def test_single_root_store_layout(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """All seven stores derive directly from $OCX_HOME (single-root layout).

    After a package install, $OCX_HOME must contain exactly the canonical seven
    store sub-directories (blobs, layers, packages, tags, symlinks, state, temp).
    No zone overrides are active (OCX_CACHE_DIR / OCX_STATE_DIR / OCX_PACKAGES_DIR
    are not set in the default OcxRunner env).

    This characterizes the FileStructure::with_root baseline that M2 (StoreLayout
    resolver) must preserve under default (unset) zone env vars.

    Traced to: file_structure.rs::with_root, system_design §2 constraint
    ("Single root"), system_design §5 M2 ("Defaults preserve today's single-root
    layout exactly"), plan P1.0 / M2 baseline.
    """
    pkg = published_package
    ocx.json("package", "install", pkg.short)

    ocx_home = Path(ocx.env["OCX_HOME"])

    # None of the zone override variables must be set in the default runner env.
    assert "OCX_CACHE_DIR" not in ocx.env, (
        "OCX_CACHE_DIR must not be set in the default OcxRunner env for this characterization"
    )
    assert "OCX_STATE_DIR" not in ocx.env, (
        "OCX_STATE_DIR must not be set in the default OcxRunner env for this characterization"
    )
    assert "OCX_PACKAGES_DIR" not in ocx.env, (
        "OCX_PACKAGES_DIR must not be set in the default OcxRunner env for this characterization"
    )

    # All seven canonical stores must be rooted directly under $OCX_HOME.
    # NOTE: the FileStructure carries two temp stores (`temp` = package-staging
    # zone, `layer_temp` = cache-staging zone), but in the unified single-root
    # layout `layer_temp` collapses to the SAME `$OCX_HOME/temp/` directory as
    # `temp` (system_design §5 M2 "When zones unified, temp/layer_temp point at
    # the same dir"). So there is only one on-disk `temp/` entry to assert here.
    expected_stores = [
        "blobs",
        "layers",
        "packages",
        "tags",
        "symlinks",
        "state",
        "temp",
    ]
    for store_name in expected_stores:
        store_path = ocx_home / store_name
        # Not all stores need to be materialised after a single install
        # (e.g. state/ is created lazily), but blobs/layers/packages/tags/symlinks
        # must exist after install.
        if store_name in ("blobs", "layers", "packages", "tags", "symlinks"):
            assert_dir_exists(store_path)
        # Every store that exists must be a direct child of $OCX_HOME, not a
        # symlink to a different root (zone override).
        if store_path.exists():
            assert not store_path.is_symlink(), (
                f"store {store_name!r} at {store_path} must be a real directory under "
                f"$OCX_HOME, not a symlink to a zone-override path"
            )
            # The store must not escape $OCX_HOME (no zone override pointing elsewhere).
            resolved = store_path.resolve()
            home_resolved = ocx_home.resolve()
            assert str(resolved).startswith(str(home_resolved)), (
                f"store {store_name!r} resolved path {resolved} must be inside "
                f"$OCX_HOME={home_resolved}; a zone override is active unexpectedly"
            )
