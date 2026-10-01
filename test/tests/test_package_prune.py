# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package prune`, the outside view of the unit tests.

Real pushes against zot, a static index whose root marks tags ephemeral or
durable, and `registry:2` (tag deletion disabled) for the registry's refusal.
Every deletion assertion reads the registry's own tag list before and after, so
a green run cannot come from a command that only reported.
"""

from __future__ import annotations

import hashlib
import json
import urllib.error
import urllib.request
from collections.abc import Iterator
from pathlib import Path

import pytest
from announce_helpers import INDEX_OWNER, INDEX_REPO, announce_json
from fake_forge import FakeForge

from src import static_index
from src.helpers import make_package
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner

pytestmark = pytest.mark.command("package_prune*")

BUILD_1 = "0.5.0-canary_20260101000000"
BUILD_2 = "0.5.0-canary_20260102000000"
BUILD_3 = "0.5.0-canary_20260103000000"
ROLLING = "0.5.0-canary"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _tags(registry: str, repository: str) -> list[str]:
    """The registry's own tag list; a repository that does not exist lists nothing."""
    try:
        with urllib.request.urlopen(f"http://{registry}/v2/{repository}/tags/list", timeout=10) as response:
            return sorted(json.load(response).get("tags") or [])
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return []
        raise


def _publish(ocx: OcxRunner, repository: str, tags: list[str], tmp_path: Path) -> None:
    """Pushes one package per tag, without cascading and without a keep tag, as an ephemeral build is pushed."""
    for tag in tags:
        make_package(
            ocx,
            repository,
            tag,
            tmp_path,
            cascade=False,
            index=False,
            extra_push_args=["--no-keep-tag"],
        )


def _prune(ocx: OcxRunner, *args: str, check: bool = False):
    return ocx.run("package", "prune", *args, format="json", check=check)


def _documents(stdout: str) -> list[dict]:
    """Every JSON value on stdout, in order, so a second document cannot hide behind the first."""
    decoder = json.JSONDecoder()
    documents: list[dict] = []
    position = 0
    while position < len(stdout):
        while position < len(stdout) and stdout[position].isspace():
            position += 1
        if position >= len(stdout):
            break
        document, position = decoder.raw_decode(stdout, position)
        documents.append(document)
    return documents


def _report(result) -> dict:
    """The run's report: under `--format json` stdout is exactly one document, failed run or not."""
    documents = _documents(result.stdout)
    assert len(documents) == 1, f"exactly one JSON document expected, stdout was: {result.stdout!r}"
    report = documents[0]
    assert "selection" in report and "tags" in report, f"the document is not a prune report: {report!r}"
    return report


def _rows(report: dict) -> dict[str, dict]:
    return {row["tag"]: row for row in report["tags"]}


def _actions(report: dict) -> list[tuple[str, str, str | None]]:
    return [(row["tag"], row["action"], row["reason"]) for row in report["tags"]]


def _tags_file(path: Path) -> list[str]:
    return path.read_text().split()


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "index-root"
    root.mkdir()
    with static_index.running(root) as server:
        static_index.write_config(server.root)
        yield server


def _write_root(
    server: static_index.StaticIndexServer,
    repository: str,
    physical: str,
    tags: dict[str, tuple[str, bool]],
) -> Path:
    """Writes `p/<repository>.json`: each tag maps to `(content digest, ephemeral)`.

    A durable row carries no `ephemeral` key at all, the shape every root that
    predates the marker has.
    """
    rows = {}
    for tag, (content, ephemeral) in tags.items():
        row: dict[str, object] = {"content": content, "observed": "2026-01-01T00:00:00Z"}
        if ephemeral:
            row["ephemeral"] = True
        rows[tag] = row
    path = server.root / "p" / f"{repository}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"repository": physical, "tags": rows}))
    return path


