#!/usr/bin/env python3
"""Navigation and page-shape checks for the docs-quality rule set.

Rules covered:
  DOC-NAV-01  run this family only where a docs-site generator config exists
  DOC-NAV-02  sidebar no deeper than three levels, third level collapsed
  DOC-NAV-03  group the top-level navigation once a site reaches eight pages
  DOC-NAV-04  a nav at three levels carries a breadcrumb, or comes back to two
  DOC-NAV-05  cap in-page headings at H4 unless the page is a reference page
  DOC-NAV-06  split a non-reference page once it passes 4000 prose words

Nav config is read line by line rather than through a YAML library, so the
!ENV and !!python/name: tags that mkdocs.yml carries cannot break the read.
mdBook's own mandatory first heading in SUMMARY.md is a title, not a divider.

Usage:
  nav_depth.py [--root DIR] [PATH ...] [--format text|json] [--self-test]

Exit codes: 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# strip_prose is the shared library every prose rule reads through. It blanks
# headings too, so the H5 scan below reads raw lines and tracks fences itself.
from strip_prose import strip as strip_prose

MAX_NAV_DEPTH = 3  # 3 levels (NN/g progressive disclosure, two open plus one collapsed)
FLAT_NAV_FLOOR = 8  # 8 pages (argued, docs-navigation-search.md DOC-NAV-03)
MAX_PROSE_WORDS = 4000  # 4000 words (fleet distribution, docs-shape.md section 4)

VITEPRESS_CONFIGS = (".vitepress", "docs/.vitepress", "website/.vitepress")
DECL_TYPE_RE = re.compile(
    r"^\s*(?:<!--|\{/\*|\.\.|%)\s*doc_type\s*:\s*([A-Za-z][\w-]*)", re.MULTILINE
)
NAV_ITEM_RE = re.compile(r"^(\s*)-\s+(?:([^:]+?)\s*:\s*)?(\S+)?\s*$")


def headings_below_h4(text: str) -> list[int]:
    """Line numbers of H5 and H6 headings, ignoring anything inside a fence."""
    out: list[int] = []
    fence = ""
    for i, line in enumerate(text.splitlines(), 1):
        m = re.match(r"^\s*(`{3,}|~{3,})", line)
        if fence:
            if m and m.group(1)[0] == fence[0] and len(m.group(1)) >= len(fence):
                fence = ""
            continue
        if m:
            fence = m.group(1)
            continue
        if re.match(r"^#{5,6} ", line):
            out.append(i)
    return out


def find_generator(root: Path) -> tuple[str, Path] | None:
    for rel in ("mkdocs.yml", "mkdocs.yaml"):
        if (root / rel).is_file():
            return "mkdocs", root / rel
    for rel in VITEPRESS_CONFIGS:
        for cand in sorted((root / rel).glob("config.*")):
            return "vitepress", cand
    for rel in ("book.toml", "docs/book.toml"):
        if (root / rel).is_file():
            for summary in ("SUMMARY.md", "src/SUMMARY.md"):
                cand = (root / rel).parent / summary
                if cand.is_file():
                    return "mdbook", cand
    return None


def mkdocs_nav(text: str) -> tuple[int, int, bool]:
    """Return (max depth, bare top-level entries, an expanded level-3 node)."""
    inside = False
    indents: list[int] = []
    rows: list[tuple[int, bool]] = []
    for line in text.splitlines():
        if re.match(r"^nav:\s*$", line):
            inside = True
            continue
        if inside and line.strip() and not line.startswith((" ", "\t")):
            break
        if not inside:
            continue
        m = NAV_ITEM_RE.match(line)
        if not m:
            continue
        indent = len(m.group(1))
        if indent not in indents:
            indents.append(indent)
            indents.sort()
        rows.append((indent, bool(m.group(3))))
    if not rows:
        return 0, 0, False
    depth = max(indents.index(i) + 1 for i, _ in rows)
    top = min(indents)
    bare_top = sum(1 for i, has_target in rows if i == top and has_target)
    expanded = depth >= MAX_NAV_DEPTH and bool(re.search(r"navigation\.expand", text))
    return depth, bare_top, expanded


def _balanced(text: str, start: int) -> str:
    open_ch = text[start]
    close_ch = {"{": "}", "[": "]"}[open_ch]
    level = 0
    for i in range(start, len(text)):
        if text[i] == open_ch:
            level += 1
        elif text[i] == close_ch:
            level -= 1
            if level == 0:
                return text[start : i + 1]
    return text[start:]


def vitepress_nav(text: str) -> tuple[int, int, bool]:
    """VitePress config is TypeScript, so this walks brackets rather than
    parsing the module. Level 1 is the top nav bar, level 2 the sidebar's own
    list, and each nested items: array adds one more level."""
    m = re.search(r"\bsidebar:\s*(\{|\[)", text)
    if not m:
        return 0, 0, False
    block = _balanced(text, m.start(1))
    items_depth, max_items, groups, collapsed, bare_top = 0, 0, 0, 0, 0
    for tok in re.finditer(r"(items:\s*\[)|(\[)|(\])|(collapsed:\s*true)|(link:\s*['\"])", block):
        if tok.group(1):
            groups += 1
            items_depth += 1
            max_items = max(max_items, items_depth)
        elif tok.group(3) and items_depth > 0:
            items_depth -= 1
        elif tok.group(4):
            collapsed += 1
        elif tok.group(5) and items_depth == 0:
            bare_top += 1
    has_navbar = bool(re.search(r"\bnav:\s*\[", text))
    total = (1 if has_navbar else 0) + 1 + max_items
    expanded = total >= MAX_NAV_DEPTH and collapsed < groups
    return total, bare_top, expanded


def mdbook_nav(text: str) -> tuple[int, int, bool]:
    indents: list[int] = []
    rows: list[int] = []
    dividers = 0
    seen_title = False
    for line in text.splitlines():
        if re.match(r"^#\s+\S", line):
            if not seen_title:
                seen_title = True  # SUMMARY.md's own mandatory title line
                continue
            dividers += 1
            continue
        m = re.match(r"^(\s*)[-*]\s+\[", line)
        if not m:
            continue
        indent = len(m.group(1))
        if indent not in indents:
            indents.append(indent)
            indents.sort()
        rows.append(indent)
    if not rows:
        return 0, 0, False
    depth = max(indents.index(i) + 1 for i in rows)
    top = sum(1 for i in rows if i == min(indents))
    if dividers:
        depth += 1  # a "# Part Title" divider groups without adding a nav level
        top = 0
    return depth, top, False


def check_nav(root: Path) -> list[dict]:
    found = find_generator(root)
    if found is None:
        print("not applicable: no docs-site generator config found (DOC-NAV-01)")
        return []
    generator, config = found
    text = config.read_text(encoding="utf-8", errors="replace")
    depth, bare_top, expanded = {
        "mkdocs": mkdocs_nav,
        "vitepress": vitepress_nav,
        "mdbook": mdbook_nav,
    }[generator](text)

    out: list[dict] = []

    def add(rule: str, message: str) -> None:
        out.append({"page": str(config), "line": 1, "rule": rule, "message": message})

    if depth > MAX_NAV_DEPTH:
        add(
            "DOC-NAV-02",
            f"{generator} nav is {depth} levels deep, over the {MAX_NAV_DEPTH}-level cap",
        )
    if expanded:
        add(
            "DOC-NAV-02",
            f"{generator} nav reaches level {MAX_NAV_DEPTH} with that level expanded by default",
        )
    if bare_top >= FLAT_NAV_FLOOR:
        add(
            "DOC-NAV-03",
            f"{bare_top} top-level nav entries with no group, "
            f"at or over the {FLAT_NAV_FLOOR}-page grouping floor",
        )
    if depth >= MAX_NAV_DEPTH:
        if generator == "mkdocs":
            has_crumb = bool(re.search(r"navigation\.path", text))
        elif generator == "vitepress":
            has_crumb = bool(re.search(r"(?i)breadcrumb", text))
        else:
            has_crumb = False  # mdBook ships no breadcrumb and no config for one
        if not has_crumb:
            add(
                "DOC-NAV-04",
                f"nav reaches {depth} levels with no breadcrumb, "
                "so a reader at level 3 has no trail back up",
            )
    return out


def check_page(path: Path) -> list[dict]:
    text = path.read_text(encoding="utf-8", errors="replace")
    m = DECL_TYPE_RE.search("\n".join(text.splitlines()[:14]))
    doc_type = m.group(1) if m else ""
    out: list[dict] = []
    if doc_type == "reference":
        return out
    for line_no in headings_below_h4(text)[:1]:
        out.append(
            {
                "page": str(path),
                "line": line_no,
                "rule": "DOC-NAV-05",
                "message": "heading below H4 on a page that does not declare doc_type: reference",
            }
        )
    words = len(strip_prose(text).split())
    if words > MAX_PROSE_WORDS:
        out.append(
            {
                "page": str(path),
                "line": 1,
                "rule": "DOC-NAV-06",
                "message": f"{words} prose words, over the {MAX_PROSE_WORDS}-word split trigger",
            }
        )
    return out


def self_test() -> int:
    base = Path(__file__).parent / "fixtures" / "nav_depth"
    bad = 0
    for path in sorted(base.iterdir()):
        want_fail = path.name.startswith("fail-")
        got = bool(check_nav(path) if path.is_dir() else check_page(path))
        if got != want_fail:
            print(
                f"self-test: {path.name} expected "
                f"{'findings' if want_fail else 'clean'}, got the opposite"
            )
            bad += 1
    print(f"self-test: {'FAILED' if bad else 'ok'}, {bad} fixture mismatches")
    return 1 if bad else 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--root", default=".")
    ap.add_argument("paths", nargs="*")
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    root = Path(args.root)
    if not root.is_dir():
        print(f"missing input: {root}", file=sys.stderr)
        return 2

    findings = check_nav(root)
    for name in args.paths:
        path = Path(name)
        if not path.is_file():
            print(f"missing input: {path}", file=sys.stderr)
            return 2
        findings += check_page(path)

    if args.format == "json":
        print(json.dumps(findings, indent=2))
    else:
        for f in findings:
            print(f"{f['page']}:{f['line']}: {f['rule']}: {f['message']}")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
