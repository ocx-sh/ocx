# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Reference-shape gate: a `doc_type: reference` page is entries, not essays.

A reference page is an optional intro of at most two sentences, then entries. An entry is a heading
with an explicit `{#kebab-id}`, a description of at most two sentences, tables or `key: value`
lists, at most one fenced block and at most one "See" link line. `scan` reports `page:line rule`
for each breach; only pages declaring `doc_type: reference` are read.

Rules: `paragraph` (more than two sentences), `aside` (`:::` or `<Aside`), `fences` (a second
fenced block under one heading), `anchor` (a heading without `{#id}`).
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "reference_shape"

MAX_PARAGRAPH_SENTENCES = 2
MAX_FENCES_PER_ENTRY = 1

_DECLARATION = re.compile(r"^\s*(?:<!--|\{/\*)\s*doc_type\s*:\s*reference\b")
_DECLARATION_LINES = 12
_FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
_HEADING = re.compile(r"^\s{0,3}#{1,6}\s")
_ANCHOR = re.compile(r"\\?\{#[a-z0-9][a-z0-9_-]*\\?\}\s*$")
_ASIDE = re.compile(r"^\s*(?::::*\s*\w|<Aside\b)")
# Lines that open a table, list item or other non-prose block.
_NOT_PROSE = re.compile(r"^\s*(?:\||[-*+]\s|\d+[.)]\s|<|:::|#)")
_ABBREVIATION = re.compile(r"\b(?:e\.g|i\.e|etc|vs)\.", re.IGNORECASE)
_CODE_SPAN = re.compile(r"`[^`]*`")
_LINK = re.compile(r"\[([^\]]*)\]\([^)]*\)")
_TERMINATOR = re.compile(r"[.!?]+(?=\s|$)")


def reference_pages(root: Path) -> list[Path]:
    """Every markdown file under `root` that declares `doc_type: reference`, sorted."""
    return sorted(
        p
        for p in root.rglob("*")
        if p.is_file() and p.suffix in (".md", ".mdx") and any(_DECLARATION.match(line) for line in _head(p))
    )


def scan(root: Path) -> list[str]:
    """`page:line rule` for each breach on a reference page, `page` relative to `root`, sorted by page then line."""
    findings: list[tuple[str, int, str]] = []
    for page in reference_pages(root):
        name = page.relative_to(root).as_posix()
        findings += [(name, line, rule) for line, rule in _scan_page(page.read_text(encoding="utf-8"))]
    return [f"{name}:{line} {rule}" for name, line, rule in sorted(findings)]


def _head(page: Path) -> list[str]:
    lines = page.read_text(encoding="utf-8", errors="replace").splitlines()
    if lines and lines[0].strip() == "---":
        end = next((i for i in range(1, len(lines)) if lines[i].strip() == "---"), 0)
        lines = lines[end + 1 :]
    return lines[:_DECLARATION_LINES]


def _sentences(paragraph: str) -> int:
    text = _LINK.sub(r"\1", _CODE_SPAN.sub("x", _ABBREVIATION.sub("x", paragraph)))
    tail = _TERMINATOR.split(text)[-1]
    return len(_TERMINATOR.findall(text)) + (1 if re.search(r"\w", tail) else 0)


def _scan_page(text: str) -> list[tuple[int, str]]:
    lines = text.splitlines()
    start = 0
    if lines and lines[0].strip() == "---":
        start = next((i + 1 for i in range(1, len(lines)) if lines[i].strip() == "---"), 0)
    findings: list[tuple[int, str]] = []
    fence: str | None = None
    fences_in_entry = 0
    paragraph: list[str] = []
    paragraph_start = 0

    def flush() -> None:
        if paragraph and _sentences(" ".join(paragraph)) > MAX_PARAGRAPH_SENTENCES:
            findings.append((paragraph_start, "paragraph"))
        paragraph.clear()

    for number, line in enumerate(lines[start:], start=start + 1):
        if fence is not None:
            if line.strip().startswith(fence) and not line.strip().strip(fence[0]):
                fence = None
            continue
        opener = _FENCE.match(line)
        if opener:
            flush()
            fence = opener.group(1)
            fences_in_entry += 1
            if fences_in_entry > MAX_FENCES_PER_ENTRY:
                findings.append((number, "fences"))
            continue
        if _HEADING.match(line):
            flush()
            fences_in_entry = 0
            if not _ANCHOR.search(line):
                findings.append((number, "anchor"))
            continue
        if _ASIDE.match(line):
            flush()
            findings.append((number, "aside"))
            continue
        if not line.strip() or _NOT_PROSE.match(line):
            flush()
            continue
        if not paragraph:
            paragraph_start = number
        paragraph.append(line.strip())
    flush()
    return findings


def test_planted_page_reports_each_breach_with_its_line() -> None:
    assert scan(FIXTURES / "planted") == [
        "planted.md:14 paragraph",
        "planted.md:20 aside",
        "planted.md:26 aside",
        "planted.md:34 fences",
        "planted.md:38 anchor",
    ]


def test_clean_page_reports_nothing_and_was_read() -> None:
    root = FIXTURES / "clean"
    assert [p.name for p in reference_pages(root)] == ["clean.md"], "the scanner read nothing"
    assert scan(root) == []


def test_a_page_of_another_type_is_not_read() -> None:
    root = FIXTURES / "skipped"
    assert reference_pages(root) == []
    assert scan(root) == []


def test_sentences_inside_a_fence_are_not_prose() -> None:
    # clean.md's second entry holds four sentences, all inside a fenced block.
    assert not any("clean.md:" in finding for finding in scan(FIXTURES / "clean"))