def _configure(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    *,
    trusted: bool = True,
    mirror: static_index.StaticIndexServer | None = None,
) -> None:
    """Points `[registries."ocx.sh"] index` at the fixture, optionally with an index-role mirror of it."""
    lines = [f'[registries."ocx.sh"]\nindex = "{server.base_url}"\n']
    if trusted:
        lines.append(f'trusted_hosts = ["{ocx.registry.split(":", 1)[0]}"]\n')
    if mirror is not None:
        lines.append(f'\n[mirrors."{server.host}"]\nindex = "{mirror.base_url}"\n')
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text("".join(lines))
    hosts = [ocx.registry, server.host] + ([mirror.host] if mirror is not None else [])
    ocx.env["OCX_INSECURE_REGISTRIES"] = ",".join(hosts)


def _indexed(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    repository: str,
    ephemeral: dict[str, bool],
) -> str:
    """Publishes an index root for `ocx.sh/<repository>/pkg` over the registry repository `repository`.

    `ephemeral` maps each tag the root lists to its marker; the content each
    row records is the digest the registry serves the tag under. Returns the
    package to hand to `prune`.
    """
    tags = {tag: (fetch_manifest_digest(ocx.registry, repository, tag), marked) for tag, marked in ephemeral.items()}
    _write_root(server, f"{repository}/pkg", f"oci://{ocx.registry}/{repository}", tags)
    _configure(ocx, server)
    return f"ocx.sh/{repository}/pkg"


# ---------------------------------------------------------------------------
# Deleting what the index marks ephemeral
# ---------------------------------------------------------------------------


@pytest.mark.smoke
def test_prune_deletes_a_tag_the_index_marks_ephemeral(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: True, BUILD_2: True})
    served_root = index_server.root / "p" / f"{unique_repo}/pkg.json"

    result = _prune(ocx, package, BUILD_1)

    assert result.returncode == 0, result.stderr
    assert _tags(ocx.registry, unique_repo) == [BUILD_2], "exactly the named tag left the registry"
    report = _report(result)
    assert _actions(report) == [(BUILD_1, "deleted", None)]
    assert report["selection"] == {"tags": [BUILD_1]}
    assert report["force"] is False and report["dry_run"] is False
    assert report["tags"][0]["digest"] is not None, "a deleted row keeps the digest the index recorded"
    assert report["index"] == {
        "url": index_server.base_url,
        "root_sha256": "sha256:" + hashlib.sha256(served_root.read_bytes()).hexdigest(),
    }, "the report names the index and the hash of the root bytes as served"
    assert f"{ocx.registry}/{unique_repo}" in report["repository"], "the registry repository comes from the root"


def test_a_dry_run_of_a_structural_selection_reports_and_deletes_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2, BUILD_3, ROLLING], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {tag: True for tag in (BUILD_1, BUILD_2, BUILD_3, ROLLING)})
    before = _tags(ocx.registry, unique_repo)
    tags_file = tmp_path / "removed.txt"

    dry = _prune(
        ocx, "--dry-run", "--tags-file", str(tags_file), "--prerelease", ROLLING, "--keep-builds", "1", package
    )

    assert dry.returncode == 0, dry.stderr
    assert _tags(ocx.registry, unique_repo) == before, "a dry run leaves the registry untouched"
    assert not tags_file.exists(), "a dry run writes no tags file"
    report = _report(dry)
    assert report["dry_run"] is True
    assert report["selection"] == {"prerelease": ROLLING, "keep_builds": 1}
    assert _actions(report) == [
        (BUILD_1, "would_delete", None),
        (BUILD_2, "would_delete", None),
        (BUILD_3, "kept", "newest"),
        (ROLLING, "kept", "rolling"),
    ], "builds oldest first, the rolling tag last, the newest build and the rolling tag kept"

    real = _prune(ocx, "--tags-file", str(tags_file), "--prerelease", ROLLING, "--keep-builds", "1", package)

    assert real.returncode == 0, real.stderr
    assert _tags(ocx.registry, unique_repo) == sorted([BUILD_3, ROLLING])
    assert [(tag, action) for tag, action, _ in _actions(_report(real))] == [
        (BUILD_1, "deleted"),
        (BUILD_2, "deleted"),
        (BUILD_3, "kept"),
        (ROLLING, "kept"),
    ]
    assert sorted(_tags_file(tags_file)) == [BUILD_1, BUILD_2], "the tags file lists what is gone, not what was kept"
    assert tags_file.read_text().endswith("\n")


