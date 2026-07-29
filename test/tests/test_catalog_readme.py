# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The catalog readme `ocx package describe --readme` publishes must render
outside this repository.

`describe` validates the *logo*'s bytes but never looks at the readme's links,
so a readme carrying a repo-relative reference publishes with exit 0 and a
broken image on the catalog page. A green describe run is not evidence — the
published root is, and that is not reachable from a test. This closes the gap
on the input side instead.

Scope is deliberately the reference *targets*, not markdown quality: an `<img
src="./assets/logo.svg">` or a `[license](LICENSE)` resolves against the GitHub
repository page and nowhere else. The logo already travels separately via
`--logo`, so an inline image reference is redundant even when it does resolve.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

# The readmes the publish workflows hand to `ocx package describe --readme`.
# One entry per describe call in `.github/workflows/oci-publish.yml`.
CATALOG_READMES = [Path(__file__).parents[2] / "packaging" / "ocx" / "CATALOG.md"]

# Markdown `![alt](target)` / `[text](target)`, HTML `src="target"` /
# `href="target"`, reference-style definitions (`[key]: target` — how the
# README's badge block is written), and autolinks (`<https://…>`). Autolinks
# are absolute by construction, so they never fail the assertion; they are
# matched so a file using only them does not read as "nothing to check".
_REFERENCES = re.compile(
    r"""!?\[[^\]]*\]\(\s*([^)\s]+)"""
    r"""|<[^>]+\s(?:src|href)\s*=\s*["']([^"']+)["']"""
    r"""|^\[[^\]]+\]:\s*(\S+)"""
    r"""|<([a-z][a-z0-9+.-]*:[^>\s]+)>""",
    re.MULTILINE | re.IGNORECASE,
)

# A target that resolves on its own: absolute URL, protocol-relative, anchor,
# or mailto. Anything else is relative to wherever the file happens to sit.
_RESOLVES = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|//|#)", re.IGNORECASE)


def _targets(text: str) -> list[str]:
    return [group for match in _REFERENCES.findall(text) for group in match if group]


@pytest.mark.parametrize("readme", CATALOG_READMES, ids=lambda path: path.name)
def test_catalog_readme_has_no_repo_relative_references(readme: Path) -> None:
    """Every link and image target in a published catalog readme resolves
    without the repository around it."""
    assert readme.is_file(), f"{readme} is handed to `describe --readme` but does not exist"
    text = readme.read_text(encoding="utf-8")

    targets = _targets(text)
    # Without this the assertion below passes vacuously on a readme whose
    # references the pattern failed to match at all.
    assert targets, f"{readme.name} carries no link or image references; the extractor matched nothing"

    relative = [target for target in targets if not _RESOLVES.match(target)]
    assert not relative, (
        f"{readme.name} is published to a catalog, where these resolve nowhere: {relative}. "
        f"Use an absolute URL; the logo travels separately via `describe --logo`."
    )
