# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Generate url_index JSON for python-build-standalone CPython releases."""

import argparse
import logging
import re
import sys

from ocx_gen import IndexBuilder
from ocx_gen.github_graphql import list_releases

log = logging.getLogger("cpython")

# Filename regex: cpython-{version}+{date}-{triple}-install_only[_stripped].tar.gz
# Only matches stable releases (no alpha/beta/rc).
# Group 1: X.Y.Z, Group 2: date, Group 3: triple
FILENAME_RE = re.compile(
    r"cpython-(\d+\.\d+\.\d+)\+(\d+)-(.+)-install_only(?:_stripped)?\.tar\.gz$"
)

PLATFORMS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
}


def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(name)s: %(message)s",
        stream=sys.stderr,
    )

    parser = argparse.ArgumentParser(description="Generate url_index JSON for CPython releases")
    parser.add_argument("--minor", required=True, help="Minor version to generate (e.g., 3.13)")
    args = parser.parse_args()

    minor_prefix = args.minor + "."

    releases = list_releases(
        "astral-sh", "python-build-standalone",
        include_prereleases=False, include_drafts=False,
    )

    index = IndexBuilder()
    seen_versions: set[str] = set()

    for release in releases:
        assets: dict[str, str] = {}
        version: str | None = None
        build_date: str | None = None

        for asset in release.assets:
            m = FILENAME_RE.match(asset.name)
            if m is None:
                continue

            py_version, date, triple = m.group(1), m.group(2), m.group(3)

            if not py_version.startswith(minor_prefix):
                continue
            if triple not in PLATFORMS:
                continue

            if version is None:
                version = py_version
                build_date = date

            assets[asset.name] = asset.browser_download_url

        if version is None:
            continue

        ocx_version = f"{version}+{build_date}"
        if ocx_version in seen_versions:
            continue
        seen_versions.add(ocx_version)

        log.info("  %s -> %s (%d assets)", release.tag_name, ocx_version, len(assets))
        index.add_version(ocx_version, assets=assets)

    if len(index) == 0:
        log.error("no versions generated for %s — check GitHub API response", args.minor)
        sys.exit(1)

    log.info("done — %d versions total", len(index))
    index.emit()


if __name__ == "__main__":
    main()
