# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package create --extract` / `--strip-components`.

`create` takes a directory. `--extract` lets it take the archive a CI job
already produced instead, unpacking it into a temporary tree that becomes the
content root for everything downstream — the bundle, the metadata sidecar, the
binaries scan and the build receipt all see exactly what a directory input
would give them, which is the last test in this module.

The three cases a publisher's own extraction wrapper gets wrong are each
pinned here: a hard-link entry under `--strip-components`, a `0o777` zip entry
that must not publish a world-writable file, and a strip deeper than the
archive, which refuses (exit 65) rather than bundling an empty tree.

Unix modes are asserted throughout, so the module is skipped on Windows. The
zip mode cap is asserted here as an end-to-end property, but its *discriminating*
test is `archive/zip.rs::extract_masks_group_and_other_write_bits` - see
`test_world_writable_zip_entry_is_not_published` for why.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from src.helpers import (
    build_archive,
    bundle_members,
    bundle_text,
    resolved_metadata_path,
    resolved_receipt_path,
)
from src.runner import OcxRunner, current_platform

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="asserts on Unix modes of archive entries and bundle members",
)

EXIT_SUCCESS = 0
EXIT_USAGE_ERR = 64  # ocx_lib::cli::UsageError
EXIT_DATA_ERR = 65  # empty extraction, unsupported format, or containment refusal

HELLO_BODY = "#!/bin/sh\necho hello\n"
README_BODY = "# hello\n"

#: One release layout, spelled once: a version-named top directory that
#: `--strip-components 1` is expected to remove.
RELEASE_TREE = {
    "hello-1.2.3/bin/hello": (HELLO_BODY, 0o755),
    "hello-1.2.3/README.md": (README_BODY, 0o644),
}

ARCHIVE_SUFFIXES = [".tar.gz", ".tar.bz2", ".tar.zst", ".zip"]


def _archive(tmp_path: Path, suffix: str, files: dict[str, tuple[str, int]], **kwargs) -> Path:
    """Build `hello<suffix>` under `tmp_path`.

    Every suffix here is buildable unconditionally: `compression.zstd` is stdlib
    from 3.14, which `pyproject.toml` declares as the floor. The
    `pytest.importorskip` that used to guard `.tar.zst` never observed its own
    cause (W23) — it could only fire below the declared floor, so a green row
    and a silently skipped one were indistinguishable."""
    return build_archive(tmp_path / f"hello{suffix}", files, **kwargs)


def _create(ocx: OcxRunner, source: Path, out: Path, *args: str, check: bool = True):
    return ocx.plain("package", "create", "-o", str(out), *args, str(source), check=check)


def _mode(bundle: Path, name: str) -> int:
    members = bundle_members(bundle)
    assert name in members, f"{name} missing from bundle; got {sorted(members)}"
    return members[name].mode & 0o777


# ---------------------------------------------------------------------------
# The four archive formats, with and without a strip
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("suffix", ARCHIVE_SUFFIXES)
def test_extract_bundles_the_archive_tree(ocx: OcxRunner, tmp_path: Path, suffix: str):
    """`--extract` bundles what the archive holds, paths unchanged."""
    archive = _archive(tmp_path, suffix, RELEASE_TREE)
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--extract")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert set(bundle_members(out)) == {"hello-1.2.3/bin/hello", "hello-1.2.3/README.md"}
    assert bundle_text(out, "hello-1.2.3/bin/hello") == HELLO_BODY
    assert _mode(out, "hello-1.2.3/bin/hello") == 0o755


@pytest.mark.parametrize("suffix", ARCHIVE_SUFFIXES)
def test_strip_components_drops_the_leading_directory(ocx: OcxRunner, tmp_path: Path, suffix: str):
    """`--strip-components 1` removes the version-named release directory."""
    archive = _archive(tmp_path, suffix, RELEASE_TREE)
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--extract", "--strip-components", "1")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert set(bundle_members(out)) == {"bin/hello", "README.md"}
    assert bundle_text(out, "bin/hello") == HELLO_BODY


