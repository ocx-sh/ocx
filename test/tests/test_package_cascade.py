# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package cascade check`/`repair` (ocx-sh/ocx#273).

The pure fold/diff/plan core (matrices A-E), the concurrent gather/apply
halves (F/G), and the push-replay equivalence oracle (H) are all covered at
the unit level in `crates/ocx_lib/src/package/cascade/`. This module is the
outside view: real pushes against the `registry:2` fixture, a hand-corrupted
alias via direct HTTP PUT, and — for the logical-identifier path — the
`index.ocx.sh`-shaped `StaticIndexServer` fixture from
`test_index_ocx_sh.py`. Case numbers below (J1-J12) match the plan's test
matrix so a finding traces back to its row.
"""

from __future__ import annotations

import contextlib
import json
import re
from collections.abc import Iterator
from pathlib import Path

from src import static_index
from src.helpers import make_package
from src.registry import fetch_manifest_digest, fetch_manifest_raw, put_manifest
from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Command helpers
# ---------------------------------------------------------------------------


def _check(ocx: OcxRunner, *args: str, index_dir: Path | None = None, check: bool = True):
    global_args = ["--index", str(index_dir)] if index_dir is not None else []
    return ocx.run(*global_args, "package", "cascade", "check", *args, format="json", check=check)


def _repair(ocx: OcxRunner, *args: str, index_dir: Path | None = None, check: bool = True):
    global_args = ["--index", str(index_dir)] if index_dir is not None else []
    return ocx.run(*global_args, "package", "cascade", "repair", *args, format="json", check=check)


def _reports(result) -> list[dict]:
    return json.loads(result.stdout)["reports"]


def _entries(result) -> list[dict]:
    return json.loads(result.stdout)["entries"]


# ---------------------------------------------------------------------------
# J1-J9: physical identifiers against the plain `registry:2` fixture
# ---------------------------------------------------------------------------


def test_j1_healthy_package_checks_clean(ocx: OcxRunner, published_package: PackageInfo) -> None:
    """A package whose rolling tags all agree with its one published version
    checks clean: exit 0, every row `ok`, no index or unrepairable findings.
    """
    result = _check(ocx, published_package.repo)
    assert result.returncode == 0

    report = _reports(result)[0]
    assert report["index_findings"] == []
    assert report["unrepairable"] == []
    assert report["rows"], "a healthy package still reports its ok rows"
    assert all(row["status"] == "ok" for row in report["rows"]), report["rows"]


def test_j2_repair_re_points_a_broken_alias_without_publishing_new_content(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Stripping a platform from `latest` is a `check` finding naming that
    platform; `repair` re-points the alias back to exactly the digest it held
    before the break — a pure re-point, no new content published.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "amd64", platform="linux/amd64", new=True)
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "arm64", platform="linux/arm64", new=False)

    original_body, original_digest = fetch_manifest_raw(ocx.registry, unique_repo, "latest")
    document = json.loads(original_body)
    assert len(document["manifests"]) == 2, "precondition: both platforms present before the break"
    document["manifests"] = [
        entry for entry in document["manifests"] if entry["platform"]["architecture"] != "arm64"
    ]
    put_manifest(ocx.registry, unique_repo, "latest", json.dumps(document).encode(), document["mediaType"])

    broken = _check(ocx, unique_repo, check=False)
    assert broken.returncode == 65
    broken_rows = [
        row for row in _reports(broken)[0]["rows"] if row["tag"] == "latest" and row["status"] == "missing"
    ]
    assert len(broken_rows) == 1, "exactly the dropped platform is missing from latest"
    assert broken_rows[0]["platform"]["architecture"] == "arm64"

    repaired = _repair(ocx, unique_repo)
    assert repaired.returncode == 0
    outcomes = [o for o in _entries(repaired)[0]["outcomes"] if o["tag"] == "latest"]
    assert outcomes and outcomes[0]["outcome"]["outcome"] == "written"

    healed = _check(ocx, unique_repo)
    assert healed.returncode == 0

    _, healed_digest = fetch_manifest_raw(ocx.registry, unique_repo, "latest")
    assert healed_digest == original_digest, "repair reconstructed exactly the pre-break content"


def test_j2b_dry_run_repair_leaves_the_registry_byte_identical(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`repair --dry-run` computes and reports the same plan a real run would,
    but the registry is untouched until the flag is dropped — both outcomes
    proven on the same break.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "amd64", platform="linux/amd64", new=True)
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "arm64", platform="linux/arm64", new=False)

    body, _ = fetch_manifest_raw(ocx.registry, unique_repo, "latest")
    document = json.loads(body)
    document["manifests"] = [
        entry for entry in document["manifests"] if entry["platform"]["architecture"] != "arm64"
    ]
    put_manifest(ocx.registry, unique_repo, "latest", json.dumps(document).encode(), document["mediaType"])
    _, broken_digest = fetch_manifest_raw(ocx.registry, unique_repo, "latest")

    preview = _repair(ocx, "--dry-run", unique_repo, check=False)
    assert preview.returncode == 65, "a preview with a remaining plan is not a clean run"
    preview_entry = _entries(preview)[0]
    assert preview_entry["planned"], "the dry run computed a plan"
    assert preview_entry["outcomes"] == [], "a preview attempts nothing"

    _, after_preview_digest = fetch_manifest_raw(ocx.registry, unique_repo, "latest")
    assert after_preview_digest == broken_digest, "the dry run wrote nothing to the registry"

    plain_preview = ocx.plain("package", "cascade", "repair", "--dry-run", unique_repo, check=False)
    assert "preview only" in plain_preview.stdout
    assert "--refresh" not in plain_preview.stdout, (
        "a preview found no index staleness, so the announce --refresh hop does not apply"
    )
    assert "ocx package announce" not in plain_preview.stdout, (
        "a preview moved no tags, so there is nothing to publish yet"
    )

    real = _repair(ocx, unique_repo)
    assert real.returncode == 0

    _, after_real_digest = fetch_manifest_raw(ocx.registry, unique_repo, "latest")
    assert after_real_digest != broken_digest, "the real run did write the alias"
    assert _check(ocx, unique_repo).returncode == 0


def test_j3_never_created_aliases_become_present_after_repair(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Two versions pushed without `--cascade` (the pnpm shape) leave every
    rolling tag `absent`; `repair` creates all of them from existing content.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False, new=True)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, cascade=False, new=False)

    broken = _check(ocx, unique_repo, check=False)
    assert broken.returncode == 65
    broken_report = _reports(broken)[0]
    assert broken_report["aliases"], "the whole default track is in scope"
    assert all(state["state"] == "absent" for state in broken_report["aliases"].values())

    repaired = _repair(ocx, unique_repo)
    assert repaired.returncode == 0

    healed = _check(ocx, unique_repo)
    assert healed.returncode == 0
    healed_report = _reports(healed)[0]
    assert all(state["state"] == "present" for state in healed_report["aliases"].values())


def test_j4_a_scoped_repair_leaves_the_sibling_majors_alias_untouched(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
) -> None:
    """Repairing `pkg:1.0` fixes only the `1.x` track; major `2`'s own alias,
    outside the requested scope, is byte-identical before and after.
    """
    v1, _v2 = published_two_versions
    repo = v1.repo

    body, _ = fetch_manifest_raw(ocx.registry, repo, "1")
    document = json.loads(body)
    document["manifests"] = []
    put_manifest(ocx.registry, repo, "1", json.dumps(document).encode(), document["mediaType"])

    before_sibling_digest = fetch_manifest_digest(ocx.registry, repo, "2")

    result = _repair(ocx, f"{repo}:1.0")
    assert result.returncode == 0
    outcomes = _entries(result)[0]["outcomes"]
    assert [o["tag"] for o in outcomes] == ["1"], "only the broken in-scope alias is written"

    after_sibling_digest = fetch_manifest_digest(ocx.registry, repo, "2")
    assert after_sibling_digest == before_sibling_digest, "the sibling major's alias must not move"


def test_j5_usage_errors_exit_64(ocx: OcxRunner, published_package: PackageInfo) -> None:
    """A digest reference, a non-version tag, and a version the package never
    published are all usage errors — raised before repair could even attempt
    a write.
    """
    digest_reference = f"{published_package.repo}@sha256:{'a' * 64}"
    assert _check(ocx, digest_reference, check=False).returncode == 64

    non_version_tag = f"{published_package.repo}:nightly-build"
    assert _check(ocx, non_version_tag, check=False).returncode == 64

    unknown_version = f"{published_package.repo}:9.9.9"
    assert _check(ocx, unknown_version, check=False).returncode == 64


def test_j6_two_platform_cascade_checks_clean(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """Both platforms of a single cascaded version check clean, and both
    appear in the report's rows.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "amd64", platform="linux/amd64", new=True)
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "arm64", platform="linux/arm64", new=False)

    result = _check(ocx, unique_repo)
    assert result.returncode == 0
    report = _reports(result)[0]
    assert all(row["status"] == "ok" for row in report["rows"])
    platforms = {(row["platform"]["os"], row["platform"]["architecture"]) for row in report["rows"]}
    assert ("linux", "amd64") in platforms
    assert ("linux", "arm64") in platforms


