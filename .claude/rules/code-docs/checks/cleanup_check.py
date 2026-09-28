#!/usr/bin/env python3
"""Diff-time safety check for a comment-cleanup change (code-docs rule set).

Rules covered, blocking (exit 1):
  CLN-01  carve-out: a prose block the diff removes whole, whose base text
          fires the guard recogniser or carries a SAFETY: or ponytail: label,
          needs a replacement in the same diff. A replacement is a clause of
          it that survives, a pointer or a named test added to the same file,
          or a lint form beside it. Removed and added blocks are paired across
          the whole diff by SequenceMatcher ratio, so a moved block is no loss.
  CLN-03  every non-prose line stays byte-identical, compared per file as an
          ordered sequence: code, interface doc text (every doc comment in a
          file under .code-docs.json's interface_files), tool directives, licence
          headers, and fenced code or doctests inside doc comments (Python:
          Example:/Examples: sections, '::' literal blocks and >>> doctests
          with their output, as comment_census.py_examples ends them). A
          Python file that parsed at the base must still parse.
  CLN-04  prose the diff removes does not reappear elsewhere in the same
          file's comments (80 percent token overlap or more). A clause
          reordered inside its own block (the post block holding most of the
          base block's kept words) is no move. Prose removed
          beside a new pointer to a declared relocation record lands in that
          record. Text appended to a record no longer lives in the source, so
          a pointer and the paraphrase it replaces never coexist.
  CLN-05  no test file, in-file test region, lock or manifest file, generated
          or golden file, or non-source path other than a declared relocation
          record, which is new or append-only. __pycache__ is not an edit.
  LNK-05  a record pointer the diff adds to a comment (linkage_check's LNK-02
          grammar, the linkage config's records globs included) names a file
          its block named before the edit, a --relocation record, or a
          --cites record: a tracked record that already holds the removed
          argument.
Advisory (printed as review items, exit 0):
  CLN-02  a shortened or moved guard block has fewer sentences that fire the
          recogniser than before. A regex misses good rewrites, so this never
          fails the diff.
  LNK-05  on a clean run, every --cites record, for a human to confirm it
          holds the argument each pointer to it replaced.

Everything is derived from the diff between the merge base of --base and HEAD
and the working tree, untracked files included. The claims the check takes
from the editing agent are --relocation, which it verifies, and --cites, which
it requires to be tracked and lists for a human to confirm.

The recogniser's recall is about 0.43, so CLN-01 protects only the guards it
sees. A block replaced in place by any new prose counts as shortened, not
removed, and gets the CLN-02 review instead of the CLN-01 gate.

Usage:
  cleanup_check.py --base REF [--root DIR] [--relocation PATH ...] [--cites PATH ...]
                   [--format text|json]
  cleanup_check.py --self-test

Exit codes: 0 clean (review items may print), 1 blocking findings, 2 usage or
missing input.
"""

from __future__ import annotations

import argparse
import ast
import bisect
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from difflib import SequenceMatcher
from pathlib import Path, PurePosixPath

sys.dont_write_bytecode = True  # no __pycache__ beside an installed copy (CLN-05 would read it)
sys.path.insert(0, str(Path(__file__).resolve().parent))
import comment_census as cc  # noqa: E402 - after the bytecode switch above
import linkage_check as lc  # noqa: E402
from guard_recogniser import classify as recognise  # noqa: E402

GONE = 0.8  # CLN-04 overlap bar; provisional
PAIR_RATIO = 0.6  # difflib's own "close match" bar; provisional
MIN_RUN = 8  # words: a shorter removed run is a word edit, not prose that could hide
MIN_MATCH = 3  # words: a shorter shared run is coincidence ("of the")
MIN_CLAUSE = 6  # words: a shorter sentence is too generic to prove a clause survived
LINT_WINDOW = 12  # lines around a removed block searched for a lint form