def test_strip_components_implies_extract(ocx: OcxRunner, tmp_path: Path):
    """`--strip-components` alone extracts; the issue specifies implication,
    not a refusal that would make the pair mandatory."""
    archive = _archive(tmp_path, ".tar.gz", RELEASE_TREE)
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--strip-components", "1")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert set(bundle_members(out)) == {"bin/hello", "README.md"}


def test_without_extract_the_archive_is_bundled_as_one_file(ocx: OcxRunner, tmp_path: Path):
    """The opt-in matters: absent `--extract`, today's store-the-file
    behaviour is unchanged, so the bundle holds the archive itself."""
    archive = _archive(tmp_path, ".tar.gz", RELEASE_TREE)
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert set(bundle_members(out)) == {"hello.tar.gz"}


# ---------------------------------------------------------------------------
# The three edge cases a hand-rolled extraction wrapper gets wrong
# ---------------------------------------------------------------------------


def test_hard_link_entry_under_strip_keeps_its_contents(ocx: OcxRunner, tmp_path: Path):
    """A tar hard link addresses an earlier entry by its in-archive path, so
    the link name takes the same strip as the entry names. Both files land in
    the bundle with the same contents."""
    archive = _archive(
        tmp_path,
        ".tar.gz",
        RELEASE_TREE,
        hard_links={"hello-1.2.3/bin/hello-alias": "hello-1.2.3/bin/hello"},
    )
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--extract", "--strip-components", "1")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert set(bundle_members(out)) == {"bin/hello", "bin/hello-alias", "README.md"}
    assert bundle_text(out, "bin/hello-alias") == HELLO_BODY


def test_world_writable_zip_entry_is_not_published(ocx: OcxRunner, tmp_path: Path):
    """A `0o777` zip entry reaches the bundle without group/other write, and
    keeps its executable bit.

    Two independent guards defend this, so removing either one alone leaves it
    green: the extractor masks group/other write (`archive/zip.rs`), and
    `BundleBuilder` writes tar headers in `HeaderMode::Deterministic`, which
    normalises every member to 0o755 or 0o644 on the way in. Measured - with
    the mask reverted this test still passed, and only
    `archive/zip.rs::extract_masks_group_and_other_write_bits` went red.

    What it does discriminate is an over-broad mask: strip user-execute and the
    deterministic header derives 0o644 from it, so `bin/hello` stops being
    runnable. Keep it for that, and for the day `HeaderMode` changes."""
    archive = _archive(
        tmp_path,
        ".zip",
        {
            "hello-1.2.3/bin/hello": (HELLO_BODY, 0o777),
            "hello-1.2.3/README.md": (README_BODY, 0o666),
        },
    )
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--extract", "--strip-components", "1")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _mode(out, "bin/hello") == 0o755
    assert _mode(out, "README.md") == 0o644


def test_strip_deeper_than_the_archive_is_refused(ocx: OcxRunner, tmp_path: Path):
    """Every entry emptied by the strip means no content tree at all —
    refused (exit 65) instead of bundling a package that installs no files."""
    archive = _archive(tmp_path, ".tar.gz", RELEASE_TREE)
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--strip-components", "5", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert "hello.tar.gz" in result.stderr, result.stderr
    assert "--strip-components 5" in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"


def test_empty_archive_without_strip_names_no_strip_clause(ocx: OcxRunner, tmp_path: Path):
    """An archive that holds nothing is refused (exit 65) even without a strip.
    The message must not invent a `--strip-components 0` the operator never
    typed (W10): the strip clause appears only for a non-zero strip."""
    archive = build_archive(tmp_path / "empty.tar.gz", {})
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out, "--extract", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert "empty.tar.gz" in result.stderr, result.stderr
    assert "--strip-components" not in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"


def test_extract_on_a_directory_is_a_usage_error(ocx: OcxRunner, tmp_path: Path):
    """`--extract` treats PATH as an archive; a directory has no format to
    unpack. Refused as a usage error (exit 64) that names the flag, rather than
    failing deep in the extractor with 'Is a directory'."""
    directory = tmp_path / "a-directory"
    directory.mkdir()
    out = tmp_path / "bundle.tar.xz"

    result = ocx.plain("package", "create", "-o", str(out), "--extract", str(directory), check=False)

    assert result.returncode == EXIT_USAGE_ERR, result.stdout + result.stderr
    assert "--extract" in result.stderr, result.stderr
    assert not out.exists(), "a refused invocation must leave no bundle behind"


