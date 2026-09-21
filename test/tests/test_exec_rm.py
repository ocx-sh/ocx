"""``ocx package exec --rm`` — remove the package once the command finishes.

Four properties, one per test: the package a run leaves behind is gone, the
child's exit status survives the removal, a package something else holds is
kept with its install symlink intact, and a package held only by a project's
``ocx.lock`` is kept too.
"""

from pathlib import Path

from src import OcxRunner, PackageInfo, assert_not_exists


def _store_path(ocx: OcxRunner, short: str) -> Path:
    """The package-store directory ``ocx package which`` resolves ``short`` to."""
    return Path(ocx.json("package", "which", short)[short]["path"])


def test_exec_rm_removes_the_package_it_leaves_behind(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package pull <pkg>; ocx package exec --rm <pkg> -- hello; the store path is gone"""
    pkg = published_package
    ocx.plain("package", "pull", pkg.short)
    store_path = _store_path(ocx, pkg.short)
    assert store_path.is_dir(), (
        f"precondition: the pull must materialize {store_path}, or the assertion "
        f"below passes for a package that was never there"
    )

    result = ocx.plain("package", "exec", "--rm", pkg.short, "--", "hello")
    assert result.stdout.strip() == pkg.marker, (
        f"the command still runs under --rm; stderr: {result.stderr}"
    )

    assert_not_exists(
        store_path,
        "--rm removes a package nothing else holds once the command finishes",
    )


def test_exec_rm_preserves_the_child_exit_code(
    ocx: OcxRunner, published_package: PackageInfo
):
    """A child exiting 3 under --rm still makes ocx package exec exit 3."""
    result = ocx.run(
        "package",
        "exec",
        "--rm",
        published_package.short,
        "--",
        "sh",
        "-c",
        "exit 3",
        format=None,
        check=False,
    )
    assert result.returncode == 3, (
        f"--rm spawns and waits instead of replacing the process, and must still "
        f"forward the child status verbatim; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


def test_exec_rm_keeps_an_installed_package(
    ocx: OcxRunner, published_package: PackageInfo
):
    """A package that is also installed survives --rm, candidate symlink intact."""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)
    store_path = _store_path(ocx, pkg.short)
    candidate = Path(
        ocx.json("package", "which", "--candidate", pkg.short)[pkg.short]["path"]
    )

    ocx.plain("package", "exec", "--rm", pkg.short, "--", "hello")

    assert store_path.is_dir(), (
        f"the install symlink holds this package; --rm must not remove {store_path}"
    )
    assert candidate.resolve() == store_path.resolve(), (
        f"the candidate symlink must still resolve to the package root, not dangle; "
        f"{candidate} -> {candidate.resolve()}"
    )
    result = ocx.plain("package", "exec", pkg.short, "--", "hello")
    assert result.stdout.strip() == pkg.marker, (
        f"the retained package must still be runnable; stderr: {result.stderr}"
    )


def test_exec_rm_keeps_a_package_a_project_lock_pins(ocx: OcxRunner, tmp_path: Path):
    """A project's ``ocx.lock`` holds its packages, so ``--rm`` keeps them.

    The retention row above holds an install symlink; this one holds nothing of
    the kind — the package is reachable only through the project ledger entry
    ``ocx lock``/``ocx pull`` wrote. ``--rm`` decides by reachability, never by
    "did this run pull it", and collecting a lock-pinned package here would
    break every later ``ocx exec`` in that project. The child exits 3, so the
    row also says the retention branch forwards the status like the removal
    branch does.
    """
    # Imported in the body, not at module scope: this is the one row that
    # needs a project, and `test_diff_guard.py` permits an added line inside a
    # wholly new test but not a new module-level import.
    from src.toolchain_fixtures import locked_project, run_in

    project = locked_project(ocx, tmp_path, label="rm")
    pkg = project.default_package
    store_path = _store_path(ocx, pkg.short)
    assert store_path.is_dir(), (
        f"precondition: the project pull must materialize {store_path}, or the "
        f"assertion below passes for a package that was never there"
    )

    result = run_in(
        ocx,
        project.directory,
        "package",
        "exec",
        "--rm",
        pkg.short,
        "--",
        "sh",
        "-c",
        "exit 3",
    )

    assert result.returncode == 3, (
        f"--rm must forward the child status verbatim on the retention branch too; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert store_path.is_dir(), (
        f"the project lock pins this package, so --rm must not remove {store_path}"
    )
