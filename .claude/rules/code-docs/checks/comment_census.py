#!/usr/bin/env python3
"""Comment census for source trees: density, block lengths, caps and the ratchet.

Rules covered (code-docs length group):
  LEN-01  a plain comment block runs at most 5 lines
  LEN-02  a doc block runs at most 10 lines, or 15 for a public item in a
          library-kind package
  LEN-03  per file, the prod comment lines in over-cap blocks never rise above
          the checked-in baseline
  LEN-04  ratio and char ratio are context beside the human bands, never gated
  LEN-07  signature-restating sections do not count toward a doc cap
  LEN-08  interface doc blocks count toward the doc cap, as public items

Classifies every line of every source file as code, doc comment, plain
comment, interface doc, license header, tool directive or blank, then reports
per group. Comment density is (doc + plain comment lines) / code lines.

Buckets the ratio leaves out, because they are not prose for a maintainer:
  interface  doc text that renders into a user surface: Rust items deriving
             clap Parser/Args/Subcommand/ValueEnum or schemars JsonSchema,
             Python click/typer command docstrings and pydantic model docstrings,
             and every doc comment in a file under interface_files
  license    the first comment block of a file naming a copyright or licence
  directive  tool pragmas: noqa, eslint-disable, //go:build, nolint, @ts-ignore
  trailing   a comment sharing its line with code (the line counts as code)

Scopes: prod, test (test paths, Rust #[cfg(test)] items, Go testdata) and
other (examples, benches, fuzz, docs, scripts, xtask). Generated files
(@generated, "Code generated ... DO NOT EDIT"), vendored trees, and installed
agent rules and skills (a rules/ or skills/ directory inside a top-level dot
directory such as .claude/, where this rule's own checks and fixtures sit) are
skipped.

A block is the consecutive comment lines of one kind (doc, plain or interface)
with only blank lines between them. The blank lines do not count, and a block
never crosses a test/prod boundary. --split-at-blank restores the old
definition (a run of doc or plain lines, split at every blank line) for
--list-blocks, --sample and the census table.

Caps and package kind come from .code-docs.json at the root, or --config:
  {"library": ["crates/sdk", "src/pkg"],
   "caps": {"plain": 5, "doc": 10, "library_public": 15},
   "strip_sections": true, "interface_files": ["src/schema/*.ts"]}
"library" lists the paths of library-kind packages; anything undeclared is
app-kind. "interface_files" globs the sources a generator the census cannot
see renders, such as a TS-to-JSON-Schema script's types: their doc comments
count as interface lines. "strip_sections": false declines LEN-07; take
library_public 20 with it. Public means reachable from outside the package: Rust pub but not
pub(crate) or pub(super); a Python name with no leading underscore, under no
private class or function; a TS/JS export or a member of one; a Go capitalised
identifier; Java public or an interface member; Kotlin public or no modifier.
An interface item is public. An item the detector cannot place, such as a
module doc, gets its package kind's tier.

Usage:
  comment_census.py [--root DIR] [--group package|lang|top|none] [--format text|json]
  comment_census.py --root DIR --list-blocks --min-block 20 [--kind doc|line|interface|any]
  comment_census.py --root DIR --sample 40 --min-block 1 --seed 7
  comment_census.py --root DIR --over-cap
  comment_census.py --root DIR --check BASELINE
  comment_census.py --root DIR --update BASELINE [--allow-regression]
  comment_census.py --root DIR --report
  comment_census.py --self-test

--report also notes each package not declared under "library" that looks
published: a pyproject.toml with [build-system], a package.json with a name and
no "private": true, or a Cargo.toml with a lib target and no publish = false.

--check fails on any file whose over-cap lines rose above its baseline entry
(an absent key counts as 0), and on a baseline key whose file still exists but
was not measured. --update writes drops freely and refuses a rise unless
--allow-regression is given, which prints every raised key.

Exit codes: 0 success or clean, 1 findings (over-cap blocks, a ratchet rise,
a refused update), 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import ast
import contextlib
import fnmatch
import io
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import tokenize
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import NamedTuple

EXT_LANG = {
    ".rs": "rust",
    ".go": "go",
    ".py": "python",
    ".ts": "ts",
    ".tsx": "ts",
    ".mts": "ts",
    ".cts": "ts",
    ".js": "js",
    ".jsx": "js",
    ".mjs": "js",
    ".cjs": "js",
    # single-file components: comments in their script and style blocks; HTML comments are not seen
    ".vue": "ts",
    ".svelte": "ts",
    ".astro": "ts",
    ".java": "java",
    ".kt": "kotlin",
    ".kts": "kotlin",
    ".swift": "swift",
    ".cs": "csharp",
    ".scala": "scala",
    ".c": "c",
    ".h": "c",
    ".cc": "cpp",
    ".cpp": "cpp",
    ".cxx": "cpp",
    ".hpp": "cpp",
    ".hh": "cpp",
}
SKIP_DIRS = {
    ".git",
    "node_modules",
    "target",
    "vendor",
    "_vendor",
    "vendored",
    "third_party",
    "third-party",
    "external",
    ".venv",
    "venv",
    "dist",
    "build",
    "out",
    "__pycache__",
    ".tox",
    ".worktrees",
    "worktrees",
    ".next",
    "coverage",
}
TEST_DIRS = {
    "tests",
    "test",
    "__tests__",
    "testdata",
    "fixtures",
    "fixture",
    "spec",
    "e2e",
    "testing",
    "test-utils",
}
OTHER_DIRS = {
    "examples",
    "example",
    "benches",
    "bench",
    "benchmarks",
    "fuzz",
    "docs",
    "doc",
    "scripts",
    "xtask",
    "tools",
    ".claude",
    ".github",
    ".agents",
    ".devcontainer",
}
TEST_FILE_RE = re.compile(
    r"(_test\.go|^test_.*\.py|_test\.py|^conftest\.py|\.(test|spec)\.[cm]?[jt]sx?|Tests?\.(java|kt)|^tests?\.rs|_tests?\.rs)$"
)
MANIFESTS = (
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
)
GENERATED_RE = re.compile(
    r"@generated|Code generated .* DO NOT EDIT|auto-generated|autogenerated by|DO NOT EDIT",
    re.IGNORECASE,
)
LICENSE_RE = re.compile(
    r"copyright|licen[cs]ed under|spdx-license-identifier|all rights reserved|permission is hereby granted",
    re.IGNORECASE,
)
DIRECTIVE_RE = re.compile(
    r"^(noqa|type:\s*ignore|pragma|pylint:|mypy:|fmt:\s*(off|on|skip)|isort:|ruff:|pyright:|eslint|@ts-|prettier-ignore|"
    r"istanbul |c8 |v8 ignore|nolint|go:|\+build|NOLINT|clang-format|cspell:|biome-ignore|deno-lint|deno-fmt|jshint|tslint:|"
    r"#?region\b|#?endregion\b|swiftlint:|ktlint|@formatter:|noinspection|language=|rustfmt::|-\*-|coverage:|codespell:)",
    re.IGNORECASE,
)
RUST_IFACE_RE = re.compile(
    r"#\[(?:cfg_attr\(.*?,\s*)?derive\([^)]*\b(Parser|Args|Subcommand|ValueEnum|JsonSchema)\b"
)
PY_IFACE_DECOR_RE = re.compile(r"(\.command|\.group|\.callback|click\.)")

KINDS = ("code", "doc", "line", "interface", "license", "directive", "blank")


@dataclass
class Line:
    code: bool = False
    doc: bool = False
    com: bool = False  # non-doc comment text present
    text: str = ""  # comment text on this line, markers stripped
    depth: int = 0  # brace depth at line start (C-like only)
    delta: int = 0  # net brace change on this line
    opens: int = 0
    kind: str = "blank"
    trailing: bool = False
    test: bool = False


# --- C-like lexer -----------------------------------------------------------

_TOK = {
    "rust": re.compile(r'//|/\*|(?<![A-Za-z0-9_])b?r(#*)"|"|\'|\{|\}'),
    "go": re.compile(r'//|/\*|"|\'|`|\{|\}'),
    "js": re.compile(r'//|/\*|"|\'|`|\{|\}'),
    "kotlin": re.compile(r'//|/\*|"""|"|\'|\{|\}'),
    "java": re.compile(r'//|/\*|"""|"|\'|\{|\}'),
    "swift": re.compile(r'//|/\*|"""|"|\{|\}'),
    "scala": re.compile(r'//|/\*|"""|"|\'|\{|\}'),
    "c": re.compile(r'//|/\*|"|\'|\{|\}'),
}
_RUST_CHAR = re.compile(r"'(\\u\{[0-9A-Fa-f]+\}|\\.|[^\\'\n])'")


