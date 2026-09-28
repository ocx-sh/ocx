#!/usr/bin/env python3
"""Interface-leak scanner for the code-docs rule set.

Rules covered:
  SRF-01  interface text carries no plan or process ID, record or source
          filename or ISO date, and raw-copied text no code path or rustdoc
          intra-doc link
  SRF-03  the leak check runs on rendered output (the gate), with a source
          scan of the text that will render as a cheaper pre-build complement

Output mode, the gate. Reads rendered interface text from files, directories,
stdin ("-") or a recursive --help walk. JSON input (a JSON Schema, an MCP
tools/list result, a package.json) is walked value by value: every string under
a description or title key is scanned and reported with its JSON pointer. A
.yml or .yaml file (action.yml, an OpenAPI spec) is read the same way by key,
block scalars included. Any
other input is scanned line by line (captured --help, extracted API docs).
Every hit is a finding and exits 1, unless the allow-list names it.

Source mode (--source), advisory. Scans the text that a generator will copy:
  - census interface lines: doc comments on Rust items deriving clap
    Parser/Args/Subcommand/ValueEnum or schemars JsonSchema, Python click/typer
    command docstrings and pydantic model docstrings
  - Rust #[command|arg|clap|value|group|schemars|tool(...)] string values of
    about, long_about, help, long_help, before/after(_long)_help, description
  - Rust clap builder calls (.about("..."), .help("..."), ...) and hand-written
    schema literals ("description": "..." in json!)
  - rmcp's fallback: a #[tool] without description publishes the fn's ///
  - Python help=, description=, epilog=, short_help= keyword strings (argparse,
    click, typer, pydantic Field)
  - TS/JS commander .description(), .summary(), .command(name, desc),
    .option/.requiredOption/.argument(flags, desc), new Option/Argument(x, desc)
  - with --published, every doc comment in prod scope (a published package's
    API reference renders all of them); rustdoc resolves intra-doc links and
    code paths there, so only the raw-copy lines above are held to them
It exits 0 with findings unless --strict. It cannot see a description built at
runtime or passed positionally to a helper; the output gate can.

Token set: SRF01_PATTERNS, 20 families in 21 patterns, importable. The last
RUST_ONLY patterns are Rust code syntax; --lang python|ts drops them, because
mkdocstrings and TypeDoc cross-references use the same bracket syntax.

Per-generator test shapes (SRF-03). Each walks the generator's own model,
feeds the rendered text to this scanner and asserts exit 0. Proof: plant
"(C-999)" in one rendered doc comment and watch the test fail.
  clap        render_long_help() of every Command in the Cli::command() tree
  commander   helpInformation() of the program and every .commands entry, or
              --help-walk "node dist/cli/index.js" in CI
  argparse    format_help() of the parser and every parser in each
              _SubParsersAction's .choices
  schemars    every emitted schema file: the committed goldens, or the binary's
              own schema-export output
  MCP         the tool list serialized in-process (ToolRouter::list_all()); it
              covers description strings, the /// fallback and every
              JsonSchema argument struct
  Python docs griffe (mkdocstrings' loader) over the public API under the docs
              build's filters, every docstring including attribute docstrings;
              never inspect.getdoc() (misses attribute docstrings) and never a
              grep of built HTML (counts show_source listings)

Rust (clap), in the CLI crate's tests:
    fn walk(c: &mut clap::Command, out: &mut String) {
        out.push_str(&c.render_long_help().to_string());
        c.get_subcommands_mut().for_each(|s| walk(s, out));
    }
    let mut help = String::new();
    walk(&mut Cli::command(), &mut help);
    let mut scan = std::process::Command::new("python3")
        .args(["../../tools/interface_leak.py", "-"])
        .stdin(std::process::Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(&mut scan.stdin.take().unwrap(), help.as_bytes()).unwrap();
    assert!(scan.wait().unwrap().success(), "SRF-01 token in --help");
  schemars: python3 tools/interface_leak.py tests/golden

Python (published API, griffe):
    import griffe, subprocess, sys
    def walk(obj, seen):
        try:
            if obj.canonical_path in seen:
                return
            seen.add(obj.canonical_path)
            doc = obj.docstring
        except griffe.AliasResolutionError:
            return
        if doc:
            yield f"== {obj.path}\\n{doc.value}"
        for name, member in obj.members.items():
            if not name.startswith("_"):  # mkdocs.yml filters: ["!^_"]
                yield from walk(member, seen)
    text = "\\n".join(walk(griffe.load("my_pkg", resolve_aliases=True), set()))
    subprocess.run([sys.executable, "tools/interface_leak.py", "--lang", "python", "-"],
                   input=text, text=True, check=True)

TypeScript (commander, vitest; export buildProgram first):
    import { execFileSync } from "node:child_process";
    import type { Command } from "commander";
    test("help carries no internal references", () => {
      const screens: string[] = [];
      const walk = (c: Command): void => { screens.push(c.helpInformation()); c.commands.forEach(walk); };
      walk(buildProgram());
      execFileSync("python3", ["tools/interface_leak.py", "--lang", "ts", "-"], { input: screens.join("\\n") });
    });

Allow-list (--allow FILE), one reviewed exception per line, whole-line # comments:
  TOKEN              allowed anywhere
  GLOB TOKEN         allowed where GLOB (fnmatch) matches the finding's path
                     or path#pointer, e.g. *reports.json#/$defs/* C-050

Usage:
  interface_leak.py [--lang rust|python|ts] [--allow FILE] [--format text|json] INPUT ...
  interface_leak.py --help-walk "CMD ARGS" [--help-walk ...] [--allow FILE]
  interface_leak.py --source [--root DIR] [--published] [--strict] [--allow FILE]
  interface_leak.py --self-test

Exit codes: 0 clean (and source mode without --strict), 1 findings,
2 usage or missing input.
"""

