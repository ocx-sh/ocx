"""Acceptance tests for `ocx package export`.

An export directory holds `ocx-export.json`, `index/<slug>/...` (roots limited to
the exported tags) and `blobs/<slug>/...` (every blob the packages need). The
export never leaves a layer archive in the home's own `blobs/`.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from src.export_dir import MARKER, blob_data_files, blob_path, read_marker
from src.helpers import make_package
from src.registry import (
    fetch_manifest_raw,
    fetch_platform_manifest_digest,
    push_referrer,
)
from src.runner import OcxRunner, PackageInfo, registry_dir
from tests.conftest import RegistryPair

pytestmark = pytest.mark.command("package_export")

PLATFORMS = ("linux/amd64", "linux/arm64", "darwin/arm64")


def _export(
    ocx: OcxRunner,
    out: Path,
    *packages: str,
    platforms: tuple[str, ...] = (),
    no_referrers: bool = False,
    offline: bool = False,
    check: bool = True,
):
    args: list[str] = ["--offline"] if offline else []
    args += ["package", "export", "-o", str(out)]
    for platform in platforms:
        args += ["-p", platform]
    if no_referrers:
        args.append("--no-referrers")
    args += packages
    return ocx.run(*args, check=check)


def _report(result) -> dict[str, Any]:
    return json.loads(result.stdout)


def _blob(directory: Path, registry: str, digest: str) -> Path:
    return blob_path(directory, registry_dir(registry), digest)


def _root(index_dir: Path, repo: str) -> dict[str, Any]:
    """The one package root for `repo` under `index_dir` (dispatch objects sit under `o/`)."""
    roots = [
        p
        for p in index_dir.rglob("*.json")
        if "o" not in p.relative_to(index_dir).parts
        and (p.stem == repo or p.parent.name == repo)
    ]
    assert len(roots) == 1, (
        f"expected one root for {repo} under {index_dir}, got {roots}"
    )
    return json.loads(roots[0].read_text(encoding="utf-8"))


def _manifest_chain(
    directory: Path, registry: str, leaf_digest: str
) -> tuple[str, list[str]]:
    """`(config digest, layer digests)` of a leaf manifest read from the export's blobs."""
    leaf = json.loads(_blob(directory, registry, leaf_digest).read_bytes())
    return leaf["config"]["digest"], [layer["digest"] for layer in leaf["layers"]]


def _platform_name(platform: dict[str, str]) -> str:
    """The `os/arch[/variant]` spelling of a report's platform object."""
    parts = [platform["os"], platform["architecture"]]
    if platform.get("variant"):
        parts.append(platform["variant"])
    return "/".join(parts)


def _publish_platforms(ocx: OcxRunner, repo: str, tmp_path: Path) -> None:
    for platform in PLATFORMS:
        build = tmp_path / f"build-{platform.replace('/', '-')}"
        build.mkdir()
        make_package(ocx, repo, "1.0.0", build, platform=platform)


def _push_leaf_referrer(registry: str, repo: str, pkg: PackageInfo) -> str:
    """Attach one referrer to the package's host-platform leaf and return its digest."""
    leaf_digest = fetch_platform_manifest_digest(
        registry, repo, pkg.tag, platform=pkg.platform
    )
    body, _ = fetch_manifest_raw(registry, repo, leaf_digest)
    digest, _ = push_referrer(
        registry,
        repo,
        leaf_digest,
        len(body),
        artifact_type="application/vnd.ocx.test.export+json",
    )
    return digest