def _clike_doc_line(lang: str, rest: str) -> bool | None:
    """rest starts after '//'. Returns True doc, False plain, None directive-like code."""
    if lang in ("ts", "js"):
        if rest.startswith("/ <"):
            return None
        return False
    if lang in ("rust", "c", "cpp", "swift", "csharp"):
        return (rest.startswith("/") and not rest.startswith("//")) or rest.startswith("!")
    return False


def lex_clike(src: str, lang: str) -> list[Line]:
    fam = {"ts": "js", "cpp": "c", "csharp": "c"}.get(lang, lang)
    tok = _TOK[fam]
    out: list[Line] = []
    state: tuple = ("code",)
    depth = 0
    for raw in src.split("\n"):
        ln = Line(depth=depth)
        texts: list[str] = []
        i, n = 0, len(raw)
        while i < n:
            if state[0] == "block":
                _, bdepth, bdoc = state
                j = raw.find("*/", i)
                k = raw.find("/*", i) if lang == "rust" else -1
                if k != -1 and (j == -1 or k < j):
                    texts.append(raw[i:k])
                    state = ("block", bdepth + 1, bdoc)
                    i = k + 2
                    continue
                seg = raw[i : j if j != -1 else n]
                texts.append(seg)
                if bdoc:
                    ln.doc = True
                else:
                    ln.com = True
                if j == -1:
                    i = n
                elif bdepth > 1:
                    state = ("block", bdepth - 1, bdoc)
                    i = j + 2
                else:
                    state = ("code",)
                    i = j + 2
                continue
            if state[0] == "str":
                delim = state[1]
                ln.code = True
                if delim == "raw":
                    end = '"' + "#" * state[2]
                    j = raw.find(end, i)
                    if j == -1:
                        i = n
                    else:
                        state, i = ("code",), j + len(end)
                    continue
                j = i
                closed = False
                while j < n:
                    c = raw[j]
                    if c == "\\" and not (delim == "`" and fam == "go"):
                        j += 2
                        continue
                    if raw.startswith(delim, j):
                        closed = True
                        j += len(delim)
                        break
                    j += 1
                if closed:
                    state = ("code",)
                i = min(j, n)
                continue
            m = tok.search(raw, i)
            pre = raw[i : m.start() if m else n]
            if pre.strip():
                ln.code = True
            if not m:
                break
            t = m.group(0)
            i = m.end()
            if t == "//":
                rest = raw[i:]
                d = _clike_doc_line(lang, rest)
                if d is None:
                    ln.code = True
                else:
                    body = rest.lstrip("/!").strip()
                    texts.append(body)
                    if ln.code:
                        ln.trailing = True
                    elif d:
                        ln.doc = True
                    else:
                        ln.com = True
                break
            if t == "/*":
                rest = raw[i:]
                isdoc = (
                    rest.startswith("*") and not rest.startswith("*/") and not rest.startswith("**")
                ) or (rest.startswith("!") and lang in ("rust", "c", "cpp"))
                if lang == "go":
                    isdoc = False
                if ln.code:
                    # comment after code on the same line: still trailing until it closes
                    j = raw.find("*/", i)
                    if j != -1:
                        ln.trailing = True
                        i = j + 2
                        continue
                state = ("block", 1, isdoc)
                if isdoc:
                    ln.doc = True
                    i += 1
                else:
                    ln.com = True
                continue
            if t in ('"', '"""', "`"):
                ln.code = True
                state = ("str", t)
                continue
            if t == "'":
                ln.code = True
                if lang == "rust":
                    mm = _RUST_CHAR.match(raw, i - 1)
                    if mm:
                        i = mm.end()
                    continue
                state = ("str", "'")
                continue
            if t == "{":
                ln.code = True
                ln.delta += 1
                ln.opens += 1
                continue
            if t == "}":
                ln.code = True
                ln.delta -= 1
                continue
            # rust raw string
            ln.code = True
            state = ("str", "raw", len(m.group(1) or ""))
        if (
            state[0] == "str"
            and state[1] in ("'", '"')
            and fam != "rust"
            and not raw.endswith("\\")
        ):
            state = ("code",)
        ln.text = " ".join(t.strip() for t in texts).strip()
        depth += ln.delta
        out.append(ln)
    if lang == "go":
        _go_doc(out, src.split("\n"))
    return out


_GO_DECL = re.compile(r"^\s*(func|type|var|const|package)\b|^\s+[A-Z]\w*(\s|$)")


def _go_doc(lines: list[Line], raw: list[str]) -> None:
    """A Go comment block directly above a declaration or exported field is a doc comment."""
    i = 0
    while i < len(lines):
        if lines[i].com and not lines[i].code:
            j = i
            while j < len(lines) and lines[j].com and not lines[j].code:
                j += 1
            if j < len(lines) and lines[j].code and _GO_DECL.match(raw[j]):
                for k in range(i, j):
                    lines[k].doc, lines[k].com = True, False
            i = j
        else:
            i += 1


# --- Python -------------------------------------------------------------------


def _docstring(node: ast.AST) -> ast.Expr | None:
    body = getattr(node, "body", None)
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        return body[0]
    return None


def lex_python(src: str) -> list[Line]:
    raw = src.split("\n")
    out = [Line() for _ in raw]
    try:
        toks = list(tokenize.generate_tokens(io.StringIO(src).readline))
    except (tokenize.TokenError, SyntaxError, IndentationError):
        toks = []
    doc_lines: set[int] = set()
    iface_lines: set[int] = set()
    try:
        tree = ast.parse(src)
        for node in ast.walk(tree):
            if not isinstance(
                node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)
            ):
                continue
            ds = _docstring(node)
            if ds is None:
                continue
            rng = range(ds.lineno - 1, (ds.end_lineno or ds.lineno))
            iface = False
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                iface = any(PY_IFACE_DECOR_RE.search(ast.unparse(d)) for d in node.decorator_list)
            if isinstance(node, ast.ClassDef):
                iface = any("BaseModel" in ast.unparse(b) for b in node.bases)
            (iface_lines if iface else doc_lines).update(rng)
    except (SyntaxError, ValueError, RecursionError):
        pass
    for t in toks:
        r = t.start[0] - 1
        if r >= len(out):
            continue
        if t.type == tokenize.COMMENT:
            body = t.string.lstrip("#").strip()
            out[r].text = (out[r].text + " " + body).strip()
            if out[r].code:
                out[r].trailing = True
            else:
                out[r].com = True
        elif t.type not in (
            tokenize.NL,
            tokenize.NEWLINE,
            tokenize.INDENT,
            tokenize.DEDENT,
            tokenize.ENDMARKER,
        ):
            for rr in range(t.start[0] - 1, t.end[0]):
                if rr < len(out) and rr not in doc_lines and rr not in iface_lines:
                    out[rr].code = True
                    if out[rr].com:
                        out[rr].com, out[rr].trailing = False, True
    for r in doc_lines:
        if r < len(out):
            out[r].doc = True
            out[r].text = raw[r].strip().strip("\"'")
    for r in iface_lines:
        if r < len(out):
            out[r].kind = "interface"
    return out