FROZEN_KINDS = ("code", "interface", "directive", "license")
# Adopters extend these two lists (CLN-05 portability note).
LOCKS = frozenset(
    [
        "Cargo.lock",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
        "deno.lock",
        "uv.lock",
        "poetry.lock",
        "pdm.lock",
        "Pipfile.lock",
        "go.sum",
        "go.work.sum",
        "Gemfile.lock",
        "composer.lock",
        "flake.lock",
        "MODULE.bazel.lock",
        "gradle.lockfile",
        "Package.resolved",
        "mix.lock",
        "pubspec.lock",
    ]
)
MANIFESTS = frozenset(
    [
        "Cargo.toml",
        "package.json",
        "go.mod",
        "go.work",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "Pipfile",
        "requirements.txt",
        "Gemfile",
        "composer.json",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "MODULE.bazel",
        "WORKSPACE",
        "WORKSPACE.bazel",
    ]
)
GOLDEN_DIRS = frozenset({"golden", "goldens", "snapshots", "__snapshots__"})
GENERATED_RE = re.compile(
    r"@generated|Code generated .* DO NOT EDIT|auto-generated|autogenerated by|DO NOT EDIT",
    re.IGNORECASE,
)
TOKEN_RE = re.compile(r"[a-z0-9_]+")
SENTENCE_RE = re.compile(r"(?<=[.!?])\s+|\n\s*\n|\n\s*[-*]\s+")
LABEL_RE = re.compile(r"\b(?:SAFETY|ponytail)\s*:", re.IGNORECASE)
POINTER_RE = re.compile(
    r"https?://\S+|[\w./-]+\.(?:md|rst|adoc|txt|rs|py|pyi|ts|tsx|js|go|java|kt|toml|ya?ml|json)\b"
)
LINT_FORM_RE = re.compile(r"#\[(?:deny|forbid)\(|#\[must_use|:\s*never\s*=|assert_never\(")
IDENT_RE = re.compile(r"`(?:[\w:]*::)?(\w{4,})(?:\(\))?`|\b([a-z][a-z0-9]*(?:_[a-z0-9]+){2,})\b")


class UsageError(Exception):
    pass


@dataclass
class Finding:
    path: str
    line: int
    rule: str
    message: str
    blocking: bool = True


# --- git ----------------------------------------------------------------------


def git(root: Path, *args: str, ok: tuple[int, ...] = (0,)) -> str:
    r = subprocess.run(  # noqa: S603
        ["git", "-C", str(root), "-c", "core.quotepath=off", *args],  # noqa: S607
        capture_output=True,
        check=False,
    )
    if r.returncode not in ok:
        raise UsageError(f"git {' '.join(args)}: {r.stderr.decode('utf-8', 'replace').strip()}")
    return r.stdout.decode("utf-8", "replace")


def changed_paths(top: Path, base: str) -> list[tuple[str, str]]:
    """(status, path) for every path the working tree changes against base."""
    out = git(top, "diff", "--name-status", "-z", "--no-renames", "--ignore-submodules=dirty", base)
    parts = [p for p in out.split("\0") if p]
    changes = [(parts[i][0], parts[i + 1]) for i in range(0, len(parts) - 1, 2)]
    untracked = git(top, "ls-files", "-z", "--others", "--exclude-standard")
    changes += [("A", p) for p in untracked.split("\0") if p]
    # Bytecode a check run leaves beside an installed copy is no edit of the cleanup's.
    changes = [c for c in changes if "__pycache__" not in c[1].split("/")[:-1]]
    return sorted(changes, key=lambda c: c[1])


# --- per-file model -------------------------------------------------------------


@dataclass
class Side:
    raw: list
    lines: list
    prose: list  # comment text per prose line, None elsewhere
    frozen: list  # True for a non-prose line CLN-03 freezes
    blocks: list  # (start, end) per prose block, test regions left out
    blk: list  # block index per prose line, -1 elsewhere
    toks: list  # lowercased word tokens per prose line


def side(path: str, src: str, lang: str, interface: bool) -> Side:
    raw = src.split("\n")
    lines = cc.classify(path, src, lang, interface)
    n = len(lines)
    prose: list = [None] * n
    frozen = [ln.kind in FROZEN_KINDS for ln in lines]
    blk = [-1] * n
    blocks = []
    for kind, start, length in cc.blocks_of(lines):
        if lines[start].test:
            continue
        fence = doctest = False
        # Python example sections, literal blocks and doctests, by comment_census's end rules.
        ex = cc.py_examples(raw[start : start + length]) if lang == "python" else None
        for i in range(start, start + length):
            t = lines[i].text.lstrip("*").strip()
            if kind == "doc":
                if t.startswith(("```", "~~~")):
                    fence = not fence
                    frozen[i] = True
                    continue
                if ex is None:
                    doctest = t.startswith(">>>") or (doctest and bool(t))
                else:
                    doctest = ex[i - start] == "body"
                if fence or doctest:
                    frozen[i] = True
                    continue
            prose[i] = lines[i].text
            blk[i] = len(blocks)
        blocks.append((start, start + length))
    toks = [TOKEN_RE.findall(p.lower()) if p else [] for p in prose]
    return Side(raw, lines, prose, frozen, blocks, blk, toks)


