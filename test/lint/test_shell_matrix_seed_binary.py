# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`shell_matrix.seed_binary` gives each test a binary without a copy per test.

The zoo seeds an `ocx` into every test's install root. A debug binary is
hundreds of megabytes and the three modules run hundreds of tests, so a copy per
test filled the host disk. One copy per process is hard-linked into each root.
"""

from __future__ import annotations

import stat
from pathlib import Path

from src import shell_matrix as matrix


def _source(tmp_path: Path) -> Path:
    source = tmp_path / "source" / "ocx"
    source.parent.mkdir()
    source.write_bytes(b"\x7fELF-fixture")
    source.chmod(0o644)
    return source


def test_seeded_binaries_share_one_inode(tmp_path: Path) -> None:
    source = _source(tmp_path)
    first, second = tmp_path / "a" / "bin" / "ocx", tmp_path / "b" / "bin" / "ocx"
    matrix.seed_binary(source, first)
    matrix.seed_binary(source, second)
    assert first.read_bytes() == second.read_bytes() == source.read_bytes()
    assert first.stat().st_ino == second.stat().st_ino, "each test got its own copy of the binary"
    assert first.stat().st_ino != source.stat().st_ino


def test_seeded_binary_is_executable(tmp_path: Path) -> None:
    destination = tmp_path / "a" / "ocx"
    matrix.seed_binary(_source(tmp_path), destination)
    assert destination.stat().st_mode & stat.S_IXUSR


def test_seeding_over_an_existing_file_replaces_it(tmp_path: Path) -> None:
    destination = tmp_path / "a" / "ocx"
    destination.parent.mkdir()
    destination.write_bytes(b"stale")
    matrix.seed_binary(_source(tmp_path), destination)
    assert destination.read_bytes() == b"\x7fELF-fixture"