def test_j7_a_second_repair_after_one_writes_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Repairing an already-healthy graph is a no-op: the second run of two
    back-to-back repairs plans and writes nothing.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False, new=True)

    first = _repair(ocx, unique_repo)
    assert first.returncode == 0
    assert _entries(first)[0]["outcomes"], "the first run had absent aliases to create"

    second = _repair(ocx, unique_repo)
    assert second.returncode == 0
    second_entry = _entries(second)[0]
    assert second_entry["planned"] == []
    assert second_entry["outcomes"] == []


def test_j8_json_key_sets_are_stable(ocx: OcxRunner, published_package: PackageInfo) -> None:
    """The exact key sets a `--format json` consumer parses, pinned at the
    acceptance layer: the check/repair envelopes and the platform grammar a
    row's `platform` object carries.
    """
    check_payload = json.loads(_check(ocx, published_package.repo).stdout)
    assert set(check_payload.keys()) == {"reports"}
    report = check_payload["reports"][0]
    assert set(report.keys()) == {
        "aliases",
        "identifier",
        "ignored_tags",
        "index_findings",
        "logical",
        "rows",
        "unrepairable",
    }
    assert report["rows"], "a single-version, single-platform package still reports its ok rows"
    assert set(report["rows"][0]["platform"].keys()) == {"architecture", "os"}, (
        "no null-valued variant/os.features cruft for an unqualified platform"
    )

    repair_payload = json.loads(_repair(ocx, published_package.repo, "--dry-run").stdout)
    assert set(repair_payload.keys()) == {"announce_tags_path", "dry_run", "entries"}
    assert set(repair_payload["entries"][0].keys()) == {"announce_tags", "outcomes", "planned", "report"}