@dataclass
class Diff:
    path: str
    pre: Side
    post: Side
    ops: list
    op_of_pre: list  # line-level opcode index per pre line
    pre_kept: list  # per pre line, per token: survives in place
    post_kept: list  # per post line, per token: carried over from the base
    pair: dict  # pre block -> the post block holding most of its kept tokens

    def post_line(self, i: int) -> int:
        """1-based post line where pre line i sits now."""
        tag, i1, _i2, j1, _j2 = self.ops[self.op_of_pre[i]]
        j = j1 + (i - i1) if tag == "equal" else j1
        return min(j, len(self.post.raw) - 1) + 1

    def inserted(self) -> list[tuple[str, int]]:
        return [
            (tok, j)
            for j, toks in enumerate(self.post.toks)
            for k, tok in enumerate(toks)
            if not self.post_kept[j][k]
        ]


def align(path: str, pre: Side, post: Side) -> Diff:
    ops = SequenceMatcher(None, pre.raw, post.raw).get_opcodes()
    pre_kept = [[False] * len(t) for t in pre.toks]
    post_kept = [[False] * len(t) for t in post.toks]
    op_of_pre = [0] * len(pre.raw)
    links: dict[tuple[int, int], int] = {}  # (pre block, post block) -> kept tokens shared

    def link(i: int, j: int, n: int) -> None:
        if pre.blk[i] >= 0 and post.blk[j] >= 0:
            key = (pre.blk[i], post.blk[j])
            links[key] = links.get(key, 0) + n

    for o, (tag, i1, i2, j1, j2) in enumerate(ops):
        op_of_pre[i1:i2] = [o] * (i2 - i1)
        if tag == "equal":
            for i in range(i1, i2):
                pre_kept[i] = [True] * len(pre.toks[i])
                link(i, j1 + i - i1, len(pre.toks[i]))
            for j in range(j1, j2):
                post_kept[j] = [True] * len(post.toks[j])
            continue
        a = [(i, k) for i in range(i1, i2) for k in range(len(pre.toks[i]))]
        b = [(j, k) for j in range(j1, j2) for k in range(len(post.toks[j]))]
        sm = SequenceMatcher(
            None, [pre.toks[i][k] for i, k in a], [post.toks[j][k] for j, k in b], autojunk=False
        )
        for m in sm.get_matching_blocks():
            if m.size >= MIN_MATCH:
                for d in range(m.size):
                    i, k = a[m.a + d]
                    pre_kept[i][k] = True
                    j, k = b[m.b + d]
                    post_kept[j][k] = True
                    link(i, j, 1)
    pair = {}
    for (pb, qb), _n in sorted(links.items(), key=lambda kv: kv[1]):
        pair[pb] = qb  # ascending, so the last write per pre block is its largest share
    return Diff(path, pre, post, ops, op_of_pre, pre_kept, post_kept, pair)


def removed_runs(d: Diff) -> list[dict]:
    """Contiguous prose tokens the diff removes, split at block and opcode edges."""
    runs: list[dict] = []
    cur: dict | None = None
    for i, toks in enumerate(d.pre.toks):
        for k, tok in enumerate(toks):
            if d.pre_kept[i][k]:
                cur = None
                continue
            key = (d.pre.blk[i], d.op_of_pre[i])
            if cur is None or cur["key"] != key:
                cur = {"key": key, "first": i, "last": i, "toks": []}
                runs.append(cur)
            cur["toks"].append(tok)
            cur["last"] = i
    return runs


class Hay:
    """A token sequence to search, with difflib's index built once."""

    def __init__(self, toks: list[str]):
        self.toks = toks
        self.sm = SequenceMatcher(None, [], toks, autojunk=False)

    def cover(self, needle: list[str]) -> tuple[float, int]:
        """Share of needle found in shared runs of MIN_MATCH words, and where the longest starts."""
        if not needle or not self.toks:
            return 0.0, -1
        self.sm.set_seq1(needle)
        hits = [m for m in self.sm.get_matching_blocks() if m.size >= MIN_MATCH]
        if not hits:
            return 0.0, -1
        return sum(m.size for m in hits) / len(needle), max(hits, key=lambda m: m.size).b


def sentences(text: str) -> list[str]:
    return [s for s in SENTENCE_RE.split(text) if s and s.strip()]


def firing(text: str) -> int:
    """Sentences that fire the recogniser; a block that fires only as a whole counts one."""
    return max(sum(recognise(s)[0] for s in sentences(text)), int(recognise(text)[0]))


def block_text(s: Side, b: int) -> str:
    start, end = s.blocks[b]
    return "\n".join(s.prose[i] for i in range(start, end) if s.prose[i] is not None)


def block_toks(s: Side, b: int) -> list[str]:
    start, end = s.blocks[b]
    return [t for i in range(start, end) for t in s.toks[i]]


# --- CLN-03 and the in-file part of CLN-05 --------------------------------------


def show(line: str) -> str:
    line = line.strip()
    return line if len(line) <= 70 else line[:67] + "..."


