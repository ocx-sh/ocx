#!/usr/bin/env python3
"""Raw-markdown link pass for the docs-quality rule set.

Rule covered:
  DOC-OBS-02  a pre-build markdown link pass has a source root and an
              exclusion for every page whose anchors are generated at build time

Without both, the pass either floods the log with false positives or silently
checks nothing. A raw pass with no source root traced 65 phantom dead links to
one four-line generated stub (docs-shape.md section 5).

Three resolutions, all before a link is called dead:
  1. an explicit {#kebab-id} anchor on the target heading
  2. a root-relative path, resolved against --root, with .md, .mdx and
     /index.md tried for an extensionless target
  3. a page whose anchors are generated at build time, which is skipped and
     listed rather than reported

Usage:
  links_raw.py [--root DIR] [PATH ...] [--format text|json] [--self-test]

Exit codes: 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# --- project configuration -------------------------------------------------
# A page carrying one of these markers has its anchors written by the build,
# so this pass cannot see them and never calls one of its anchors dead.
GENERATED_ANCHOR_MARKERS = (
    "Auto-generated",
    "auto-generated",
    ":::",  # mkdocstrings and MyST directive blocks
    "{{#include",  # mdBook include
    "<<<",  # VitePress include
    "--8<--",  # MkDocs snippets
)
# Targets never resolved on disk.
EXTERNAL_RE = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|//)", re.IGNORECASE)
PAGE_SUFFIXES = (".md", ".mdx")

LINK_RE = re.compile(r"(?<!\!)\[[^\]]*\]\(\s*<?([^)\s>]+)>?[^)]*\)")
HEADING_RE = re.compile(r"^#{1,6}\s+(.*?)\s*$")
EXPLICIT_ID_RE = re.compile(r"\{\s*#\s*([A-Za-z0-9_.:-]+)[^}]*\}")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")


def slugify(text: str) -> str:
    """The Python-Markdown toc slug, which every fleet generator matches closely.
    Underscores survive, and a run of hyphens or spaces collapses to one hyphen."""
    text = re.sub(r"\{[^}]*\}", "", text)
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"[^\w\s-]", "", text.lower()).strip()
    return re.sub(r"[-\s]+", "-", text)


def anchors(text: str) -> set[str]:
    out: set[str] = set()
    fence = ""
    for line in text.splitlines():
        m = FENCE_RE.match(line)
        if fence:
            if m and m.group(1)[0] == fence[0]:
                fence = ""
            continue
        if m:
            fence = m.group(1)
            continue
        h = HEADING_RE.match(line)
        if not h:
            continue
        explicit = EXPLICIT_ID_RE.search(h.group(1))
        out.add(explicit.group(1) if explicit else slugify(h.group(1)))
    for m in re.finditer(r'<a\s+(?:id|name)="([^"]+)"', text):
        out.add(m.group(1))
    return out


def generated(text: str) -> bool:
    return any(marker in text for marker in GENERATED_ANCHOR_MARKERS)


def resolve(target: str, page: Path, root: Path) -> Path | None:
    """Resolve a link target to a source page, or None when it is not a page."""
    base = root if target.startswith("/") else page.parent
    raw = target.lstrip("/")
    cand = (base / raw) if raw else page
    if cand.suffix in PAGE_SUFFIXES:
        return cand
    for extra in PAGE_SUFFIXES:
        if (probe := cand.with_suffix(extra)).is_file():
            return probe
        if (probe := cand / f"index{extra}").is_file():
            return probe
    return cand.with_suffix(".md") if not cand.suffix else None


def check_page(page: Path, root: Path, skipped: set[str]) -> list[dict]:
    text = page.read_text(encoding="utf-8", errors="replace")
    out: list[dict] = []
    fence = ""
    for line_no, line in enumerate(text.splitlines(), 1):
        m = FENCE_RE.match(line)
        if fence:
            if m and m.group(1)[0] == fence[0]:
                fence = ""
            continue
        if m:
            fence = m.group(1)
            continue
        for match in LINK_RE.finditer(line):
            target = match.group(1)
            if not target or EXTERNAL_RE.match(target):
                continue
            path_part, _, fragment = target.partition("#")
            if not path_part:
                dest, dest_text = page, text
            else:
                if EXTERNAL_RE.match(path_part):
                    continue
                dest = resolve(path_part, page, root)
                if dest is None:
                    continue
                if not dest.is_file():
                    out.append(
                        {
                            "page": str(page),
                            "line": line_no,
                            "rule": "DOC-OBS-02",
                            "message": f"link target '{target}' resolves to "
                            f"{dest}, which does not exist",
                        }
                    )
                    continue
                dest_text = dest.read_text(encoding="utf-8", errors="replace")
            if not fragment:
                continue
            if generated(dest_text):
                skipped.add(str(dest))
                continue
            if fragment not in anchors(dest_text):
                out.append(
                    {
                        "page": str(page),
                        "line": line_no,
                        "rule": "DOC-OBS-02",
                        "message": f"anchor '#{fragment}' is not a heading id in "
                        f"{dest}. Give that heading a {{#{fragment}}} id",
                    }
                )
    return out


def collect(root: Path, paths: list[str]) -> list[Path]:
    if paths:
        return [Path(p) for p in paths]
    return sorted(p for p in root.rglob("*") if p.suffix in PAGE_SUFFIXES and p.is_file())


def run(root: Path, files: list[Path]) -> tuple[list[dict], list[str]]:
    skipped: set[str] = set()
    findings: list[dict] = []
    for page in files:
        findings += check_page(page, root, skipped)
    return findings, sorted(skipped)


def self_test() -> int:
    base = Path(__file__).parent / "fixtures" / "links_raw"
    bad = 0
    for path in sorted(base.glob("*.md")):
        if not (path.name.startswith("fail-") or path.name.startswith("pass-")):
            continue
        want_fail = path.name.startswith("fail-")
        got = bool(run(base, [path])[0])
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
    ap.add_argument(
        "--root", default=".", help="site source root, the resolution base for /root-relative links"
    )
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
    files = collect(root, args.paths)
    for f in files:
        if not f.is_file():
            print(f"missing input: {f}", file=sys.stderr)
            return 2
    findings, skipped = run(root, files)

    if args.format == "json":
        print(json.dumps({"findings": findings, "skipped": skipped}, indent=2))
    else:
        for f in findings:
            print(f"{f['page']}:{f['line']}: {f['rule']}: {f['message']}")
        for s in skipped:
            print(f"skipped, anchors generated at build time: {s}", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
