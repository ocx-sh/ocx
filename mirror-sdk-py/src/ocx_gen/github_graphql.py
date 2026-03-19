# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""GitHub GraphQL API client for repos with large releases.

The REST releases endpoint returns 504 on repos with many assets per release
(e.g., python-build-standalone has ~735 assets per release). This module uses
the GraphQL API to fetch only the fields we need and paginates assets within
each release.

Each release's assets are cached individually with a 7-day TTL since assets
are immutable once published. The release list itself uses a 1-hour TTL.
"""

import logging

import httpx

from ocx_gen.cache import FileCache
from ocx_gen.github_types import Release, fetch_and_filter, get_token

log = logging.getLogger(__name__)

_releases_cache = FileCache("github-graphql")
_assets_cache = FileCache("github-graphql-assets", max_age=7 * 86400)

_RELEASES_QUERY = """
query($owner: String!, $repo: String!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    releases(first: 20, after: $cursor, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        tagName
        isDraft
        isPrerelease
        releaseAssets(first: 100) {
          pageInfo { hasNextPage endCursor }
          nodes { name downloadUrl }
        }
      }
    }
  }
}
"""

_ASSETS_PAGE_QUERY = """
query($owner: String!, $repo: String!, $tag: String!, $cursor: String!) {
  repository(owner: $owner, name: $repo) {
    release(tagName: $tag) {
      releaseAssets(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { name downloadUrl }
      }
    }
  }
}
"""


def _graphql(client: httpx.Client, headers: dict, query: str, variables: dict) -> dict:
    resp = client.post(
        "https://api.github.com/graphql",
        headers=headers,
        json={"query": query, "variables": variables},
    )
    resp.raise_for_status()
    data = resp.json()
    if "errors" in data:
        raise RuntimeError(f"GraphQL errors: {data['errors']}")
    return data["data"]


def _fetch_all_assets(
    client: httpx.Client, headers: dict, owner: str, repo: str, tag: str, first_page: dict
) -> list[dict]:
    """Fetch all assets for a release, with per-release caching."""
    cache_key = f"{owner}/{repo}/{tag}"
    cached = _assets_cache.get_json(cache_key)
    if cached is not None:
        log.debug("  %s: %d assets (cached)", tag, len(cached))
        return cached

    assets = list(first_page["nodes"])
    page_info = first_page["pageInfo"]

    while page_info["hasNextPage"]:
        log.debug("  %s: fetching more assets (%d so far)", tag, len(assets))
        data = _graphql(client, headers, _ASSETS_PAGE_QUERY, {
            "owner": owner, "repo": repo, "tag": tag, "cursor": page_info["endCursor"],
        })
        page = data["repository"]["release"]["releaseAssets"]
        assets.extend(page["nodes"])
        page_info = page["pageInfo"]

    log.debug("  %s: %d total assets", tag, len(assets))
    _assets_cache.put_json(cache_key, assets)
    return assets


def list_releases(
    owner: str,
    repo: str,
    *,
    include_prereleases: bool = True,
    include_drafts: bool = True,
    cache: FileCache | None = None,
) -> list[Release]:
    """Return releases for *owner/repo* via the GitHub GraphQL API.

    Same interface as :func:`ocx_gen.github.list_releases` but uses GraphQL
    to avoid 504 errors on repos with large asset counts.

    Requires ``GITHUB_TOKEN`` (GraphQL API does not support anonymous access).
    """
    token = get_token()
    if not token:
        raise ValueError("GITHUB_TOKEN is required for the GraphQL API (anonymous access is not supported)")

    effective_cache = cache or _releases_cache
    headers = {"Authorization": f"Bearer {token}"}

    def fetch() -> list[dict]:
        all_releases: list[dict] = []
        cursor = None
        max_pages = 20

        with httpx.Client(timeout=30.0) as client:
            for page_num in range(max_pages):
                log.info("fetching releases page %d for %s/%s (%d so far)", page_num + 1, owner, repo, len(all_releases))
                data = _graphql(client, headers, _RELEASES_QUERY, {
                    "owner": owner, "repo": repo, "cursor": cursor,
                })
                releases_data = data["repository"]["releases"]
                nodes = releases_data["nodes"]

                if not nodes:
                    break

                for node in nodes:
                    tag = node["tagName"]
                    assets_raw = _fetch_all_assets(
                        client, headers, owner, repo, tag, node["releaseAssets"]
                    )
                    all_releases.append({
                        "tag_name": tag,
                        "body": "",
                        "prerelease": node["isPrerelease"],
                        "draft": node["isDraft"],
                        "assets": [
                            {"name": a["name"], "browser_download_url": a["downloadUrl"]}
                            for a in assets_raw
                        ],
                    })

                page_info = releases_data["pageInfo"]
                if not page_info["hasNextPage"]:
                    break
                cursor = page_info["endCursor"]
            else:
                log.warning("reached page limit (%d pages, %d releases) for %s/%s — results may be truncated",
                            max_pages, len(all_releases), owner, repo)

        return all_releases

    return fetch_and_filter(
        owner, repo, effective_cache, fetch,
        include_prereleases=include_prereleases, include_drafts=include_drafts,
    )
