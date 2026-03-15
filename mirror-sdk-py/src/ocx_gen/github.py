# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""GitHub API client for generator scripts.

Thin wrapper around ``github3.py`` providing paginated release listing
with automatic authentication via the ``GITHUB_TOKEN`` environment variable.

Responses are cached to disk (default: ``~/.cache/ocx-gen/github/``, 1 hour
TTL) so repeated runs within the same window avoid API rate limits.
"""

import logging
import os
from dataclasses import dataclass

import github3

from ocx_gen.cache import FileCache

log = logging.getLogger(__name__)

_cache = FileCache("github")


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
            body=data["body"],
            prerelease=data["prerelease"],
            draft=data["draft"],
            assets=[Asset(name=a["name"], browser_download_url=a["browser_download_url"]) for a in data["assets"]],
        )


def _login() -> github3.GitHub:
    """Create a GitHub client, authenticated if GITHUB_TOKEN is set."""
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        return github3.login(token=token)
    return github3.GitHub()


def list_releases(
    owner: str,
    repo: str,
    *,
    include_prereleases: bool = True,
    include_drafts: bool = True,
    cache: FileCache | None = None,
) -> list[Release]:
    """Return releases for *owner/repo* as :class:`Release` objects.

    Args:
        include_prereleases: If ``False``, pre-releases are excluded.
        include_drafts: If ``False``, draft releases are excluded.
        cache: Optional cache override.  Defaults to the module-level
            1-hour cache.  Pass ``FileCache("github", max_age=0)`` to
            disable caching entirely.

    Note: The GitHub REST API does not support server-side filtering of
    prereleases or drafts.  The full result set is cached and filtering
    is applied after cache retrieval, so different filter settings share
    the same cache entry and never cause extra API calls.

    Responses are cached for 1 hour under ``~/.cache/ocx-gen/github/``.

    Pagination is handled automatically by the underlying library.
    Authentication uses the ``GITHUB_TOKEN`` environment variable when set.
    """
    effective_cache = cache or _cache

    def fetch() -> list[dict]:
        gh = _login()
        repository = gh.repository(owner, repo)
        if repository is None:
            raise ValueError(f"Repository not found: {owner}/{repo}")

        results: list[dict] = []
        for release in repository.releases():
            assets = [
                {"name": asset.name, "browser_download_url": asset.browser_download_url}
                for asset in release.assets()
            ]
            results.append(
                {
                    "tag_name": release.tag_name,
                    "body": release.body or "",
                    "prerelease": release.prerelease,
                    "draft": release.draft,
                    "assets": assets,
                }
            )
        return results

    log.info("fetching releases for %s/%s", owner, repo)
    raw = effective_cache.fetch_json(f"{owner}/{repo}/releases", fetch)
    log.info("got %d releases for %s/%s", len(raw), owner, repo)
    releases = [Release.from_dict(r) for r in raw]

    if not include_prereleases:
        releases = [r for r in releases if not r.prerelease]
    if not include_drafts:
        releases = [r for r in releases if not r.draft]

    if not include_prereleases or not include_drafts:
        log.info("after filtering: %d releases", len(releases))

    return releases
