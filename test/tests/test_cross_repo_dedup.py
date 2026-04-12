"""Regression for ocx-sh/ocx#40 — cross-repo content-addressed dedup.

Two repositories on the same registry may point at byte-identical manifest
content (e.g. ``tools/cmake`` and ``mirrors/cmake``). The package store
shards by registry + digest only, so both installs share a single on-disk
directory. Before the fix, ``resolve.json`` stored the full pinned
identifier of *whichever installer wrote the directory first*, and later
commands (``find``, ``deps``) reported that stale repository name when
queried via the second installer's path.

The fix drops the redundant root identifier from ``resolve.json`` and
reconstructs it from the caller's query identifier instead.
"""

from __future__ import annotations

import json
import stat
import sys
from pathlib import Path
from uuid import uuid4

from src import OcxRunner, current_platform


def test_cross_repo_dedup_preserves_query_repository(
    ocx: OcxRunner, tmp_path: Path,
):
    tag = "3.28"
    plat = current_platform()
    marker = f"xrepo-{uuid4().hex[:8]}"
    # Unique repo prefixes so parallel test runs don't collide on the shared
    # registry; both repos must use byte-identical content+metadata so the
    # manifest digest is the same and the package store collapses the two
    # installs into one directory.
    uid = uuid4().hex[:8]
    tools_repo = f"tools_{uid}/cmake"
    mirrors_repo = f"mirrors_{uid}/cmake"

    # --- Build one bundle + metadata, push to two repositories -----------
    pkg_dir = tmp_path / "pkg"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    hello = bin_dir / "hello"
    if sys.platform == "win32":
        hello = hello.with_suffix(".bat")
        hello.write_text(f"@echo {marker}\n")
    else:
        hello.write_text(f"#!/bin/sh\necho {marker}\n")
        hello.chmod(hello.stat().st_mode | stat.S_IEXEC)

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
                    },
                ],
            }
        )
    )
    bundle = tmp_path / "bundle.tar.xz"
    ocx.plain("package", "create", "-m", str(metadata), "-o", str(bundle), str(pkg_dir))

    def push(repo: str) -> None:
        fq = f"{ocx.registry}/{repo}:{tag}"
        ocx.plain("package", "push", "-n", "-p", plat, "-m", str(metadata), fq, str(bundle))
        ocx.plain("index", "update", f"{repo}:{tag}")

    push(tools_repo)
    push(mirrors_repo)

    tools_short = f"{tools_repo}:{tag}"
    mirrors_short = f"{mirrors_repo}:{tag}"

    # --- First installer wins cross-repo dedup ---------------------------
    ocx.json("install", tools_short)
    # Second installer reuses the shared content-addressed package dir.
    ocx.json("install", mirrors_short)

    # --- find should report the repository the user queried --------------
    find_result = ocx.json("find", mirrors_short)
    assert mirrors_short in find_result, (
        f"find should key result by queried identifier, got keys {list(find_result)}"
    )

    # --- deps --flat entry for the root should carry the queried repo ---
    flat = ocx.json("deps", "--flat", mirrors_short)
    entries = flat["entries"]
    assert entries, f"expected at least one entry in deps --flat output, got {flat!r}"

    def entry_repo(ident: str) -> str:
        # String identifier of shape "{registry}/{repo}:{tag}@{digest}"
        head = ident.split("@", 1)[0]
        if ":" in head.rsplit("/", 1)[-1]:
            head = head.rsplit(":", 1)[0]
        return head.split("/", 1)[1] if "/" in head else head

    assert any(entry_repo(e["identifier"]) == mirrors_repo for e in entries), (
        f"deps --flat for {mirrors_short} should contain an entry with repo "
        f"{mirrors_repo!r}, got entries={entries!r}"
    )