# --- classification ---------------------------------------------------------


def classify(path: str, src: str, lang: str, interface: bool = False) -> list[Line]:
    """Every line's kind. interface: the file is under interface_files, so its prod doc lines
    render through a generator the census cannot see (LEN-08)."""
    lines = lex_python(src) if lang == "python" else lex_clike(src, lang)
    raw = src.split("\n")
    # base kinds
    for idx, ln in enumerate(lines):
        if ln.kind == "interface":
            continue
        if ln.code:
            ln.kind = "code"
        elif ln.doc:
            ln.kind = "doc"
        elif ln.com:
            ln.kind = "line"
        else:
            ln.kind = "blank"
        if ln.kind in ("doc", "line") and (
            DIRECTIVE_RE.match(ln.text) or (idx == 0 and raw[0].startswith("#!"))
        ):
            ln.kind = "directive"
    if lang == "python":
        _pep723(lines, raw)
    # license header: first comment block within the first 3 non-blank lines
    first = next((i for i, ln in enumerate(lines) if ln.kind != "blank"), None)
    if first is not None and first < 5 and lines[first].kind in ("doc", "line", "directive"):
        j = first
        while j < len(lines) and lines[j].kind in (lines[first].kind, "directive"):
            j += 1
        block = " ".join(lines[k].text for k in range(first, j))
        if LICENSE_RE.search(block):
            for k in range(first, j):
                if lines[k].kind in ("doc", "line"):
                    lines[k].kind = "license"
    if lang == "rust":
        _rust_regions(lines, raw)
    for ln in lines if interface else ():
        if ln.kind == "doc" and not ln.test:
            ln.kind = "interface"
    return lines


def _pep723(lines: list[Line], raw: list[str]) -> None:
    """PEP 723 inline script metadata (# /// script ... # ///) is tool input, not prose."""
    inside = False
    for i, text in enumerate(raw):
        t = text.rstrip()
        if not inside and t.startswith("# /// "):
            inside = True
        if inside and lines[i].kind == "line":
            lines[i].kind = "directive"
        if inside and t == "# ///":
            inside = False


def _item_end(lines: list[Line], raw: list[str], start: int) -> int:
    """Index of the line closing the item that starts at or after start."""
    base = lines[start].depth
    opened = False
    for k in range(start, len(lines)):
        opened = opened or lines[k].opens > 0
        if opened and lines[k].depth + lines[k].delta <= base:
            return k
        if not opened and raw[k].rstrip().endswith(";"):
            return k
    return len(lines) - 1


def _rust_regions(lines: list[Line], raw: list[str]) -> None:
    for i, text in enumerate(raw):
        s = text.strip()
        if s.startswith("#[cfg(test)]") or s.startswith("#[cfg(all(test"):
            end = _item_end(lines, raw, i)
            for k in range(i, end + 1):
                lines[k].test = True
        elif lines[i].kind == "code" and RUST_IFACE_RE.search(s):
            top = i
            while top > 0 and (
                lines[top - 1].kind in ("doc", "directive") or raw[top - 1].strip().startswith("#[")
            ):
                top -= 1
            end = _item_end(lines, raw, i)
            for k in range(top, end + 1):
                if lines[k].kind == "doc":
                    lines[k].kind = "interface"


# --- walking ------------------------------------------------------------------


def list_files(root: Path) -> list[Path]:
    git = shutil.which("git")
    try:
        if git is None:
            raise FileNotFoundError("git")
        # S603: argv is the resolved git binary and the caller's own root.
        res = subprocess.run(  # noqa: S603
            [git, "-C", str(root), "ls-files", "-z"], capture_output=True, check=True
        )
        rels = [p for p in res.stdout.decode("utf-8", "replace").split("\0") if p]
        files = [root / p for p in rels]
        if not files:
            # an untracked tree inside a work tree lists nothing; walk it instead
            raise FileNotFoundError(root)
    except (subprocess.CalledProcessError, FileNotFoundError):
        files = []
        for dp, dns, fns in os.walk(root):
            dns[:] = [d for d in dns if d not in SKIP_DIRS]
            files += [Path(dp) / f for f in fns]
    keep = []
    for f in files:
        rel = f.relative_to(root)
        if any(part in SKIP_DIRS for part in rel.parts[:-1]):
            continue
        if rel.parts[0].startswith(".") and {"rules", "skills"} & set(rel.parts[1:-1]):
            continue  # installed agent config: .claude/rules/code-docs/checks/ and the like
        if f.suffix in EXT_LANG and not f.name.endswith((".min.js", ".d.ts")) and f.is_file():
            keep.append(f)
    return keep


def scope_of(rel: Path) -> str:
    parts = [p.lower() for p in rel.parts[:-1]]
    if (
        TEST_FILE_RE.search(rel.name)
        or any(p in TEST_DIRS for p in parts)
        or "src/test" in rel.as_posix()
    ):
        return "test"
    if any(p in OTHER_DIRS for p in parts):
        return "other"
    return "prod"


def package_of(root: Path, f: Path, cache: dict) -> str:
    d = f.parent
    while True:
        if d in cache:
            return cache[d]
        if any((d / m).exists() for m in MANIFESTS) or d == root:
            name = d.relative_to(root).as_posix() or "."
            cache[d] = name
            return name
        d = d.parent


def _pct(v: list[int], p: float) -> int:
    return v[min(len(v) - 1, int(p * len(v)))]


@dataclass
class Agg:
    files: int = 0
    n: dict = field(
        default_factory=lambda: (
            {k: 0 for k in KINDS} | {"trailing": 0, "code_chars": 0, "comment_chars": 0}
        )
    )
    blocks: dict = field(default_factory=lambda: {"doc": [], "line": [], "interface": []})

    def ratio(self) -> float:
        return (
            round((self.n["doc"] + self.n["line"]) / self.n["code"], 3) if self.n["code"] else 0.0
        )

    def summary(self) -> dict:
        out = {"files": self.files, **self.n, "ratio": self.ratio()}
        out["ratio_chars"] = (
            round(self.n["comment_chars"] / self.n["code_chars"], 3)
            if self.n["code_chars"]
            else 0.0
        )
        for k, lengths in self.blocks.items():
            v = sorted(lengths)
            if not v:
                out[f"{k}_blocks"] = {"count": 0}
                continue
            out[f"{k}_blocks"] = {
                "count": len(v),
                "p50": _pct(v, 0.5),
                "p90": _pct(v, 0.9),
                "p99": _pct(v, 0.99),
                "max": v[-1],
                "gt3": sum(x > 3 for x in v),
                "gt5": sum(x > 5 for x in v),
                "gt10": sum(x > 10 for x in v),
                "gt20": sum(x > 20 for x in v),
                "lines_in_gt5": sum(x for x in v if x > 5),
            }
        return out


