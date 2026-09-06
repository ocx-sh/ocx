#!/usr/bin/env python3
"""Page-level checks scoped by a page's declared type.

Covers DOC-TYPE-03 (type mixing), DOC-TYPE-04 (reference prose),
DOC-TYPE-07 (concept preamble length), DOC-TYPE-08 and DOC-TYPE-09
(troubleshooting entry shape and placement), DOC-TYPE-22 (how-to goal
sentence), DOC-TYPE-32 and DOC-TYPE-33 (README shape), DOC-TYPE-37 and
DOC-TYPE-41 (changelog shape), DOC-DISC-16 (words before a first-steps page's
first command) and DOC-DISC-17 (branching on a tutorial).

A page with no doc_type declaration is skipped and counted, never guessed
from its path. doc_declaration.py owns the missing-declaration finding.

Usage: page_type.py [--root DIR] [PATH ...] [--format text|json] [--self-test]
Exit 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import strip_prose as sp
from strip_prose import Finding

# DOC-TYPE-07: 150 words. Measured, 12 of 13 real how-to preambles pass at
# 150 and only 9 of 13 pass at 100.
PREAMBLE_WORD_CAP = 150
# DOC-DISC-16: 100 words, shared with DOC-TYPE-07's family and unsourced.
FIRST_COMMAND_WORD_CAP = 100
# DOC-TYPE-09: GitLab states the five-entry split trigger.
TROUBLESHOOTING_SPLIT = 4
# DOC-TYPE-03 landing arm, both argued from the fetched exemplars.
LANDING_ORDERED_ITEMS = 2
LANDING_TABLE_ROWS = 3

TUTORIAL_VOICE = re.compile(
    r"\b(we'll|we are going to|we're going to|let's (?:build|create|set up|walk))\b", re.IGNORECASE
)
HOWTO_BRANCH = re.compile(
    r"if you (?:want|need|prefer)\b.{0,40}?\b(?:run|use|pass|do)\b", re.IGNORECASE
)
REFERENCE_VOICE = re.compile(
    r"\blet's\b|\bnow that we\b|\byou'll want to\b|\b(?:we|our)\b", re.IGNORECASE
)
REFERENCE_FRAMING = re.compile(
    r"problem|pain point|frustrat|annoying|wasteful|struggle|tedious", re.IGNORECASE
)
ENTRY_HEADING = re.compile(r"^\s{0,3}#{2,4}\s+(.*)$")
TAGGED_ENTRY = re.compile(r"^(?:Error|Warning):")
CAUSE_SENTENCE = re.compile(r"This issue occurs when", re.IGNORECASE)
ORDERED_ITEM = re.compile(r"^\s*\d+[.)]\s+\S")
# DOC-DISC-16 names these four callout shapes. A `::: code-group` block is a
# tabbed example, not a callout, so the pattern never matches a bare `:::`.
CALLOUT = re.compile(
    r"^\s*(?:!!!|\?\?\?|::: ?(?:tip|note|warning|danger|info|caution|important|details)"
    r"|>\s*\[!\w+\]|<Aside\b)",
    re.IGNORECASE,
)
TAB_SYNTAX = re.compile(r"::: ?code-group|<Tabs\b|^\s*=== \"|\{% tab")
PROSE_BRANCH = re.compile(r"\b(?:or,? with|alternatively|if you (?:use|prefer))\b", re.IGNORECASE)
FENCE_LANG = re.compile(r"^\s*(?:`{3,}|~{3,})\s*([A-Za-z0-9_+-]+)")
COMMAND_BLOCK = re.compile(r"^\s*(?:`{3,}|~{3,}|<<<|--8<--)|\{\{#include")
START_HEADING = re.compile(r"install|quick.?start|usage|get(?:ting)?[ -]started", re.IGNORECASE)
LINK_TEXT = re.compile(r"\[([^\]]+)\]\([^)\s]+")
CHANGELOG_CATEGORIES = {"Added", "Changed", "Deprecated", "Removed", "Fixed", "Security"}
VERSION_HEADING = re.compile(r"^\[?(?:Unreleased|v?\d+\.\d+)")

# A single-element cell, not a plain int: check() mutates it in place so it
# never needs a `global` statement to update the module-level count.
skipped = [0]


def outside_fences(text: str) -> list[tuple[int, str]]:
    out: list[tuple[int, str]] = []
    fence: str | None = None
    for line_no, line in enumerate(text.split("\n"), start=1):
        m = sp.FENCE.match(line)
        if fence is not None:
            if m and m.group(2)[0] == fence[0]:
                fence = None
            continue
        if m:
            fence = m.group(2)
            continue
        out.append((line_no, line))
    return out


def headings(text: str) -> list[tuple[int, int, str]]:
    out = []
    for line_no, line in outside_fences(text):
        m = sp.HEADING.match(line)
        if m:
            out.append((line_no, len(m.group(1)), m.group(2).strip()))
    return out


def preamble(text: str, prose_lines: list[str]) -> tuple[int, int]:
    """Word count and line number of the block between the H1 and the first H2."""
    heads = headings(text)
    h1 = next((h for h in heads if h[1] == 1), None)
    start = h1[0] if h1 else 0
    h2 = next((h for h in heads if h[1] == 2 and h[0] > start), None)
    end = (h2[0] - 1) if h2 else len(prose_lines)
    block = "\n".join(prose_lines[start:end])
    return sp.word_count(block), start + 1


def first_match(pattern: re.Pattern[str], lines: list[tuple[int, str]]) -> int | None:
    for line_no, line in lines:
        if pattern.search(line):
            return line_no
    return None


def check(path: str, text: str, root: str) -> list[Finding]:
    decl = sp.declaration(text)
    doc_type = decl.get("doc_type")
    if not doc_type:
        skipped[0] += 1
        return []
    prose = sp.strip(text)
    prose_lines = prose.split("\n")
    plain = [(i, ln) for i, ln in enumerate(prose_lines, start=1) if ln.strip()]
    heads = headings(text)
    out: list[Finding] = []

    if doc_type in ("how-to", "tutorial"):
        voice = first_match(TUTORIAL_VOICE, plain)
        branch = first_match(HOWTO_BRANCH, plain)
        if voice and branch:
            out.append(
                Finding(
                    path,
                    min(voice, branch),
                    "DOC-TYPE-03",
                    f"a {doc_type} page carries both a learning opener and "
                    "conditional task instructions, split it",
                )
            )
    if doc_type == "landing":
        items = sum(1 for _, ln in outside_fences(text) if ORDERED_ITEM.match(ln))
        rows = sum(1 for _, ln in outside_fences(text) if sp.TABLE_ROW.match(ln))
        if items > LANDING_ORDERED_ITEMS:
            out.append(
                Finding(
                    path,
                    1,
                    "DOC-TYPE-03",
                    f"{items} ordered-list items on a landing page (cap "
                    f"{LANDING_ORDERED_ITEMS}), that is a walkthrough",
                )
            )
        if rows > LANDING_TABLE_ROWS:
            out.append(
                Finding(
                    path,
                    1,
                    "DOC-TYPE-03",
                    f"{rows} table rows on a landing page (cap "
                    f"{LANDING_TABLE_ROWS}), that is reference content",
                )
            )

    if doc_type == "reference":
        line_no = first_match(REFERENCE_VOICE, plain)
        if line_no:
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-TYPE-04",
                    "first person or narrative voice in reference prose",
                )
            )
        first_para = next(sp.iter_paragraphs(prose), (1, ""))
        if REFERENCE_FRAMING.search(first_para[1]):
            out.append(
                Finding(
                    path,
                    first_para[0],
                    "DOC-TYPE-04",
                    "problem framing in a reference page's opening paragraph",
                )
            )

    if doc_type in ("how-to", "reference"):
        words, line_no = preamble(text, prose_lines)
        if words > PREAMBLE_WORD_CAP:
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-TYPE-07",
                    f"{words} words of concept preamble before the first "
                    f"heading (cap {PREAMBLE_WORD_CAP}, measured on 13 "
                    "real how-to preambles)",
                )
            )
    if doc_type == "how-to":
        words, line_no = preamble(text, prose_lines)
        if words == 0:
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-TYPE-22",
                    "no goal sentence between the title and the first "
                    "section, state what the page gets done",
                )
            )

    entries = [(n, t) for n, level, t in heads if 2 <= level <= 4]
    tagged = [(n, t) for n, t in entries if TAGGED_ENTRY.match(t)]
    if doc_type == "troubleshooting":
        if entries and len(tagged) != len(entries):
            out.append(
                Finding(
                    path,
                    entries[0][0],
                    "DOC-TYPE-08",
                    f"{len(entries) - len(tagged)} of {len(entries)} entry "
                    "titles do not open with Error: or Warning:",
                )
            )
        causes = len(CAUSE_SENTENCE.findall(prose))
        if tagged and causes != len(tagged):
            out.append(
                Finding(
                    path,
                    tagged[0][0],
                    "DOC-TYPE-08",
                    f"{len(tagged)} tagged entries but {causes} cause "
                    "paragraphs opening with This issue occurs when",
                )
            )
    if doc_type in ("troubleshooting", "how-to", "reference") and tagged:
        if len(tagged) > TROUBLESHOOTING_SPLIT:
            out.append(
                Finding(
                    path,
                    tagged[0][0],
                    "DOC-TYPE-09",
                    f"{len(tagged)} troubleshooting entries, move them to "
                    "their own page at five (GitLab states the trigger)",
                )
            )
        others = [n for n, level, t in heads if level == 2 and not TAGGED_ENTRY.match(t)]
        if others and tagged[0][0] < max(others):
            out.append(
                Finding(
                    path,
                    tagged[0][0],
                    "DOC-TYPE-09",
                    "troubleshooting entries sit above other sections, put them last",
                )
            )

    if doc_type in ("tutorial", "how-to", "landing") and decl.get("doc_tier") == "first-steps":
        out += _first_command(path, text, prose_lines, heads)
    if doc_type == "tutorial":
        line_no = first_match(TAB_SYNTAX, outside_fences(text))
        if line_no:
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-DISC-17",
                    "tabbed alternatives on a tutorial, a tutorial promises one path",
                )
            )
        langs = [m.group(1) for _, ln in outside_fences(text) for m in [FENCE_LANG.match(ln)] if m]
        if len(langs) != len(set(langs)):
            line_no = first_match(PROSE_BRANCH, plain)
            if line_no:
                out.append(
                    Finding(
                        path,
                        line_no,
                        "DOC-DISC-17",
                        "a prose branch between two same-language examples on a tutorial",
                    )
                )

    if doc_type == "readme":
        out += _readme(path, text, prose_lines, heads)
    if doc_type == "changelog":
        out += _changelog(path, text, heads)
    return out


def _first_command(path, text, prose_lines, heads) -> list[Finding]:
    """DOC-DISC-16: padding between the introducing heading and the command."""
    raw = list(enumerate(text.split("\n"), start=1))
    block = next((n for n, ln in raw if COMMAND_BLOCK.search(ln)), None)
    if block is None:
        return [
            Finding(path, 1, "DOC-DISC-16", "a first-steps page with no runnable command block")
        ]
    start = max([n for n, _, _ in heads if n < block] or [0])
    words = sp.word_count("\n".join(prose_lines[start : block - 1]))
    out = []
    if words > FIRST_COMMAND_WORD_CAP:
        out.append(
            Finding(
                path,
                block,
                "DOC-DISC-16",
                f"{words} words between the introducing heading and the "
                f"first command (cap {FIRST_COMMAND_WORD_CAP}, shared with "
                "DOC-TYPE-07 which owns the number)",
            )
        )
    for line_no, line in raw:
        if start < line_no < block and CALLOUT.match(line):
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-DISC-16",
                    "a callout sits between the heading and the first "
                    "command, move it below the command",
                )
            )
            break
    return out


def _readme(path, text, prose_lines, heads) -> list[Finding]:
    out = []
    description = next(
        (n for n, ln in enumerate(prose_lines, start=1) if len(sp.WORD.findall(ln)) >= 4), None
    )
    table = next((n for n, ln in outside_fences(text) if sp.TABLE_ROW.match(ln)), None)
    if description is None:
        out.append(
            Finding(
                path,
                1,
                "DOC-TYPE-32",
                "no plain-language description, a reader cannot tell what this project is",
            )
        )
    elif table is not None and table < description:
        out.append(Finding(path, table, "DOC-TYPE-32", "a table precedes the project description"))
    start_heading = any(START_HEADING.search(t) for _, _, t in heads)
    start_link = any(
        START_HEADING.search(m.group(1))
        for _, ln in outside_fences(text)
        for m in LINK_TEXT.finditer(ln)
    )
    fenced = "```" in text or "~~~" in text
    if not ((start_heading and fenced) or start_link):
        out.append(
            Finding(
                path, 1, "DOC-TYPE-33", "no install, quickstart or usage block and no link to one"
            )
        )
    return out


def _changelog(path, text, heads) -> list[Finding]:
    out = []
    for line_no, level, title in heads:
        if level == 3 and title not in CHANGELOG_CATEGORIES and not VERSION_HEADING.match(title):
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-TYPE-37",
                    f"category heading {title!r} is not one of "
                    f"{', '.join(sorted(CHANGELOG_CATEGORIES))} "
                    "(Keep a Changelog 1.1.0)",
                )
            )
    if "All notable changes to this project" in text and "keepachangelog.com" not in text:
        out.append(
            Finding(
                path,
                1,
                "DOC-TYPE-41",
                "uses the Keep a Changelog template sentence without crediting it",
            )
        )
    return out


if __name__ == "__main__":
    code = sp.run_cli(__doc__ or "", check, "page_type")
    if skipped[0]:
        print(
            f"page_type.py: skipped {skipped[0]} pages with no doc_type declaration",
            file=sys.stderr,
        )
    sys.exit(code)