from __future__ import annotations

import argparse
import ast
import bisect
import fnmatch
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
from collections.abc import Iterator
from pathlib import Path

sys.dont_write_bytecode = True  # no __pycache__ beside an installed copy (CLN-05 would read it)
sys.path.insert(0, str(Path(__file__).resolve().parent))
import comment_census as cc  # noqa: E402 - after the bytecode switch above

# SRF-01's token set. The S- and first .rs patterns use a lookahead where an rg
# form would consume a context char: the same lines match, and the token prints
# clean. The lookahead makes the .rs$ pattern redundant; it stays so the list
# reads line for line against the rg form. record and record-dir are LNK-02's
# record forms; source-path is any source file named with a directory.
SRF01_PATTERNS: tuple[tuple[str, str], ...] = (
    ("C-", r"\bC-[0-9]{1,4}\b"),
    ("C-S", r"\bC-S[0-9]+(?:-[0-9]+)?\b"),
    ("RUL-", r"\bRUL-[0-9]{1,3}\b"),
    ("A-", r"\bA-[0-9]{1,3}\b"),
    ("S-", r"\bS-[0-9]{1,3}(?![0-9-])"),  # not a Windows SID S-1-5-18
    ("WP-", r"\bWP-[0-9]{1,3}\b"),
    ("DX-", r"\bDX-[0-9]{1,3}\b"),
    ("DEC-", r"\bDEC-[A-Za-z0-9]{1,6}\b"),
    ("D-", r"\bD-V?[0-9]{1,3}\b"),
    ("ADR-", r"\bADR-[0-9]{1,4}\b"),
    ("adr_", r"\badr_[A-Za-z0-9_.-]+"),
    ("plan_", r"\bplan_[A-Za-z0-9_.-]+"),
    ("iso-date", r"\b20[0-9]{2}-[0-9]{2}-[0-9]{2}\b"),
    (
        "record",
        r"\b(?:adr|plan|rulings?|subsystem|decisions?|design_spec|handover)[-_][\w.-]*\.md\b",
    ),
    ("record-dir", r"\b(?:adr|adrs|decisions)/[\w./-]*\.md\b"),
    (
        "source-path",
        r"\b[\w.-]+(?:/[\w.-]+)*/[\w-]+\.(?:rs|pyi?|[cm]?[jt]sx?|go|java|kts?|swift|cs|scala|c|h|cc|cpp|cxx|hpp|hh)\b",
    ),
    (".rs", r"\b[a-z_][a-z0-9_]*\.rs(?![/a-z0-9])"),  # not a URL host: example.rs/x
    (".rs", r"\b[a-z_][a-z0-9_]*\.rs$"),
    ("intra-doc", r"\[`[A-Za-z_][A-Za-z0-9_:]*`\]"),
    ("Self::", r"\bSelf::"),
    ("crate::", r"\bcrate::"),
)
RUST_ONLY = 5
FAMILIES = tuple(dict.fromkeys(f for f, _ in SRF01_PATTERNS))