def blocks_of(lines: list[Line]):
    """Yield (kind, start_index, length) for runs of same-kind doc or plain lines.

    The old definition: it splits at every blank line and skips interface
    lines. comment_blocks() is the LEN-01 definition the caps use."""
    i = 0
    while i < len(lines):
        k = lines[i].kind
        if k in ("doc", "line"):
            j = i
            while j < len(lines) and lines[j].kind == k and lines[j].test == lines[i].test:
                j += 1
            yield k, i, j - i
            i = j
        else:
            i += 1


@dataclass
class Block:
    kind: str
    start: int  # index of the first comment line
    end: int  # index after the last comment line
    n: int  # comment lines; the blank lines between them do not count
    test: bool
    stripped: int = 0  # LEN-07 section lines left out of the count
    public: bool | None = None  # None: the detector could not place the item
    cap: int = 0

    @property
    def counted(self) -> int:
        return self.n - self.stripped


BLOCK_KINDS = ("doc", "line", "interface")


def _inner_doc(s: str) -> bool:
    return s.lstrip().startswith(("//!", "/*!"))


def comment_blocks(lines: list[Line], raw: list[str] | None = None) -> list[Block]:
    """Blocks per LEN-01: consecutive comment lines of one kind with only blank lines between them.
    Given the raw lines, a module doc (//!) and an item doc (///) across a blank line stay apart:
    they document different items."""
    out: list[Block] = []
    cur: Block | None = None
    for i, ln in enumerate(lines):
        if ln.kind in BLOCK_KINDS:
            other_item = raw is not None and _inner_doc(raw[i]) != _inner_doc(
                raw[cur.end - 1] if cur else ""
            )
            if (
                cur
                and cur.kind == ln.kind
                and cur.test == ln.test
                and not (cur.end < i and other_item)
            ):
                cur.end, cur.n = i + 1, cur.n + 1
            else:
                cur = Block(ln.kind, i, i + 1, 1, ln.test)
                out.append(cur)
        elif ln.kind != "blank":
            cur = None
    return out


def _legacy_blocks(lines: list[Line]) -> list[Block]:
    return [Block(k, s, s + n, n, lines[s].test) for k, s, n in blocks_of(lines)]


def _group_of(group: str, root: Path, f: Path, cache: dict) -> str:
    if group == "package":
        return package_of(root, f, cache)
    if group == "lang":
        return EXT_LANG[f.suffix]
    rel = f.relative_to(root)
    if group == "top":
        return rel.parts[0] if len(rel.parts) > 1 else "."
    return "all"


def interface_file(globs: list[str], rel: str) -> bool:
    return any(fnmatch.fnmatchcase(rel, g) for g in globs)


def census(root: Path, group: str, legacy: bool = False, interface_files: list[str] | None = None):
    aggs: dict[tuple, Agg] = {}
    per_file = []
    cache: dict = {}
    for f in list_files(root):
        try:
            src = f.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if GENERATED_RE.search("\n".join(src.split("\n", 6)[:6])):
            continue
        lang = EXT_LANG[f.suffix]
        rel = f.relative_to(root)
        fscope = scope_of(rel)
        lines = classify(
            rel.as_posix(), src, lang, interface_file(interface_files or [], rel.as_posix())
        )
        g = _group_of(group, root, f, cache)
        raw = src.split("\n")
        seen = set()
        for idx, ln in enumerate(lines):
            sc = "test" if ln.test else fscope
            key = (g, sc)
            a = aggs.setdefault(key, Agg())
            if key not in seen:
                a.files += 1
                seen.add(key)
            a.n[ln.kind] += 1
            if ln.trailing:
                a.n["trailing"] += 1
            if ln.kind == "code":
                a.n["code_chars"] += len(raw[idx].strip())
            elif ln.kind in ("doc", "line"):
                a.n["comment_chars"] += len(raw[idx].strip())
        for b in _legacy_blocks(lines) if legacy else comment_blocks(lines, raw):
            sc = "test" if b.test else fscope
            aggs[(g, sc)].blocks[b.kind].append(b.n)
        per_file.append((rel.as_posix(), fscope, lines, raw))
    return aggs, per_file


# --- LEN-07: structured sections ------------------------------------------------

_FENCE = ("```", "~~~")
# Per family: (any section heading, the headings whose section LEN-07 leaves out).
# Safety, Undefined behavior, Implementation notes, Note: and WARNING stay
# counted: a heading that bought length would be a marker exemption.
_HEADS = {
    "rust": (
        re.compile(r"#{1,6}\s"),
        re.compile(r"#{1,6}\s*(Errors|Panics|Examples?|Aborts)(\s*\([^)]*\))?\s*$"),
    ),
    "python": (
        re.compile(r"[A-Z][\w ]*:\s*$"),
        re.compile(r"(Args|Arguments|Returns|Raises|Yields|Attributes):\s*$"),
    ),
    "ts": (re.compile(r"@\w"), re.compile(r"@(param|returns?|throws|example)\b")),
    "java": (re.compile(r"@\w"), re.compile(r"@(param|return|throws|see)\b")),
}
_REST_FIELD = re.compile(r":(param|type|returns?|rtype|raises?)\b")
_MARKS = ("///", "//!", "/**", "/*!", "/*", "*/", "*", '"""', "'''", "//")


def _doc_text(raw: str) -> str:
    s = raw.strip()
    for m in _MARKS:
        if s.startswith(m):
            return s[len(m) :].strip()
    return s


# Python example forms, per docstring row. Section-end rules, kept simple:
#   - an Example:/Examples: section, or the literal block after a line ending in '::', ends at
#     the first non-blank line indented no deeper than its header, or at the next Google section
#     header (napoleon's names), as napoleon and reST read them;
#   - a >>> doctest runs through its output and ends at a blank line, as doctest reads it;
#   - the closing quotes end all three, and blank lines after the last body line stay prose.
_PY_SECTION = re.compile(
    r"(Args|Arguments|Attention|Attributes|Caution|Danger|Error|Examples?|Hint|Important|"
    r"Keyword Args|Keyword Arguments|Methods|Notes?|Other Parameters|Parameters|Returns?|"
    r"Raises?|References|See Also|Tip|Todo|Warnings?|Warns|Yields?):\s*$"
)


def py_examples(rows: list[str]) -> list[str]:
    """Per raw docstring line: "head" for an Example:/Examples: heading, "body" for a line of an
    example section, literal block or doctest, "" for prose. LEN-07 leaves both out of the count;
    CLN-03 freezes the body."""
    out = [""] * len(rows)
    head, last, doctest = -1, -1, False  # head: indent of the open section's header
    for i, raw in enumerate(rows):
        s, t = raw.strip(), _doc_text(raw)
        indent, closing = len(raw) - len(raw.lstrip()), s in ('"""', "'''")
        if head >= 0:
            if not s or (indent > head and not closing and not _PY_SECTION.match(t)):
                out[i], last = "body", i if s else last
                continue
            out[last + 1 : i] = [""] * (i - last - 1)
            head = -1
        if doctest and s and not closing:
            out[i] = "body"
            continue
        doctest = t.startswith(">>>")
        if doctest:
            out[i] = "body"
        elif t.endswith("::") or re.match(r"Examples?:\s*$", t):
            head, last = indent, i
            out[i] = "" if t.endswith("::") else "head"
    if head >= 0:
        out[last + 1 :] = [""] * (len(rows) - last - 1)
    return out