def frozen_findings(d: Diff, lang: str) -> list[Finding]:
    out = []
    pre = [i for i, f in enumerate(d.pre.frozen) if f and not d.pre.lines[i].test]
    post = [j for j, f in enumerate(d.post.frozen) if f and not d.post.lines[j].test]
    a, b = [d.pre.raw[i] for i in pre], [d.post.raw[j] for j in post]
    if a != b:
        for tag, i1, i2, j1, j2 in SequenceMatcher(None, a, b, autojunk=False).get_opcodes():
            if tag == "equal":
                continue
            at = post[j1] + 1 if j1 < len(post) else len(d.post.raw)
            ln = d.pre.lines[pre[i1]] if i1 < i2 else d.post.lines[post[j1]]
            kind = {"doc": "doc-comment code", "line": "doc-comment code"}.get(ln.kind, ln.kind)
            if ln.trailing:
                kind = "code (a trailing comment is out of cleanup scope)"
            if tag == "delete":
                msg = f"{i2 - i1} {kind} line(s) removed, first: {show(a[i1])!r}"
            elif tag == "insert":
                msg = f"{j2 - j1} {kind} line(s) added, first: {show(b[j1])!r}"
            else:
                msg = f"{kind} line changed: {show(a[i1])!r} -> {show(b[j1])!r}"
            out.append(
                Finding(
                    d.path,
                    at,
                    "CLN-03",
                    msg + "; a cleanup leaves every non-prose line byte-identical",
                )
            )
    if lang == "python" and parses("\n".join(d.pre.raw)):
        try:
            ast.parse("\n".join(d.post.raw))
        except SyntaxError as e:
            out.append(
                Finding(
                    d.path,
                    e.lineno or 1,
                    "CLN-03",
                    f"the file no longer parses ({e.msg}); a body whose only statement was a docstring lost it",
                )
            )
    ta = [d.pre.raw[i] for i, ln in enumerate(d.pre.lines) if ln.test]
    tb = [d.post.raw[j] for j, ln in enumerate(d.post.lines) if ln.test]
    if ta != tb:
        first = next((j for j, ln in enumerate(d.post.lines) if ln.test), 0)
        out.append(
            Finding(
                d.path,
                first + 1,
                "CLN-05",
                "a line inside an in-file test region (#[cfg(test)]) changed",
            )
        )
    return out


def parses(src: str) -> bool:
    try:
        ast.parse(src)
    except SyntaxError:
        return False
    return True


# --- CLN-01, CLN-02, CLN-04 ---------------------------------------------------------


def names_test(top: Path, text: str) -> str | None:
    """An identifier in text that is defined as a test function in the working tree."""
    for m in IDENT_RE.finditer(text):
        name = m.group(1) or m.group(2)
        hits = git(
            top,
            "grep",
            "-n",
            "-w",
            "-F",
            "-e",
            f"fn {name}",
            "-e",
            f"def {name}",
            "-e",
            f"func {name}",
            ok=(0, 1),
        )
        for hit in hits.splitlines():
            path, line, _ = hit.split(":", 2)
            if cc.scope_of(Path(path)) == "test" or name.startswith("test"):
                return name
            if path.endswith(".rs"):
                src = (top / path).read_bytes().decode("utf-8", "replace")
                lines = cc.classify(path, src, "rust")
                if lines[int(line) - 1].test:
                    return name
    return None


def replacement(b: int, d: Diff, post_hays: list[Hay], top: Path) -> str | None:
    """How a removed guard block is replaced in the diff, or None."""
    text = block_text(d.pre, b)
    sents = [s for s in sentences(text) if len(TOKEN_RE.findall(s.lower())) >= MIN_CLAUSE]
    for s in [s for s in sents if recognise(s)[0]] or sents:
        if any(hay.cover(TOKEN_RE.findall(s.lower()))[0] >= GONE for hay in post_hays):
            return "surviving clause"
    added = "\n".join(
        d.post.prose[j] or "" for j in range(len(d.post.raw)) if not all(d.post_kept[j])
    )
    # ponytail: a pointer or test name added anywhere in the same file counts; tie it to
    # the removal site if agents start parking one unrelated pointer per file.
    if (m := POINTER_RE.search(added)) is not None:
        return f"pointer {m.group(0)}"
    if (name := names_test(top, added)) is not None:
        return f"named test {name}"
    start, end = d.pre.blocks[b]
    for i in range(max(0, start - LINT_WINDOW), min(len(d.pre.raw), end + LINT_WINDOW)):
        if d.pre.lines[i].kind == "code" and LINT_FORM_RE.search(d.pre.raw[i]):
            return "lint form"
    return None


