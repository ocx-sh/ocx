# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""GitHub REST API client for generator scripts.

Thin wrapper around ``github3.py`` providing paginated release listing
with automatic authentication via the ``GITHUB_TOKEN`` environment variable.

Responses are cached to disk (default: ``~/.cache/ocx-gen/github/``, 1 hour
TTL) so repeated runs within the same window avoid API rate limits.
"""

import logging

import github3

from ocx_gen.cache import FileCache
from ocx_gen.github_types import Release, fetch_and_filter, get_token

log = logging.getLogger(__name__)

_cache = FileCache("github")


def _login() -> github3.GitHub:
    """Create a GitHub client, authenticated if GITHUB_TOKEN is set."""
    token = get_token()
    gh = github3.login(token=token) if token else github3.GitHub()
    gh.session.default_read_timeout = 30
    return gh


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

    return fetch_and_filter(
        owner, repo, effective_cache, fetch,
        include_prereleases=include_prereleases, include_drafts=include_drafts,
    )
