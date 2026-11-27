import re
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_not_exists
from src.helpers import make_package, make_package_with_entrypoints, write_ocx_toml
from src.toolchain_fixtures import (
    EXIT_SUCCESS,
    SHELLS_DIR,
    ToolchainProject,
    active_shell,
    generated_shells,
    is_link,
    locked_project,
    make_link,
    read_link,
    read_render_stamp,
    run_in,
    snapshot_tree,
)

pytestmark = pytest.mark.command("clean")


@pytest.mark.smoke
def test_clean_removes_unreferenced_objects(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx uninstall <pkg>; ocx clean"""
    pkg = published_package
    result = ocx.json("package", "install", pkg.short)["packages"]
    candidate = Path(result[pkg.short]["path"])
    content = candidate.resolve()
    assert_dir_exists(content)

    ocx.plain("package", "uninstall", pkg.short)
    assert_dir_exists(content)

    ocx.plain("clean")
    assert_not_exists(content)


def test_clean_removes_stale_temp_directories(ocx: OcxRunner):
    """ocx clean should remove stale temp dir + sibling .lock file."""
    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    stale = temp_dir / "stale_abcdef1234567890abcdef1234567890"
    lock_file = stale.with_suffix(".lock")
    stale.mkdir(parents=True)
    lock_file.touch()
    (stale / "leftover.tar.gz").write_bytes(b"stale data")

    ocx.plain("clean")
    assert_not_exists(stale)
    assert_not_exists(lock_file)


def test_clean_removes_orphan_lock_file(ocx: OcxRunner):
    """ocx clean should remove a .lock file with no corresponding directory."""
    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    temp_dir.mkdir(parents=True, exist_ok=True)
    lock_file = temp_dir / "orphan_abcdef1234567890abcdef12345678.lock"
    lock_file.touch()

    ocx.plain("clean")
    assert_not_exists(lock_file)


def test_clean_removes_orphan_temp_directory(ocx: OcxRunner):
    """ocx clean should remove a temp directory with no .lock file."""
    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    orphan = temp_dir / "orphan_abcdef1234567890abcdef12345678"
    orphan.mkdir(parents=True)
    (orphan / "leftover.tar.gz").write_bytes(b"stale data")

    ocx.plain("clean")
    assert_not_exists(orphan)


def test_clean_preserves_referenced_objects(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx clean"""
    pkg = published_package
    result = ocx.json("package", "install", pkg.short)["packages"]
    content = Path(result[pkg.short]["path"]).resolve().parent

    ocx.plain("clean")
    assert_dir_exists(content)


def test_clean_preserves_config_blob_of_installed_package(
    ocx: OcxRunner, ocx_home: Path, published_package: PackageInfo
):
    """Regression: ocx clean must not delete an installed package's metadata
    config blob.

    Before the architectural fix, ResolvedChain.chain listed only manifest
    blobs (image-index + image-manifest), so ReferenceManager::link_blobs did
    not create a refs/blobs/{config_digest} symlink. The garbage collector
    treated the config blob as orphan and deleted it on the next `ocx clean`,
    which broke `ocx --offline install` rehydration from the local CAS.
    """
    import json

    from src.assertions import assert_path_exists

    pkg = published_package

    # Install the package. The JSON output names the install symlink ocx made
    # for *this* package, which is the identity handle for its manifest.
    #
    # Selecting instead by `next(packages_root.rglob("manifest.json"))` picks
    # the first manifest in directory order. That is one manifest today and an
    # arbitrary one the moment the fixture gains a dependency — and since a
    # dependency's config blob is linked too, the wrong pick would still find a
    # blob on disk and pass, silently asserting about a different package.
    installs = ocx.json("package", "install", "--select", pkg.short)["packages"]
    assert len(installs) == 1, f"one identifier in, one entry out: {installs}"

    # `path` is the candidate symlink, and both `current` and `candidates/{tag}`
    # target the package root itself — not `content/` (see SymlinkStore's doc
    # comment). manifest.json sits directly in that root.
    package_root = Path(next(iter(installs.values()))["path"]).resolve()
    manifest = json.loads((package_root / "manifest.json").read_text())
    config_digest = manifest["config"]["digest"]  # e.g. "sha256:abcd..."

    algo, hex_digest = config_digest.split(":", 1)
    # Same sharding as BlobStore::path: {algorithm}/{hex[0..2]}/{hex[2..32]}/data
    # Layout: {ocx_home}/packages/{registry_slug}/{algo}/{2hex}/{30hex}/
    packages_root = ocx_home / "packages"
    registry_slug = package_root.relative_to(packages_root).parts[0]
    blob_data = (
        ocx_home
        / "blobs"
        / registry_slug
        / algo
        / hex_digest[:2]
        / hex_digest[2:32]
        / "data"
    )
    # Sanity: the blob is on disk after install.
    assert blob_data.exists(), f"expected config blob on disk after install at {blob_data}"

    # Run clean — should be a no-op for reachable objects.
    ocx.plain("clean")

    # Regression assertion.
    assert_path_exists(
        blob_data,
        (
            f"ocx clean deleted the config blob at {blob_data} while the package "
            "is installed; refs/blobs/ edge is missing for manifest.config.digest"
        ),
    )


def test_clean_preserves_committed_index_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """B1 (`adr_index_indirection.md` #5): `ocx clean` never inspects or
    collects any file under the index home.

    The `IndexStore` is deliberately outside the GC reachability graph
    (Decision B1) — it is git/user-managed reproducibility data with its own
    lifecycle, not a `CasTier` the sweep walks. Populate a committed index
    home via `--index` (simulating a project's committed `.ocx/index/`), run
    `ocx clean` against the ordinary `$OCX_HOME` (no `--index`), and assert
    every file under the index home survives bit-identical — a machine-local
    GC must never be able to eat committed, git-tracked snapshot data.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)

    index_home = tmp_path / "committed_index"
    index_home.mkdir()
    ocx.plain("--index", str(index_home), "index", "update", pkg.short)

    before = sorted(
        (str(path.relative_to(index_home)), path.read_bytes())
        for path in index_home.rglob("*")
        if path.is_file()
    )
    assert before, "precondition: index update must populate the index home"

    ocx.plain("clean")

    after = sorted(
        (str(path.relative_to(index_home)), path.read_bytes())
        for path in index_home.rglob("*")
        if path.is_file()
    )
    assert before == after, (
        "ocx clean must not touch any file under the committed index home. "
        f"Before: {[name for name, _ in before]}, after: {[name for name, _ in after]}"
    )


# ---------------------------------------------------------------------------
# Launcher generations (adr_unified_launcher.md section 12)
# ---------------------------------------------------------------------------

_GENERATION = re.compile(r"^[0-9a-f]{16}$")


def _hex16() -> str:
    """A 16-hex name no real generation or render collides with."""
    return uuid4().hex[:16]


def _launchers_entries(report: dict) -> list[dict]:
    """The report's entries of kind ``launchers``."""
    return [item for item in report["items"] if item["kind"] == "launchers"]


def _reported(report: dict) -> set[Path]:
    """The resolved paths of the ``launchers`` entries. Resolved, because the
    report names the path as ocx joined it and the test planted it through the
    home the runner configured; both can differ by a link."""
    return {Path(item["path"]).resolve() for item in _launchers_entries(report)}


def _plant_launcher_dir(directory: Path, name: str = "tool") -> Path:
    """A complete-looking launcher directory: ``NAME`` and ``NAME.launch``."""
    directory.mkdir(parents=True)
    (directory / name).write_bytes(b"\x7fELF-not-run")
    (directory / f"{name}.launch").write_text("{}", encoding="utf-8")
    return directory


class _Generations:
    """An installed package with a planted superseded generation, and a planted
    ``deferred/<G'>`` subtree."""

    def __init__(self, ocx: OcxRunner, tmp_path: Path, unique_repo: str) -> None:
        pkg = make_package_with_entrypoints(
            ocx, unique_repo, tmp_path, ["tool"], bins=["tool"]
        )
        installed = ocx.json("package", "install", pkg.short)["packages"]
        # `path` is the candidate link, which targets the package root; first
        # use writes `launchers/<G>/` there.
        self.package_root = Path(installed[pkg.short]["path"]).resolve()
        launchers = self.package_root / "launchers"
        assert launchers.is_dir(), (
            f"C-137 precondition: first use writes `launchers/<G>/` under {self.package_root}"
        )
        current = [p for p in launchers.iterdir() if _GENERATION.match(p.name)]
        assert len(current) == 1, f"one current generation expected; found {current}"
        self.current = current[0]
        self.stale = _plant_launcher_dir(launchers / _hex16())
        home = Path(ocx.env["OCX_HOME"])
        self.deferred = home / "deferred" / _hex16()
        _plant_launcher_dir(
            self.deferred / "ocx.sh" / "repo" / "sha256" / "ab" / ("c" * 30) / "l"
        )


def test_clean_retires_a_superseded_package_generation_as_launchers(
    ocx: OcxRunner, tmp_path: Path, unique_repo: str
) -> None:
    """`launchers/<G'>` is retired and reported as
    `launchers`; the current generation stays.

    RED: a clean that walks no `launchers/` leaves `G'` and reports nothing.
    """
    generations = _Generations(ocx, tmp_path, unique_repo)

    report = ocx.json("clean")

    assert generations.stale.resolve() in _reported(report), (
        f"V-L41 -- the report lists {generations.stale} as `launchers`; got {report['items']}"
    )
    assert_not_exists(generations.stale)
    assert_dir_exists(generations.current)
    assert generations.current.resolve() not in _reported(report), (
        "V-L41 -- the generation this ocx writes is never retired"
    )


def test_clean_retires_a_superseded_deferred_generation_as_launchers(
    ocx: OcxRunner, tmp_path: Path, unique_repo: str
) -> None:
    """the whole `deferred/<G'>` subtree is retired as `launchers`.

    RED: a clean that never walks `deferred/` leaves the subtree.
    """
    generations = _Generations(ocx, tmp_path, unique_repo)

    report = ocx.json("clean")

    assert generations.deferred.resolve() in _reported(report), (
        f"V-L41 -- the report lists {generations.deferred} as `launchers`; got {report['items']}"
    )
    assert_not_exists(generations.deferred)


def test_clean_dry_run_reports_launcher_generations_and_removes_nothing(
    ocx: OcxRunner, tmp_path: Path, unique_repo: str
) -> None:
    """`--dry-run` reports the same `launchers` entries a real
    clean would, and every directory is still there afterwards.

    RED: no `launchers` entry is reported, or the directories are gone.
    """
    generations = _Generations(ocx, tmp_path, unique_repo)

    dry = ocx.json("clean", "--dry-run")

    planted = {generations.stale.resolve(), generations.deferred.resolve()}
    assert planted <= _reported(dry), (
        f"V-L41 -- the dry run names both superseded directories; got {dry['items']}"
    )
    assert all(item["dry_run"] for item in _launchers_entries(dry)), (
        "V-L41 -- every dry-run entry says so"
    )
    assert_dir_exists(generations.stale)
    assert_dir_exists(generations.deferred)
    assert_dir_exists(generations.current)

    real = ocx.json("clean")
    assert _reported(real) == _reported(dry), (
        "S-105 -- the dry run reported exactly what the real clean retired"
    )


# -- toolchain homes ---------------------------------------------------------


def _render_twice(ocx: OcxRunner, tmp_path: Path) -> tuple[ToolchainProject, str, str]:
    """A project rendered, then rendered again with one more tool, so the v2 stamp
    names ``shell_dir`` and a different ``previous_shell_dir``. Returns the
    project and the two directory names (previous, current)."""
    project = locked_project(ocx, tmp_path)
    first = active_shell(project.home)
    label = _hex16()[:8]
    extra = make_package(
        ocx, f"t_{label}_extra", "1.0.0", tmp_path, cascade=False, bins=[f"xbin{label}"]
    )
    body = project.config_body().replace(
        "[tools]\n", f'[tools]\nxkey{label} = "{extra.fq}"\n', 1
    )
    write_ocx_toml(project.directory, body)
    assert run_in(ocx, project.directory, "lock").returncode == EXIT_SUCCESS
    pulled = run_in(ocx, project.directory, "pull", "-g", "all")
    assert pulled.returncode == EXIT_SUCCESS, pulled.stderr
    second = active_shell(project.home)
    assert first and second and first != second, (
        "precondition: new content published a new directory"
    )
    return project, first, second


def test_clean_retires_superseded_toolchain_directories_and_keeps_the_stamped_two(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """an older `default-<hex>`, a `.staging-<hex>` and a `.gc-<hex>`
    are retired as `launchers`; the stamp's `shell_dir` and `previous_shell_dir`
    stay, and so does `shells/default`.

    RED: a clean that walks no `shells/` retires none of the three.
    """
    project, previous, current = _render_twice(ocx, tmp_path)
    stamp = read_render_stamp(ocx, project.home)
    assert stamp is not None
    assert (stamp["shell_dir"], stamp["previous_shell_dir"]) == (current, previous)

    shells = project.home / SHELLS_DIR
    older = _plant_launcher_dir(shells / f"default-{_hex16()}" / "bin").parent
    staging = _plant_launcher_dir(shells / f".staging-{_hex16()}" / "bin").parent
    gc = _plant_launcher_dir(shells / f".gc-{_hex16()}" / "bin").parent
    default = _plant_launcher_dir(shells / "default" / "bin").parent
    default_tree = snapshot_tree(default)
    kept_tree = {name: snapshot_tree(shells / name) for name in (previous, current)}

    report = ocx.json("clean")

    reported = _reported(report)
    for planted in (older, staging, gc):
        assert planted.resolve() in reported, (
            f"C-137 -- {planted.name} is reported as `launchers`; got {report['items']}"
        )
        assert_not_exists(planted)
    assert not reported & {
        (shells / previous).resolve(),
        (shells / current).resolve(),
    }, "V-L30 -- the stamped two are never reported"
    for name, tree in kept_tree.items():
        assert snapshot_tree(shells / name) == tree, f"V-L30 -- {name} stays whole"
    assert active_shell(project.home) == current
    assert snapshot_tree(default) == default_tree, (
        "C-132 -- `shells/default` is never touched"
    )


def test_clean_never_retires_a_symlink_candidate_in_shells(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """a link named like a generated directory is not a candidate.

    RED: a clean that removes by name alone deletes the link, or follows it and
    deletes the directory it names.
    """
    project, previous, current = _render_twice(ocx, tmp_path)
    shells = project.home / SHELLS_DIR
    link = shells / f"default-{_hex16()}"
    make_link(shells / previous, link)
    previous_tree = snapshot_tree(shells / previous)

    ocx.json("clean")

    assert is_link(link), "V-L30 -- the symlink child is left in place"
    assert read_link(link).endswith(previous), "V-L30 -- and still names its target"
    assert snapshot_tree(shells / previous) == previous_tree
    assert sorted(generated_shells(project.home)) == sorted(
        [previous, current, link.name]
    )


def test_clean_dry_run_reports_superseded_toolchain_directories_and_removes_nothing(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """`--dry-run` names the superseded toolchain directories and
    removes none.

    RED: nothing is reported, or the directories are gone.
    """
    project, _, _ = _render_twice(ocx, tmp_path)
    shells = project.home / SHELLS_DIR
    older = _plant_launcher_dir(shells / f"default-{_hex16()}" / "bin").parent
    staging = _plant_launcher_dir(shells / f".staging-{_hex16()}" / "bin").parent

    dry = ocx.json("clean", "--dry-run")

    assert {older.resolve(), staging.resolve()} <= _reported(dry), (
        f"S-105 -- the dry run names both; got {dry['items']}"
    )
    assert_dir_exists(older)
    assert_dir_exists(staging)
