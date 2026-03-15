# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Generate url_index JSON for Amazon Corretto releases."""

import argparse
import logging
import re
import sys

from ocx_gen import IndexBuilder
from ocx_gen.github import list_releases
from ocx_gen.text import extract_urls

log = logging.getLogger("corretto")

# (jdk_major, github_owner, github_repo)
REPOS = [
    (8, "corretto", "corretto-8"),
    (11, "corretto", "corretto-11"),
    (17, "corretto", "corretto-17"),
    (21, "corretto", "corretto-21"),
    (25, "corretto", "corretto-25"),
]

# Filename patterns to exclude (non-JDK, installers, modules, alpine/musl)
EXCLUDE_RE = re.compile(r"\.(deb|rpm|pkg|msi)$|-jre-|-headful-|-jmods-|-alpine-|\.sig$|\.md5$|\.sha256$")

# Platform patterns: regex → platform key
PLATFORM_PATTERNS = [
    (re.compile(r"linux-x64\.tar\.gz$"), "linux/amd64"),
    (re.compile(r"linux-aarch64\.tar\.gz$"), "linux/arm64"),
    (re.compile(r"macosx-x64\.tar\.gz$"), "darwin/amd64"),
    (re.compile(r"macosx-aarch64\.tar\.gz$"), "darwin/arm64"),
    (re.compile(r"windows-x64-jdk\.zip$"), "windows/amd64"),
]


def corretto_to_ocx(tag: str, major: int) -> str | None:
    """Convert a Corretto tag to an OCX version string.

    All JDK versions are normalized to 5 parts (major.minor.update.jdkBuild.correttoRev)
    then mapped to ``major.minor.update_jdkBuild*1000+correttoRev``.

    JDK 8 tags have 4 parts (no minor) — minor is set to 0.

    Returns None if the tag doesn't match the expected format.
    """
    parts = tag.split(".")
    try:
        nums = [int(p) for p in parts]
    except ValueError:
        return None

    # JDK 8: 4 parts (major.update.jdkBuild.correttoRev) — insert minor=0
    if major == 8 and len(nums) == 4:
        nums = [nums[0], 0, nums[1], nums[2], nums[3]]

    if len(nums) != 5:
        return None

    maj, minor, update, jdk_build, corretto_rev = nums
    build = jdk_build * 1000 + corretto_rev
    return f"{maj}.{minor}.{update}_{build}"


def classify_url(url: str) -> str | None:
    """Return the platform key for a URL, or None if it should be skipped."""
    if EXCLUDE_RE.search(url):
        return None
    for pattern, platform in PLATFORM_PATTERNS:
        if pattern.search(url):
            return platform
    return None


def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(name)s: %(message)s",
        stream=sys.stderr,
    )

    parser = argparse.ArgumentParser(description="Generate url_index JSON for Corretto releases")
    parser.add_argument("--major", type=int, help="Only generate for this JDK major version")
    args = parser.parse_args()

    repos = REPOS
    if args.major is not None:
        repos = [(m, o, r) for m, o, r in REPOS if m == args.major]
        if not repos:
            log.error("no repository found for JDK major %d", args.major)
            sys.exit(1)

    index = IndexBuilder()

    for major, owner, repo in repos:
        log.info("fetching releases for %s/%s (JDK %d)", owner, repo, major)
        releases = list_releases(
            owner, repo, include_prereleases=False, include_drafts=False
        )
        log.info("  got %d releases", len(releases))

        for release in releases:
            ocx_version = corretto_to_ocx(release.tag_name, major)
            if ocx_version is None:
                log.debug("  skipping tag %s (could not parse version)", release.tag_name)
                continue

            urls = extract_urls(release.body, pattern=r"corretto\.aws/downloads")

            assets: dict[str, str] = {}
            for url in urls:
                if classify_url(url) is None:
                    continue
                filename = url.rsplit("/", 1)[-1]
                assets.setdefault(filename, url)

            log.info("  %s -> %s (%d assets)", release.tag_name, ocx_version, len(assets))

            index.add_version(
                ocx_version,
                assets=assets,
                prerelease=release.prerelease,
            )

    if len(index) == 0:
        log.error("no versions generated — check tag format or GitHub API response")
        sys.exit(1)

    log.info("done — %d versions total", len(index))
    index.emit()


if __name__ == "__main__":
    main()
