# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Self-tests for the export/import harness: `registry_guard`, `second_home`,
`registry_pair`, and the helpers in `src/export_dir.py`.

Export and import do not exist yet, so the guard is exercised with
`ocx package pull`, which has to reach the registry.
"""

from __future__ import annotations

import json
import threading
import time
from pathlib import Path

import pytest

from src import export_dir
from src import registry_guard as registry_guard_mod
from src.runner import OcxRunner, PackageInfo
from tests.conftest import RegistryPair

# --- registry_guard -------------------------------------------------------


def test_unguarded_home_pulls_and_guard_counts_nothing(
    ocx: OcxRunner,
    published_package: PackageInfo,
    registry_guard: registry_guard_mod.RegistryGuard,
) -> None:
    ocx.plain("package", "pull", published_package.short)

    assert registry_guard.count == 0


def test_guarded_home_fails_and_the_guard_counts_the_attempt(
    ocx: OcxRunner,
    published_package: PackageInfo,
    registry_guard: registry_guard_mod.RegistryGuard,
) -> None:
    registry_guard.deny(ocx)

    result = ocx.plain("package", "pull", published_package.short, check=False)

    assert result.returncode != 0
    assert registry_guard.count >= 1
    assert any(
        "/v2" in line or line.startswith("CONNECT") for line in registry_guard.requests
    )


def test_guard_lifted_lets_the_home_succeed_again_without_counting(
    ocx: OcxRunner,
    published_package: PackageInfo,
    registry_guard: registry_guard_mod.RegistryGuard,
) -> None:
    registry_guard.deny(ocx)
    ocx.plain("package", "pull", published_package.short, check=False)
    attempted = registry_guard.count
    assert attempted >= 1

    registry_guard.allow(ocx)
    ocx.plain("package", "pull", published_package.short)

    assert registry_guard.count == attempted


def test_each_guard_counts_only_the_home_it_was_applied_to(
    ocx: OcxRunner, second_home: OcxRunner, published_package: PackageInfo
) -> None:
    with registry_guard_mod.running() as first, registry_guard_mod.running() as second:
        first.deny(ocx)
        second.deny(second_home)
        ocx.plain("package", "pull", published_package.short, check=False)

        assert first.count >= 1
        assert second.count == 0


# --- second_home ----------------------------------------------------------


def test_second_home_has_its_own_empty_home_on_the_same_registry(
    ocx: OcxRunner, second_home: OcxRunner, published_package: PackageInfo
) -> None:
    assert second_home.ocx_home != ocx.ocx_home
    assert second_home.registry == ocx.registry

    ocx.plain("package", "pull", published_package.short)
    second_home.plain("package", "pull", published_package.short)

    assert (second_home.ocx_home / "packages").is_dir()
    assert not (second_home.ocx_home / "packages" / "does-not-exist").exists()


# --- registry_pair --------------------------------------------------------


def test_registry_pair_serves_one_repo_path_with_different_content(
    registry_pair: RegistryPair,
) -> None:
    pair = registry_pair
    assert pair.source.registry != pair.target.registry
    assert pair.source_pkg.repo == pair.target_pkg.repo == pair.repo
    assert pair.source_pkg.marker != pair.target_pkg.marker

    pair.source.plain("package", "pull", f"{pair.repo}:1.0.0")
    pair.target.plain("package", "pull", f"{pair.repo}:1.0.0")


# --- export_dir helpers ---------------------------------------------------

_DIGEST = "sha256:" + "ab" * 32


def _fake_export(root: Path) -> Path:
    root.mkdir()
    (root / "ocx-export.json").write_text(
        json.dumps(
            {"format_version": 1, "registries": {"localhost_5000": "localhost:5000"}}
        )
    )
    entry = root / "blobs" / "localhost_5000" / "sha256" / "ab" / ("ab" * 15)
    entry.mkdir(parents=True)
    (entry / "data").write_bytes(b"0123456789")
    (entry / "digest").write_text(_DIGEST)
    (entry / "referrers").mkdir()
    (entry / "referrers" / ".complete").write_text("")
    pkg = root / "index" / "localhost_5000" / "p" / "ns"
    (pkg / "tool" / "o" / "sha256").mkdir(parents=True)
    (pkg / "tool.json").write_text("{}")
    (pkg / "tool" / "o" / "sha256" / "cd.json").write_text("{}")
    return root


def test_read_marker_parses_the_export_marker(tmp_path: Path) -> None:
    root = _fake_export(tmp_path / "export")

    assert export_dir.read_marker(root)["format_version"] == 1


def test_blob_helpers_find_the_data_file_by_listing_and_by_digest(
    tmp_path: Path,
) -> None:
    root = _fake_export(tmp_path / "export")

    [data] = export_dir.blob_data_files(root)

    assert data == export_dir.blob_path(root, "localhost_5000", _DIGEST)


def test_flip_byte_changes_one_byte_and_keeps_the_length(tmp_path: Path) -> None:
    data = export_dir.blob_data_files(_fake_export(tmp_path / "export"))[0]
    before = data.read_bytes()

    export_dir.flip_byte(data, 3)

    after = data.read_bytes()
    assert len(after) == len(before)
    assert [i for i in range(len(before)) if before[i] != after[i]] == [3]


def test_truncate_blob_shortens_the_file(tmp_path: Path) -> None:
    data = export_dir.blob_data_files(_fake_export(tmp_path / "export"))[0]

    export_dir.truncate_blob(data)

    assert data.read_bytes() == b"01234"


def test_delete_blob_removes_the_whole_entry(tmp_path: Path) -> None:
    root = _fake_export(tmp_path / "export")
    data = export_dir.blob_data_files(root)[0]

    export_dir.delete_blob(data)

    assert not data.parent.exists()
    assert export_dir.blob_data_files(root) == []


def test_corrupt_index_root_breaks_a_root_but_not_a_dispatch_object(
    tmp_path: Path,
) -> None:
    root = _fake_export(tmp_path / "export")

    target = export_dir.corrupt_index_root(root)

    assert target.name == "tool.json"
    with pytest.raises(json.JSONDecodeError):
        json.loads(target.read_text())
    dispatch = target.parent / "tool" / "o" / "sha256" / "cd.json"
    assert json.loads(dispatch.read_text()) == {}


# --- run_together ---------------------------------------------------------


def test_run_together_releases_every_call_only_after_all_have_started() -> None:
    started = []
    lock = threading.Lock()

    def call(i: int):
        def run() -> tuple[int, int]:
            with lock:
                started.append(i)
            time.sleep(0.05)
            with lock:
                return i, len(started)

        return run

    results = export_dir.run_together([call(0), call(1), call(2)])

    assert [r[0] for r in results] == [0, 1, 2]
    assert all(r[1] == 3 for r in results)


def test_run_together_reraises_a_call_failure() -> None:
    def boom() -> None:
        raise ValueError("boom")

    with pytest.raises(ValueError, match="boom"):
        export_dir.run_together([lambda: None, boom])