def test_pruning_every_tag_of_a_repository_leaves_none(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2, ROLLING], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {tag: True for tag in (BUILD_1, BUILD_2, ROLLING)})

    result = _prune(ocx, "--prerelease", ROLLING, package)

    assert result.returncode == 0, result.stderr
    assert _tags(ocx.registry, unique_repo) == [], "the last tags of a repository go too"
    assert _actions(_report(result)) == [
        (BUILD_1, "deleted", None),
        (BUILD_2, "deleted", None),
        (ROLLING, "deleted", None),
    ]


# ---------------------------------------------------------------------------
# The safeguard
# ---------------------------------------------------------------------------


def test_a_durable_tag_is_refused_with_and_without_a_dry_run(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: True, BUILD_2: False})
    before = _tags(ocx.registry, unique_repo)

    for flags in ([], ["--dry-run"]):
        result = _prune(ocx, *flags, package, BUILD_1, BUILD_2)

        assert result.returncode == 81, f"{flags}: {result.stderr}"
        assert _tags(ocx.registry, unique_repo) == before, f"{flags}: a refusal deletes nothing, not even the ephemeral tag"
        assert "--force" in result.stderr, f"{flags}: the hint names the way past the refusal"
        rows = _rows(_report(result))
        assert rows[BUILD_2]["action"] == "refused" and rows[BUILD_2]["reason"] == "durable", flags
        assert rows[BUILD_1]["action"] == "not_attempted", f"{flags}: the tag behind the refusal is never reached"


def test_a_tag_the_index_does_not_list_is_refused_as_not_in_the_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: True})
    before = _tags(ocx.registry, unique_repo)

    result = _prune(ocx, package, BUILD_2)

    assert result.returncode == 75, result.stderr
    assert _tags(ocx.registry, unique_repo) == before
    assert "--tags-file" in result.stderr and "--ephemeral" in result.stderr, (
        "the hint names the announce that marks the tag ephemeral"
    )
    row = _rows(_report(result))[BUILD_2]
    assert row["action"] == "refused" and row["reason"] == "not_in_index"
    assert row["digest"] == fetch_manifest_digest(ocx.registry, unique_repo, BUILD_2), (
        "with no root row the digest is the one the registry served"
    )


def test_no_index_refuses_and_force_then_deletes(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2], tmp_path)
    package = f"{ocx.registry}/{unique_repo}"
    before = _tags(ocx.registry, unique_repo)

    refused = _prune(ocx, package, BUILD_1)

    assert refused.returncode == 81, refused.stderr
    assert _tags(ocx.registry, unique_repo) == before
    assert "--force" in refused.stderr

    forced = _prune(ocx, "--force", package, BUILD_1)

    assert forced.returncode == 0, forced.stderr
    assert _tags(ocx.registry, unique_repo) == [BUILD_2]
    report = _report(forced)
    assert report["index"] is None
    assert report["force"] is True
    assert _actions(report) == [(BUILD_1, "deleted", None)]


def test_force_deletes_a_durable_tag_and_leaves_a_tag_absent_everywhere_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: False})

    result = _prune(ocx, "--force", package, BUILD_1, BUILD_2)

    assert result.returncode == 0, result.stderr
    assert _tags(ocx.registry, unique_repo) == []
    assert _actions(_report(result)) == [(BUILD_1, "deleted", None), (BUILD_2, "absent", None)], (
        "force skips the refusal but the probe still finds BUILD_2 nowhere"
    )