def _alternation(pats: tuple[tuple[str, str], ...]) -> re.Pattern[str]:
    # One alternation, leftmost-first and non-overlapping, like rg -o: a
    # [`Self::x`] link is one intra-doc token, not also a Self:: token.
    return re.compile("|".join(f"(?P<g{i}>{p})" for i, (_, p) in enumerate(pats)), re.MULTILINE)


_RX_RUST = _alternation(SRF01_PATTERNS)
_RX_OTHER = _alternation(SRF01_PATTERNS[:-RUST_ONLY])


def _in_url(text: str, pos: int) -> bool:
    """The whitespace-delimited word around pos is a URL, such as a spec version's link."""
    return "://" in text[max(text.rfind(c, 0, pos) for c in " \t\n") + 1 : pos]


def scan_text(text: str, rust: bool = True) -> list[tuple[int, str, str]]:
    """Return (offset, family, token) for every SRF-01 token in text.
    An ISO date inside a URL is an external spec version, never a leak."""
    rx = _RX_RUST if rust else _RX_OTHER
    hits = [
        (m.start(), SRF01_PATTERNS[int(m.lastgroup[1:])][0], m.group(0)) for m in rx.finditer(text)
    ]
    return [h for h in hits if h[1] != "iso-date" or not _in_url(text, h[0])]


def _finding(path: str, line: int, hit: tuple[int, str, str], ctx: str, where: str = "") -> dict:
    pos, fam, tok = hit
    lo = max(0, pos - 40)
    snip = " ".join(ctx[lo : pos + len(tok) + 40].split())
    return {
        "path": path,
        "line": line,
        "pointer": where,
        "rule": "SRF-01",
        "family": fam,
        "token": tok,
        "context": snip,
    }


# --- output mode ---------------------------------------------------------------

RENDERED_KEY_RE = re.compile(r"(?i)(description|title|deprecationmessage)s?$")
YAML_KEY_RE = re.compile(r"^(\s*)(?:-\s+)?([A-Za-z_][\w.-]*)\s*:\s*(.*)$")


def _json_strings(node: object, ptr: str, rendered: bool) -> Iterator[tuple[str, str]]:
    """Yield (JSON pointer, string) for rendered strings, in document order."""
    if isinstance(node, dict):
        for k, v in node.items():
            esc = k.replace("~", "~0").replace("/", "~1")
            yield from _json_strings(v, f"{ptr}/{esc}", bool(RENDERED_KEY_RE.search(k)))
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield from _json_strings(v, f"{ptr}/{i}", rendered)
    elif isinstance(node, str) and rendered:
        yield ptr, node


def _yaml_rendered(text: str):
    """Yield (line, value) for every rendered key in YAML, block scalars included.
    Line-based: enough for action.yml and OpenAPI files, not for anchors or flow maps."""
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        m = YAML_KEY_RE.match(lines[i])
        i += 1
        if not m or not RENDERED_KEY_RE.search(m.group(2)):
            continue
        indent, val = len(m.group(1)), m.group(3).strip()
        if val[:1] in ("|", ">"):
            start = i
            while i < len(lines) and (
                not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) > indent
            ):
                i += 1
            yield from ((k + 1, lines[k].strip()) for k in range(start, i) if lines[k].strip())
        elif val:
            yield i, val.strip("'\"")


