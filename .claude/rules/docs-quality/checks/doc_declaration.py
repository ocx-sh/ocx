#!/usr/bin/env python3
"""Page declaration checks for the docs-quality rule set.

Rules covered:
  DOC-TYPE-01  a doc_type comment stands inside the file's first 12 lines
  DOC-TYPE-02  the type decision reads file content only, never a file name
  DOC-TYPE-28  the declaration is never written as YAML front matter
  DOC-TYPE-29  the declaration never sits above an existing front matter block
  DOC-TYPE-30  an .mdx file uses the {/* */} opener, never an HTML comment
  DOC-DISC-13  tutorial, how-to and landing pages also declare a doc_tier

DOC-TYPE-02 holds by construction. read_declaration() and classify() take text
and take nothing else, so no directory or file name can reach the type decision.
The --seed mode reads a nav config, and a seed is a one-off migration proposal
that a human reviews, never a runtime source of truth.

Usage:
  doc_declaration.py [--root DIR] [PATH ...] [--format text|json] [--self-test]
  doc_declaration.py --seed [--root DIR]

Exit codes: 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

DOC_TYPES = (
    "tutorial",
    "how-to",
    "reference",
    "explanation",
    "troubleshooting",
    "runbook",
    "landing",
    "readme",
    "changelog",
)
DOC_TIERS = ("first-steps", "everyday", "integration")
TIER_REQUIRED = ("tutorial", "how-to", "landing")

# One comment opener per markup family, measured on MkDocs Material 9.7.7,
# mdBook 0.5.3, VitePress 2.0.0-alpha.20 and an MDX 3.1.1 compile
# (wave2-declaration-key.md section 1).
DECL_RE = re.compile(r"^\s*(<!--|\{/\*|\.\.|%)\s*(doc_type|doc_tier)\s*:\s*([A-Za-z][\w-]*)")
FM_KEY_RE = re.compile(r"^\s*(doc_type|doc_tier)\s*:", re.MULTILINE)
MAX_DECL_LINE = 12  # 12 lines (wave2-declaration-key.md section 3)

# Nav group label to doc_type. Measured 115 of 122 fleet nav pages, 94.3 percent
# (wave2-declaration-key.md section 10). Seeding only.
LABEL_TYPE = {
    "home": "landing",
    "index": "landing",
    "overview": "landing",
    "how-to": "how-to",
    "how to": "how-to",
    "guide": "how-to",
    "guides": "how-to",
    "recipes": "how-to",
    "getting started": "how-to",
    "contributing": "how-to",
    "tutorial": "tutorial",
    "tutorials": "tutorial",
    "reference": "reference",
    "api reference": "reference",
    "api": "reference",
    "schema": "reference",
    "cli": "reference",
    "explanation": "explanation",
    "concepts": "explanation",
    "architecture": "explanation",
    "design": "explanation",
    "troubleshooting": "troubleshooting",
    "faq": "troubleshooting",
    "changelog": "changelog",
    "release notes": "changelog",
}


def split_front_matter(lines: list[str]) -> int:
    """Return the index of the first line after a leading front matter block."""
    if not lines or lines[0].strip() != "---":
        return 0
    for i in range(1, min(len(lines), 40)):
        if lines[i].strip() == "---":
            return i + 1
    return 0


def read_declaration(text: str) -> dict[str, tuple[int, str, str]]:
    """Map key to (line number, opener, value). Takes text and nothing else."""
    lines = text.splitlines()
    start = split_front_matter(lines)
    found: dict[str, tuple[int, str, str]] = {}
    for offset, line in enumerate(lines[start : start + MAX_DECL_LINE]):
        m = DECL_RE.match(line)
        if m and m.group(2) not in found:
            found[m.group(2)] = (start + offset + 1, m.group(1), m.group(3))
    return found


def classify(text: str, is_mdx: bool) -> list[tuple[int, str, str]]:
    """Return (line, rule id, message) for one page. Takes text and nothing else."""
    out: list[tuple[int, str, str]] = []
    lines = text.splitlines()
    fm_end = split_front_matter(lines)

    if fm_end and FM_KEY_RE.search("\n".join(lines[1 : fm_end - 1])):
        out.append(
            (
                1,
                "DOC-TYPE-28",
                (
                    "declaration written as YAML front matter, which mdBook "
                    "renders as a fake heading and indexes for search"
                ),
            )
        )

    head = lines[:MAX_DECL_LINE]
    if fm_end == 0:
        for i, line in enumerate(head):
            if line.strip() == "---" and i > 0:
                if any(DECL_RE.match(x) for x in head[:i]):
                    out.append(
                        (
                            i + 1,
                            "DOC-TYPE-29",
                            (
                                "declaration comment sits above a front matter "
                                "block, which turns that block into page text"
                            ),
                        )
                    )
                break

    if is_mdx and "<!--" in text:
        out.append(
            (
                1,
                "DOC-TYPE-30",
                (
                    "HTML comment in an .mdx file, which fails the MDX compile. "
                    "Use the {/* doc_type: V */} opener"
                ),
            )
        )

    decl = read_declaration(text)
    if "doc_type" not in decl:
        out.append(
            (1, "DOC-TYPE-01", f"no doc_type declaration in the first {MAX_DECL_LINE} lines")
        )
        return out
    line_no, _opener, value = decl["doc_type"]
    if value not in DOC_TYPES:
        out.append(
            (line_no, "DOC-TYPE-01", f"doc_type '{value}' is not one of {', '.join(DOC_TYPES)}")
        )
        return out
    if value in TIER_REQUIRED:
        tier = decl.get("doc_tier")
        if tier is None:
            out.append(
                (
                    line_no,
                    "DOC-DISC-13",
                    f"doc_type '{value}' requires a doc_tier and none is declared",
                )
            )
        elif tier[2] not in DOC_TIERS:
            out.append(
                (
                    tier[0],
                    "DOC-DISC-13",
                    f"doc_tier '{tier[2]}' is not one of {', '.join(DOC_TIERS)}",
                )
            )
    return out


def check_paths(paths: list[Path]) -> list[dict]:
    findings = []
    for page in paths:
        text = page.read_text(encoding="utf-8", errors="replace")
        for line, rule, message in classify(text, page.suffix == ".mdx"):
            findings.append({"page": str(page), "line": line, "rule": rule, "message": message})
    return findings


# --- seed mode -------------------------------------------------------------

NAV_ITEM_RE = re.compile(r"^(\s*)-\s+(?:(?P<label>[^:]+?)\s*:\s*)?(?P<target>\S+)?\s*$")

# Each parser returns (group label, item label, target page).


def mkdocs_nav(text: str) -> list[tuple[str, str, str]]:
    """Read the nav block line by line. Unknown YAML tags elsewhere in the file,
    such as !ENV or !!python/name:, cannot reach this parser."""
    items: list[tuple[str, str, str]] = []
    inside = False
    group, group_indent = "", -1
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
        label = (m.group("label") or "").strip().strip("'\"")
        target = (m.group("target") or "").strip().strip("'\"")
        if indent <= group_indent:
            group, group_indent = "", -1
        if not target:
            group, group_indent = label, indent
            continue
        items.append((group, label, target))
    return items


def summary_nav(text: str) -> list[tuple[str, str, str]]:
    """mdBook SUMMARY.md. The file's own mandatory first heading is its title,
    not a part divider, so it never becomes a group label."""
    items: list[tuple[str, str, str]] = []
    group = ""
    seen_title = False
    for line in text.splitlines():
        if re.match(r"^#\s+\S", line):
            if not seen_title:
                seen_title = True
                continue
            group = line.lstrip("# ").strip()
            continue
        m = re.match(r"^\s*[-*]\s+\[([^\]]*)\]\(([^)]+)\)", line)
        if m:
            items.append((group, m.group(1).strip(), m.group(2).strip()))
    return items


def vitepress_nav(text: str) -> list[tuple[str, str, str]]:
    """VitePress config is TypeScript, so this reads text, link and items tokens
    in order rather than parsing the module."""
    items: list[tuple[str, str, str]] = []
    group, pending = "", ""
    for m in re.finditer(r"text:\s*['\"]([^'\"]+)['\"]|link:\s*['\"]([^'\"]+)['\"]|(items:)", text):
        if m.group(1):
            pending = m.group(1)
        elif m.group(3):
            group = pending
        elif m.group(2):
            items.append((group, pending, m.group(2)))
    return items


def seed(root: Path) -> list[tuple[str, str, str, str]]:
    """Propose a doc_type per page from the nav config. Returns
    (page, proposed type, confidence, source). A seed is a migration proposal a
    human reviews, never a runtime source of truth."""
    configs = [
        (root / "mkdocs.yml", mkdocs_nav, "mkdocs.yml nav"),
        (root / "docs" / "SUMMARY.md", summary_nav, "SUMMARY.md"),
        (root / "docs" / "src" / "SUMMARY.md", summary_nav, "SUMMARY.md"),
    ]
    # The VitePress config paths are named, never globbed. A recursive glob
    # finds vendored copies under node_modules and built output.
    for rel in (".vitepress", "docs/.vitepress", "website/.vitepress"):
        for cand in sorted((root / rel).glob("config.*")):
            configs.append((cand, vitepress_nav, "vitepress sidebar"))
    rows: list[tuple[str, str, str, str]] = []
    for page, parser, source in configs:
        if not page.exists():
            continue
        text = page.read_text(encoding="utf-8", errors="replace")
        for group, label, target in parser(text):
            by_group = LABEL_TYPE.get(group.strip().lower())
            by_label = LABEL_TYPE.get(label.strip().lower())
            if by_group:
                rows.append((target, by_group, "high", source))
            elif by_label:
                rows.append((target, by_label, "medium", source))
            else:
                rows.append((target, "unknown", "none", source))
    return rows


# --- cli -------------------------------------------------------------------


def collect(root: Path, paths: list[str]) -> list[Path]:
    if paths:
        return [Path(p) for p in paths]
    return sorted(p for p in root.rglob("*") if p.suffix in (".md", ".mdx", ".rst") and p.is_file())


def self_test() -> int:
    base = Path(__file__).parent / "fixtures" / "doc_declaration"
    bad = 0
    for page in sorted(base.glob("*")):
        if not page.is_file():
            continue
        want_fail = page.name.startswith("fail-")
        got = bool(check_paths([page]))
        if got != want_fail:
            print(
                f"self-test: {page.name} expected "
                f"{'findings' if want_fail else 'clean'}, got the opposite"
            )
            bad += 1
    print(f"self-test: {'FAILED' if bad else 'ok'}, {bad} fixture mismatches")
    return 1 if bad else 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--root", default=".")
    ap.add_argument("pages", nargs="*")
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--seed", action="store_true")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    root = Path(args.root)
    if not root.exists():
        print(f"missing input: {root}", file=sys.stderr)
        return 2
    if args.seed:
        for page, proposed, confidence, source in seed(root):
            print(f"{page}\t{proposed}\t{confidence}\t{source}")
        return 0

    files = collect(root, args.pages)
    for f in files:
        if not f.exists():
            print(f"missing input: {f}", file=sys.stderr)
            return 2
    findings = check_paths(files)
    if args.format == "json":
        print(json.dumps(findings, indent=2))
    else:
        for f in findings:
            print(f"{f['page']}:{f['line']}: {f['rule']}: {f['message']}")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
