#!/usr/bin/env python3
"""Baseline coverage check: every item the old docs carried has a home or a ledger entry.

Files, all beside this script:

  grades.json             {"<page>": {"must": int, "should": int}}   docs-review findings per old page
  grades-notes.json       {"<page>": {"note", "by_id", ...}}         optional detail behind grades.json
  inventory/<page>.json   {"anchors": [], "commands": [], "flags": [], "env": [], "config_keys": [],
                           "exit_codes": [], "slugs": [], "links": [], "fences": [], "facts": []}
                          one per old page; every list holds strings; an extra int "lines" is the page length
  grep-baseline.txt       the file list the phase-7 grep matched, one path per line ("#" lines are comments);
                          must be non-empty
  ledger/<pipeline>.toml  [[row]] tables: item = "<inventory item>", page = "<old page>",
                          action = "moved" | "deleted", home = "<new page or crate path>" (moved only),
                          reason = "duplicate" | "obsolete" | "speculative" | "upstream-patch" (deleted only)
  sweep/*.json            optional phase-8 sweep logs: {"agent": str, "pages": {"<page>": <lines read>}};
                          a "<page> lines=<n>" text line in any other sweep file counts the same

A "moved" row covers its item only when its home resolves: a content page under --root (.md or .mdx) that
exists and, when "#id" is given, carries that id (an explicit {#id}, an <a id>, or the heading's slug); a
"crates/...::<name>" home resolves when the file exists and mentions <name>; a repo-relative file outside the
content tree resolves when it exists. Unresolved homes print as
"<ledger>:<row> home unresolved: <home>".

An item has a home when the new tree under --root (alias --docs; default website/src/content/docs, falling
back to website/src/docs) states it. It is covered by the ledger when some row names its item and page.
The check prints each uncovered item as "<page>: <kind>: <item>" and reads the union of the ledger files.
Kinds checked: anchors, commands, flags, env, config_keys, exit_codes, slugs, facts (links and fences are
inventory only). Anchors, commands, flags, env, exit codes and slugs match the extractor's own output over the
new tree; flags, env and slugs also match as whole words in its text; a config key matches as its dotted path or
as its leaf beside its parent table. A fact matches by a normalized key-token heuristic: a new page whose
6-letter word stems cover at least 60% of the fact's key stems (words of 5+ letters, numbers and code-like
tokens) is its home. When sweep logs exist, a page whose largest stated line count is below the inventory's
line count, or that no log mentions, is reported as a sweep shortfall.

Exit codes: 0 every item covered, 1 at least one uncovered item (or an empty grep-baseline.txt, a bad ledger
row, a sweep shortfall), 2 usage error or a missing input.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
import tomllib

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
import extract  # noqa: E402

KINDS = ("anchors", "commands", "flags", "env", "config_keys", "exit_codes", "slugs")
REASONS = {"duplicate", "obsolete", "speculative", "upstream-patch"}
ACTIONS = {"moved", "deleted"}
STOP = set(
    "about after again also because before being between could does each every from have into just more most "
    "must only other over same should since some such than that their them then there these they this those "
    "through under until used uses using when where which while will with without would your".split()
)
THRESHOLD = 0.6


def usage(msg: str = "") -> int:
    if msg:
        print(msg, file=sys.stderr)
    print("usage: check.py [--root DIR]", file=sys.stderr)
    return 2


def stems(text: str) -> set[str]:
    """Key stems of a sentence: 6-letter prefixes of words of 5+ letters, plus numbers and code-like tokens."""
    out = set()
    for t in re.findall(r"[A-Za-z0-9_.\-/]+", text.lower()):
        t = t.strip(".-/")
        if not t:
            continue
        if re.search(r"[\d_./]", t) or t.startswith("--"):
            out.add(t)
        elif len(t) >= 5 and t not in STOP:
            out.add(t[:6])
    return out


def fact_text(fact: str) -> str:
    return re.sub(r"\s*\[L\d+\]\s*$", "", fact).strip()


def word_in(corpus: str, token: str) -> bool:
    return re.search(r"(?<![\w-])" + re.escape(token) + r"(?![\w-])", corpus) is not None


def page_ids(text: str) -> set[str]:
    ids = set(re.findall(r"\\?\{#([\w.-]+)\\?\}", text)) | set(re.findall(r"<a\s+[^>]*\bid=[\"']([^\"']+)[\"']", text))
    for h in re.findall(r"^#{1,6}\s+(.*?)\s*$", text, re.M):  # github-slugger ids Starlight emits
        h = re.sub(r"\s*\\?\{#[^}]*\}", "", h).replace("`", "")
        ids.add(re.sub(r"\s", "-", re.sub(r"[^\w\s-]", "", h.lower())).strip("-"))
    return ids


def home_resolves(home: str, root: pathlib.Path) -> bool:
    if "::" in home:
        path, _, name = home.partition("::")
        f = REPO / path
        return f.is_file() and name in f.read_text(encoding="utf-8", errors="replace")
    page, _, frag = home.partition("#")
    f = root / page
    if not frag and (REPO / page).is_file() and not f.exists():
        return True  # a file outside the content tree, e.g. an upstream patch note
    if f.suffix not in (".md", ".mdx") or not f.is_file():
        return False
    return not frag or frag in page_ids(f.read_text(encoding="utf-8"))


class Tree:
    def __init__(self, root: pathlib.Path):
        files = sorted(p for p in root.rglob("*") if p.suffix in (".md", ".mdx"))
        tops = extract.top_level_tables(files)
        self.sets = {k: set() for k in KINDS}
        self.stems: list[set[str]] = []
        texts = []
        for f in files:
            text = f.read_text(encoding="utf-8")
            texts.append(text)
            inv = extract.extract_page(text, tops)
            for k in KINDS:
                self.sets[k].update(inv[k])
            self.sets["anchors"].update(page_ids(text))
            self.stems.append(stems(re.sub(r"[`*]", "", text)))
        self.corpus = "\n".join(texts)
        self.pages = len(files)

    def has(self, kind: str, item: str) -> bool:
        if item in self.sets.get(kind, ()):
            return True
        if kind in ("flags", "env", "slugs"):
            return word_in(self.corpus, item)
        if kind == "commands":
            return item in re.sub(r"\s+", " ", self.corpus)
        if kind == "exit_codes":
            return re.search(r"\|\s*`?" + re.escape(item) + r"`?\s*\|", self.corpus) is not None
        if kind == "config_keys":
            if item in self.corpus:
                return True
            parent, _, leaf = item.rpartition(".")
            return bool(parent) and word_in(self.corpus, leaf) and parent in self.corpus
        return False

    def has_fact(self, fact: str) -> bool:
        want = stems(fact_text(fact))
        if not want:
            return True
        need = len(want) if len(want) < 3 else THRESHOLD * len(want)
        return any(len(want & s) >= need for s in self.stems)


def load_ledger(paths: list[pathlib.Path], root: pathlib.Path) -> tuple[set[tuple[str, str]], list[str]]:
    covered, bad = set(), []
    for p in paths:
        try:
            rows = tomllib.loads(p.read_text(encoding="utf-8")).get("row", [])
        except tomllib.TOMLDecodeError as e:
            bad.append(f"{p.name}: ledger: not valid TOML: {e}")
            continue
        for i, r in enumerate(rows, 1):
            where = f"{p.name}: ledger: row {i}"
            action = r.get("action")
            if not (r.get("item") and r.get("page")):
                bad.append(f"{where} lacks item or page")
            elif action not in ACTIONS:
                bad.append(f"{where} has action {action!r}, want moved or deleted")
            elif action == "moved" and not r.get("home"):
                bad.append(f"{where} is moved without a home")
            elif action == "deleted" and r.get("reason") not in REASONS:
                bad.append(f"{where} is deleted with reason {r.get('reason')!r}")
            elif action == "moved" and not home_resolves(r["home"], root):
                bad.append(f"{p.name}:{i} home unresolved: {r['home']}")
            else:
                covered.add((r["page"], fact_text(r["item"])))
    return covered, bad


def sweep_shortfalls(logs: list[pathlib.Path], inv_lines: dict[str, int]) -> list[str]:
    read: dict[str, int] = {}
    for p in logs:
        if p.suffix == ".json":
            try:
                pages = json.loads(p.read_text(encoding="utf-8")).get("pages", {})
            except json.JSONDecodeError:
                continue
            pairs = pages.items()
        else:
            pairs = [(m.group(1), int(m.group(2))) for m in re.finditer(r"(\S+\.md)\s+lines=(\d+)", p.read_text(encoding="utf-8"))]
        for page, n in pairs:
            read[page] = max(read.get(page, 0), int(n))
    out = []
    for page, total in sorted(inv_lines.items()):
        if read.get(page, 0) < total:
            out.append(f"{page}: sweep: read {read.get(page, 0)} of {total} lines")
    return out


def main(argv: list[str]) -> int:
    root = None
    it = iter(argv)
    for a in it:
        if a in ("--root", "--docs"):
            root = next(it, None)
            if root is None:
                return usage("--root needs a directory")
        else:
            return usage(f"unknown argument {a!r}")
    if root:
        tree_root = pathlib.Path(root)
    else:
        tree_root = next((p for p in (REPO / "website/src/content/docs", REPO / "website/src/docs") if p.is_dir()), None)
    if tree_root is None or not tree_root.is_dir():
        return usage(f"content root not found: {root or 'website/src/content/docs, website/src/docs'}")

    grades, grep = HERE / "grades.json", HERE / "grep-baseline.txt"
    inv_files = sorted((HERE / "inventory").rglob("*.json"))
    for need in (grades, grep):
        if not need.is_file():
            return usage(f"missing input {need}")
    if not inv_files:
        return usage(f"missing input {HERE / 'inventory'}/**/*.json")
    try:
        graded = json.loads(grades.read_text(encoding="utf-8"))
        inventories = {f.relative_to(HERE / "inventory").as_posix()[:-5]: json.loads(f.read_text(encoding="utf-8")) for f in inv_files}
    except json.JSONDecodeError as e:
        return usage(f"unreadable input: {e}")

    findings: list[str] = []
    if not [l for l in grep.read_text(encoding="utf-8").splitlines() if l.strip() and not l.startswith("#")]:
        findings.append("grep-baseline.txt: empty file list")
    for page in sorted(set(inventories) ^ set(graded)):
        findings.append(f"{page}: grades: page is missing from {'grades.json' if page in inventories else 'inventory/'}")

    covered, bad = load_ledger(sorted((HERE / "ledger").glob("*.toml")), tree_root)
    findings += bad
    tree = Tree(tree_root)
    for page, inv in sorted(inventories.items()):
        for kind in KINDS:
            for item in inv.get(kind, []):
                if not tree.has(kind, item) and (page, item) not in covered:
                    findings.append(f"{page}: {kind}: {item}")
        for fact in inv.get("facts", []):
            if (page, fact_text(fact)) not in covered and not tree.has_fact(fact):
                findings.append(f"{page}: facts: {fact}")

    logs = sorted(p for p in (HERE / "sweep").glob("*") if p.is_file()) if (HERE / "sweep").is_dir() else []
    if logs:
        findings += sweep_shortfalls(logs, {p: int(v.get("lines", 0)) for p, v in inventories.items()})

    print("\n".join(findings))
    print(f"check.py: {len(findings)} uncovered over {len(inventories)} baseline pages, {tree.pages} pages under {tree_root}", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