def guard_findings(d: Diff, changed_post: list, post_hays: list[Hay], top: Path) -> list[Finding]:
    """CLN-01 carve-out, and the CLN-02 review list, for one file's base blocks."""
    out = []
    op_blocks: dict[int, set] = {}
    op_new: dict[int, int] = {}
    for i, toks in enumerate(d.pre.toks):
        if toks:
            op_blocks.setdefault(d.op_of_pre[i], set()).add(d.pre.blk[i])
    for o, (tag, _i1, _i2, j1, j2) in enumerate(d.ops):
        op_new[o] = 0 if tag == "equal" else sum(len(d.post.toks[j]) for j in range(j1, j2))
    for b, (start, end) in enumerate(d.pre.blocks):
        rows = [i for i in range(start, end) if d.pre.toks[i]]
        kept = sum(sum(d.pre_kept[i]) for i in rows)
        total = sum(len(d.pre.toks[i]) for i in rows)
        if not rows or kept == total:
            continue
        text = block_text(d.pre, b)
        rules = recognise(text)[1] + (["label"] if LABEL_RE.search(text) else [])
        if not rules:
            continue
        ops = {d.op_of_pre[i] for i in rows}
        at = d.post_line(start)
        before = firing(text)
        if kept >= MIN_MATCH or any(op_blocks.get(o) == {b} and op_new[o] for o in ops):
            post_rows = set()
            for o in ops:
                tag, i1, _i2, j1, j2 = d.ops[o]
                if tag == "equal":
                    post_rows |= {j1 + i - i1 for i in rows if d.op_of_pre[i] == o}
                else:
                    post_rows |= set(range(j1, j2))
            after = firing("\n".join(d.post.prose[j] for j in sorted(post_rows) if d.post.prose[j]))
            if after < before:
                out.append(
                    Finding(
                        d.path,
                        at,
                        "CLN-02",
                        f"review: guard block (base lines {start + 1}-{end}) was shortened and its recogniser-firing sentences fell from {before} to {after}; confirm every clause survived",
                        blocking=False,
                    )
                )
            continue
        mine = block_toks(d.pre, b)
        best = max(
            (
                (SequenceMatcher(None, mine, toks, autojunk=False).ratio(), path, line, ptext)
                for path, line, toks, ptext in changed_post
            ),
            default=(0.0, "", 0, ""),
        )
        if best[0] >= PAIR_RATIO:
            after = firing(best[3])
            if after < before:
                out.append(
                    Finding(
                        d.path,
                        at,
                        "CLN-02",
                        f"review: guard block (base lines {start + 1}-{end}) moved to {best[1]}:{best[2]} and its recogniser-firing sentences fell from {before} to {after}",
                        blocking=False,
                    )
                )
            continue
        if replacement(b, d, post_hays, top) is None:
            out.append(
                Finding(
                    d.path,
                    at,
                    "CLN-01",
                    f"guard block removed whole (base lines {start + 1}-{end}, fired {', '.join(rules)}) with no replacement in the diff: keep its clause, point at its home, name the test that fails on the edit, or show the lint that owns it",
                )
            )
    return out


def gone_findings(d: Diff) -> list[Finding]:
    """CLN-04: removed non-guard prose must not reappear elsewhere in the file. A clause that
    reappears in its own block's pair, the post block holding most of the block's kept words, was
    reordered, not moved; a block with no kept words has no pair and is searched everywhere."""
    ins = d.inserted()
    hays: dict[int, tuple[list, Hay]] = {}  # paired post block -> inserted tokens outside it
    out = []
    for r in removed_runs(d):
        if len(r["toks"]) < MIN_RUN:
            continue
        text = "\n".join(d.pre.prose[i] or "" for i in range(r["first"], r["last"] + 1))
        if recognise(text)[0] or LABEL_RE.search(text):
            continue
        own = d.pair.get(r["key"][0], -1)
        if own not in hays:
            rest = [(t, j) for t, j in ins if d.post.blk[j] != own]
            hays[own] = (rest, Hay([t for t, _ in rest]))
        rest, hay = hays[own]
        share, where = hay.cover(r["toks"])
        if share >= GONE:
            out.append(
                Finding(
                    d.path,
                    rest[where][1] + 1,
                    "CLN-04",
                    f"{share:.0%} of the prose removed at base line {r['first'] + 1} reappears here; a cleanup removes text, it does not move it within the file",
                )
            )
    return out


