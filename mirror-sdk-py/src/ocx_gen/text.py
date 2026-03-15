# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Text utilities for generator scripts."""

import re

# Matches URLs starting with http:// or https://, stopping at whitespace or
# common markdown/html delimiters.
_URL_RE = re.compile(r"https?://[^\s)<>\"\]]+")


def extract_urls(text: str, *, pattern: str | None = None) -> list[str]:
    """Extract URLs from *text*, optionally filtering by *pattern*.

    Returns a deduplicated list of URLs in the order they first appear.
    If *pattern* is given, only URLs matching ``re.search(pattern, url)``
    are included.
    """
    seen: set[str] = set()
    urls: list[str] = []
    for match in _URL_RE.finditer(text):
        url = match.group(0).rstrip(".,;:")
        if url in seen:
            continue
        if pattern and not re.search(pattern, url):
            continue
        seen.add(url)
        urls.append(url)
    return urls
