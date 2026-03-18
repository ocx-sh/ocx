# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen", "httpx"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Generate url_index JSON for JFrog CLI releases.

JFrog hosts raw binaries (not archives) at releases.jfrog.io.
Version discovery is done by scraping the HTML directory index.
"""

import re

from ocx_gen import IndexBuilder
from ocx_gen.http import fetch_text

BASE_URL = "https://releases.jfrog.io/artifactory/jfrog-cli/v2-jf"

# JFrog platform directory name → (asset filename, OCX-facing filename pattern)
# We use the "jf" binary name (current CLI name, not legacy "jfrog").
# macOS "mac-386" is actually amd64 (Intel) — JFrog's naming quirk.
PLATFORMS = {
    "jfrog-cli-linux-amd64": "jf",
    "jfrog-cli-linux-arm64": "jf",
    "jfrog-cli-mac-386": "jf",
    "jfrog-cli-mac-arm64": "jf",
    "jfrog-cli-windows-amd64": "jf.exe",
}


def main():
    html = fetch_text(BASE_URL + "/")
    versions = re.findall(r'href="(\d+\.\d+\.\d+)/"', html)

    index = IndexBuilder()

    for version in versions:
        assets: dict[str, str] = {}
        for platform_dir, binary_name in PLATFORMS.items():
            # Use a flat filename (no path separator) — the pipeline uses
            # the asset name as a filename in the work directory.
            filename = f"{platform_dir}-{binary_name}"
            url = f"{BASE_URL}/{version}/{platform_dir}/{binary_name}"
            assets[filename] = url

        if assets:
            index.add_version(version, assets=assets)

    index.emit()


if __name__ == "__main__":
    main()