def relocation_findings(rel: str, top: Path, base: str, diffs: list[Diff]) -> list[Finding]:
    """CLN-04 landed and coexistence checks for one declared relocation record."""
    target = top / rel
    if not target.is_file():
        return [
            Finding(
                rel, 1, "CLN-04", "declared relocation record does not exist in the working tree"
            )
        ]
    post = target.read_bytes().decode("utf-8", "replace")
    hay = Hay(TOKEN_RE.findall(post.lower()))
    out = []
    landed_any = False
    for d in diffs:
        for r in removed_runs(d):
            if len(r["toks"]) < MIN_RUN:
                continue
            share = hay.cover(r["toks"])[0]
            landed_any = landed_any or share >= GONE
            _tag, _i1, _i2, j1, j2 = d.ops[r["key"][1]]
            claimed = any(Path(rel).name in (d.post.prose[j] or "") for j in range(j1, j2))
            if claimed and share < GONE:
                out.append(
                    Finding(
                        d.path,
                        d.post_line(r["first"]),
                        "CLN-04",
                        f"prose removed beside the new pointer to {rel} did not land there ({share:.0%} found)",
                    )
                )
    if not landed_any:
        out.append(
            Finding(
                rel,
                1,
                "CLN-04",
                "declared relocation record contains none of the prose the diff removes",
            )
        )
    pre = git(top, "show", f"{base}:{rel}", ok=(0, 128)).split("\n")
    lines = post.split("\n")
    added = []
    for tag, _i1, _i2, j1, j2 in SequenceMatcher(None, pre, lines, autojunk=False).get_opcodes():
        if tag in ("insert", "replace"):
            added += [*lines[j1:j2], ""]
    chunks, cur = [], []
    for line in added:
        if line.strip():
            cur.append(line)
        elif cur:
            chunks.append(cur)
            cur = []
    sources = []
    for d in diffs:
        rows = [j for j, ts in enumerate(d.post.toks) for _ in ts]
        sources.append((d, rows, Hay([t for ts in d.post.toks for t in ts])))
    for chunk in chunks:
        toks = TOKEN_RE.findall(" ".join(chunk).lower())
        if len(toks) < MIN_RUN:
            continue
        for d, rows, src in sources:
            share, at = src.cover(toks)
            if share >= GONE:
                out.append(
                    Finding(
                        d.path,
                        rows[at] + 1,
                        "CLN-04",
                        f"{share:.0%} of the text appended to {rel} still lives here; a pointer and the paraphrase it replaces never coexist",
                    )
                )
    return out


# --- LNK-05 -------------------------------------------------------------------------


def names_record(record: str, path: str) -> bool:
    """A pointer's path names record as a whole or as its trailing part, as LNK-02 resolves it."""
    tail = "/".join(x for x in PurePosixPath(path).parts if x not in (".", ".."))
    return record == tail or record.endswith("/" + tail)


def joined(s: Side, rows: range) -> tuple[str, list[int], list[int]]:
    """A block's prose joined as linkage_check reads it, URLs blanked: the text, and each prose
    line's offset in it and 1-based number."""
    keep = [i for i in rows if s.prose[i] is not None]
    text, starts = lc.join_block([(i + 1, s.prose[i], "") for i in keep])
    return lc.URL_RE.sub(lambda m: " " * len(m.group()), text), starts, [i + 1 for i in keep]


def paired_text(d: Diff, start: int, end: int) -> str:
    """The prose of every base block the diff aligns with post lines start..end."""
    # ponytail: pairs by line alignment only, so a pointer on a sentence split off to another
    # line reads as added; pair by token provenance if real cleanups trip it (--cites clears it).
    rows: set[int] = set()
    for tag, i1, i2, j1, j2 in d.ops:
        if not (j1 < end and start < j2) and not (j1 == j2 and start <= j1 <= end):
            continue
        if tag == "equal":
            rows |= {i1 + j - j1 for j in range(max(j1, start), min(j2, end))}
        else:
            rows |= set(range(i1, i2))
    blocks = sorted({d.pre.blk[i] for i in rows if d.pre.blk[i] >= 0})
    return " ".join(joined(d.pre, range(*d.pre.blocks[b]))[0] for b in blocks)


def pointer_findings(
    d: Diff, top: Path, globs: list[str] | None, relocs: set[str], cited: dict[str, list[str]]
) -> list[Finding]:
    """LNK-05: a record pointer the diff adds names a file its block named before the edit, a
    --relocation record or a --cites record. Pointers are linkage_check's LNK-02 grammar."""
    out = []
    for start, end in d.post.blocks:
        text, starts, lines = joined(d.post, range(start, end))
        before = None
        for m in lc.POINTER_RE.finditer(text):
            path = m["path"]
            if not lc.is_record(path, None if m["repo"] else top, globs):
                continue
            if before is None:
                before = paired_text(d, start, end)
            name = re.escape(PurePosixPath(path).name)
            if re.search(rf"(?<![\w.-]){name}(?![\w-])", before) or any(
                names_record(r, path) for r in relocs
            ):
                continue
            line = lines[bisect.bisect_right(starts, m.start()) - 1]
            if (rec := next((c for c in cited if names_record(c, path)), None)) is not None:
                cited[rec].append(f"{d.path}:{line}")
                continue
            out.append(
                Finding(
                    d.path,
                    line,
                    "LNK-05",
                    f"added pointer {m.group().rstrip('.-')} names a record this block did not name before the edit: "
                    "pass it with --relocation if the removed prose moved there, or with --cites if it "
                    "already holds the removed argument; otherwise delete the pointer",
                )
            )
    return out