def scan_rendered(label: str, text: str, rust: bool = True) -> list[dict]:
    out: list[dict] = []
    if label.endswith((".yml", ".yaml")):  # only rendered keys: runs.main is a path, not text
        for n, val in _yaml_rendered(text):
            out += [_finding(label, n, h, val) for h in scan_text(val, rust)]
        return out
    doc = None
    if text.lstrip()[:1] in ("{", "["):
        try:
            doc = json.loads(text)
        except ValueError:
            doc = None
    if doc is None:
        for n, line in enumerate(text.splitlines(), 1):
            out += [_finding(label, n, h, line) for h in scan_text(line, rust)]
        return out
    cursor = 0
    for ptr, s in _json_strings(doc, "", False):
        hits = scan_text(s, rust)
        if not hits:
            continue
        # Walk order is text order, so a forward search finds this occurrence.
        idx = -1
        for ascii_only in (False, True):
            idx = text.find(json.dumps(s, ensure_ascii=ascii_only), cursor)
            if idx != -1:
                break
        line = text.count("\n", 0, idx) + 1 if idx != -1 else 0
        cursor = max(cursor, idx + 1)
        out += [_finding(label, line, h, s, ptr) for h in hits]
    return out


def expand(inputs: list[str]) -> list[tuple[str, Path | None]]:
    """Map CLI inputs to (label, path); a directory expands to its files."""
    out: list[tuple[str, Path | None]] = []
    for arg in inputs:
        if arg == "-":
            out.append(("<stdin>", None))
            continue
        p = Path(arg)
        if p.is_dir():
            out += [(str(f), f) for f in sorted(p.rglob("*")) if f.is_file()]
        elif p.is_file():
            out.append((arg, p))
        else:
            raise FileNotFoundError(arg)
    return out


COMMANDS_RE = re.compile(r"^(Commands|Subcommands|COMMANDS|SUBCOMMANDS):\s*$")


def subcommands(help_text: str) -> list[str]:
    """Names in a clap or commander Commands: section, without help."""
    names: list[str] = []
    indent = None
    inside = False
    for line in help_text.splitlines():
        if COMMANDS_RE.match(line):
            inside = True
            continue
        if not inside:
            continue
        if not line.strip() or not line.startswith(" "):
            break
        width = len(line) - len(line.lstrip())
        indent = width if indent is None else indent
        if width == indent:  # deeper lines continue a wrapped description
            name = re.split(r"[\s,|]", line.strip(), maxsplit=1)[0]
            if name != "help":
                names.append(name)
    return names


def help_walk(cmd: str, depth: int = 4) -> list[tuple[str, str]]:
    """Capture `CMD [sub ...] --help` for every subcommand, breadth first."""
    base = shlex.split(cmd)
    env = {**os.environ, "NO_COLOR": "1", "COLUMNS": "200"}
    screens: list[tuple[str, str]] = []
    queue: list[list[str]] = [[]]
    while queue:
        sub = queue.pop(0)
        argv = [*base, *sub, "--help"]
        # S603: argv is the caller's own --help-walk command, run as given.
        res = subprocess.run(argv, capture_output=True, text=True, timeout=10, env=env, check=False)  # noqa: S603
        text = res.stdout or res.stderr
        screens.append((" ".join([cmd, *sub, "--help"]), text))
        if len(sub) < depth:
            queue += [[*sub, n] for n in subcommands(text)]
    return screens


# --- source mode ---------------------------------------------------------------

_ESC_RE = re.compile(r"\\(.)", re.DOTALL)


def _neutral(body: str) -> str:
    """Blank escape sequences at equal length so offsets map back to source."""
    return _ESC_RE.sub(lambda m: " " + (m.group(1) if m.group(1) in "\"'`\n" else " "), body)