def test_unsupported_suffix_under_extract_is_refused(ocx: OcxRunner, tmp_path: Path):
    """An input whose suffix names no archive format is refused (exit 65) up
    front, not attempted as a plain tar that fails deep in the extractor."""
    thing = tmp_path / "thing.rar"
    thing.write_bytes(b"not an archive")
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, thing, out, "--extract", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert "unsupported archive format" in result.stderr, result.stderr
    assert not out.exists(), "a refused invocation must leave no bundle behind"


def test_bzip2_output_is_refused(ocx: OcxRunner, tmp_path: Path):
    """bzip2 goes one way: `--extract` reads `.tar.bz2`, but `-o *.tar.bz2`
    is refused (exit 65) rather than writing a bundle no `ocx package push`
    could publish — no OCI layer media type spells bzip2.

    The refusal names bzip2 and the direction. Asserting only the exit code
    would not discriminate: before bzip2 was recognised at all, this same
    invocation already exited 65, with `unsupported archive format` naming the
    internal `._tmp_` path instead of the operator's format."""
    tree = tmp_path / "tree"
    (tree / "bin").mkdir(parents=True)
    (tree / "bin" / "hello").write_text(HELLO_BODY)
    out = tmp_path / "bundle.tar.bz2"

    result = _create(ocx, tree, out, check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert "bzip2 archives can be extracted but not written" in result.stderr, result.stderr
    assert not out.exists(), "a refused invocation must leave no bundle behind"


# ---------------------------------------------------------------------------
# Archive input and directory input compile to the same artifacts
# ---------------------------------------------------------------------------


def _write_directory_tree(root: Path) -> Path:
    """Write `RELEASE_TREE` minus its leading component as a real directory."""
    for arcname, (text, mode) in RELEASE_TREE.items():
        target = root / Path(arcname).relative_to("hello-1.2.3")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        target.chmod(mode)
    return root


def test_receipt_and_metadata_match_the_directory_input(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """The compiled sidecar and the build receipt are byte-identical whether
    the same tree arrived as a directory or inside an archive — the whole
    claim that `--extract` only swaps the content root."""
    metadata = tmp_path / "metadata.json"
    metadata.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": "${installPath}/bin",
                        "visibility": "public",
                    }
                ],
            }
        )
    )

    from_dir = tmp_path / "from-dir"
    from_archive = tmp_path / "from-archive"
    from_dir.mkdir()
    from_archive.mkdir()

    directory = _write_directory_tree(tmp_path / "tree")
    archive = _archive(tmp_path, ".tar.gz", RELEASE_TREE)

    shared = (
        "-m",
        str(metadata),
        "-p",
        current_platform(),
        "-i",
        f"{unique_repo}:1.2.3",
    )
    directory_result = _create(ocx, directory, from_dir / "bundle.tar.xz", *shared)
    archive_result = _create(
        ocx,
        archive,
        from_archive / "bundle.tar.xz",
        *shared,
        "--extract",
        "--strip-components",
        "1",
    )

    assert directory_result.returncode == EXIT_SUCCESS, directory_result.stderr
    assert archive_result.returncode == EXIT_SUCCESS, archive_result.stderr

    directory_bundle = from_dir / "bundle.tar.xz"
    archive_bundle = from_archive / "bundle.tar.xz"

    assert (
        resolved_metadata_path(archive_bundle).read_bytes()
        == resolved_metadata_path(directory_bundle).read_bytes()
    )
    assert (
        resolved_receipt_path(archive_bundle).read_bytes()
        == resolved_receipt_path(directory_bundle).read_bytes()
    )
    # The scan ran against the extracted tree, not against the archive file.
    assert json.loads(resolved_metadata_path(archive_bundle).read_text())["binaries"] == ["hello"]

    def entries(bundle: Path) -> dict[str, tuple[str, int]]:
        return {
            name: (bundle_text(bundle, name), member.mode & 0o777)
            for name, member in bundle_members(bundle).items()
        }

    assert entries(archive_bundle) == entries(directory_bundle)

