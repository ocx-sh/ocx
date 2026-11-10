#!/usr/bin/env python3
"""Fail when a built page is over the size budget.

Usage: check_budget.py <dist-docs-dir>

Budget per HTML page: at most 14,200 bytes gzipped and 800 DOM elements.
Exit 0 clean, 1 findings, 2 nothing to check.
"""

import gzip
import sys
from html.parser import HTMLParser
from pathlib import Path

MAX_GZIP_BYTES = 14_200
MAX_ELEMENTS = 800


class _Counter(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.elements = 0

    def handle_starttag(self, tag, attrs):  # noqa: ARG002
        self.elements += 1


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    root = Path(argv[1])
    pages = sorted(root.rglob("*.html"))
    if not pages:
        print(f"check_budget: no HTML pages under {root}", file=sys.stderr)
        return 2
    findings = []
    for page in pages:
        raw = page.read_bytes()
        size = len(gzip.compress(raw, compresslevel=9, mtime=0))
        counter = _Counter()
        counter.feed(raw.decode("utf-8", errors="replace"))
        rel = page.relative_to(root)
        if size > MAX_GZIP_BYTES:
            findings.append(f"{rel}: {size} B gzip > {MAX_GZIP_BYTES}")
        if counter.elements > MAX_ELEMENTS:
            findings.append(f"{rel}: {counter.elements} DOM elements > {MAX_ELEMENTS}")
    for line in findings:
        print(line)
    print(f"check_budget: {len(pages)} pages, {len(findings)} findings")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
