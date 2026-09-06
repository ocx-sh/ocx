#!/usr/bin/env python3
"""Strip markdown down to prose, and share the CLI the other checks reuse.

Covers DOC-PLAIN-04: every readability number and every word count in this
rule set is computed on stripped prose, never raw file text. A code fence, a
table row or a link target counted as prose produces a number that means
nothing (wave-2 measured a 10.8-point Flesch gap, and 121 of 4,674 flagged
sentences that passed once link targets were gone). It reports one finding of
its own, DOC-PLAIN-04: an unclosed fence, which corrupts every later count.

Public API for the other checks: strip(text), iter_sentences(prose),
iter_paragraphs(prose), declaration(text) and run_cli(), the shared front end.

Usage: strip_prose.py [--root DIR] [PATH ...] [--format text|json] [--self-test]
Exit 0 clean, 1 findings, 2 usage or missing input. Stripped prose goes to
stdout so a rule row can pipe it into grep. Findings go to stderr.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import NamedTuple

HERE = Path(__file__).resolve().parent
DOC_SUFFIXES = (".md", ".mdx", ".markdown")

# DOC-TYPE-31 and DOC-PLAIN-23: never walk agent notes or build output.
SKIP_DIRS = {
    ".git",
    ".agents",
    ".claude",
    ".serena",
    ".worktrees",
    "node_modules",
    "dist",
    "target",
    "build",
    "site",
    "book",
    ".venv",
    "venv",
    "__pycache__",
}


class Finding(NamedTuple):
    path: str
    line: int
    rule: str
    message: str


FRONT_MATTER = re.compile(r"\A---[ \t]*\r?\n.*?\r?\n---[ \t]*(?:\r?\n|\Z)", re.DOTALL)
FENCE = re.compile(r"^(\s*)(`{3,}|~{3,})")
DECL = re.compile(r"^\s*(?:<!--|\{/\*|\.\.|%)\s*doc_(type|tier)\s*:\s*([\w-]+)")
HEADING = re.compile(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
# A separator row must carry a pipe. Without that, a MkDocs grid card's own
# `---` divider read as a table row and reported 11 table rows on one real
# landing page that has no table at all.
TABLE_ROW = re.compile(r"^\s*\|.*\|\s*$|^[\s:|-]*\|[\s:|-]*$")
REF_DEF = re.compile(r"^\s*\[[^\]]+\]:\s*\S+")
THEMATIC = re.compile(r"^\s{0,3}([-*_])(?:\s*\1){2,}\s*$|^\s{0,3}={3,}\s*$")
INCLUDE_LINE = re.compile(r"^\s*(<<<|--8<--)")
INCLUDE_INLINE = re.compile(r"\{\{#include[^}]*\}\}")
ADMONITION = re.compile(r"^\s*(!!!|\?\?\?\+?|:::+|>\s*\[!\w+\]|</?Aside\b[^>]*>)\s*.*$")
CODE_SPAN = re.compile(r"(`+)(?:(?!\1).)*?\1", re.DOTALL)
IMAGE = re.compile(r"!\[[^\]]*\]\([^)]*\)")
INLINE_LINK = re.compile(r"\[([^\]]*)\]\([^)\s]*(?:\s+[^)]*)?\)")
REF_LINK = re.compile(r"\[([^\]]+)\]\[[^\]]*\]")
AUTOLINK = re.compile(r"<(?:https?|mailto):[^>\s]*>")
HTML_TAG = re.compile(r"</?[A-Za-z][^<>]*>")
# Paired emphasis is markup. Left in, a bold run-in sentence ending `.**`
# hid its own full stop and merged two sentences into one over-long one.
# Single `_` is left alone, because it is part of many identifiers.
EMPHASIS = re.compile(r"\*\*(.+?)\*\*|__(.+?)__|\*(.+?)\*")
# A raw <style> or <script> block is not prose: one page's CSS reported
# 127 semicolons as banned punctuation until this landed.
HTML_BLOCK_OPEN = re.compile(r"<(style|script)\b", re.IGNORECASE)
HTML_BLOCK_CLOSE = re.compile(r"</(style|script)>", re.IGNORECASE)
WORD = re.compile(r"[A-Za-z']+")
# Wave-2 calibration: a bare list marker split as its own sentence turned a
# compliant 3-item list into 6 sentences on 16 of 153 flagged paragraphs.
LIST_MARKER = re.compile(r"^\d{1,2}[.)]$")
# A code span leaves the sentinel behind, so a sentence opening with an
# identifier still reads as a new sentence. A bare space merged the two.
CODE_SENTINEL = "~"
SENT_SPLIT = re.compile(r"(?<=[.!?])\s+(?=[A-Z0-9\"'(\[~])")
# A bullet is its own unit, not a sentence inside the paragraph above it.
# Joined, a 6-item list read as a 6-sentence paragraph.
LIST_START = re.compile(r"^\s*(?:[-*+]|\d{1,2}[.)])\s")
# DOC-PLAIN-02 false positive: a Contents/breadcrumb line, once its links
# are read as plain text, is one long run of words that flags as a run-on
# sentence. It is navigation, not prose, once nothing sits between its link
# texts but a leading label and the separators "· | , -".
NAV_LABEL = re.compile(r"^\s*[A-Za-z][\w /]*:\s*")
NAV_GAP = re.compile(r"[\s·|,\-]*(?:\0[\s·|,\-]*)+")
NAV_SENTENCE_PUNCT = re.compile(r"[.!?;]")


def _is_nav_line(line: str) -> bool:
    """True for a line of two or more link texts joined only by separators."""
    body = NAV_LABEL.sub("", line, count=1)
    texts = INLINE_LINK.findall(body)
    if len(texts) < 2:
        return False
    skeleton = INLINE_LINK.sub("\0", body)
    if not NAV_GAP.fullmatch(skeleton):
        return False
    return not NAV_SENTENCE_PUNCT.search(" ".join(texts))


def strip(text: str) -> str:
    """Return prose only, one output line per input line."""
    m = FRONT_MATTER.match(text)
    if m:
        text = "\n" * m.group(0).count("\n") + text[m.end() :]
    out: list[str] = []
    fence: str | None = None
    html_block = False
    for line in text.split("\n"):
        if html_block:
            out.append("")
            html_block = not HTML_BLOCK_CLOSE.search(line)
            continue
        if HTML_BLOCK_OPEN.search(line) and not HTML_BLOCK_CLOSE.search(line):
            html_block = True
            out.append("")
            continue
        fm = FENCE.match(line)
        if fence is not None:
            out.append("")
            if fm and fm.group(2)[0] == fence[0] and len(fm.group(2)) >= len(fence):
                fence = None
            continue
        if fm:
            fence = fm.group(2)
            out.append("")
            continue
        if (
            DECL.match(line)
            or HEADING.match(line)
            or TABLE_ROW.match(line)
            or REF_DEF.match(line)
            or THEMATIC.match(line)
            or INCLUDE_LINE.match(line)
            or ADMONITION.match(line)
            or _is_nav_line(line)
        ):
            out.append("")
            continue
        out.append(inline(line))
    return "\n".join(out)


def inline(line: str) -> str:
    """Drop inline markup from one line, keeping its prose words."""
    line = CODE_SPAN.sub(CODE_SENTINEL, line)
    line = INCLUDE_INLINE.sub(" ", line)
    line = IMAGE.sub(" ", line)
    line = INLINE_LINK.sub(lambda m: m.group(1), line)
    line = REF_LINK.sub(lambda m: m.group(1), line)
    line = AUTOLINK.sub(" ", line)
    line = HTML_TAG.sub(" ", line)
    return EMPHASIS.sub(lambda m: next(g for g in m.groups() if g is not None), line)


def iter_paragraphs(prose: str, start_line: int = 1) -> Iterator[tuple[int, str]]:
    """Yield (first line number, joined text) for each blank-separated block."""
    block: list[str] = []
    first = start_line
    for offset, line in enumerate(prose.split("\n"), start=start_line):
        if line.strip():
            if block and LIST_START.match(line):
                yield first, " ".join(block)
                block = []
            if not block:
                first = offset
            block.append(line.strip())
        elif block:
            yield first, " ".join(block)
            block = []
    if block:
        yield first, " ".join(block)


def iter_sentences(prose: str, start_line: int = 1) -> Iterator[tuple[int, str]]:
    """Yield (line number, sentence) for every sentence in the prose."""
    for line_no, block in iter_paragraphs(prose, start_line):
        for raw_part in SENT_SPLIT.split(block):
            part = raw_part.strip()
            if part and not LIST_MARKER.match(part):
                yield line_no, part


def declaration(text: str, limit: int = 12) -> dict[str, str]:
    """Read doc_type and doc_tier from the file's first `limit` lines."""
    found: dict[str, str] = {}
    for line in text.split("\n")[:limit]:
        m = DECL.match(line)
        if m:
            found.setdefault("doc_" + m.group(1), m.group(2))
    return found


