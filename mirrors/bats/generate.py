# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen", "httpx"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Generate url_index JSON for bats-core releases.

bats-core releases do not upload binary assets — bats is pure shell. We
publish the GitHub source archive for each release tag under one synthetic
filename per OCX platform (all five point at the same source tarball).
"""

import re

from ocx_gen import IndexBuilder, list_releases

OWNER = "bats-core"
REPO = "bats-core"
ARCHIVE_URL = f"https://github.com/{OWNER}/{REPO}/archive/refs/tags"

PLATFORMS = ["linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64", "windows-amd64"]

TAG_RE = re.compile(r"^v(?P<version>\d+\.\d+\.\d+)$")


def main():
    index = IndexBuilder()
    for release in list_releases(OWNER, REPO, include_prereleases=False, include_drafts=False):
        m = TAG_RE.match(release.tag_name)
        if not m:
            continue
        version = m.group("version")
        url = f"{ARCHIVE_URL}/{release.tag_name}.tar.gz"
        assets = {f"bats-core-{version}-{plat}.tar.gz": url for plat in PLATFORMS}
        index.add_version(version, assets=assets, prerelease=release.prerelease)

    index.emit()


if __name__ == "__main__":
    main()