# --- driver ---------------------------------------------------------------------------


def repo_rel(top: Path, r: str) -> str:
    return Path(os.path.relpath(Path(r).resolve(), top) if Path(r).is_absolute() else r).as_posix()


def is_source(p: Path) -> bool:
    return p.suffix in cc.EXT_LANG and not p.name.endswith((".min.js", ".d.ts"))


def check(
    root: Path, base: str, relocations: list[str], cites: list[str]
) -> tuple[list[Finding], int, str]:
    top = Path(git(root, "rev-parse", "--show-toplevel").strip())
    mb = git(top, "merge-base", base, "HEAD").strip()
    relocs = {repo_rel(top, r) for r in relocations}
    cited: dict[str, list[str]] = {}
    for c in (repo_rel(top, r) for r in cites):
        if c not in git(top, "ls-files", "-z", "--", c).split("\0"):
            raise UsageError(
                f"--cites {c}: git does not track it; a cited record is committed first"
            )
        cited[c] = []
    try:
        globs = lc.load_config(top, None).get("records")
        census_cfg = cc.load_config(top, None)
    except (lc.MissingInputError, ValueError) as e:
        raise UsageError(str(e)) from e
    changes = changed_paths(top, mb)
    found: list[Finding] = []
    diffs: list[Diff] = []
    langs: dict[str, str] = {}
    for status, path in changes:
        p = Path(path)
        if p.name in LOCKS or p.name in MANIFESTS:
            found.append(
                Finding(path, 1, "CLN-05", "a cleanup never touches a lock or manifest file")
            )
        elif cc.scope_of(p) == "test":
            found.append(Finding(path, 1, "CLN-05", "a cleanup never touches a test file"))
        elif GOLDEN_DIRS.intersection(part.lower() for part in p.parts[:-1]):
            found.append(
                Finding(path, 1, "CLN-05", "a cleanup never touches a golden or snapshot file")
            )
        elif path in relocs and not is_source(p):
            if status == "D":
                found.append(
                    Finding(
                        path,
                        1,
                        "CLN-05",
                        "a relocation record is append-only, and this one was deleted",
                    )
                )
            elif status != "A":
                pre = git(top, "show", f"{mb}:{path}").split("\n")
                post = (top / path).read_bytes().decode("utf-8", "replace").split("\n")
                for tag, i1, _i2, j1, _j2 in SequenceMatcher(
                    None, pre, post, autojunk=False
                ).get_opcodes():
                    if tag in ("delete", "replace"):
                        found.append(
                            Finding(
                                path,
                                j1 + 1,
                                "CLN-05",
                                f"a relocation record is append-only, and base line {i1 + 1} was changed or removed",
                            )
                        )
                        break
        elif not is_source(p):
            found.append(
                Finding(
                    path,
                    1,
                    "CLN-05",
                    "a cleanup touches only source files; declare the record prose moved to with --relocation",
                )
            )
        else:
            pre_src = git(top, "show", f"{mb}:{path}") if status != "A" else ""
            post_src = (top / path).read_bytes().decode("utf-8", "replace") if status != "D" else ""
            head = "\n".join((pre_src or post_src).split("\n", 6)[:6])
            if GENERATED_RE.search(head):
                found.append(Finding(path, 1, "CLN-05", "a cleanup never touches a generated file"))
            elif status in ("A", "D"):
                found.append(
                    Finding(
                        path,
                        1,
                        "CLN-03",
                        f"a cleanup never {'adds' if status == 'A' else 'deletes'} a source file",
                    )
                )
            else:
                lang = cc.EXT_LANG[p.suffix]
                langs[path] = lang
                iface = cc.interface_file(census_cfg["interface_files"], path)
                pre, post = (side(path, x, lang, iface) for x in (pre_src, post_src))
                diffs.append(align(path, pre, post))
    changed_post = []
    for d in diffs:
        for b, (start, end) in enumerate(d.post.blocks):
            if any(not all(d.post_kept[j]) for j in range(start, end)):
                changed_post.append(
                    (d.path, start + 1, block_toks(d.post, b), block_text(d.post, b))
                )
    post_hays = [Hay([t for toks in d.post.toks for t in toks]) for d in diffs]
    for d in diffs:
        found += frozen_findings(d, langs[d.path])
        found += guard_findings(d, changed_post, post_hays, top)
        found += gone_findings(d)
        found += pointer_findings(d, top, globs, relocs, cited)
    for rel in sorted(relocs):
        found += relocation_findings(rel, top, mb, diffs)
    if not any(f.blocking for f in found):
        found += [
            Finding(
                rec,
                1,
                "LNK-05",
                f"review: --cites record, pointed at from {', '.join(sites) or 'no added pointer'}; "
                "confirm it holds, in its own words, the argument each pointer replaced",
                blocking=False,
            )
            for rec, sites in sorted(cited.items())
        ]
    return found, len(changes), mb