def strip_count(lang: str, rows: list[str]) -> int:
    """Comment lines of a doc block (raw source lines) that LEN-07 leaves out of the count:
    fenced code, and the sections that restate the signature or give examples."""
    heads = _HEADS.get({"js": "ts", "kotlin": "java"}.get(lang, lang))
    examples = py_examples(rows) if lang == "python" else [""] * len(rows)
    n, fence, drop, field_indent = 0, False, False, -1
    for raw, ex in zip(rows, examples, strict=True):
        t, s = _doc_text(raw), raw.strip()
        if t.startswith(_FENCE):
            fence = not fence
            n += 1
            continue
        if fence or ex:
            drop = drop and ex != "head"
            n += 1
            continue
        if s.startswith("*/") or s in ('"""', "'''"):  # a closing marker always counts
            drop = False
        elif lang == "python":
            indent = len(raw) - len(raw.lstrip())
            if field_indent >= 0 and t and indent > field_indent:
                n += 1
                continue
            field_indent = indent if _REST_FIELD.match(t) else -1
            if field_indent >= 0:
                n += 1
                continue
            if not t:  # a blank line ends a Google section
                drop = False
        if heads and heads[0].match(t):
            drop = bool(heads[1].match(t))
        n += drop
    return n


# --- LEN-02: item visibility ----------------------------------------------------

_RS_ATTRS = re.compile(r"(#\[.*?\]\s*)+")
_ANNOTATIONS = re.compile(r"(@[\w.]+(\([^)]*\))?\s*)+")
_RS_DECL = re.compile(
    r"((unsafe|async|const|default|extern(\s+\"\w+\")?)\s+)*(fn|struct|enum|trait|type|const|static|mod|union|impl|macro_rules!)\b|[a-z_]\w*\s*:(?!:)"
)
_GO_NAME = re.compile(r"(?:func\s+(?:\([^)]*\)\s*)?|type\s+|var\s+|const\s+)?([A-Za-z_]\w*)")


def _balance(s: str) -> int:
    return s.count("(") + s.count("[") - s.count(")") - s.count("]")


def _item_line(lang: str, raw: list[str], lines: list[Line], k: int) -> tuple[int, str] | None:
    """(index, text) of the declaration after a doc block ending before k.
    Skips blank lines, comments and attributes or decorators, which may span lines."""
    opener, lead = ("#[", _RS_ATTRS) if lang == "rust" else ("@", _ANNOTATIONS)
    bal = 0
    for j in range(k, len(raw)):
        s = raw[j].strip()
        if bal > 0:
            bal += _balance(s)
            continue
        if lines[j].kind != "code":
            continue
        if s.startswith(opener):
            bal = _balance(s)
            m = lead.match(s)
            rest = s[m.end() :] if m else ""
            if bal > 0 or not rest:
                continue
            s = rest
        return j, s
    return None


def _enclosing(lines: list[Line], k: int) -> int | None:
    """Index of the line opening the brace that encloses line k."""
    d = lines[k].depth
    if d <= 0:
        return None
    return next((j for j in range(k - 1, -1, -1) if lines[j].depth < d), None)


def _rust_public(raw: list[str], lines: list[Line], j: int, s: str) -> bool | None:
    if re.match(r"pub\s*\(", s):
        return False
    if re.match(r"pub\b", s):
        return True
    enc = _enclosing(lines, j)
    if enc is not None:
        e = raw[enc].strip()
        if re.match(r"(unsafe\s+)?impl\b.*\bfor\b", e):
            return None  # a trait impl item: public if the trait is, which lives elsewhere
        if re.match(r"(pub(\s*\([^)]*\))?\s+)?(unsafe\s+)?(trait|enum)\b", e):
            return _rust_public(raw, lines, enc, e)  # trait items and enum variants inherit
    return False if _RS_DECL.match(s) else None


def _ts_public(raw: list[str], lines: list[Line], j: int, s: str) -> bool:
    if re.match(r"export\b|module\.exports\b|exports\.", s):
        return True
    enc = _enclosing(lines, j)
    if enc is None or not re.search(
        r"\b(class|interface|enum|namespace|module)\b|=\s*\{\s*$", raw[enc]
    ):
        return False  # unexported, or a local inside a function body
    if re.match(r"(private\b|#)", s):
        return False
    top = enc
    while (up := _enclosing(lines, top)) is not None:
        top = up
    return bool(re.match(r"\s*export\b", raw[top]))


def _go_public(s: str) -> bool | None:
    if s.startswith("package ") or re.match(r"(var|const|type)\s*\($", s):
        return None
    m = _GO_NAME.match(s)
    return m.group(1)[0].isupper() if m else None


def _jvm_public(lang: str, raw: list[str], lines: list[Line], j: int, s: str) -> bool:
    head = set(re.split(r"[(={:<]", s)[0].split())
    if lang == "kotlin":
        return not head & {"private", "internal", "protected"}
    if "public" in head:
        return True
    if head & {"private", "protected"}:
        return False
    enc = _enclosing(lines, j)  # an interface member is public; the rest is package-private
    return enc is not None and bool(re.search(r"\binterface\b", raw[enc]))


def _py_owners(src: str) -> dict[int, bool]:
    """Docstring start index -> public, for every class and function docstring.
    A name is public with no leading underscore (dunders count), under no private
    class and inside no function. A module docstring is absent: unplaced."""
    owners: dict[int, bool] = {}

    def walk(node: ast.AST, public: bool) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                name = child.name
                pub = public and (
                    not name.startswith("_") or (name.startswith("__") and name.endswith("__"))
                )
                ds = _docstring(child)
                if ds is not None:
                    owners[ds.lineno - 1] = pub
                walk(child, pub and isinstance(child, ast.ClassDef))
            else:
                walk(child, public)

    with contextlib.suppress(SyntaxError, ValueError, RecursionError):
        walk(ast.parse(src), True)
    return owners


def _public(
    lang: str, raw: list[str], lines: list[Line], b: Block, py_owners: dict[int, bool]
) -> bool | None:
    if b.kind == "interface":
        return True  # LEN-08
    if lang == "python":
        return py_owners.get(b.start)
    if lang not in ("rust", "ts", "js", "go", "java", "kotlin") or raw[b.start].lstrip().startswith(
        ("//!", "/*!")
    ):
        return None  # an inner (module) doc, or a language with no detector
    item = _item_line(lang, raw, lines, b.end)
    if item is None:
        return None
    j, s = item
    if lang == "rust":
        return _rust_public(raw, lines, j, s)
    if lang == "go":
        return _go_public(s)
    if lang in ("ts", "js"):
        return _ts_public(raw, lines, j, s)
    return _jvm_public(lang, raw, lines, j, s)


# --- caps and the ratchet -------------------------------------------------------

CONFIG_NAME = ".code-docs.json"
CAPS = {"plain": 5, "doc": 10, "library_public": 15}
RULE_OF = {"line": "LEN-01", "doc": "LEN-02", "interface": "LEN-08"}
# Human reference bands: pre-2022 snapshot medians of 16 apps and 16 open-source
# libraries, measured 2026-09-27. Context only; no ratio is gated.
BANDS = {
    "app": {"ratio": 0.117, "iqr": "0.056-0.199", "ratio_chars": 0.197},
    "library": {"ratio": 0.273, "iqr": "0.204-0.589", "ratio_chars": 0.394},
}


