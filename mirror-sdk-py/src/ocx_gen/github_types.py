# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Shared types and helpers for GitHub API clients."""

import logging
import os
from collections.abc import Callable
from dataclasses import dataclass

from ocx_gen.cache import FileCache

log = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class Asset:
    """A downloadable file attached to a GitHub release."""

    name: str
    browser_download_url: str


@dataclass(frozen=True, slots=True)
class Release:
    """A GitHub release with its metadata and assets."""

    tag_name: str
    body: str
    prerelease: bool
    draft: bool
    assets: list[Asset]

    def to_dict(self) -> dict:
        return {
            "tag_name": self.tag_name,
            "body": self.body,
            "prerelease": self.prerelease,
            "draft": self.draft,
            "assets": [{"name": a.name, "browser_download_url": a.browser_download_url} for a in self.assets],
        }

    @classmethod
    def from_dict(cls, data: dict) -> "Release":
        return cls(
            tag_name=data["tag_name"],
            body=data.get("body", ""),
            prerelease=data["prerelease"],
            draft=data["draft"],
            assets=[Asset(name=a["name"], browser_download_url=a["browser_download_url"]) for a in data["assets"]],
        )


def get_token() -> str | None:
    """Return the GitHub token from the environment, or None."""
    return os.environ.get("GITHUB_TOKEN")


def filter_releases(
    releases: list[Release],
    *,
    include_prereleases: bool = True,
    include_drafts: bool = True,
) -> list[Release]:
    """Filter releases by prerelease/draft status."""
    if not include_prereleases:
        releases = [r for r in releases if not r.prerelease]
    if not include_drafts:
        releases = [r for r in releases if not r.draft]
    return releases


def fetch_and_filter(
    owner: str,
    repo: str,
    cache: FileCache,
    loader: Callable[[], list[dict]],
    *,
    include_prereleases: bool = True,
    include_drafts: bool = True,
) -> list[Release]:
    """Shared fetch-deserialize-filter-log pipeline for both API clients."""
    log.info("fetching releases for %s/%s", owner, repo)
    raw = cache.fetch_json(f"{owner}/{repo}/releases", loader)
    log.info("got %d releases for %s/%s", len(raw), owner, repo)
    releases = [Release.from_dict(r) for r in raw]

    releases = filter_releases(releases, include_prereleases=include_prereleases, include_drafts=include_drafts)
    if not include_prereleases or not include_drafts:
        log.info("after filtering: %d releases", len(releases))

    return releases
