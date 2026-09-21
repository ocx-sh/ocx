"""``ocx package exec --rm`` — remove the package once the command finishes.

Three properties, one per test: the package a run leaves behind is gone, the
child's exit status survives the removal, and a package something else holds
is kept with its install symlink intact.
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
