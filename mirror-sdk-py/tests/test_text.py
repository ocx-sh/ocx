# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

from ocx_gen.text import extract_urls


def test_extract_urls_basic():
    text = "Download from https://example.com/tool.tar.gz here."
    assert extract_urls(text) == ["https://example.com/tool.tar.gz"]


def test_extract_urls_multiple():
    text = "Get https://a.com/one and https://b.com/two today."
    assert extract_urls(text) == ["https://a.com/one", "https://b.com/two"]


def test_extract_urls_dedup():
    text = "Link: https://a.com/x and again https://a.com/x here."
    assert extract_urls(text) == ["https://a.com/x"]


def test_extract_urls_with_pattern():
    text = """
    https://corretto.aws/downloads/resources/21.0.10.7.1/amazon-corretto-21.0.10.7.1-linux-x64.tar.gz
    https://example.com/unrelated.tar.gz
    https://corretto.aws/downloads/resources/21.0.10.7.1/amazon-corretto-21.0.10.7.1-linux-aarch64.tar.gz
    """
    result = extract_urls(text, pattern=r"corretto\.aws/downloads")
    assert len(result) == 2
    assert all("corretto.aws" in u for u in result)


def test_extract_urls_no_match():
    text = "No URLs here at all."
    assert extract_urls(text) == []


def test_extract_urls_pattern_filters_all():
    text = "See https://example.com/thing for details."
    assert extract_urls(text, pattern=r"corretto\.aws") == []


def test_extract_urls_markdown_brackets():
    text = "[download](https://example.com/file.tar.gz) is available."
    result = extract_urls(text)
    assert result == ["https://example.com/file.tar.gz"]


def test_extract_urls_preserves_order():
    text = "https://c.com/3 https://a.com/1 https://b.com/2"
    assert extract_urls(text) == ["https://c.com/3", "https://a.com/1", "https://b.com/2"]


def test_extract_urls_strips_trailing_period():
    text = "See https://example.com/file.tar.gz."
    assert extract_urls(text) == ["https://example.com/file.tar.gz"]


def test_extract_urls_strips_trailing_comma():
    text = "url: https://example.com/x, and more"
    assert extract_urls(text) == ["https://example.com/x"]


def test_extract_urls_strips_trailing_semicolon():
    text = "visit https://example.com/page;"
    assert extract_urls(text) == ["https://example.com/page"]