def word_count(prose: str) -> int:
    return len(WORD.findall(prose))


def read_text(path: str) -> str:
    with Path(path).open(encoding="utf-8", errors="replace") as fh:
        return fh.read()


def collect(paths: list[str], root: str) -> list[str]:
    """Expand PATHs, or walk --root, into a sorted list of markdown files."""
    found: list[str] = []
    for path in paths or [root]:
        if Path(path).is_file():
            found.append(path)
            continue
        if not Path(path).is_dir():
            print(f"strip_prose: no such path: {path}", file=sys.stderr)
            raise SystemExit(2)
        for dirpath, dirnames, filenames in os.walk(path):
            dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
            found += [str(Path(dirpath) / f) for f in filenames if f.endswith(DOC_SUFFIXES)]
    return sorted(set(found))


def emit(findings: list[Finding], fmt: str, stream=sys.stdout) -> None:
    if fmt == "json":
        json.dump([f._asdict() for f in findings], stream, indent=2)
        stream.write("\n")
        return
    for f in findings:
        print(f"{f.path}:{f.line}: {f.rule}: {f.message}", file=stream)


Check = Callable[[str, str, str], list[Finding]]


def self_test(check: Check, stem: str) -> int:
    """Run `check` over checks/fixtures/<stem>/ and prove it can go red."""
    directory = HERE / "fixtures" / stem
    fails = sorted(directory.glob("fail-*.md"))
    passes = sorted(directory.glob("pass-*.md"))
    if not fails or not passes:
        print(
            f"self-test: {directory} needs at least one fail-*.md and one "
            f"pass-*.md, found {len(fails)} and {len(passes)}"
        )
        return 1
    ok = True
    for path in fails:
        hits = check(str(path), read_text(str(path)), str(directory))
        ok &= bool(hits)
        print(
            f"{'ok  ' if hits else 'FAIL'} {path.name}: {len(hits)} findings, expected at least 1"
        )
        for hit in hits[:3]:
            print(f"       {hit.rule}: {hit.message}")
    for path in passes:
        hits = check(str(path), read_text(str(path)), str(directory))
        ok &= not hits
        print(f"{'ok  ' if not hits else 'FAIL'} {path.name}: {len(hits)} findings, expected 0")
        emit(hits, "text")
    print("self-test passed" if ok else "self-test failed")
    return 0 if ok else 1