def load_config(root: Path, path: str | None) -> dict:
    p = Path(path) if path else root / CONFIG_NAME
    cfg = {"library": [], "caps": dict(CAPS), "strip_sections": True, "interface_files": []}
    if not p.is_file():
        if path:
            raise ValueError(f"missing config: {p}")
        return cfg
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        raise ValueError(f"unreadable config {p}: {e}") from e
    if not isinstance(data, dict) or set(data) - set(cfg):
        raise ValueError(f"config {p}: an object with only the keys {', '.join(cfg)}")
    libs, caps, strip, iface = (
        data.get("library", []),
        data.get("caps", {}),
        data.get("strip_sections", True),
        data.get("interface_files", []),
    )
    if not (isinstance(libs, list) and all(isinstance(x, str) for x in libs)):
        raise ValueError(f"config {p}: library is a list of package paths")
    if not (isinstance(iface, list) and all(isinstance(x, str) for x in iface)):
        raise ValueError(f"config {p}: interface_files is a list of path globs")
    if (
        not isinstance(caps, dict)
        or set(caps) - set(CAPS)
        or not all(isinstance(v, int) and v > 0 for v in caps.values())
    ):
        raise ValueError(f"config {p}: caps holds positive integers for {', '.join(CAPS)}")
    if not isinstance(strip, bool):
        raise ValueError(f"config {p}: strip_sections is true or false")
    cfg["library"] = [Path(x).as_posix() for x in libs]  # "./src/pkg/" reads as "src/pkg"
    cfg["caps"] |= caps
    cfg["strip_sections"] = strip
    cfg["interface_files"] = iface
    return cfg


def _is_library(libs: list[str], rel: str) -> bool:
    return any(d in (".", rel) or rel.startswith(d + "/") for d in libs)


class Scan(NamedTuple):
    rows: list  # (rel, scope, package, library, Block) per measured block
    measured: set  # the prod files the run read
    aggs: dict  # the census per (package, scope)


def scan(root: Path, cfg: dict) -> Scan:
    """Every block with its stripped lines, visibility and cap (LEN-01, LEN-02, LEN-07, LEN-08)."""
    aggs, per_file = census(root, "package", interface_files=cfg["interface_files"])
    caps, cache = cfg["caps"], {}
    rows, measured = [], set()
    for rel, fscope, lines, raw in per_file:
        lang = EXT_LANG[Path(rel).suffix]
        pkg = package_of(root, root / rel, cache)
        lib = _is_library(cfg["library"], rel)
        owners = _py_owners("\n".join(raw)) if lang == "python" else {}
        if fscope == "prod":
            measured.add(rel)
        for b in comment_blocks(lines, raw):
            rows.append((rel, "test" if b.test else fscope, pkg, lib, b))
            if b.kind == "line":
                b.cap = caps["plain"]
                continue
            if cfg["strip_sections"]:
                body = [raw[i] for i in range(b.start, b.end) if lines[i].kind == b.kind]
                b.stripped = strip_count(lang, body)
            b.public = _public(lang, raw, lines, b, owners)
            b.cap = caps["library_public"] if lib and b.public is not False else caps["doc"]
    return Scan(rows, measured, aggs)


def _finding(rel: str, b: Block, lib: bool) -> dict:
    if b.kind == "line":
        msg = f"plain comment block of {b.n} lines over the cap of {b.cap}; split it to its risk sites or compress it, guards included (LEN-05)"
    else:
        if not lib:
            tier = "an app-kind package"
        elif b.public is None:
            tier = "an unplaced item in a library-kind package"
        else:
            tier = "a public item in a library-kind package" if b.public else "a non-public item"
        what = (
            "interface doc block, which renders into --help or a schema,"
            if b.kind == "interface"
            else "doc block"
        )
        msg = f"{what} counts {b.counted} lines over the cap of {b.cap} for {tier}"
        if b.stripped:
            msg += f"; {b.stripped} structured-section lines not counted (LEN-07)"
    return {"path": rel, "line": b.start + 1, "rule": RULE_OF[b.kind], "message": msg}


def over_cap(rows: list, scope: str = "prod") -> list[dict]:
    hits = [
        _finding(rel, b, lib)
        for rel, sc, _pkg, lib, b in rows
        if scope in ("all", sc) and b.counted > b.cap
    ]
    return sorted(hits, key=lambda f: (f["path"], f["line"]))