def test_j9_canonical_digest_tag_is_ignored_not_a_finding(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """The canonical `sha256.<hex>` tag every push leaves behind (default-on
    `--canonical-tag`) is reported in `ignored_tags`, never as a row.
    """
    result = _check(ocx, published_package.repo)
    assert result.returncode == 0
    report = _reports(result)[0]
    assert report["ignored_tags"], "the default canonical-tag push leaves one behind"
    assert all(re.fullmatch(r"sha256\.[0-9a-f]{64}", tag) for tag in report["ignored_tags"])
    assert not any(row["tag"].startswith("sha256.") for row in report["rows"])


# ---------------------------------------------------------------------------
# J10-J12: logical identifiers against the `index.ocx.sh`-shaped fixture
#
# `configure_index_source`/`index_server` mirror `test_index_ocx_sh.py`
# verbatim (that fixture is file-local, not shared via conftest.py); the root
# document itself is written directly rather than through
# `static_index.write_package` — cascade's index layer reads only the root's
# `tags[tag].content`, never the dispatch object that helper also builds.
# ---------------------------------------------------------------------------


@contextlib.contextmanager
def _index_server() -> Iterator[static_index.StaticIndexServer]:
    import tempfile

    with tempfile.TemporaryDirectory() as root:
        with static_index.running(Path(root)) as server:
            yield server


def _configure_index_source(ocx: OcxRunner, server: static_index.StaticIndexServer) -> None:
    """Points `[registries."ocx.sh"] index` at the fixture, mirroring
    `test_index_ocx_sh.py::configure_index_source`.
    """
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path.write_text(
        f'[registries."ocx.sh"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{registry_host}"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"


def _write_root(server: static_index.StaticIndexServer, repository: str, physical_repository: str, tags: dict[str, str]) -> None:
    """Writes a minimal `p/<repository>.json` root document directly: only
    `tags[tag].content`, the field `index_findings` reads. No dispatch object
    is written — cascade's gather never fetches one.
    """
    root = {
        "repository": physical_repository,
        "tags": {tag: {"content": digest, "observed": "2026-01-01T00:00:00Z"} for tag, digest in tags.items()},
    }
    path = server.root / "p" / f"{repository}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(root))


def test_j10_logical_check_reports_index_staleness(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A logical `ocx.sh/...` identifier resolves two hops and, once the
    registry alias moves without the index root following, reports the
    committed-versus-live mismatch and prints the `announce --refresh`
    follow-up. The whole run touches zero local index bytes.
    """
    with _index_server() as server:
        pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
        before_digests = {
            tag: fetch_manifest_digest(ocx.registry, pkg.repo, tag) for tag in ("latest", "1", "1.0")
        }

        _configure_index_source(ocx, server)
        static_index.write_config(server.root)
        repository = f"{unique_repo}/pkg"
        _write_root(server, repository, f"oci://{ocx.registry}/{pkg.repo}", before_digests)
        logical_id = f"ocx.sh/{repository}:latest"

        index_dir = tmp_path / "index_dir"
        clean = _check(ocx, logical_id, index_dir=index_dir)
        assert clean.returncode == 0
        assert _reports(clean)[0]["index_findings"] == []
        assert not index_dir.exists(), "a pure read must leave zero local index bytes"

        make_package(ocx, unique_repo, "1.1.0", tmp_path, new=False, index=False)
        after_latest_digest = fetch_manifest_digest(ocx.registry, pkg.repo, "latest")
        assert after_latest_digest != before_digests["latest"], "precondition: the alias actually moved"

        stale = _check(ocx, logical_id, index_dir=index_dir, check=False)
        assert stale.returncode == 65
        report = _reports(stale)[0]
        findings = {finding["tag"]: finding for finding in report["index_findings"]}
        assert findings["latest"] == {
            "finding": "stale",
            "tag": "latest",
            "committed": before_digests["latest"],
            "live": after_latest_digest,
        }
        assert "1.0" not in findings, "the untouched minor alias carries no finding"
        assert not index_dir.exists(), "the stale run also writes zero local index bytes"

        plain = ocx.plain("--index", str(index_dir), "package", "cascade", "check", logical_id, check=False)
        assert "ocx package announce" in plain.stdout
        assert "--refresh" in plain.stdout
        assert "ocx index update" in plain.stdout, (
            "announcing publishes the index; the local copy is a second hop after it"
        )
        header = next(line for line in plain.stdout.splitlines() if "Tag" in line and "Status" in line)
        assert "Package" not in header, (
            f"a single-package report drops the constant Package column: {header!r}"
        )


def test_j11_physical_identifier_makes_zero_index_requests(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The same `[registries."ocx.sh"]` source configured for J10 is never
    consulted when the identifier names the physical registry directly: a
    foreign-registry jurisdiction check costs no I/O.
    """
    with _index_server() as server:
        pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
        _configure_index_source(ocx, server)
        static_index.write_config(server.root)

        result = _check(ocx, f"{ocx.registry}/{pkg.repo}:latest")

        assert result.returncode == 0
        assert _reports(result)[0]["index_findings"] == []
        assert server.requests == [], "a physical identifier must never reach the index fixture"


def test_j12_announce_tags_file_holds_exactly_the_created_aliases(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--announce-tags` records exactly the aliases a repair created — the
    ones whose write landed, since announce re-observes each tag it is given
    and would otherwise commit a digest this run never wrote. A preview
    records its whole plan instead (it wrote nothing, so "what landed" is
    empty and would make a useless preview), and a second repair against the
    now-healthy graph writes an empty file.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False, new=True)
    expected = ["1", "1.0", "latest"]

    tags_path = tmp_path / "tags.txt"
    preview = _repair(ocx, "--dry-run", "--announce-tags", str(tags_path), unique_repo, check=False)
    assert preview.returncode == 65, "a preview with a remaining plan is not a clean run"
    assert sorted(line for line in tags_path.read_text().splitlines() if line) == expected, (
        "a preview records the whole plan it would have written"
    )

    result = _repair(ocx, "--announce-tags", str(tags_path), unique_repo)
    assert result.returncode == 0

    created = sorted(line for line in tags_path.read_text().splitlines() if line)
    assert created == expected
    assert json.loads(result.stdout)["entries"][0]["announce_tags"] == created

    again = _repair(ocx, "--announce-tags", str(tags_path), unique_repo)
    assert again.returncode == 0
    assert tags_path.read_text() == ""


def test_j13_announce_tags_rejects_a_multi_package_run(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """One `--announce-tags` file holds one bare list of tag names, and
    `announce --tags-from-file` takes one `--package`. Two packages in one
    run would hand the follow-up a list it must attribute to a single
    package, so the flag is a usage error there — and nothing is written.
    """
    other_repo = f"{unique_repo}_second"
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "first", cascade=False, new=True)
    make_package(ocx, other_repo, "1.0.0", tmp_path / "second", cascade=False, new=True)

    tags_path = tmp_path / "tags.txt"
    rejected = _repair(ocx, "--announce-tags", str(tags_path), unique_repo, other_repo, check=False)
    assert rejected.returncode == 64
    assert not tags_path.exists(), "a rejected run writes no handoff file"
    assert _check(ocx, unique_repo, check=False).returncode == 65, (
        "the rejection happened before any repair: the broken graph is untouched"
    )

    # The negative control: the same flag, the same file, one package — the
    # rejection must be about the package count, not about the flag. Run in
    # plain mode so the publish hop's hint is asserted where it does apply
    # (J2b asserts the same lines absent from a preview, which only
    # discriminates against a hint block that can actually fire).
    accepted = ocx.plain("package", "cascade", "repair", "--announce-tags", str(tags_path), unique_repo)
    assert accepted.returncode == 0
    assert sorted(line for line in tags_path.read_text().splitlines() if line) == ["1", "1.0", "latest"]
    assert "ocx package announce" in accepted.stdout
    assert "--tags-from-file" in accepted.stdout, "a run that moved tags names the file to hand the follow-up"

    # Without the flag, a multi-package run is ordinary work.
    both = _repair(ocx, unique_repo, other_repo)
    assert both.returncode == 0
    assert len(_entries(both)) == 2
