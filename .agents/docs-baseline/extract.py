#!/usr/bin/env python3
"""Baseline extractor: one inventory file per docs page.

Usage: extract.py [--src website/src] [--out .agents/docs-baseline/inventory]

Pages: every *.md under <src>/docs plus <src>/index.md and <src>/team.md. The inventory key is the path
relative to <src> (docs/reference/environment.md); the file is <out>/<key>.json with the lists
(anchors, commands, flags, env, config_keys, exit_codes, slugs, links, fences, facts) plus an int `lines`
(the sweep reader floor). `facts` is filled by the haiku workers and is preserved across re-runs.
The gitignored generated page docs/reference/dependencies.md exists only after a site build; its inventory was
produced from a checkout that had it (--src). extract_page() is shared with check.py, which runs it over the new tree.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import sys

LISTS = ("anchors", "commands", "flags", "env", "config_keys", "exit_codes", "slugs", "links", "fences", "facts")

# Words that can follow `ocx` in a command; frozen from the command-line reference headings plus retired spellings.
VOCAB = set(
    "about activate add allow announce attest cascade catalog check ci claim clean completion config copy create deps "
    "description deselect direnv env exec export freeze hook index init inspect install list lock login logout package "
    "patch prune publish pull push receipt regenerate remove repair revoke sbom select self setup shell sign state "
    "status sync test uninstall update upgrade verify version which why run describe info help".split()
)
FILE_EXT = {"sh", "fish", "nu", "elv", "ps1", "json", "toml", "md", "lock", "exe", "yml", "yaml"}
VALUE_FLAGS = {"--format", "--log-level", "--color", "--config", "--project", "--index", "--jobs", "--remote"}

FENCE = re.compile(r"^\s*(`{3,}|~{3,})\s*([^\s`{]*)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*$")
EXPLICIT = re.compile(r"\\?\{#([A-Za-z0-9_\-:.]+)\\?\}")
INLINE = re.compile(r"(`+)(.+?)\1")
CMD = re.compile(r"(?:^|[\s$(|;&\"'`])ocx((?:\s+[^\s`|;&)\"']+){0,8})")
FLAG = re.compile(r"(?<![\w-])--[a-z][a-z0-9]*(?:-[a-z0-9]+)*")
ENV = re.compile(r"(?<![A-Za-z0-9])_{0,2}OCX_[A-Z][A-Z0-9_]*")
EXIT = re.compile(r"\bexit(?:s|ed|ing)?\s+(?:with\s+)?(?:status\s+|codes?\s+)?`?(\d{1,3})\b", re.I)
SNAKE = re.compile(r"^[a-z][a-z0-9]*(?:_[a-z0-9]+)+$")
TOML_TABLE = re.compile(r"^\s*\[\[?([^\]\n]+)\]\]?\s*$")
TOML_KEY = re.compile(r"^\s*([A-Za-z0-9_\-.\"]+)\s*=")
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(\s*<?([^)\s>]+)")
REFDEF = re.compile(r"^\s{0,3}\[([^\]]+)\]:\s*<?(\S+?)>?(?:\s|$)")
REFUSE = re.compile(r"\[[^\]]*\]\[([^\]]*)\]")
HREF = re.compile(r"""(?:href|src)=["']([^"']+)["']""")


def page_files(src: pathlib.Path) -> list[pathlib.Path]:
    files = sorted((src / "docs").rglob("*.md"))
    files += [p for p in (src / "index.md", src / "team.md") if p.exists()]
    return files


def slugify(text: str) -> str:
    """VitePress heading slug: specials and whitespace become hyphens, collapsed, trimmed, lowercased."""
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"\[([^\]]*)\]\[[^\]]*\]", r"\1", text)
    text = text.replace("`", "")
    text = re.sub(r"<[^>]+>", "", text)
    s = re.sub(r"[\s~`!@#$%^&*()\-_+=\[\]{}|\\;:\"'“”‘’<>,.?/]+", "-", text)
    s = re.sub(r"-{2,}", "-", s).strip("-").lower()
    return "_" + s if re.match(r"^\d", s) else s


def split_page(text: str):
    """Yield (line_no, line, fence_lang_or_None); fence_lang is '' for an untagged fence, None outside."""
    lines = text.split("\n")
    i = 0
    if lines and lines[0].strip() == "---":
        for j in range(1, len(lines)):
            if lines[j].strip() == "---":
                i = j + 1
                break
    fence = None
    out = []
    for n in range(i, len(lines)):
        line = lines[n]
        m = FENCE.match(line)
        if fence is None:
            if m:
                fence = (m.group(1)[0], len(m.group(1)), m.group(2).lower())
                out.append((n + 1, line, "open", fence[2]))
            else:
                out.append((n + 1, line, None, None))
        else:
            if m and m.group(1)[0] == fence[0] and len(m.group(1)) >= fence[1] and not line.strip().strip("`~"):
                out.append((n + 1, line, "close", fence[2]))
                fence = None
            else:
                out.append((n + 1, line, "body", fence[2]))
    return out


def command_of(tail: str) -> str | None:
    toks = tail.split()
    words, k = [], 0
    while k < len(toks) and toks[k].startswith("-") and len(words) == 0 and k < 6:
        k += 2 if toks[k] in VALUE_FLAGS else 1
    while k < len(toks):
        t = toks[k]
        if t in VOCAB and re.fullmatch(r"[a-z][a-z-]*", t):
            words.append(t)
            k += 1
        else:
            break
    return "ocx " + " ".join(words) if words else None


def extract_page(text: str, top_tables: set[str] | None = None) -> dict:
    inv: dict[str, set[str]] = {k: set() for k in LISTS}
    parsed = split_page(text)
    implicit_seen: dict[str, int] = {}
    refs: dict[str, str] = {}
    prose: list[tuple[int, str]] = []
    code_lines: list[str] = []
    fence_buf: list[str] = []
    table_header = ""
    table_first = False
    toml_table = ""
    in_table = False

    for n, line, kind, lang in parsed:
        if kind == "open":
            fence_buf = []
            toml_table = ""
            continue
        if kind == "close":
            body = "\n".join(fence_buf)
            first = next((l.strip() for l in fence_buf if l.strip()), "")
            digest = hashlib.sha1(re.sub(r"\s+", " ", body).encode()).hexdigest()[:8]
            inv["fences"].add(f"{lang or 'text'}:{digest}: {first[:60]}")
            continue
        if kind == "body":
            fence_buf.append(line)
            code_lines.append(line)
            if lang.startswith("toml"):
                s = line.strip()
                if s.startswith("#"):
                    continue
                m = TOML_TABLE.match(line)
                if m:
                    toml_table = m.group(1).strip().replace('"', "")
                    inv["config_keys"].add(toml_table)
                    continue
                m = TOML_KEY.match(line)
                if m:
                    key = m.group(1).strip().replace('"', "")
                    inv["config_keys"].add(f"{toml_table}.{key}" if toml_table else key)
            continue
        if line.startswith("<<<"):
            inv["fences"].add("include:" + line[3:].strip())
        # prose line
        d = REFDEF.match(line)
        if d:
            refs[d.group(1).lower()] = d.group(2)
        prose.append((n, line))
        h = HEADING.match(line)
        if h:
            title = h.group(2)
            ex = EXPLICIT.search(title)
            if ex:
                inv["anchors"].add(ex.group(1))
            else:
                s = slugify(title)
                if s:
                    c = implicit_seen.get(s, 0)
                    implicit_seen[s] = c + 1
                    inv["anchors"].add(s if c == 0 else f"{s}-{c}")
        else:
            for ex in EXPLICIT.finditer(line):
                inv["anchors"].add(ex.group(1))
        for m in re.finditer(r"""<a\s+[^>]*\bid=["']([^"']+)["']""", line):
            inv["anchors"].add(m.group(1))

        # tables: numeric first cell of an exit-code table
        if line.lstrip().startswith("|"):
            if not in_table:
                in_table, table_header = True, line.lower()
                table_first = bool(re.search(r"\b(exit|code|status)\b", table_header.split("|")[1] if "|" in table_header else ""))
            m = re.match(r"^\s*\|\s*`?(\d{1,3})`?\s*\|", line)
            if m and table_first:
                inv["exit_codes"].add(m.group(1))
            if re.search(r"detail|slug|kind", table_header):
                m = re.match(r"^\s*\|\s*`([a-z][a-z0-9_]*)`\s*\|", line)
                if m and SNAKE.match(m.group(1)):
                    inv["slugs"].add(m.group(1))
        else:
            in_table = False

        for m in EXIT.finditer(line):
            if int(m.group(1)) <= 255:
                inv["exit_codes"].add(m.group(1))
        for m in INLINE.finditer(line):
            code_lines.append(m.group(2))
            tok = m.group(2).strip()
            if re.search(r"detail|slug|kind", line, re.I) and SNAKE.match(tok):
                inv["slugs"].add(tok)
            t = re.fullmatch(r"\[{1,2}([a-z][a-z0-9_.\-<>*]*)\]{1,2}", tok)
            key = t.group(1) if t else tok
            if (
                top_tables
                and re.fullmatch(r"[a-z_][a-z0-9_\-]*(?:\.[a-z0-9_<>\-*]+)*", key)
                and key.split(".")[0] in top_tables
                and key.rsplit(".", 1)[-1] not in FILE_EXT
            ):
                inv["config_keys"].add(key)

    alltext = "\n".join(l for _, l, _, _ in parsed)
    for _, line in prose:
        for m in LINK.finditer(line):
            inv["links"].add(m.group(1))
        for m in HREF.finditer(line):
            inv["links"].add(m.group(1))
        for m in REFUSE.finditer(line):
            ref = m.group(1).lower()
            if ref in refs:
                inv["links"].add(refs[ref])
        m = re.match(r"^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s|$)", line)
        if m:
            inv["links"].add(m.group(1))
    for m in ENV.finditer(alltext):
        name = m.group(0)
        if not name.endswith("_"):
            inv["env"].add(name)
    for line in code_lines:
        for m in FLAG.finditer(line):
            inv["flags"].add(m.group(0))
        for m in CMD.finditer(line):
            c = command_of(m.group(1))
            if c:
                inv["commands"].add(c)
    out = {k: sorted(inv[k]) for k in LISTS}
    out["lines"] = text.count("\n") + 1
    return out


def top_level_tables(files: list[pathlib.Path]) -> set[str]:
    tops: set[str] = set()
    for f in files:
        for _, line, kind, lang in split_page(f.read_text(encoding="utf-8")):
            if kind == "body" and lang.startswith("toml"):
                m = TOML_TABLE.match(line)
                if m:
                    tops.add(m.group(1).strip().replace('"', "").split(".")[0])
    return tops


def main(argv: list[str]) -> int:
    src, out = pathlib.Path("website/src"), pathlib.Path(__file__).parent / "inventory"
    it = iter(argv)
    for a in it:
        if a == "--src":
            src = pathlib.Path(next(it))
        elif a == "--out":
            out = pathlib.Path(next(it))
        else:
            print(__doc__)
            return 2
    files = page_files(src)
    if not files:
        print(f"no pages under {src}", file=sys.stderr)
        return 2
    tops = top_level_tables(files)
    for f in files:
        key = f.relative_to(src).as_posix()
        dest = out / (key + ".json")
        inv = extract_page(f.read_text(encoding="utf-8"), tops)
        if dest.exists():
            inv["facts"] = json.loads(dest.read_text())["facts"]
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(json.dumps(inv, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"{len(files)} pages -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