def test_export_tagged_package_layout(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    referrer = _push_leaf_referrer(ocx.registry, unique_repo, pkg)
    out = tmp_path / "export"

    report = _report(_export(ocx, out, pkg.short))

    slug = registry_dir(ocx.registry)
    assert read_marker(out)["registries"] == {slug: ocx.registry}
    root = _root(out / "index" / slug, unique_repo)
    assert set(root["tags"]) == {"1.0.0"}, "the root lists exactly the exported tag"

    package = report["packages"][0]
    index_digest = package["tags"][0]["digest"]
    leaf_digest = package["manifests"][0]["digest"]
    config_digest, layer_digests = _manifest_chain(out, ocx.registry, leaf_digest)
    for digest in (index_digest, leaf_digest, config_digest, *layer_digests, referrer):
        assert _blob(out, ocx.registry, digest).is_file(), (
            f"{digest} missing from export blobs"
        )
    leaf_entry = _blob(out, ocx.registry, leaf_digest).parent
    algorithm, hex_digest = referrer.split(":", 1)
    assert (leaf_entry / "referrers" / f"{algorithm}-{hex_digest}").is_file()
    assert (leaf_entry / "referrers" / ".complete").is_file()

    home_blobs = ocx.ocx_home / "blobs"
    for digest in layer_digests:
        assert not _blob(home_blobs, ocx.registry, digest).exists(), (
            "layer archive leaked into the home"
        )


def test_export_bare_identifier_exports_latest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    for tag in ("1.0.0", "2.0.0"):
        build = tmp_path / f"build-{tag}"
        build.mkdir()
        make_package(ocx, unique_repo, tag, build)
    slug = registry_dir(ocx.registry)

    bare = tmp_path / "bare"
    _export(ocx, bare, f"{ocx.registry}/{unique_repo}")
    assert set(_root(bare / "index" / slug, unique_repo)["tags"]) == {"latest"}

    named = tmp_path / "named"
    _export(ocx, named, f"{unique_repo}:1", f"{unique_repo}:2")
    assert set(_root(named / "index" / slug, unique_repo)["tags"]) == {"1", "2"}


def test_export_keeps_registries_apart(
    registry_pair: RegistryPair, tmp_path: Path
) -> None:
    pair = registry_pair
    out = tmp_path / "export"
    a, b = pair.source.registry, pair.target.registry

    report = _report(
        _export(pair.source, out, f"{a}/{pair.repo}:1.0.0", f"{b}/{pair.repo}:1.0.0")
    )

    leaves = {
        row["identifier"].split("/", 1)[0]: row["manifests"][0]["digest"]
        for row in report["packages"]
    }
    assert set(leaves) == {a, b}
    assert leaves[a] != leaves[b], (
        "fixture precondition: the two registries serve different content"
    )
    assert _blob(out, a, leaves[a]).is_file()
    assert _blob(out, b, leaves[b]).is_file()
    assert not _blob(out, a, leaves[b]).exists(), (
        "registry b's leaf leaked into a's subtree"
    )
    assert not _blob(out, b, leaves[a]).exists(), (
        "registry a's leaf leaked into b's subtree"
    )
    for registry in (a, b):
        root = _root(out / "index" / registry_dir(registry), pair.repo)
        assert set(root["tags"]) == {"1.0.0"}
    assert read_marker(out)["registries"] == {registry_dir(a): a, registry_dir(b): b}


def test_export_platform_filter(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    _publish_platforms(ocx, unique_repo, tmp_path)

    everything = tmp_path / "all"
    rows = _report(_export(ocx, everything, f"{unique_repo}:1.0.0"))["packages"][0][
        "manifests"
    ]
    digests = {_platform_name(row["platform"]): row["digest"] for row in rows}
    assert set(digests) == set(PLATFORMS)
    for digest in digests.values():
        assert _blob(everything, ocx.registry, digest).is_file()

    two = tmp_path / "two"
    rows = _report(
        _export(
            ocx, two, f"{unique_repo}:1.0.0", platforms=("linux/amd64", "darwin/arm64")
        )
    )["packages"][0]["manifests"]
    assert {_platform_name(row["platform"]) for row in rows} == {"linux/amd64", "darwin/arm64"}
    for platform, digest in digests.items():
        held = _blob(two, ocx.registry, digest).is_file()
        assert held == (platform != "linux/arm64"), (
            f"{platform} leaf presence wrong: {held}"
        )


def test_export_slug_collision_refused(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    upper = ocx.registry.upper()
    out = tmp_path / "export"
    out.mkdir()
    marker = {"format_version": 1, "registries": {registry_dir(upper): upper}}
    (out / MARKER).write_text(json.dumps(marker), encoding="utf-8")

    result = _export(ocx, out, published_package.short, check=False)

    assert result.returncode == 65, result.stdout + result.stderr
    assert json.loads(result.stdout)["error"]["detail"] == "export_dir_invalid"
    text = result.stdout + result.stderr
    assert ocx.registry in text and (upper in text or registry_dir(upper) in text), (
        "must name both"
    )
    assert json.loads((out / MARKER).read_text(encoding="utf-8")) == marker


def test_export_commits_first_sight_pin(
    ocx: OcxRunner,
    second_home: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    index_dir = second_home.ocx_home / "index"
    assert not list(index_dir.rglob("*.json")), (
        "precondition: the home pins nothing yet"
    )
    out = tmp_path / "export"

    report = _report(_export(second_home, out, published_package.short))

    digest = report["packages"][0]["tags"][0]["digest"]
    tag = published_package.tag
    pinned = _root(index_dir, published_package.repo)["tags"][tag]["content"]
    assert pinned == digest


def test_offline_export_of_unknown_tag_is_81(
    second_home: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    out = tmp_path / "export"

    result = _export(
        second_home, out, published_package.short, offline=True, check=False
    )

    assert result.returncode == 81, result.stdout + result.stderr
    assert json.loads(result.stdout)["error"]["detail"] == "index_resolution_blocked"
    assert not (out / MARKER).exists()


def test_export_requires_output_and_package(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    no_output = ocx.run("package", "export", published_package.short, check=False)
    assert no_output.returncode == 64, no_output.stdout + no_output.stderr

    no_package = ocx.run(
        "package", "export", "-o", str(tmp_path / "export"), check=False
    )
    assert no_package.returncode == 64, no_package.stdout + no_package.stderr


def test_archive_onto_a_directory_is_refused(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """`--archive -o <existing directory>` is a data error that says why; the directory is untouched."""
    target = tmp_path / "already-a-directory"
    target.mkdir()

    result = ocx.run(
        "package",
        "export",
        "--archive",
        "-o",
        str(target),
        published_package.short,
        check=False,
    )

    assert result.returncode == 65, result.stdout + result.stderr
    assert "existing directory" in result.stderr, result.stderr
    assert target.is_dir() and not any(target.iterdir())


def test_archive_shaped_output_without_the_flag_warns_and_writes_a_directory(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """`-o bundle.tar` without `--archive` still writes a directory, and says so on stderr only."""
    target = tmp_path / "bundle.tar"

    result = _export(ocx, target, published_package.short)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "pass --archive" in result.stderr, result.stderr
    assert result.stderr.count("pass --archive") == 1, result.stderr
    assert target.is_dir() and (target / MARKER).is_file()
    assert "pass --archive" not in result.stdout
    assert _report(result)["packages"], result.stdout


def test_import_of_a_file_that_is_not_an_archive_names_the_file(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A regular file is read as an archive; one that is not fails and says which file."""
    garbage = tmp_path / "garbage.tar"
    garbage.write_bytes(b"this is not a tar archive at all, just some text")

    result = ocx.run("package", "import", str(garbage), check=False)

    assert result.returncode == 65, result.stdout + result.stderr
    assert str(garbage) in result.stderr, result.stderr


def test_export_report_fields(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    _push_leaf_referrer(ocx.registry, unique_repo, pkg)
    out = tmp_path / "export"

    first = _report(_export(ocx, out, pkg.short))

    assert first["output_dir"] == str(out)
    assert first["schema_version"] == 1
    package = first["packages"][0]
    assert package["identifier"].endswith(f"/{unique_repo}:1.0.0")
    assert package["tags"][0]["tag"] == "1.0.0"
    assert package["tags"][0]["status"] == "added"
    assert [row["status"] for row in package["manifests"]] == ["written"]
    assert package["referrer_count"] == 1
    data_files = blob_data_files(out)
    assert first["blob_count"] > 0
    assert first["blob_count"] == len(data_files)
    assert first["written_size"] == sum(f.stat().st_size for f in data_files)

    second = _report(_export(ocx, out, pkg.short))

    assert second["packages"][0]["tags"][0]["status"] == "unchanged"
    assert [row["status"] for row in second["packages"][0]["manifests"]] == ["present"]
    assert second["blob_count"] == 0
    assert second["written_size"] == 0

    bare = tmp_path / "no-referrers"
    assert (
        _report(_export(ocx, bare, pkg.short, no_referrers=True))["packages"][0][
            "referrer_count"
        ]
        == 0
    )