def ratchet(root: Path, sc: Scan, path: Path, update: bool, allow: bool):
    """LEN-03 against the baseline at path; --update writes it. Returns (exit code, findings, notices)."""
    baseline = load_baseline(path, must_exist=not update)
    cur: dict[str, int] = {}
    stripped: dict[str, int] = {}
    for rel, scope, _pkg, _lib, b in sc.rows:
        if scope == "prod" and b.counted > b.cap:
            cur[rel] = cur.get(rel, 0) + b.counted
            stripped[rel] = stripped.get(rel, 0) + b.stripped
    blocks = over_cap(sc.rows)
    rises = sorted(k for k, v in cur.items() if v > baseline.get(k, 0))
    missing = sorted(k for k in baseline if k not in sc.measured and (root / k).is_file())
    drops = [k for k, v in baseline.items() if cur.get(k, 0) < v and k not in missing]
    findings: list[dict] = []
    for k in rises:
        mine = [f for f in blocks if f["path"] == k]
        msg = f"lines in over-cap blocks rose from {baseline.get(k, 0)} to {cur[k]}"
        if stripped.get(k):
            msg += f" ({stripped[k]} structured-section lines not counted)"
        findings += [{"path": k, "line": mine[0]["line"], "rule": "LEN-03", "message": msg}, *mine]
    for k in missing:
        findings.append(
            {
                "path": k,
                "line": 1,
                "rule": "LEN-03",
                "message": f"the baseline counts {baseline[k]} lines here, but the run did not measure this file "
                "(generated header, skipped tree or non-prod path); a skipped file never reads as a decrease",
            }
        )
    notices = (
        [f"{len(drops)} files dropped below the baseline; run --update to lock the drop in"]
        if drops and not update
        else []
    )
    if not update:
        return (1 if findings else 0), findings, notices
    if findings and not allow:
        notices.append(
            "refused: --update never raises a key or drops an unmeasured one; review the keys above, then rerun with --allow-regression"
        )
        return 1, findings, notices
    if findings:
        notices.append(
            f"--allow-regression accepted {len(rises)} raised and {len(missing)} unmeasured keys, listed above"
        )
    path.write_text(json.dumps(cur, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0, findings, notices


def load_baseline(path: Path, must_exist: bool) -> dict:
    if not path.is_file():
        if must_exist:
            raise ValueError(f"missing baseline: {path}")
        return {}  # creating a baseline is itself a rise, so it takes --allow-regression once
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        raise ValueError(f"unreadable baseline {path}: {e}") from e
    if not isinstance(data, dict) or not all(
        isinstance(v, int) and not isinstance(v, bool) and v >= 0 for v in data.values()
    ):
        raise ValueError(f"baseline {path}: an object of file path to line count")
    return data


def _looks_published(d: Path) -> str | None:
    """Why the package at d looks like one a registry publishes, or None. Library-kind is a
    declaration, so --report notes an undeclared candidate; it never changes the exit code."""

    def load(name: str, parse) -> dict:
        try:
            data = parse((d / name).read_text(encoding="utf-8"))
        except (OSError, ValueError):  # absent or unparsable: no evidence either way
            return {}
        return data if isinstance(data, dict) else {}

    if "build-system" in load("pyproject.toml", tomllib.loads):
        return "a pyproject.toml with a [build-system] table"
    js = load("package.json", json.loads)
    if js.get("name") and js.get("private") is not True:
        return 'a package.json with a name and no "private": true'
    rs = load("Cargo.toml", tomllib.loads)
    pkg = rs.get("package")
    if (
        isinstance(pkg, dict)
        and pkg.get("publish", True) not in (False, [])
        and ("lib" in rs or (d / "src" / "lib.rs").is_file())
    ):
        return "a Cargo.toml with a lib target and no publish = false"
    return None


def report(sc: Scan, cfg: dict, root: Path) -> dict:
    """LEN-03 roll-up and LEN-04 context, per package, prod scope, and the app-kind packages
    that look published."""
    pk: dict[str, dict] = {}

    def entry(p: str) -> dict:
        kind = "library" if _is_library(cfg["library"], "" if p == "." else p) else "app"
        return pk.setdefault(
            p,
            {
                "kind": kind,
                "code": 0,
                "ratio": 0.0,
                "ratio_chars": 0.0,
                "over_blocks": 0,
                "over_lines": 0,
                "stripped": 0,
            },
        )

    for (g, scope), agg in sc.aggs.items():
        if scope == "prod":
            s = agg.summary()
            entry(g).update(code=s["code"], ratio=s["ratio"], ratio_chars=s["ratio_chars"])
    for _rel, scope, pkg, _lib, b in sc.rows:
        if scope == "prod" and b.counted > b.cap:
            e = entry(pkg)
            e["over_blocks"] += 1
            e["over_lines"] += b.counted
            e["stripped"] += b.stripped
    total = {
        k: sum(p[k] for p in pk.values()) for k in ("code", "over_blocks", "over_lines", "stripped")
    }
    cands = {
        p: why
        for p, e in sorted(pk.items())
        if e["kind"] == "app" and (why := _looks_published(root / p))
    }
    return {
        "bands": BANDS,
        "packages": dict(sorted(pk.items())),
        "total": total,
        "library_candidates": cands,
    }


def _print_report(rep: dict) -> None:
    app, lib = rep["bands"]["app"], rep["bands"]["library"]
    print(
        f"Human reference bands, context only (LEN-04): apps median ratio {app['ratio']} (IQR {app['iqr']}, chars {app['ratio_chars']}), "
        f"libraries {lib['ratio']} (IQR {lib['iqr']}, chars {lib['ratio_chars']}). No ratio is a target and none is gated; "
        "the caps bind whatever the surrounding file's density is."
    )
    print(
        f"{'package':40} {'kind':>7} {'code':>8} {'ratio':>6} {'chars':>6} {'blocks':>7} {'lines':>7} {'stripped':>8}"
    )
    for name, p in rep["packages"].items():
        print(
            f"{name[:40]:40} {p['kind']:>7} {p['code']:>8} {p['ratio']:>6} {p['ratio_chars']:>6} {p['over_blocks']:>7} {p['over_lines']:>7} {p['stripped']:>8}"
        )
    t = rep["total"]
    print(
        f"{'total':40} {'':>7} {t['code']:>8} {'':>6} {'':>6} {t['over_blocks']:>7} {t['over_lines']:>7} {t['stripped']:>8}"
    )
    for name, why in rep["library_candidates"].items():
        print(
            f"note: {name} looks published ({why}) but is not declared under library in {CONFIG_NAME}, "
            "so it gets app caps; declare it if a registry publishes it (LEN-02)"
        )


# --- cli ----------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").split("\n\n")[0])
    ap.add_argument("--root", default=".")
    ap.add_argument("--group", default="none", choices=["package", "lang", "top", "none"])
    ap.add_argument("--format", default="text", choices=["text", "json"])
    ap.add_argument("--scope", default="prod", choices=["prod", "test", "other", "all"])
    ap.add_argument("--list-blocks", action="store_true")
    ap.add_argument("--sample", type=int, default=0)
    ap.add_argument("--min-block", type=int, default=1)
    ap.add_argument("--kind", default="any", choices=["doc", "line", "interface", "any"])
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument(
        "--split-at-blank",
        action="store_true",
        help="the old block definition, for results measured with it",
    )
    ap.add_argument("--config", help=f"caps and library packages; default ROOT/{CONFIG_NAME}")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument(
        "--over-cap", action="store_true", help="list every over-cap block (LEN-01, LEN-02, LEN-08)"
    )
    mode.add_argument(
        "--check", metavar="BASELINE", help="fail on a per-file rise over BASELINE (LEN-03)"
    )
    mode.add_argument(
        "--update", metavar="BASELINE", help="write the current counts to BASELINE (LEN-03)"
    )
    mode.add_argument(
        "--report",
        action="store_true",
        help="per package: ratio, char ratio, lines in over-cap blocks",
    )
    ap.add_argument(
        "--allow-regression", action="store_true", help="let --update raise keys; prints each one"
    )
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    if a.allow_regression and not a.update:
        ap.error("--allow-regression only goes with --update")
    root = Path(a.root).resolve()
    if not root.is_dir():
        print(f"no such directory: {root}", file=sys.stderr)
        return 2
    try:
        cfg = load_config(root, a.config)
    except ValueError as e:
        print(e, file=sys.stderr)
        return 2
    if a.over_cap or a.check or a.update or a.report:
        try:
            sc = scan(root, cfg)
            if a.report:
                code, payload, notices = 0, report(sc, cfg, root), []
            elif a.over_cap:
                payload, notices = over_cap(sc.rows, a.scope), []
                code = 1 if payload else 0
            else:
                base = Path(a.check or a.update)
                code, payload, notices = ratchet(root, sc, base, bool(a.update), a.allow_regression)
        except ValueError as e:
            print(e, file=sys.stderr)
            return 2
        if a.format == "json":
            print(json.dumps(payload, indent=1))
        elif a.report:
            _print_report(payload)
        else:
            for f in payload:
                print(f"{f['path']}:{f['line']}: {f['rule']} {f['message']}")
        for n in notices:
            print(n, file=sys.stderr)
        return code
    aggs, per_file = census(root, a.group, a.split_at_blank, cfg["interface_files"])
    if a.list_blocks or a.sample:
        found = []
        for rel, fscope, lines, raw in per_file:
            for b in _legacy_blocks(lines) if a.split_at_blank else comment_blocks(lines, raw):
                sc = "test" if b.test else fscope
                if b.n < a.min_block or a.scope not in ("all", sc) or a.kind not in ("any", b.kind):
                    continue
                nxt = next(
                    (
                        raw[k].strip()
                        for k in range(b.end, min(len(raw), b.end + 4))
                        if raw[k].strip()
                    ),
                    "",
                )
                found.append(
                    {
                        "file": rel,
                        "line": b.start + 1,
                        "end": b.end,
                        "len": b.n,
                        "kind": b.kind,
                        "text": "\n".join(raw[b.start : b.end]),
                        "next": nxt,
                    }
                )
        if a.sample:
            random.Random(a.seed).shuffle(found)  # noqa: S311 - reproducible sampling, not security
            found = found[: a.sample]
        if a.format == "json":
            print(json.dumps(found, indent=1))
        else:
            for b in found:
                print(
                    f"{b['file']}:{b['line']}  {b['kind']} x{b['len']}"
                    + (
                        ""
                        if a.list_blocks and not a.sample
                        else f"\n{b['text']}\n  -> {b['next']}\n"
                    )
                )
        return 0
    rows = {
        f"{g}|{sc}": agg.summary()
        for (g, sc), agg in sorted(aggs.items())
        if a.scope in ("all", sc)
    }
    if a.format == "json":
        print(json.dumps(rows, indent=1))
        return 0
    print(
        f"{'group|scope':40} {'code':>8} {'doc':>7} {'line':>7} {'iface':>6} {'lic':>6} {'ratio':>6} {'chars':>6} {'doc>5':>6} {'line>5':>6} {'maxblk':>6}"
    )
    for key, s in rows.items():
        db, lb = s["doc_blocks"], s["line_blocks"]
        print(
            f"{key[:40]:40} {s['code']:>8} {s['doc']:>7} {s['line']:>7} {s['interface']:>6} {s['license']:>6} {s['ratio']:>6} {s['ratio_chars']:>6} "
            f"{db.get('gt5', 0):>6} {lb.get('gt5', 0):>6} {max(db.get('max', 0), lb.get('max', 0)):>6}"
        )
    return 0


# --- self-test ----------------------------------------------------------------

_FIX = {
    "rust": (
        "// Copyright 2020 X. Licensed under MIT.\n\nuse std::io;\n\n/// Returns the digest.\n///\n/// More.\n"
        'pub fn f() -> &\'static str { "// not a comment" } // trailing\n'
        "// why: lock held until return\nlet _g = lock();\n/* block\n   two */\nfn g<'a>(x: &'a str) -> char { '{' }\n"
        '#[derive(Parser)]\n/// cli help\nstruct Cli {\n    /// flag help\n    x: bool,\n}\n#[cfg(test)]\nmod tests {\n    // test comment\n    fn t() {}\n}\nlet r = r#"// raw"#;\n'
    ),
    "python": (
        '#!/usr/bin/env python3\n# /// script\n# requires-python = ">=3.11"\n# ///\n"""Module doc."""\nimport os  # noqa: F401\n\n# why comment\nx = "# not"\n\n'
        'def f():\n    """Doc line one.\n\n    More.\n    """\n    return 1\n\n@app.command()\ndef cli():\n    """CLI help."""\n'
    ),
    "go": "// Package x does y.\npackage x\n\n// Run runs.\nfunc Run() {\n\t// inner why\n\ts := `// raw`\n\t_ = s\n}\n\n//go:build linux\n",
    "ts": '/**\n * Doc.\n */\nexport function f(): string {\n  // why\n  return `// t`; // trail\n}\n/// <reference path="x" />\n',
}
_SPLIT = "fn f() {\n    // one\n    // two\n    // three\n\n    // four\n    // five\n    // six\n    let x = 1;\n}\n"
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "comment_census"
FIXTURE_SUFFIX = ".fixture"


def copy_fixture(src: Path, dst: Path) -> Path:
    """Copy a self-test fixture tree to dst, each file under its real name.

    Fixtures ship as NAME.fixture (lib.rs.fixture) so that an adopter's linter,
    compiler or formatter never reads the installed copy. Every self-test copies
    through here, the one place that strips the suffix."""
    # shutil.copy, not copy2: a fresh mtime. A checkout stamps a before/after pair with one
    # mtime, and git's stat cache then reads a same-size edit as no change.
    shutil.copytree(
        src,
        dst,
        dirs_exist_ok=True,
        copy_function=lambda s, d: shutil.copy(s, os.fspath(d).removesuffix(FIXTURE_SUFFIX)),
    )
    return dst


def _classifier_fails() -> list[str]:
    def counts(lang, src):
        c = {k: 0 for k in KINDS}
        t = 0
        for ln in classify("x", src, lang):
            c[ln.kind] += 1
            t += ln.test
        return c, t

    fails = []
    want = {
        "rust": {"license": 1, "doc": 3, "interface": 2, "line": 4},
        "python": {"doc": 5, "line": 1, "directive": 4, "interface": 1},
        "go": {"doc": 2, "line": 1, "directive": 1},
        "ts": {"doc": 3, "line": 1, "code": 4},
    }
    for lang, exp in want.items():
        c, t = counts(lang, _FIX[lang])
        fails += [f"{lang} {k}={c[k]} want {v} ({c})" for k, v in exp.items() if c[k] != v]
        if lang == "rust" and t != 5:
            fails.append(f"rust test lines={t} want 5")
    split = classify("x", _SPLIT, "rust")
    if [n for _, _, n in blocks_of(split)] != [3, 3]:
        fails.append(
            "blocks_of() no longer splits at a blank line; the old definition must stay available"
        )
    if [b.n for b in comment_blocks(split)] != [6]:
        fails.append("comment_blocks() splits at a blank line (LEN-01)")
    return fails


def _case_fails(case: Path) -> list[str]:
    """Each fixture directory is a small tree plus case.json: a list of runs, each naming
    a mode, the expected exit code and the exact 'path:line RULE' findings. A run drives
    main() on a copy outside any git work tree, so the file walk is the directory walk."""
    spec = json.loads((case / "case.json").read_text(encoding="utf-8"))
    fails = []
    for i, run in enumerate(spec["runs"]):
        where = f"{case.name} run {i} ({run['mode']})"
        with tempfile.TemporaryDirectory() as tmp:
            root = copy_fixture(case, Path(tmp) / case.name)
            base = Path(tmp) / "baseline.json"
            if "baseline" in run:
                base.write_text(json.dumps(run["baseline"]), encoding="utf-8")
            argv = ["--root", str(root), "--format", "json", f"--{run['mode']}"]
            argv += [str(base)] if run["mode"] in ("check", "update") else []
            argv += ["--allow-regression"] if run.get("allow_regression") else []
            out = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                code = main(argv)
            payload = json.loads(out.getvalue() or "[]")
            if run["mode"] == "report":
                got_lines = {p: e["over_lines"] for p, e in payload["packages"].items()}
                if got_lines != run["over_lines"]:
                    fails.append(f"{where}: over-cap lines {got_lines}, want {run['over_lines']}")
                got_cands = sorted(payload.get("library_candidates", {}))
                if got_cands != run.get("candidates", got_cands):
                    fails.append(
                        f"{where}: library candidates {got_cands}, want {run['candidates']}"
                    )
                continue
            got = sorted(f"{f['path']}:{f['line']} {f['rule']}" for f in payload)
            if code != run["exit"] or got != sorted(run["expect"]):
                fails.append(
                    f"{where}: exit {code} {got}, want exit {run['exit']} {sorted(run['expect'])}"
                )
            if "written" in run and json.loads(base.read_text(encoding="utf-8")) != run["written"]:
                fails.append(
                    f"{where}: wrote {base.read_text(encoding='utf-8')}, want {run['written']}"
                )
    return fails


def self_test() -> int:
    fails = _classifier_fails()
    cases = (
        sorted(p for p in FIXTURES.iterdir() if (p / "case.json").is_file())
        if FIXTURES.is_dir()
        else []
    )
    if not cases:
        fails.append(f"no fixture cases under {FIXTURES}")
    for case in cases:
        fails += _case_fails(case)
    with tempfile.TemporaryDirectory() as tmp:
        for rel in (".claude/rules/code-docs/checks/a.py", ".claude/hooks/a.py", "src/a.py"):
            (Path(tmp) / rel).parent.mkdir(parents=True, exist_ok=True)
            (Path(tmp) / rel).write_text("x = 1\n", encoding="utf-8")
        got = sorted(f.relative_to(tmp).as_posix() for f in list_files(Path(tmp)))
        if got != [".claude/hooks/a.py", "src/a.py"]:
            fails.append(f"list_files: installed agent config not skipped: {got}")
    for f in fails:
        print("FAIL", f)
    print(
        "self-test:",
        "ok" if not fails else f"{len(fails)} failures",
        f"({len(cases)} fixture cases)",
    )
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
