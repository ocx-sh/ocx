#!/usr/bin/env python3
"""Tested-example checks for the docs-quality rule set.

Rules covered:
  DOC-EX-01  every runnable-tagged fence carries a declared binding key
  DOC-EX-02  page keys and test-header keys are the same set, both ways
  DOC-EX-05  every fence carries a language from the project's tier list
  DOC-EX-06  a no-run snippet carries a paired marker that states why
  DOC-EX-07  a failing example names the doc page, not only the test file
  DOC-EX-20  a fence tier suffix is one hyphen-joined, whitespace-free token
  DOC-EX-21  a space-separated fence attribute only where no mkdocs.yml exists

DOC-EX-01 is a set difference, not a heuristic. It looks only at fences the
author explicitly tagged runnable, so it cannot fire on an untagged snippet.

Usage:
  doc_examples.py [--root DIR] [PATH ...] [--format text|json] [--self-test]
  doc_examples.py --tests DIR [--root DIR]      key set diff, DOC-EX-02
  doc_examples.py --harness DIR                 run each bound example file

Exit codes: 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

# --- project configuration -------------------------------------------------
# A tier token is the hyphen-joined suffix of the fence info string, so
# "bash-run" is language bash at tier run. A space in an info string is
# unparsed by pymdownx.superfences and eats the next fence (DOC-EX-20).
RUNNABLE_TIERS = ("run", "runnable", "tested")
TIER_WORDS = (*RUNNABLE_TIERS, "norun", "no-run", "ignore", "skip")
NORUN_OPEN = "doc-norun"  # <!-- doc-norun: reason the snippet must not run -->
NORUN_CLOSE = "/doc-norun"  # <!-- /doc-norun -->
# Per-language command for the harness. Add a language by adding one row.
RUNNERS = {
    ".sh": ["bash"],
    ".bash": ["bash"],
    ".py": ["python3"],
    ".ts": ["node"],
    ".mts": ["node"],
    ".js": ["node"],
    ".mjs": ["node"],
}

FENCE_RE = re.compile(r"^(\s*)(`{3,}|~{3,})[ \t]*(.*)$")
# key="value" attributes and a [Tab title] group are the documented MkDocs
# Material and VitePress forms, so neither counts as a stray space.
ATTR_RE = re.compile(r"""\w+=(?:"[^"]*"|'[^']*'|\S+)|\[[^\]]*\]|\{[^}]*\}""")
BINDING_RE = re.compile(r"^\s*(?:#|//|<!--|/\*)\s*doc:\s*(\S+)")
NORUN_OPEN_RE = re.compile(r"<!--\s*" + re.escape(NORUN_OPEN) + r"\s*(:?)\s*(.*?)-->")
NORUN_CLOSE_RE = re.compile(r"<!--\s*" + re.escape(NORUN_CLOSE) + r"\s*-->")


def bare_tokens(info: str) -> list[str]:
    return ATTR_RE.sub(" ", info).split()


def scan_page(text: str, mkdocs: bool = False) -> tuple[list[tuple[int, str, str]], set[str]]:
    """Return (findings, binding keys the page cites)."""
    out: list[tuple[int, str, str]] = []
    keys: set[str] = set()
    lines = text.splitlines()
    i, norun_line = 0, 0
    while i < len(lines):
        line = lines[i]
        m_open = NORUN_OPEN_RE.search(line)
        if m_open:
            if norun_line:
                out.append(
                    (
                        i + 1,
                        "DOC-EX-06",
                        (
                            f"a second {NORUN_OPEN} marker opens before the one on "
                            f"line {norun_line} closes"
                        ),
                    )
                )
            elif not (m_open.group(1) and m_open.group(2).strip()):
                out.append(
                    (
                        i + 1,
                        "DOC-EX-06",
                        (
                            f"{NORUN_OPEN} marker states no reason. Write "
                            f"<!-- {NORUN_OPEN}: why this must not run -->"
                        ),
                    )
                )
            norun_line = i + 1
        if NORUN_CLOSE_RE.search(line):
            norun_line = 0
        m = FENCE_RE.match(line)
        if not m:
            i += 1
            continue
        _indent, marker, info = m.group(1), m.group(2), m.group(3).strip()
        body: list[str] = []
        i += 1
        while i < len(lines):
            close = FENCE_RE.match(lines[i])
            if (
                close
                and close.group(2)[0] == marker[0]
                and len(close.group(2)) >= len(marker)
                and not close.group(3).strip()
            ):
                break
            body.append(lines[i])
            i += 1
        i += 1
        fence_line = i - len(body) - 1
        if not info:
            out.append(
                (
                    fence_line,
                    "DOC-EX-05",
                    "fence carries no language tag, so no drift check can see it",
                )
            )
            continue
        tokens = bare_tokens(info)
        if len(tokens) > 1 and tokens[1].lower() in TIER_WORDS:
            out.append(
                (
                    fence_line,
                    "DOC-EX-20",
                    (
                        f"fence info string '{info}' joins its tier with a space. "
                        "Write one hyphen-joined token such as "
                        f"'{tokens[0]}-{tokens[1]}'"
                    ),
                )
            )
            continue
        if len(tokens) > 1 and mkdocs:
            out.append(
                (
                    fence_line,
                    "DOC-EX-21",
                    (
                        f"fence info string '{info}' is space separated and this "
                        "site renders under MkDocs Material, which leaves the fence "
                        "unparsed"
                    ),
                )
            )
            continue
        first = tokens[0] if tokens else info
        tier = first.split("-", 1)[1] if "-" in first else ""
        found = [BINDING_RE.match(b) for b in body[:3]]
        found = [f for f in found if f]
        prev = lines[fence_line - 2] if fence_line >= 2 else ""
        m_prev = BINDING_RE.search(prev)
        for f in found:
            keys.add(f.group(1))
        if m_prev:
            keys.add(m_prev.group(1))
        if tier in RUNNABLE_TIERS and not (found or m_prev):
            out.append(
                (
                    fence_line,
                    "DOC-EX-01",
                    (
                        f"fence tagged '{info}' is runnable and carries no "
                        "'# doc: <slug>' binding key"
                    ),
                )
            )
    if norun_line:
        out.append(
            (
                norun_line,
                "DOC-EX-06",
                (
                    f"{NORUN_OPEN} marker on line {norun_line} is never closed with "
                    f"<!-- {NORUN_CLOSE} -->"
                ),
            )
        )
    return out, keys


