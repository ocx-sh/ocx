from pathlib import Path
from uuid import uuid4

from src import OcxRunner, PackageInfo, assert_not_exists, assert_symlink_exists, registry_dir
from src.helpers import make_package


def test_select_switches_current_symlink(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """ocx install -s <v1>; ocx install <v2>; ocx select <v2>"""
    v1, v2 = published_two_versions
    ocx.json("package", "install", "-s", v1.short)
    ocx.json("package", "install", v2.short)

    install_v2 = ocx.json("package", "install", v2.short)
    content_v2 = Path(install_v2[v2.short]["path"])

    ocx.plain("package", "select", v2.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / v1.repo
        / "current"
    )
    assert current.resolve() == content_v2.resolve()


def test_deselect_removes_current_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>; ocx deselect <pkg>"""
    pkg = published_package
    ocx.json("package", "install", "-s", pkg.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert_symlink_exists(current)

    ocx.plain("package", "deselect", pkg.short)
    assert_not_exists(current)


def test_reselect_after_deselect(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>; ocx deselect <pkg>; ocx select <pkg>"""
    pkg = published_package
    ocx.json("package", "install", "-s", pkg.short)
    ocx.plain("package", "deselect", pkg.short)
    ocx.plain("package", "select", pkg.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert_symlink_exists(current)


def test_select_multiple_packages_switches_both_current_symlinks(
    ocx: OcxRunner, tmp_path: Path
):
    """``ocx package select A B`` sets both packages' ``current`` symlinks."""
    repo_a = f"t_{uuid4().hex[:8]}_select_multi_a"
    repo_b = f"t_{uuid4().hex[:8]}_select_multi_b"
    a = make_package(ocx, repo_a, "1.0.0", tmp_path)
    b = make_package(ocx, repo_b, "1.0.0", tmp_path)
    install_a = ocx.json("package", "install", a.short)
    install_b = ocx.json("package", "install", b.short)

    ocx.plain("package", "select", a.short, b.short)

    reg = registry_dir(ocx.registry)
    current_a = Path(ocx.env["OCX_HOME"]) / "symlinks" / reg / repo_a / "current"
    current_b = Path(ocx.env["OCX_HOME"]) / "symlinks" / reg / repo_b / "current"
    assert current_a.resolve() == Path(install_a[a.short]["path"]).resolve()
    assert current_b.resolve() == Path(install_b[b.short]["path"]).resolve()


def test_select_partial_failure_exits_nonzero(
    ocx: OcxRunner, tmp_path: Path
):
    """One unresolvable package among several fails the whole batch (nonzero exit).

    ``select_all`` resolves every package before wiring any symlink
    (aggregating failures instead of aborting on the first) — a batch
    containing an unresolvable package must fail, and the failure must
    surface the offending identifier.
    """
    repo_a = f"t_{uuid4().hex[:8]}_select_partial_a"
    repo_b = f"t_{uuid4().hex[:8]}_select_partial_b"
    a = make_package(ocx, repo_a, "1.0.0", tmp_path)
    ocx.json("package", "install", a.short)
    missing = f"{repo_b}:9.9.9"

    result = ocx.run("package", "select", a.short, missing, format=None, check=False)

    assert result.returncode != 0, f"expected nonzero exit, stderr: {result.stderr}"


def test_select_multiple_wire_failures_aggregate_all(ocx: OcxRunner, tmp_path: Path):
    """Wire-up failures for MULTIPLE resolved packages aggregate — both surface.

    ``test_select_partial_failure_exits_nonzero`` fails inside ``find_all``
    (unresolvable package) *before* the wire-up loop, so it does not reach the
    C1 aggregation path. This exercises that path directly: both packages
    resolve, but the ``current`` symlink write fails for each (a populated
    directory at the target blocks the symlink), so ``select_all`` must report
    every offender rather than abort on the first.
    """
    repo_a = f"t_{uuid4().hex[:8]}_select_wirefail_a"
    repo_b = f"t_{uuid4().hex[:8]}_select_wirefail_b"
    a = make_package(ocx, repo_a, "1.0.0", tmp_path)
    b = make_package(ocx, repo_b, "1.0.0", tmp_path)
    ocx.json("package", "install", a.short)
    ocx.json("package", "install", b.short)

    # Block the `current` symlink write for BOTH repos: a non-empty directory
    # at the target path cannot be replaced by a symlink, so wire_selection
    # fails with an Internal error for each.
    reg = registry_dir(ocx.registry)
    for repo in (repo_a, repo_b):
        current = Path(ocx.env["OCX_HOME"]) / "symlinks" / reg / repo / "current"
        current.mkdir(parents=True, exist_ok=True)
        (current / "blocker").write_text("x")

    result = ocx.run("package", "select", a.short, b.short, format=None, check=False)

    assert result.returncode != 0, f"expected nonzero exit, stderr: {result.stderr}"
    # Aggregation: the fix must report BOTH offenders, not abort on repo_a alone.
    assert repo_a in result.stderr, f"repo_a missing from aggregated error: {result.stderr}"
    assert repo_b in result.stderr, f"repo_b missing from aggregated error: {result.stderr}"
    assert repo_b in result.stderr
