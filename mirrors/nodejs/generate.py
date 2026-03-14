# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen", "httpx"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Generate url_index JSON for Node.js releases."""

from ocx_gen import IndexBuilder
from ocx_gen.http import fetch_json

DIST_URL = "https://nodejs.org/dist"
INDEX_URL = f"{DIST_URL}/index.json"

# Node's "files" array identifier → (filename platform, archive extension)
# The files array uses "osx-*-tar" / "win-*-zip", but download filenames
# use "darwin-*" / "win-*".
PLATFORMS = {
    "linux-x64": ("linux-x64", ".tar.xz"),
    "linux-arm64": ("linux-arm64", ".tar.xz"),
    "osx-x64-tar": ("darwin-x64", ".tar.gz"),
    "osx-arm64-tar": ("darwin-arm64", ".tar.gz"),
    "win-x64-zip": ("win-x64", ".zip"),
    "win-arm64-zip": ("win-arm64", ".zip"),
}


def main():
    releases = fetch_json(INDEX_URL)
    index = IndexBuilder()

    for release in releases:
        version = release["version"].lstrip("v")
        files = set(release.get("files", []))

        assets: dict[str, str] = {}
        for file_key, (platform, ext) in PLATFORMS.items():
            if file_key in files:
                filename = f"node-{release['version']}-{platform}{ext}"
                assets[filename] = f"{DIST_URL}/{release['version']}/{filename}"

        if assets:
            # Node.js: lts is False for non-LTS, or a codename string for LTS
            is_prerelease = release.get("lts") is False
            index.add_version(version, assets=assets, prerelease=is_prerelease)

    index.emit()


if __name__ == "__main__":
    main()