def render(found: list[Finding], fmt: str) -> None:
    found = sorted(found, key=lambda f: (f.path, f.line))
    if fmt == "json":
        print(json.dumps([f.__dict__ for f in found], indent=1))
        return
    for f in found:
        print(f"{f.path}:{f.line}: {f.rule} {f.message}")


# --- self-test --------------------------------------------------------------------------

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "cleanup_check"


def run_case(case: Path) -> list[Finding]:
    """Commit before/ as the base, lay after/ over it as the working tree, check."""
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp)
        cc.copy_fixture(case / "before", repo)
        git(repo, "init", "-q")
        git(repo, "add", "-A")
        tree = git(repo, "write-tree").strip()
        env = {k: "fixture" for k in ("GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME")}
        env |= {k: "fixture@example.invalid" for k in ("GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL")}
        commit = (
            subprocess.run(  # noqa: S603
                ["git", "-C", str(repo), "commit-tree", "--no-gpg-sign", tree, "-m", "base"],  # noqa: S607
                capture_output=True,
                check=True,
                env=os.environ | env,
            )
            .stdout.decode()
            .strip()
        )
        git(repo, "update-ref", "HEAD", commit)
        for f in repo.iterdir():
            if f.name != ".git":
                shutil.rmtree(f) if f.is_dir() else f.unlink()
        cc.copy_fixture(case / "after", repo)
        git(repo, "read-tree", "HEAD")  # an index with no stat data compares every file by content
        relocs, cites = (
            (case / f).read_text().split() if (case / f).exists() else []
            for f in ("relocations", "cites")
        )
        return check(repo, "HEAD", relocs, cites)[0]


def self_test() -> int:
    bad = 0
    cases = sorted(p for p in FIXTURES.iterdir() if p.is_dir())
    with tempfile.TemporaryDirectory() as tmp:
        # A check's own bytecode is no edit. Built here, because a shipped __pycache__ is gitignored.
        pyc = cc.copy_fixture(FIXTURES / "doctest-kept", Path(tmp) / "pycache-ignored")
        (pyc / "after" / "src" / "__pycache__").mkdir()
        (pyc / "after" / "src" / "__pycache__" / "lib.cpython-311.pyc").write_bytes(b"\0")
        cases.append(pyc)
        for case in cases:
            want = set((case / "expect").read_text().split())
            try:
                found = run_case(case)
            except UsageError as e:  # main() exits 2
                found = [Finding(case.name, 1, "exit-2", str(e))]
            got = {f.rule if f.blocking else f"review:{f.rule}" for f in found}
            if got != want:
                bad += 1
                print(
                    f"self-test: {case.name} expected {sorted(want) or 'clean'}, got {sorted(got) or 'clean'}"
                )
                for f in found:
                    print(f"  {f.path}:{f.line}: {f.rule} {f.message}")
    print(f"self-test: {'ok' if not bad else 'FAILED'}, {len(cases)} cases, {bad} mismatches")
    return 1 if bad else 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", default=".")
    ap.add_argument(
        "--base",
        help="ref the cleanup branched from, such as origin/main; the working tree is diffed against it, "
        "so every commit since BASE is in scope",
    )
    ap.add_argument(
        "--relocation",
        action="append",
        default=[],
        metavar="PATH",
        help="repo-relative record the diff moves prose to (repeatable)",
    )
    ap.add_argument(
        "--cites",
        action="append",
        default=[],
        metavar="PATH",
        help="repo-relative tracked record that already holds an argument the diff removes, "
        "named by a pointer the diff adds (repeatable; listed for review)",
    )
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    root = Path(a.root)
    if not a.base or not root.is_dir():
        print(
            "usage: cleanup_check.py --base REF [--root DIR]; missing --base or no such directory",
            file=sys.stderr,
        )
        return 2
    try:
        found, n, mb = check(root.resolve(), a.base, a.relocation, a.cites)
    except UsageError as e:
        print(f"missing input: {e}", file=sys.stderr)
        return 2
    render(found, a.format)
    blocking = sum(f.blocking for f in found)
    print(
        f"cleanup_check: {n} changed paths against {mb[:12]}, {blocking} blocking, {len(found) - blocking} review",
        file=sys.stderr,
    )
    return 1 if blocking else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