def test_rerunning_a_finished_prune_reports_absent_and_repeats_the_tags_file(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1, BUILD_2], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: True, BUILD_2: True})
    tags_file = tmp_path / "removed.txt"

    first = _prune(ocx, "--tags-file", str(tags_file), package, BUILD_1, BUILD_2)
    assert first.returncode == 0, first.stderr
    assert sorted(_tags_file(tags_file)) == [BUILD_1, BUILD_2]

    second_file = tmp_path / "removed-again.txt"
    second = _prune(ocx, "--tags-file", str(second_file), package, BUILD_1, BUILD_2)

    assert second.returncode == 0, second.stderr
    assert _actions(_report(second)) == [(BUILD_1, "absent", None), (BUILD_2, "absent", None)]
    assert sorted(_tags_file(second_file)) == [BUILD_1, BUILD_2], (
        "a tag gone from the registry but still in the root is still owed to the announce"
    )


def test_a_tags_file_that_cannot_be_written_exits_74_after_the_deletes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: True})

    result = _prune(ocx, "--tags-file", str(tmp_path / "no-such-directory" / "removed.txt"), package, BUILD_1)

    assert result.returncode == 74, result.stderr
    assert _tags(ocx.registry, unique_repo) == [], "the write failure comes after the deletes, never instead of them"
    assert _actions(_report(result)) == [(BUILD_1, "deleted", None)]


# ---------------------------------------------------------------------------
# Refusals before the safeguard
# ---------------------------------------------------------------------------


def test_a_registry_that_cannot_delete_tags_exits_87_and_deletes_nothing(
    ocx: OcxRunner, mirror_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)
    make_package(mirror_ocx, unique_repo, "1.0.0", tmp_path, cascade=False, index=False)
    before = _tags(mirror_registry, unique_repo)
    assert "1.0.0" in before

    result = _prune(mirror_ocx, "--force", f"{mirror_registry}/{unique_repo}", "1.0.0")

    assert result.returncode == 87, result.stderr
    assert _tags(mirror_registry, unique_repo) == before, "nothing was deleted"
    assert _actions(_report(result)) == [("1.0.0", "not_attempted", None)], (
        "the report is the one document; the exit code carries the failure"
    )


def test_offline_refuses_before_any_network_request(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1], tmp_path)
    package = _indexed(ocx, index_server, unique_repo, {BUILD_1: True})
    before = _tags(ocx.registry, unique_repo)
    index_server.requests.clear()

    result = ocx.run("--offline", "package", "prune", "--force", package, BUILD_1, format="json", check=False)

    assert result.returncode == 81, result.stderr
    assert _tags(ocx.registry, unique_repo) == before, "a forced prune under --offline must still delete nothing"
    assert index_server.requests == [], "the index is not read either"


@pytest.mark.parametrize(
    "args",
    [
        pytest.param([], id="no-selection"),
        pytest.param([BUILD_1, "--prerelease", ROLLING], id="tag-and-prerelease"),
        pytest.param(["--prerelease", ROLLING, "--keep-builds", "0"], id="keep-builds-zero"),
        pytest.param([BUILD_1, "--keep-builds", "1"], id="keep-builds-without-prerelease"),
        pytest.param(["--prerelease", "0.5.0"], id="prerelease-is-a-release"),
        pytest.param(["--prerelease", BUILD_1], id="prerelease-carries-a-build"),
        pytest.param(["sha256:" + "a" * 64], id="digest-as-tag"),
    ],
)
def test_argument_errors_exit_64(ocx: OcxRunner, unique_repo: str, args: list[str]) -> None:
    result = _prune(ocx, f"{ocx.registry}/{unique_repo}", *args)

    assert result.returncode == 64, result.stderr


@pytest.mark.parametrize(
    "tag",
    [
        pytest.param(f"x/../../other/manifests/{BUILD_1}", id="path-traversal"),
        pytest.param("sha256%3A" + "a" * 64, id="percent-encoded-digest"),
        pytest.param(f"{BUILD_1}?x=1", id="query"),
        pytest.param("__ocx.keep.sha256-" + "a" * 64, id="keep-tag"),
    ],
)
def test_a_tag_outside_the_oci_grammar_exits_64_even_forced(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, tag: str
) -> None:
    _publish(ocx, unique_repo, [BUILD_1], tmp_path)
    before = _tags(ocx.registry, unique_repo)

    result = _prune(ocx, "--force", f"{ocx.registry}/{unique_repo}", tag)

    assert result.returncode == 64, result.stderr
    assert _tags(ocx.registry, unique_repo) == before, "a refused tag must never reach the registry"


