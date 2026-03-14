# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Retry-aware HTTP client for generator scripts."""

import httpx

_CLIENT: httpx.Client | None = None


def _get_client() -> httpx.Client:
    global _CLIENT
    if _CLIENT is None:
        _CLIENT = httpx.Client(
            timeout=30.0,
            follow_redirects=True,
            transport=httpx.HTTPTransport(retries=3),
        )
    return _CLIENT


def fetch_json(url: str) -> object:
    """Fetch a URL and parse the response as JSON."""
    response = _get_client().get(url)
    response.raise_for_status()
    return response.json()


def fetch_text(url: str) -> str:
    """Fetch a URL and return the response body as text."""
    response = _get_client().get(url)
    response.raise_for_status()
    return response.text