RS_LIT = r'(?:r(?P<h>#*)"(?P<raw>[\s\S]*?)"(?P=h)|"(?P<s>(?:[^"\\]|\\.)*)")'
RS_HELP = (
    r"(?:about|long_about|help|long_help|before_help|before_long_help|after_help|after_long_help)"
)
RS_ATTR_RE = re.compile(r"#\[\s*(command|arg|clap|value|group|schemars|tool)\s*[(\]]")
RS_KV_RE = re.compile(rf"\b(?:{RS_HELP}|description)\s*=\s*{RS_LIT}")
RS_CALL_RE = re.compile(rf"\.{RS_HELP}\(\s*{RS_LIT}")
RS_JSON_RE = re.compile(rf'"(?:description|title)"\s*:\s*{RS_LIT}')
TS_LIT = r"(?P<q>[\"'`])(?P<s>(?:(?!(?P=q))[^\\]|\\.)*)(?P=q)"
TS_LIT2 = TS_LIT.replace("?P<q>", "?P<q2>").replace("?P=q", "?P=q2").replace("?P<s>", "?P<s2>")
TS_CALL_RE = re.compile(
    rf"(?:\.(?:description|summary)\(\s*{TS_LIT}"
    rf"|(?:\.(?:command|option|requiredOption|argument)|\bnew\s+(?:Option|Argument))\(\s*[\"'`][^\"'`]*[\"'`]\s*,\s*{TS_LIT2})"
)
PY_KEYWORDS = {"help", "description", "epilog", "short_help"}


_RS_STR_RE = re.compile(r'"(?:[^"\\]|\\.)*"')


def _attr_end(text: str, start: int) -> int:
    """Offset just past the ] closing the attribute that opens at start."""
    depth = 0
    i = start
    while i < len(text):
        c = text[i]
        if c == '"':
            m = _RS_STR_RE.match(text, i)
            i = m.end() if m else i + 1
            continue
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(text)


class SourceScan:
    def __init__(self, published: bool) -> None:
        self.published = published
        self.findings: dict[tuple, dict] = {}
        self.stats = {"files": 0, "interface_lines": 0, "literals": 0}
        self.rel, self.src, self.starts, self.rust = "", "", [0], True

    def _add(self, pos: int, body: str, rust: bool | None = None) -> None:
        """Record every token in body, which starts at source offset pos."""
        for p, fam, tok in scan_text(body, self.rust if rust is None else rust):
            at = pos + p
            line = bisect.bisect_right(self.starts, at)
            row = self.src[self.starts[line - 1] :].split("\n", 1)[0]
            hit = (at - self.starts[line - 1], fam, tok)
            self.findings.setdefault((self.rel, line, tok), _finding(self.rel, line, hit, row))

    def file(self, rel: str, src: str, lang: str) -> None:
        self.stats["files"] += 1
        lines = cc.classify(rel, src, lang)
        raw = src.split("\n")
        self.rel, self.src, self.rust = rel, src, lang == "rust"
        self.starts = [0]
        for r in raw[:-1]:
            self.starts.append(self.starts[-1] + len(r) + 1)
        want = ("interface", "doc") if self.published else ("interface",)
        for i, ln in enumerate(lines):
            if ln.kind in want and not ln.test:
                self.stats["interface_lines"] += ln.kind == "interface"
                # rustdoc resolves intra-doc links and code paths; only raw-copy text leaks them
                self._add(self.starts[i], raw[i], self.rust and ln.kind == "interface")

        def row_of(at: int) -> int:
            return bisect.bisect_right(self.starts, at) - 1

        def literal(m: re.Match[str], group: str = "s") -> None:
            g = group if m.group(group) is not None else "raw"
            ln = lines[row_of(m.start())]
            if ln.kind == "code" and not ln.test:
                self.stats["literals"] += 1
                self._add(m.start(g), _neutral(m.group(g)))

        if lang == "rust":
            for a in RS_ATTR_RE.finditer(src):
                end = _attr_end(src, a.start() + 1)
                for m in RS_KV_RE.finditer(src, a.start(), end):
                    literal(m)
                k = row_of(a.start())
                if a.group(1) != "tool" or "description" in src[a.start() : end] or lines[k].test:
                    continue
                # rmcp falls back to the fn's /// when description is unset.
                while k > 0 and (
                    lines[k - 1].kind in ("doc", "directive")
                    or raw[k - 1].lstrip().startswith("#[")
                ):
                    k -= 1
                    if lines[k].kind == "doc":
                        self._add(self.starts[k], raw[k])
            for rx in (RS_CALL_RE, RS_JSON_RE):
                for m in rx.finditer(src):
                    literal(m)
        elif lang in ("ts", "js"):
            for m in TS_CALL_RE.finditer(src):
                literal(m, "s" if m.group("s") is not None else "s2")
        elif lang == "python":
            self._python(src)

    def _python(self, src: str) -> None:
        try:
            tree = ast.parse(src)
        except (SyntaxError, ValueError):
            return
        for node in ast.walk(tree):
            for kw in node.keywords if isinstance(node, ast.Call) else []:
                if kw.arg not in PY_KEYWORDS:
                    continue
                for c in ast.walk(kw.value):
                    if not (isinstance(c, ast.Constant) and isinstance(c.value, str)):
                        continue
                    self.stats["literals"] += 1
                    for hit in scan_text(c.value, rust=False):
                        line = c.lineno + c.value.count("\n", 0, hit[0])
                        self.findings.setdefault(
                            (self.rel, line, hit[2]), _finding(self.rel, line, hit, c.value)
                        )