# ---------------------------------------------------------------------------
# Locating the registry through the index
# ---------------------------------------------------------------------------


def test_prune_reads_the_configured_index_and_never_its_mirror(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """A root served from the index-role mirror would let a stale copy mark a durable tag ephemeral."""
    _publish(ocx, unique_repo, [BUILD_1], tmp_path)
    digest = fetch_manifest_digest(ocx.registry, unique_repo, BUILD_1)
    physical = f"oci://{ocx.registry}/{unique_repo}"
    repository = f"{unique_repo}/pkg"
    _write_root(index_server, repository, physical, {BUILD_1: (digest, False)})

    decoy_root = tmp_path / "decoy-root"
    decoy_root.mkdir()
    with static_index.running(decoy_root) as decoy:
        static_index.write_config(decoy.root)
        _write_root(decoy, repository, physical, {BUILD_1: (digest, True)})
        _configure(ocx, index_server, mirror=decoy)
        package = f"ocx.sh/{repository}"

        control = ocx.run("index", "update", package, format="json", check=False)
        assert any(record.path.startswith("/p/") for record in decoy.requests), (
            f"the mirror override must be live for ordinary resolution, or this test proves nothing: {control.stderr}"
        )
        decoy.requests.clear()
        index_server.requests.clear()

        result = _prune(ocx, package, BUILD_1)

        assert result.returncode == 81, result.stderr
        assert _tags(ocx.registry, unique_repo) == [BUILD_1], "the mirror's ephemeral marker must not delete the tag"
        assert any(record.path.startswith("/p/") for record in index_server.requests), "the configured URL was read"
        assert decoy.requests == [], "the mirror was never asked"


def test_a_repository_pointer_at_a_forbidden_host_exits_78(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    _publish(ocx, unique_repo, [BUILD_1], tmp_path)
    digest = fetch_manifest_digest(ocx.registry, unique_repo, BUILD_1)
    _write_root(index_server, f"{unique_repo}/pkg", "oci://169.254.169.254/acme/tool", {BUILD_1: (digest, True)})
    _configure(ocx, index_server, trusted=False)
    package = f"ocx.sh/{unique_repo}/pkg"

    for flags in ([], ["--force"]):
        result = _prune(ocx, *flags, package, BUILD_1)

        assert result.returncode == 78, f"{flags}: {result.stderr}"
        assert _tags(ocx.registry, unique_repo) == [BUILD_1], f"{flags}: nothing is deleted anywhere"


def test_an_unreachable_index_exits_75_with_and_without_force(ocx: OcxRunner, unique_repo: str) -> None:
    """A refused connect may clear on a rerun, so it is 75 (retry), not 69."""
    dead = "127.0.0.1:1"
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(f'[registries."ocx.sh"]\nindex = "http://{dead}"\n')
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{dead}"

    for flags in ([], ["--force"]):
        result = _prune(ocx, *flags, f"ocx.sh/{unique_repo}/pkg", BUILD_1)

        assert result.returncode == 75, f"{flags}: {result.stderr}"
        assert "the index locates the registry; retry" in result.stderr, flags


def test_a_package_the_index_has_no_root_for_exits_79(
    ocx: OcxRunner, unique_repo: str, index_server: static_index.StaticIndexServer
) -> None:
    _configure(ocx, index_server)

    for flags in ([], ["--force"]):
        result = _prune(ocx, *flags, f"ocx.sh/{unique_repo}/pkg", BUILD_1)

        assert result.returncode == 79, f"{flags}: {result.stderr}"


# ---------------------------------------------------------------------------
# The documented recipes, end to end
# ---------------------------------------------------------------------------

MR_BUILD_1 = "0.5.0-mr42_20260101000000"
MR_BUILD_2 = "0.5.0-mr42_20260102000000"
MR_ROLLING = "0.5.0-mr42"
RELEASE_TAGS = ("0.5.0", "0.5", "0", "latest")
INDEX_FORGE = ["--index-repo", f"{INDEX_OWNER}/{INDEX_REPO}"]


def _row(digest: str, ephemeral: bool) -> dict[str, object]:
    row: dict[str, object] = {"content": digest, "observed": "2026-01-01T00:00:00Z"}
    if ephemeral:
        row["ephemeral"] = True
    return row


def _seed_root(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    fake_forge: FakeForge,
    unique_repo: str,
    rows: dict[str, dict[str, object]],
) -> str:
    """Commits one root to both the served index and the forge, so prune and announce read the same rows.

    Returns the package both commands take.
    """
    repository = f"{unique_repo}/pkg"
    root = {"name": f"ocx.sh/{repository}", "repository": f"oci://{ocx.registry}/{unique_repo}", "tags": rows}
    body = (json.dumps(root, indent=2) + "\n").encode()
    served = server.root / "p" / f"{repository}.json"
    served.parent.mkdir(parents=True, exist_ok=True)
    served.write_bytes(body)
    fake_forge.seed_files(INDEX_OWNER, INDEX_REPO, {f"p/{repository}.json": body})
    _configure(ocx, server)
    return f"ocx.sh/{repository}"


def _digests(ocx: OcxRunner, repository: str, tags: list[str]) -> dict[str, str]:
    return {tag: fetch_manifest_digest(ocx.registry, repository, tag) for tag in tags}


def _written_tags(out_dir: Path, package: str) -> dict[str, dict]:
    return json.loads((out_dir / "p" / f"{package.removeprefix('ocx.sh/')}.json").read_bytes())["tags"]


def _seed_change_request_track(
    ocx: OcxRunner,
    repository: str,
    tmp_path: Path,
    server: static_index.StaticIndexServer,
    fake_forge: FakeForge,
) -> tuple[str, dict[str, dict[str, object]]]:
    """Publishes a change-request track beside a cascaded release and commits a root listing all of it.

    The track's tags are marked ephemeral and the release's are durable. Returns the package and the seeded rows.
    """
    track = [MR_BUILD_1, MR_BUILD_2, MR_ROLLING]
    _publish(ocx, repository, track, tmp_path)
    make_package(ocx, repository, "0.5.0", tmp_path / "release", index=False)
    digests = _digests(ocx, repository, track + list(RELEASE_TAGS))
    rows = {tag: _row(digest, tag in track) for tag, digest in digests.items()}
    return _seed_root(ocx, server, fake_forge, repository, rows), rows


def test_canary_recipe_replaces_old_builds_in_the_registry_and_the_written_root(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    fake_forge: FakeForge,
) -> None:
    old = [BUILD_1, BUILD_2, ROLLING]
    _publish(ocx, unique_repo, old, tmp_path)
    old_digests = _digests(ocx, unique_repo, old)
    package = _seed_root(
        ocx, index_server, fake_forge, unique_repo, {tag: _row(digest, True) for tag, digest in old_digests.items()}
    )
    tags_file = tmp_path / "tags.txt"
    out_dir = tmp_path / "index-out"

    make_package(
        ocx,
        unique_repo,
        ROLLING,
        tmp_path / "canary",
        index=False,
        extra_push_args=["--no-keep-tag", "--build-timestamp=datetime", "--tags-file", str(tags_file)],
    )
    pushed = _tags_file(tags_file)
    assert len(pushed) == 2 and ROLLING in pushed, f"push records the new build and the rolling tag, got {pushed}"
    new_build = next(tag for tag in pushed if tag != ROLLING)

    pruned = _prune(
        ocx, "--prerelease", ROLLING, "--keep-builds", "1", "--tags-file", str(tags_file), package
    )
    assert pruned.returncode == 0, pruned.stderr
    assert _tags(ocx.registry, unique_repo) == sorted([new_build, ROLLING]), "the older builds left the registry"

    repaired = ocx.run("package", "cascade", "repair", "--tags-file", str(tags_file), f"{ocx.registry}/{unique_repo}")
    assert repaired.returncode == 0, repaired.stderr
    assert sorted(_tags_file(tags_file)) == sorted([new_build, ROLLING, BUILD_1, BUILD_2]), (
        "one file carries the pushed tags and the pruned ones to the announce"
    )

    report = announce_json(
        ocx, fake_forge, *INDEX_FORGE, "--tags-file", str(tags_file), "--ephemeral", "--out", str(out_dir), package
    )

    assert sorted(report["removed"]) == sorted([BUILD_1, BUILD_2])
    rows = _written_tags(out_dir, package)
    assert sorted(rows) == sorted([new_build, ROLLING]), "the written root lost exactly the deleted builds"
    new_digest = fetch_manifest_digest(ocx.registry, unique_repo, new_build)
    assert rows[new_build]["content"] == new_digest and rows[new_build].get("ephemeral") is True
    assert rows[ROLLING]["content"] == new_digest, "the rolling row follows the tag onto the new build"
    assert rows[ROLLING].get("ephemeral") is True, "the rolling row keeps the marker it had"


def test_teardown_recipe_removes_the_track_and_leaves_the_release_tags(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    fake_forge: FakeForge,
) -> None:
    package, seeded = _seed_change_request_track(ocx, unique_repo, tmp_path, index_server, fake_forge)
    tags_file = tmp_path / "tags.txt"
    out_dir = tmp_path / "index-out"

    pruned = _prune(ocx, "--prerelease", MR_ROLLING, "--tags-file", str(tags_file), package)
    assert pruned.returncode == 0, pruned.stderr
    remaining = _tags(ocx.registry, unique_repo)
    assert not [tag for tag in remaining if tag.startswith(MR_ROLLING)], f"the whole track is gone, left {remaining}"
    assert set(RELEASE_TAGS) <= set(remaining), "latest, 0.5 and 0 are untouched"

    report = announce_json(ocx, fake_forge, *INDEX_FORGE, "--tags-file", str(tags_file), "--out", str(out_dir), package)

    assert sorted(report["removed"]) == sorted([MR_BUILD_1, MR_BUILD_2, MR_ROLLING])
    assert _written_tags(out_dir, package) == {tag: seeded[tag] for tag in RELEASE_TAGS}, (
        "the root keeps every release row exactly as it was"
    )


def test_a_rerun_teardown_selects_nothing_and_a_refresh_then_removes_the_rows(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    fake_forge: FakeForge,
) -> None:
    package, seeded = _seed_change_request_track(ocx, unique_repo, tmp_path, index_server, fake_forge)
    tags_file = tmp_path / "tags.txt"
    out_dir = tmp_path / "index-out"
    first = _prune(ocx, "--prerelease", MR_ROLLING, "--tags-file", str(tags_file), package)
    assert first.returncode == 0, first.stderr
    tags_file.unlink()
    before = _tags(ocx.registry, unique_repo)

    rerun = _prune(ocx, "--prerelease", MR_ROLLING, "--tags-file", str(tags_file), package)

    assert rerun.returncode == 0, rerun.stderr
    assert _report(rerun)["tags"] == [], "the registry no longer holds the track, so nothing is selected"
    assert tags_file.read_text() == "", "the file exists and is empty"
    assert _tags(ocx.registry, unique_repo) == before

    empty = announce_json(ocx, fake_forge, *INDEX_FORGE, "--tags-file", str(tags_file), "--out", str(out_dir), package)
    assert not out_dir.exists(), f"an empty tags file writes nothing, got {empty}"

    refreshed = announce_json(ocx, fake_forge, *INDEX_FORGE, "--refresh", "--out", str(out_dir), package)

    assert sorted(refreshed["removed"]) == sorted([MR_BUILD_1, MR_BUILD_2, MR_ROLLING])
    assert _written_tags(out_dir, package) == {tag: seeded[tag] for tag in RELEASE_TAGS}