def run_cli(
    description: str,
    check: Check,
    stem: str,
    prose_out: bool = False,
    extra_self_test: Callable[[], int] | None = None,
) -> int:
    """Shared front end. Every check in this directory takes the same flags."""
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--root", default=".", help="tree to walk when no PATH is given")
    parser.add_argument("paths", nargs="*", metavar="PATH")
    parser.add_argument("--format", choices=("text", "json"), default="text")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run over checks/fixtures/ and exit 1 on a broken rule",
    )
    args = parser.parse_args()
    if args.self_test:
        result = self_test(check, stem)
        if prose_out:
            result |= _residue_test()
        if extra_self_test is not None:
            result |= extra_self_test()
        return result
    files = collect(args.paths, args.root)
    if not files:
        print(f"{parser.prog}: no markdown files found", file=sys.stderr)
        return 2
    printing = prose_out and args.format == "text"
    findings: list[Finding] = []
    for path in files:
        text = read_text(path)
        findings += check(path, text, args.root)
        if printing:
            sys.stdout.write(strip(text) + "\n")
    emit(findings, args.format, sys.stderr if printing else sys.stdout)
    return 1 if findings else 0


def check_fences(path: str, text: str, root: str) -> list[Finding]:
    """DOC-PLAIN-04: an unclosed fence swallows the rest of the page."""
    fence: str | None = None
    opened = 0
    for line_no, line in enumerate(text.split("\n"), start=1):
        m = FENCE.match(line)
        if not m:
            continue
        if fence is None:
            fence, opened = m.group(2), line_no
        elif m.group(2)[0] == fence[0] and len(m.group(2)) >= len(fence):
            fence = None
    if fence is not None:
        return [
            Finding(
                path,
                opened,
                "DOC-PLAIN-04",
                "unclosed code fence, the stripped prose cannot be trusted",
            )
        ]
    return []


def _residue_test() -> int:
    """Prove strip() removes what it claims, not only that the CLI runs."""
    ok = True

    fixture = HERE / "fixtures" / "strip_prose" / "pass-clean.md"
    raw = read_text(str(fixture))
    prose = strip(raw)
    leaks = [marker for marker in ("```", "](", "|", "!!!", "<<<", "<div", "]:") if marker in prose]
    lines_kept = len(prose.split("\n")) == len(raw.split("\n"))
    if leaks or not lines_kept:
        print(
            f"residue-test FAILED: markup left in the prose {leaks}, "
            f"line count preserved: {lines_kept}"
        )
        ok = False
    else:
        print("residue-test ok: fixture strips clean and keeps its line count")

    # DOC-PLAIN-02's Contents-line exemption: prove the whole line drops to
    # blank, not only that no markup literally survives (a nav line whose
    # detection fails still has no "```" or "](" left once inline() runs).
    nav_fixture = HERE / "fixtures" / "strip_prose" / "pass-contents-line.md"
    raw = read_text(str(nav_fixture))
    prose = strip(raw)
    lines_kept = len(prose.split("\n")) == len(raw.split("\n"))
    dropped = prose.strip() == ""
    if not dropped or not lines_kept:
        print(
            f"residue-test FAILED: Contents line not dropped as navigation "
            f"(dropped: {dropped}), line count preserved: {lines_kept}"
        )
        ok = False
    else:
        print("residue-test ok: Contents line drops to blank and keeps its line count")

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(run_cli(__doc__ or "", check_fences, "strip_prose", prose_out=True))