def test_keys(tests: Path) -> dict[str, Path]:
    """Declared binding keys in the test tree, read from each file's header."""
    keys: dict[str, Path] = {}
    for path in sorted(tests.rglob("*")):
        if not path.is_file() or path.suffix not in RUNNERS:
            continue
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines()[:10]:
            m = BINDING_RE.match(line)
            if m:
                keys[m.group(1)] = path
                break
    return keys


def harness(tests: Path) -> int:
    """Run every bound example file as a subprocess. DOC-EX-04 and DOC-EX-07."""
    files = [p for p in sorted(tests.rglob("*")) if p.is_file() and p.suffix in RUNNERS]
    if not files:
        print(f"missing input: no runnable example files under {tests}", file=sys.stderr)
        return 2
    failed = 0
    for path in files:
        head = path.read_text(encoding="utf-8", errors="replace").splitlines()[:10]
        slug = next((m.group(1) for m in (BINDING_RE.match(x) for x in head) if m), "-")
        want = 0
        for line in head:
            m = re.match(r"^\s*(?:#|//)\s*expect_exit:\s*(-?\d+)", line)
            if m:
                want = int(m.group(1))
        # S603: argv is this repo's own RUNNERS table plus a path found by
        # rglob() under --harness's own tree, never attacker-controlled input.
        proc = subprocess.run(  # noqa: S603
            RUNNERS[path.suffix] + [str(path)], capture_output=True, text=True, check=False
        )
        if proc.returncode == want:
            print(f"ok   {path} (doc: {slug})")
            continue
        failed += 1
        tail = proc.stderr.strip().splitlines()[-1:] or [""]
        print(
            f"{path}:1: DOC-EX-07: example bound to page '{slug}' exited "
            f"{proc.returncode}, expected {want}. {tail[0]}".rstrip()
        )
    print(f"{len(files) - failed}/{len(files)} bound examples passed")
    return 1 if failed else 0


def collect(root: Path, paths: list[str]) -> list[Path]:
    if paths:
        return [Path(p) for p in paths]
    return sorted(p for p in root.rglob("*") if p.suffix in (".md", ".mdx") and p.is_file())


def check_paths(files: list[Path], mkdocs: bool = False) -> tuple[list[dict], set[str]]:
    findings: list[dict] = []
    cited: set[str] = set()
    for path in files:
        page, keys = scan_page(path.read_text(encoding="utf-8", errors="replace"), mkdocs)
        cited |= keys
        for line, rule, message in page:
            findings.append({"page": str(path), "line": line, "rule": rule, "message": message})
    return findings, cited


def self_test() -> int:
    base = Path(__file__).parent / "fixtures" / "doc_examples"
    bad = 0
    for path in sorted(base.glob("*.md")):
        want_fail = path.name.startswith("fail-")
        got = bool(check_paths([path], mkdocs=True)[0])
        if got != want_fail:
            print(
                f"self-test: {path.name} expected "
                f"{'findings' if want_fail else 'clean'}, got the opposite"
            )
            bad += 1
    for name, want in (("harness-pass", 0), ("harness-fail", 1)):
        got = harness(base / name)
        if got != want:
            print(f"self-test: {name} expected exit {want}, got {got}")
            bad += 1
    print(f"self-test: {'FAILED' if bad else 'ok'}, {bad} fixture mismatches")
    return 1 if bad else 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--root", default=".")
    ap.add_argument("paths", nargs="*")
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--tests", help="test tree holding '# doc:' bound example files")
    ap.add_argument("--harness", help="run each bound example file as a subprocess")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    if args.harness:
        tests = Path(args.harness)
        if not tests.is_dir():
            print(f"missing input: {tests}", file=sys.stderr)
            return 2
        return harness(tests)

    root = Path(args.root)
    if not root.exists():
        print(f"missing input: {root}", file=sys.stderr)
        return 2
    files = collect(root, args.paths)
    for f in files:
        if not f.exists():
            print(f"missing input: {f}", file=sys.stderr)
            return 2
    mkdocs = any((root / n).is_file() for n in ("mkdocs.yml", "mkdocs.yaml"))
    findings, cited = check_paths(files, mkdocs)

    if args.tests:
        tests = Path(args.tests)
        if not tests.is_dir():
            print(f"missing input: {tests}", file=sys.stderr)
            return 2
        declared = test_keys(tests)
        for key in sorted(cited - set(declared)):
            findings.append(
                {
                    "page": str(root),
                    "line": 1,
                    "rule": "DOC-EX-02",
                    "message": f"page cites binding key '{key}' and no test file declares it",
                }
            )
        for key in sorted(set(declared) - cited):
            findings.append(
                {
                    "page": str(declared[key]),
                    "line": 1,
                    "rule": "DOC-EX-02",
                    "message": f"test declares binding key '{key}' and no page cites it",
                }
            )

    if args.format == "json":
        print(json.dumps(findings, indent=2))
    else:
        for f in findings:
            print(f"{f['page']}:{f['line']}: {f['rule']}: {f['message']}")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