def scan_source(
    root: Path, published: bool, files: list[Path] | None = None
) -> tuple[list[dict], dict]:
    scan = SourceScan(published)
    for f in cc.list_files(root) if files is None else files:
        rel = f.relative_to(root)
        lang = cc.EXT_LANG.get(f.suffix)
        if lang not in ("rust", "python", "ts", "js") or cc.scope_of(rel) != "prod":
            continue
        try:
            src = f.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if cc.GENERATED_RE.search("\n".join(src.split("\n", 6)[:6])):
            continue
        scan.file(rel.as_posix(), src, lang)
    return sorted(scan.findings.values(), key=lambda d: (d["path"], d["line"])), scan.stats


# --- allow-list and report ----------------------------------------------------


def load_allow(path: Path) -> list[tuple[str | None, str]]:
    out: list[tuple[str | None, str]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        # Whole-line comments only: a pointer glob carries a #.
        parts = [] if line.lstrip().startswith("#") else line.split()
        if len(parts) == 1:
            out.append((None, parts[0]))
        elif len(parts) == 2:
            out.append((parts[0], parts[1]))
    return out


def allowed(f: dict, allow: list[tuple[str | None, str]]) -> bool:
    where = f"{f['path']}#{f['pointer']}"
    return any(
        tok == f["token"]
        and (g is None or fnmatch.fnmatch(f["path"], g) or fnmatch.fnmatch(where, g))
        for g, tok in allow
    )


def report(findings: list[dict], fmt: str) -> None:
    if fmt == "json":
        print(json.dumps(findings, indent=2))
        return
    for f in findings:
        at = f" at {f['pointer']}" if f["pointer"] else ""
        print(f"{f['path']}:{f['line']}: SRF-01 {f['family']} {f['token']!r}{at}: {f['context']}")


# --- self-test -----------------------------------------------------------------

PLANT_RE = re.compile(r"\bC-9[0-9]{2}\b")  # must be reported
DECOY_RE = re.compile(r"\bC-8[0-9]{2}\b")  # sits where nothing renders


def _expect(name: str, findings: list[dict], raw: str, fails: list[str]) -> None:
    got = {f["token"] for f in findings}
    missing = set(PLANT_RE.findall(raw)) - got
    decoys = {t for t in got if DECOY_RE.fullmatch(t)}
    if name.startswith("fail-") and not findings:
        fails.append(f"{name}: expected findings, got none")
    if name.startswith("pass-") and findings:
        fails.append(f"{name}: expected clean, got {sorted(got)}")
    if name.startswith("fail-") and missing:
        fails.append(f"{name}: planted tokens not reported: {sorted(missing)}")
    if decoys:
        fails.append(f"{name}: decoys reported: {sorted(decoys)}")


def self_test() -> int:
    base = Path(__file__).resolve().parent / "fixtures" / "interface_leak"
    fails: list[str] = []
    allow = load_allow(base / "output" / "allow.txt")
    for p in sorted((base / "output").glob("*-*")):
        text = p.read_text(encoding="utf-8")
        found = [
            f
            for f in scan_rendered(p.name, text, rust="python" not in p.name)
            if not allowed(f, allow)
        ]
        _expect(p.name, found, text, fails)
        if p.name == "fail-families.txt" and {f["family"] for f in found} != set(FAMILIES):
            fails.append(
                f"fail-families.txt: families missed: {sorted(set(FAMILIES) - {f['family'] for f in found})}"
            )
    if len(FAMILIES) != 20 or len(SRF01_PATTERNS) != 21:
        fails.append("token set is not 20 families in 21 patterns")
    with tempfile.TemporaryDirectory() as tmp:
        for tree in ("fail-source", "pass-source", "fail-published", "pass-published"):
            root = cc.copy_fixture(base / tree, Path(tmp) / tree)
            files = sorted(f for f in root.rglob("*") if f.is_file())
            found, _ = scan_source(root, published=tree.endswith("published"), files=files)
            _expect(tree, found, "\n".join(f.read_text(encoding="utf-8") for f in files), fails)
    screens = help_walk(shlex.join([sys.executable, str(base / "fake_cli.py.fixture")]))
    found = [f for label, text in screens for f in scan_rendered(label, text)]
    _expect("fail-help-walk", found, "\n".join(t for _, t in screens), fails)
    if len(screens) != 4:
        fails.append(f"help walk: {len(screens)} screens, want 4")
    for f in fails:
        print("FAIL", f)
    print("self-test: ok" if not fails else f"self-test: {len(fails)} failures")
    return 1 if fails else 0


# --- cli -------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="SRF-01 interface-leak scanner (SRF-03 gate).")
    ap.add_argument("inputs", nargs="*", help="rendered text: files, directories, or - for stdin")
    ap.add_argument(
        "--help-walk",
        action="append",
        default=[],
        metavar="CMD",
        help="capture CMD's --help recursively",
    )
    ap.add_argument("--lang", choices=("rust", "python", "ts"), default="rust")
    ap.add_argument("--source", action="store_true", help="scan a source tree (advisory)")
    ap.add_argument("--root", default=".")
    ap.add_argument("--published", action="store_true", help="source: every doc comment renders")
    ap.add_argument("--strict", action="store_true", help="source: exit 1 on findings")
    ap.add_argument("--allow", type=Path)
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    try:
        allow = load_allow(a.allow) if a.allow else []
    except OSError as e:
        print(f"missing input: {e}", file=sys.stderr)
        return 2
    if a.source:
        root = Path(a.root).resolve()
        if not root.is_dir():
            print(f"missing input: {root}", file=sys.stderr)
            return 2
        findings, stats = scan_source(root, a.published)
        findings = [f for f in findings if not allowed(f, allow)]
        report(findings, a.format)
        print(
            f"interface_leak: {len(findings)} findings (source, {'strict' if a.strict else 'advisory'}); {stats}",
            file=sys.stderr,
        )
        return 1 if findings and a.strict else 0
    if not a.inputs and not a.help_walk:
        ap.print_usage(sys.stderr)
        return 2
    rust = a.lang == "rust"
    findings: list[dict] = []
    try:
        for label, path in expand(a.inputs):
            text = (
                sys.stdin.read()
                if path is None
                else path.read_text(encoding="utf-8", errors="replace")
            )
            findings += scan_rendered(label, text, rust)
        for cmd in a.help_walk:
            for label, text in help_walk(cmd):
                findings += scan_rendered(label, text, rust)
    except (OSError, subprocess.TimeoutExpired, ValueError) as e:
        print(f"missing input: {e}", file=sys.stderr)
        return 2
    findings = [f for f in findings if not allowed(f, allow)]
    report(findings, a.format)
    print(f"interface_leak: {len(findings)} findings", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
