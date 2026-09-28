#!/usr/bin/env python3
"""Code-to-record linkage checks for the code-docs rule set.

Rules covered:
  LNK-01  no bare process ID from a banned family in a comment         ids, MUST
  LNK-13  no short label (A2, H1) cited outside the block defining it  ids, SHOULD
  LNK-14  a person rules on every un-catalogued ID prefix              ids --discover
  LNK-02  every record pointer resolves: tracked file, unique anchor   pointers, MUST
  LNK-03  an anchor is a short heading prefix, never a line number     pointers, SHOULD
  LNK-07  a record URL is pinned to a commit, never to a branch        pointers, SHOULD
  LNK-06  a diff that deletes or renames a path repairs every record
          line that cites it                                           records, MUST
A SHOULD finding prints as advisory and never sets the exit code. The one
exception is --discover, which exits 1 while any prefix awaits a verdict.

ids reads comment text only, through comment_census.classify(): doc, line and
interface comments, and trailing comments on code lines. The ten prefixed
families (C- S- WP- DEC- DX- RUL- A- D- D-V ADR-) and the C-S1-1 clause label
are built in and cannot be removed; the config adds a repo's own. A short label is exempt when its own comment block
defines it ("H1: ..." at a line start, banner leaders absorbed), when it is a
bare version (V2), when it follows an RFC number (RFC 9111 S4.3.4), or when it
sits in a backtick span that holds a [...] character class. --discover counts
every other PREFIX-N in non-test comments. It lists each prefix at or over
--threshold hits that the config neither adds as a family nor records as a
non-ID, so a person gives it a verdict.

pointers reads [repo:]path.md[#anchor] in comments. A bare file name counts
when it starts with a record stem (adr_, plan_, rulings_, subsystem-, ...), and
a path counts when one of its directories is adr, adrs or decisions, or when
a glob under the config's records matches it, as written or as it resolves. Requiring
.md keeps identifiers such as plan_for, and rustdoc links such as
[`plan_for`], out by construction. URLs are cut before matching. A block's
lines are joined first, so a pointer wrapped after "_" or "/" reads whole. A
file resolves when git ls-files of the named repo holds it, as a path or a
path suffix. An anchor resolves when it is a hyphen-boundary prefix of exactly
one heading slug, or of exactly one **ID** definition. Slugs follow the
Python-Markdown toc algorithm, as docs-quality's link check does.

records diffs the merge base of --base and --head (default HEAD) with rename
detection. For each deleted or renamed path, it reads every record that
matches the globs and existed at the merge base, as it stands at --head. Each
line that still names the old path is a finding, unless the line also names
the rename destination. Paths match on a path boundary, so src/lib.rs never
matches crates/x/src/lib.rs.

Ratchet (ids, pointers): the baseline is a flat JSON {key: count} with keys FILE::bare-id-prod, FILE::bare-id-test,
FILE::short-label and FILE::dead-pointer, and a missing key reads as 0.
--check fails on any rise. --update writes drops freely and refuses a rise
unless --allow-regression is given. Each subcommand rewrites only its own
keys, so both can share one baseline file. Non-test scopes (examples,
scripts, tools) count under -prod, because the rule covers every comment.

Config: .code-docs-linkage-config.json at the root when it exists, or
--config FILE. JSON, every key optional:
  {"families": ["SIM"], "non_ids": ["CWE"], "records": ["docs/adr/*.md"],
   "repos": {"infra": "../infra"}}
repos maps a pointer's repo: qualifier to a clone, relative to the config
file. A qualifier equal to the root directory's own name resolves locally.

Usage:
  linkage_check.py ids [--root DIR] [--config F] [--check B | --update B [--allow-regression]]
  linkage_check.py ids --discover [--threshold N] [--root DIR] [--config F]
  linkage_check.py pointers [--root DIR] [--config F] [--check B | --update B [--allow-regression]]
  linkage_check.py records --base REF [--head REF] [--root DIR] [--config F]
  linkage_check.py --self-test
Every subcommand takes --format text|json.

Exit codes: 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import bisect
import fnmatch
import json
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from functools import cache
from pathlib import Path, PurePosixPath

sys.dont_write_bytecode = True  # no __pycache__ beside an installed copy (CLN-05 would read it)
sys.path.insert(0, str(Path(__file__).resolve().parent))
import comment_census as cc  # noqa: E402 - after the bytecode switch above

HEADING_RE = re.compile(r"^#{1,6}\s+(.*?)\s*$")
EXPLICIT_ID_RE = re.compile(r"\{\s*#\s*([A-Za-z0-9_.:-]+)[^}]*\}")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")


def slugify(text: str) -> str:
    """The Python-Markdown toc slug; underscores survive, hyphen and space runs collapse."""
    text = re.sub(r"\{[^}]*\}", "", text)
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"[^\w\s-]", "", text.lower()).strip()
    return re.sub(r"[-\s]+", "-", text)


def heading_anchors(text: str) -> set[str]:
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
        if h:
            explicit = EXPLICIT_ID_RE.search(h.group(1))
            out.add(explicit.group(1) if explicit else slugify(h.group(1)))
    out.update(re.findall(r'<a\s+(?:id|name)="([^"]+)"', text))
    return out


HERE = Path(__file__).resolve().parent

GIT = shutil.which("git") or "git"

# --- ids ---------------------------------------------------------------------

# LNK-01. Word boundaries keep SPDX-2.3, UTF-8, SHA-256 and CWE-79 out.
FAMILY_ALTS = [
    r"C-S\d+(?:-\d+)?",  # clause label C-S1-1, before C- so it matches whole
    r"D-V\d{1,2}",
    r"C-\d{1,4}",
    r"S-\d{1,3}",
    r"WP-\d{1,3}",
    r"DEC-[A-Za-z0-9]{1,6}",
    r"DX-\d{1,3}",
    r"RUL-\d{1,3}",
    r"A-\d{1,3}",
    r"D-\d{1,3}",
    r"ADR-\d{1,4}",
]
FAMILY_PREFIXES = {"C", "S", "WP", "DEC", "DX", "RUL", "A", "D", "ADR"}
# LNK-14. Standards that share the PREFIX-N shape. A repo records its own
# verdicts in the config's non_ids.
NON_IDS = {"CWE", "CVE", "UTF", "UCS", "SHA", "CRC", "RFC", "ISO", "IEC", "IEEE", "ECMA", "PEP"}
# A compound prefix (DOC-TYPE-01) is one verdict unit, not TYPE.
GENERIC_RE = re.compile(r"\b([A-Z]{1,6}(?:-[A-Z]{1,6})*)-\d{1,4}\b")

# LNK-13
SHORT_RE = re.compile(r"\b[A-Z]\d{1,2}[a-z]?\b")
DEF_RE = re.compile(r"^[\s\-\u2500\u2501=*#]{0,8}([A-Z]\d{1,2}[a-z]?):(?:\s|$)")
VERSION_RE = re.compile(r"V\d{1,2}")
RFC_TAIL_RE = re.compile(r"\bRFC[ -]?\d{3,5}\b[^A-Za-z0-9]{0,4}$")
CHAR_CLASS_RE = re.compile(r"\[[^\]`]*\]")

# --- pointers ----------------------------------------------------------------

RECORD_STEM_RE = re.compile(r"^(?:adr|plan|rulings?|subsystem|decisions?|design_spec|handover)[-_]")
RECORD_DIRS = {"adr", "adrs", "decisions"}
POINTER_RE = re.compile(
    r"(?<![\w./-])(?:(?P<repo>[a-z][a-z0-9_-]*):)?"
    r"(?P<path>(?:[\w.-]+/)*[\w.-]+\.md)(?:#(?P<anchor>[\w.-]+))?(?![\w/-])"
)
URL_RE = re.compile(r"https?://[^\s<>`'\"()\[\]]+")
PIN_RE = re.compile(r"/(?:-/)?blob/(?P<ref>[^/]+)/(?P<path>[^#?]+\.md)")
SHA_RE = re.compile(r"[0-9a-f]{7,40}")
ID_DEF_RE = re.compile(
    r"^\s*(?:[-*+]\s+|\d+\.\s+)?\*\*([A-Z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)*)\*\*", re.MULTILINE
)

# --- records -----------------------------------------------------------------

DEFAULT_RECORDS = [
    ".claude/artifacts/adr_*.md",
    ".claude/rules/*.md",
    ".agents/adr/*.md",
    ".agents/adrs/*.md",
    "doc/adr/*.md",
    "docs/adr/*.md",
    "docs/decisions/*.md",
    "AGENTS.md",
    "CLAUDE.md",
]
PATH_CHARS = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-/")

OWNED = {"ids": ("bare-id-prod", "bare-id-test", "short-label"), "pointers": ("dead-pointer",)}


class MissingInputError(Exception):
    """Exit 2: a ref, repo, config, baseline or helper module is missing."""


def git(root: Path, *args: str, inp: str | None = None, ok: tuple[int, ...] = (0,)) -> str:
    res = subprocess.run(  # noqa: S603 - fixed git argv, no shell
        [GIT, "-C", str(root), *args],
        input=inp.encode() if inp is not None else None,
        capture_output=True,
        check=False,
    )
    if res.returncode not in ok:
        err = res.stderr.decode("utf-8", "replace").strip()
        raise MissingInputError(f"git {args[0]} in {root}: {err}")
    return res.stdout.decode("utf-8", "replace")


SHOULD = {"LNK-03", "LNK-07", "LNK-13", "LNK-14"}


def finding(path: str, line: int, rule: str, message: str, key: str = "") -> dict:
    severity = "should" if rule in SHOULD else "must"
    return {
        "path": path,
        "line": line,
        "rule": rule,
        "severity": severity,
        "message": message,
        "key": key,
    }


def comment_blocks(root: Path):
    """Yield (path, block) per comment block; a block is [(line, text, scope)].

    Runs of doc, line or interface lines with one test flag form a block. A
    trailing comment on a code line is a block of its own."""
    for f in cc.list_files(root):
        try:
            src = f.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if cc.GENERATED_RE.search("\n".join(src.split("\n", 6)[:6])):
            continue
        rel = f.relative_to(root)
        fscope = cc.scope_of(rel)
        lines = cc.classify(rel.as_posix(), src, cc.EXT_LANG[f.suffix])
        raw = src.split("\n")
        block: list[tuple[int, str, str]] = []
        prev = None
        for i, ln in enumerate(lines):
            text = ln.text
            if ln.kind == "interface" and not text:  # Python interface docstrings carry no text
                text = raw[i].strip().strip("\"'")
            scope = "test" if ln.test or fscope == "test" else "prod"
            key = (ln.kind, ln.test) if ln.kind in ("doc", "line", "interface") else None
            if key != prev and block:
                yield rel.as_posix(), block
                block = []
            prev = key
            if key:
                block.append((i + 1, text, scope))
            elif ln.kind == "code" and text:
                yield rel.as_posix(), [(i + 1, text, scope)]
        if block:
            yield rel.as_posix(), block


def family_re(extra: list[str]) -> re.Pattern:
    alts = FAMILY_ALTS + [re.escape(p) + r"-\d{1,4}" for p in extra]
    return re.compile(r"\b(?:" + "|".join(alts) + r")\b")


def block_labels(texts: list[str]) -> set[str]:
    return {m.group(1) for t in texts if (m := DEF_RE.match(t))}


def short_labels(text: str, defined: set[str]) -> list[str]:
    """Short labels in text that LNK-13 flags, in order."""
    out = []
    for m in SHORT_RE.finditer(text):
        tok = m.group()
        if tok in defined or VERSION_RE.fullmatch(tok) or RFC_TAIL_RE.search(text[: m.start()]):
            continue
        before = text[: m.start()]
        if before.count("`") % 2:
            end = text.find("`", m.end())
            if CHAR_CLASS_RE.search(text[before.rfind("`") : end if end != -1 else len(text)]):
                continue
        out.append(tok)
    return list(dict.fromkeys(out))


def scan_ids(root: Path, cfg: dict) -> tuple[list[dict], Counter, dict]:
    fam = family_re(cfg.get("families", []))
    known = FAMILY_PREFIXES | set(cfg.get("families", [])) | NON_IDS | set(cfg.get("non_ids", []))
    findings: list[dict] = []
    prefixes: Counter = Counter()
    example: dict[str, tuple[str, int]] = {}
    for rel, block in comment_blocks(root):
        defined = block_labels([t for _, t, _ in block])
        for n, text, scope in block:
            for m in GENERIC_RE.finditer(text):
                if scope == "prod" and m.group(1) not in known:
                    prefixes[m.group(1)] += 1
                    example.setdefault(m.group(1), (rel, n))
            ids = fam.findall(text)
            if ids:
                msg = f"bare process ID {', '.join(dict.fromkeys(ids))}: delete it or state the constraint"
                findings.append(finding(rel, n, "LNK-01", msg, f"{rel}::bare-id-{scope}"))
            elif labels := short_labels(text, defined):
                msg = f"short label {', '.join(labels)} is not defined in this block; say what it names, or cite the record that defines it"
                findings.append(finding(rel, n, "LNK-13", msg, f"{rel}::short-label"))
    return findings, prefixes, example


# --- pointers ----------------------------------------------------------------


def is_record(path: str, root: Path | None = None, globs: list[str] | None = None) -> bool:
    """A record's path: a record stem or directory, or, given the config's records globs,
    a path one of them matches as written or as it resolves in root (a repo's own records)."""
    p = PurePosixPath(path)
    if RECORD_STEM_RE.match(p.name) or any(d in RECORD_DIRS for d in p.parts[:-1]):
        return True
    names = [path, *(resolve(root, path) if root and globs else [])]
    return any(fnmatch.fnmatchcase(n, g) for n in names for g in globs or [])


@cache
def tracked(root: Path) -> dict[str, list[str]]:
    """Tracked files of a clone, by file name."""
    by_name: dict[str, list[str]] = {}
    for f in git(root, "ls-files", "-z").split("\0"):
        if f:
            by_name.setdefault(PurePosixPath(f).name, []).append(f)
    return by_name


def resolve(root: Path, path: str) -> list[str]:
    parts = [p for p in PurePosixPath(path).parts if p not in (".", "..")]
    rel = "/".join(parts)
    return [f for f in tracked(root).get(parts[-1], []) if f == rel or f.endswith("/" + rel)]


@cache
def anchor_pools(file: Path) -> tuple[list[str], list[str]]:
    """(heading slugs, **ID** definition slugs) of one record."""
    text = file.read_text(encoding="utf-8", errors="replace")
    ids = [
        slugify(m.group(1))
        for m in ID_DEF_RE.finditer(text)
        if any(c.isdigit() for c in m.group(1))
    ]
    return sorted(heading_anchors(text)), ids


def prefix_hits(anchor: str, pool: list[str]) -> list[str]:
    return [s for s in pool if s == anchor or s.startswith(anchor + "-")]


def check_anchor(file: Path, anchor: str) -> tuple[bool, str]:
    """(resolves, detail). detail names the shorter prefix when one suffices."""
    seen = []
    for pool in anchor_pools(file):
        hits = prefix_hits(anchor, pool)
        if len(hits) == 1:
            segs = hits[0].split("-")
            short = next(
                p
                for k in range(1, len(segs) + 1)
                if len(prefix_hits(p := "-".join(segs[:k]), pool)) == 1
            )
            return True, short if len(short) < len(anchor) else ""
        seen.append(hits)
    many = max(seen, key=len)
    if many:
        return False, f"matches {len(many)} headings ({', '.join(many[:3])})"
    return False, "matches no heading or **ID** definition"


def join_block(block: list[tuple[int, str, str]]) -> tuple[str, list[int]]:
    """One string per block, and each line's start offset in it."""
    text, starts = "", []
    for _, t, _ in block:
        if text and not text.endswith(("_", "/")):
            text += " "
        starts.append(len(text))
        text += t.strip()
    return text, starts


def scan_pointers(root: Path, cfg: dict) -> tuple[list[dict], int]:
    """(findings, pointers into repos whose clone is absent)."""
    repos: dict[str, Path] = cfg.get("repos", {})
    findings: list[dict] = []
    absent = 0
    for rel, block in comment_blocks(root):
        text, starts = join_block(block)

        def at(off: int, block=block, starts=starts) -> int:
            return block[bisect.bisect_right(starts, off) - 1][0]

        for m in URL_RE.finditer(text):
            pin = PIN_RE.search(m.group())
            if pin and is_record(pin["path"]) and not SHA_RE.fullmatch(pin["ref"]):
                msg = f"record URL pinned to branch '{pin['ref']}': write repo:path.md#anchor, or pin a commit"
                findings.append(finding(rel, at(m.start()), "LNK-07", msg))
        text = URL_RE.sub(lambda m: " " * len(m.group()), text)
        for m in POINTER_RE.finditer(text):
            path, repo = m["path"], m["repo"]
            if not is_record(path, None if repo else root, cfg.get("records")):
                continue
            n, shown = at(m.start()), m.group()
            dead = f"{rel}::dead-pointer"
            if re.match(r":\d", text[m.end() :]):
                findings.append(
                    finding(rel, n, "LNK-03", f"{shown} names a line number: anchor a heading")
                )
            target = root
            if repo:
                target = repos.get(repo) or (root if repo == root.name else None)
                if target is None:
                    msg = f"{shown}: repo '{repo}' is not in the config's repos map"
                    findings.append(finding(rel, n, "LNK-02", msg, dead))
                    continue
                if not target.is_dir():
                    absent += 1
                    continue
            hits = resolve(target, path)
            if not hits:
                where = f" in repo '{repo}'" if repo else ""
                findings.append(finding(rel, n, "LNK-02", f"{shown}: no tracked file{where}", dead))
                continue
            anchor = (m["anchor"] or "").rstrip(".-").lower()
            if not anchor:
                continue
            verdicts = [check_anchor(target / h, anchor) for h in hits]
            good = [d for ok, d in verdicts if ok]
            if not good:
                msg = f"{shown}: anchor #{anchor} {verdicts[0][1]} in {hits[0]}"
                findings.append(finding(rel, n, "LNK-02", msg, dead))
            elif all(good):
                findings.append(finding(rel, n, "LNK-03", f"{shown}: #{good[0]} is enough"))
    return findings, absent


# --- records -----------------------------------------------------------------


def cites(line: str, path: str) -> bool:
    """path occurs in line on a path boundary. A run of ./ and ../ segments may
    precede it, as in a relative markdown link from a record."""
    i = line.find(path)
    while i != -1:
        k = i
        while line[:k].endswith(("../", "./")):
            k -= 3 if line[:k].endswith("../") else 2
        after = line[i + len(path) : i + len(path) + 2]
        before_ok = k == 0 or line[k - 1] not in PATH_CHARS | {"."}
        after_ok = not after or (
            after[0] not in PATH_CHARS and not (after[0] == "." and after[1:].isalnum())
        )
        if before_ok and after_ok:
            return True
        i = line.find(path, i + 1)
    return False


def scan_records(root: Path, base: str, head: str, globs: list[str]) -> tuple[list[dict], dict]:
    head_sha = git(root, "rev-parse", "--verify", head + "^{commit}").strip()
    merge_base = git(root, "merge-base", base, head_sha).strip()
    fields = git(root, "diff", "-M", "--name-status", "-z", merge_base, head_sha).split("\0")
    gone: dict[str, str | None] = {}
    added: set[str] = set()
    i = 0
    while i < len(fields) and fields[i]:
        st = fields[i][0]
        if st in "RC":
            src, dst = fields[i + 1], fields[i + 2]
            if st == "R":
                gone[src] = dst
            else:
                added.add(dst)
            i += 3
            continue
        if st == "D":
            gone[fields[i + 1]] = None
        elif st == "A":
            added.add(fields[i + 1])
        i += 2
    summary = {
        "old_paths": len(gone),
        "stale_lines": 0,
        "records": 0,
        "renamed_citations": 0,
        "deleted_citations": 0,
        "scanned": len(
            git(root, "grep", "-l", "-I", "-e", "", head_sha, "--", *globs, ok=(0, 1)).splitlines()
        ),
    }
    if not gone:  # git grep with an empty pattern list may match every line
        return [], summary
    out = git(
        root,
        "grep",
        "-n",
        "-z",
        "-I",
        "-F",
        "-f",
        "-",
        head_sha,
        "--",
        *globs,
        inp="\n".join(gone) + "\n",
        ok=(0, 1),
    )
    findings: list[dict] = []
    for row in out.splitlines():
        parts = row.split("\0")
        if len(parts) < 3:
            continue
        path, n, text = parts[0].split(":", 1)[1], int(parts[1]), "\0".join(parts[2:])
        if path in added:  # a record the branch adds may describe the old layout
            continue
        stale = [
            (old, new)
            for old, new in gone.items()
            if old in text and cites(text, old) and not (new and cites(text, new))
        ]
        if not stale:
            continue
        said = []
        for old, new in stale:
            said.append(f"{old} (renamed to {new})" if new else f"{old} (deleted)")
            summary["renamed_citations" if new else "deleted_citations"] += 1
        msg = f"cites {'; '.join(said)} on this branch: drop the old path or name its new one"
        bare = all("/" not in old for old, _ in stale)
        if bare:  # a root filename such as tool.toml is often vocabulary, not a citation
            msg += "; a bare root filename, confirm it names this repo's file"
        findings.append(finding(path, n, "LNK-06", msg))
        if bare:
            findings[-1]["severity"] = "should"
    summary["stale_lines"] = len(findings)
    summary["records"] = len({f["path"] for f in findings})
    return findings, summary


# --- ratchet and output ------------------------------------------------------


def ratchet(
    counts: Counter, baseline: Path, owned: tuple[str, ...], update: bool, allow: bool
) -> tuple[list[str], list[str], set[str]]:
    """(rises, drops, risen files). With update, writes the baseline unless a
    rise is refused; keys other subcommands own are kept as they are."""
    try:
        base = json.loads(baseline.read_text(encoding="utf-8")) if baseline.exists() else {}
    except (OSError, ValueError) as e:
        raise MissingInputError(f"baseline {baseline}: {e}") from e
    if not isinstance(base, dict):
        raise MissingInputError(f"baseline {baseline}: not a JSON object")
    mine = {k: v for k, v in base.items() if k.rpartition("::")[2] in owned}
    rises, drops, risen = [], [], set()
    for key in sorted(set(counts) | set(mine)):
        live, entry = counts.get(key, 0), mine.get(key, 0)
        if live > entry:
            rises.append(f"{key}: {live} live, baseline {entry} (+{live - entry})")
            risen.add(key.rpartition("::")[0])
        elif live < entry:
            drops.append(f"{key}: dropped to {live} (baseline {entry}): run --update and commit")
    if update and (allow or not rises):
        merged = {k: v for k, v in base.items() if k not in mine} | {
            k: v for k, v in counts.items() if v
        }
        baseline.write_text(
            json.dumps(dict(sorted(merged.items())), indent=1) + "\n", encoding="utf-8"
        )
        drops = []
    return rises, drops, risen


def emit(findings: list[dict], fmt: str, extra: dict | None = None) -> None:
    findings = sorted(findings, key=lambda f: (f["path"], f["line"], f["rule"]))
    if fmt == "json":
        print(json.dumps({"findings": findings, **(extra or {})}, indent=1))
        return
    for f in findings:
        tag = " (advisory)" if f["severity"] == "should" else ""
        print(f"{f['path']}:{f['line']}: {f['rule']}{tag} {f['message']}")


CONFIG_NAME = ".code-docs-linkage-config.json"


def load_config(root: Path, path: str | None) -> dict:
    p = Path(path) if path else root / CONFIG_NAME
    if not path and not p.is_file():
        return {}
    try:
        cfg = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise MissingInputError(f"config {p}: {e}") from e
    if not isinstance(cfg, dict):
        raise MissingInputError(f"config {p}: not a JSON object")
    cfg["repos"] = {k: (p.parent / v).resolve() for k, v in cfg.get("repos", {}).items()}
    return cfg


def run_ratcheted(a: argparse.Namespace, findings: list[dict], summary: dict) -> int:
    must = [f for f in findings if f["severity"] == "must"]
    baseline = a.check or a.update
    if not baseline:
        emit(findings, a.format, {"summary": summary})
        if a.format == "text":
            print(f"{a.cmd}: {json.dumps(summary)}", file=sys.stderr)
        return 1 if must else 0
    counts = Counter(f["key"] for f in findings if f["key"])
    rises, drops, risen = ratchet(
        counts, Path(baseline), OWNED[a.cmd], bool(a.update), a.allow_regression
    )
    accepted = bool(a.update and a.allow_regression)
    shown = [] if accepted else [f for f in findings if f["path"] in risen]
    if a.format == "json":
        emit(shown, "json", {"summary": summary, "rises": rises, "drops": drops})
    else:
        emit(shown, "text")
        for r in rises:
            print(f"ratchet: {'raised' if accepted else 'rise'} {r}")
        for d in drops:
            print(f"ratchet: {d}")
    return 1 if rises and not accepted else 0


def main(argv: list[str]) -> int:
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", default=".")
    common.add_argument("--config")
    common.add_argument("--format", choices=("text", "json"), default="text")
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--self-test", action="store_true")
    sub = ap.add_subparsers(dest="cmd")
    for name in ("ids", "pointers"):
        p = sub.add_parser(name, parents=[common])
        g = p.add_mutually_exclusive_group()
        g.add_argument("--check", metavar="BASELINE")
        g.add_argument("--update", metavar="BASELINE")
        p.add_argument("--allow-regression", action="store_true")
        if name == "ids":
            p.add_argument("--discover", action="store_true")
            p.add_argument("--threshold", type=int, default=5)
    r = sub.add_parser("records", parents=[common])
    r.add_argument("--base", required=True)
    r.add_argument("--head", default="HEAD")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    if not a.cmd:
        ap.error("name a subcommand: ids, pointers or records")
    root = Path(a.root).resolve()
    if not root.is_dir():
        print(f"missing input: {root}", file=sys.stderr)
        return 2
    try:
        cfg = load_config(root, a.config)
        if a.cmd == "records":
            findings, summary = scan_records(
                root, a.base, a.head, cfg.get("records", DEFAULT_RECORDS)
            )
            emit(findings, a.format, {"summary": summary})
            if a.format == "text":
                print(f"records: {json.dumps(summary)}", file=sys.stderr)
            if not summary["scanned"]:
                print(
                    "records: no file matched the record globs; name the repo's records under `records` in the config",
                    file=sys.stderr,
                )
            return 1 if any(f["severity"] == "must" for f in findings) else 0
        if a.cmd == "pointers":
            git(root, "rev-parse", "--git-dir")  # pointers resolve against git ls-files
            findings, absent = scan_pointers(root, cfg)
            summary = {
                "lnk02": sum(f["rule"] == "LNK-02" for f in findings),
                "absent_repo_pointers": absent,
            }
            summary |= {r: sum(f["rule"] == r for f in findings) for r in ("LNK-03", "LNK-07")}
            return run_ratcheted(a, findings, summary)
        findings, prefixes, example = scan_ids(root, cfg)
        if a.discover:
            verdict = "add it to the config's families or non_ids"
            found = [
                finding(
                    *example[p], "LNK-14", f"prefix {p}- has {c} non-test comment hits: {verdict}"
                )
                for p, c in prefixes.most_common()
                if c >= a.threshold
            ]
            emit(found, a.format)
            return 1 if found else 0
        summary = {
            "bare_id_lines": sum(f["rule"] == "LNK-01" for f in findings),
            "bare_id_test": sum(f["key"].endswith("-test") for f in findings),
            "short_label_lines": sum(f["rule"] == "LNK-13" for f in findings),
            "files": len({f["path"] for f in findings}),
        }
        return run_ratcheted(a, findings, summary)
    except MissingInputError as e:
        print(f"missing input: {e}", file=sys.stderr)
        return 2


# --- self-test -----------------------------------------------------------------

FIX = HERE / "fixtures" / "linkage_check"
GIT_ID = [
    "-c",
    "user.name=self-test",
    "-c",
    "user.email=self-test@invalid",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.hooksPath=/dev/null",
]


def make_repo(src: Path, dst: Path) -> Path:
    """Copy a fixture tree into a fresh repo and commit it. Files named
    untracked-* stay on disk but out of the index."""
    cc.copy_fixture(src, dst)
    git(dst, "init", "-q")
    commit_tree(dst)
    return dst


def commit_tree(repo: Path) -> None:
    git(repo, "add", "-A", "--", ".", ":(exclude,glob)**/untracked-*")
    git(repo, *GIT_ID, "commit", "-qm", "fixture", "--allow-empty")


def expect(findings: list[dict], expected: Path, fails: list[str]) -> None:
    got = {f"{f['path']}:{f['line']}: {f['rule']}" for f in findings}
    want = {
        x.strip()
        for x in expected.read_text(encoding="utf-8").splitlines()
        if x.strip() and not x.startswith("#")
    }
    fails += [f"{expected.parent.name}: missed {w}" for w in sorted(want - got)]
    fails += [f"{expected.parent.name}: unexpected {g}" for g in sorted(got - want)]


def self_test() -> int:
    fails: list[str] = []
    fam = family_re([])
    for row in (FIX / "short_labels.txt").read_text(encoding="utf-8").splitlines():
        if row[:2] not in ("+ ", "- ", "~ "):
            continue
        text = row[2:]
        hit = bool(fam.search(text) or short_labels(text, block_labels([text])))
        if hit != (row[0] != "-"):
            fails.append(f"short_labels: {'flagged' if hit else 'missed'} {text!r}")
    with tempfile.TemporaryDirectory() as tmp_dir:
        tmp = Path(tmp_dir)
        ids_root = make_repo(FIX / "ids", tmp / "ids")
        findings, _, _ = scan_ids(ids_root, {})
        expect(findings, FIX / "ids" / "expected.txt", fails)

        base = tmp / "baseline.json"
        counts = Counter(f["key"] for f in findings)
        rises, _, _ = ratchet(counts, base, OWNED["ids"], update=True, allow=False)
        if not rises or base.exists():
            fails.append("ratchet: --update created a baseline without --allow-regression")
        ratchet(counts, base, OWNED["ids"], update=True, allow=True)
        if ratchet(counts, base, OWNED["ids"], update=False, allow=False)[0]:
            fails.append("ratchet: --check red on an unchanged tree")
        key = next(iter(counts))
        rises, _, risen = ratchet(
            counts + Counter({key: 1}), base, OWNED["ids"], update=False, allow=False
        )
        if not rises or key.rpartition("::")[0] not in risen:
            fails.append("ratchet: --check green on a one-line rise")
        if not ratchet(counts - Counter({key: 1}), base, OWNED["ids"], update=False, allow=False)[
            1
        ]:
            fails.append("ratchet: a drop printed no --update notice")
        ratchet(
            Counter({"x.rs::dead-pointer": 1}), base, OWNED["pointers"], update=True, allow=True
        )
        kept = json.loads(base.read_text(encoding="utf-8"))
        if key not in kept or "x.rs::dead-pointer" not in kept:
            fails.append("ratchet: one subcommand's --update dropped the other's keys")

        for name, want in (("discover-red", ["FOO"]), ("discover-green", [])):
            _, prefixes, _ = scan_ids(make_repo(FIX / name, tmp / name), {})
            if [p for p, c in prefixes.items() if c >= 5] != want:
                fails.append(f"{name}: flagged {dict(prefixes)}, want {want}")
        red = tmp / "discover-red"
        (red / CONFIG_NAME).write_text('{"non_ids": ["FOO"]}', encoding="utf-8")
        if any(c >= 5 for c in scan_ids(red, load_config(red, None))[1].values()):
            fails.append(f"config: {CONFIG_NAME} at the root was not read by default")

        other = make_repo(FIX / "pointers-other", tmp / "pointers-other")
        findings, _ = scan_pointers(
            make_repo(FIX / "pointers", tmp / "pointers"), {"repos": {"other": other}}
        )
        expect(findings, FIX / "pointers" / "expected.txt", fails)
        own = make_repo(FIX / "pointers-records", tmp / "pointers-records")
        findings, _ = scan_pointers(own, load_config(own, None))
        expect(findings, FIX / "pointers-records" / "expected.txt", fails)

        rec = make_repo(FIX / "records" / "base", tmp / "records")
        for p in rec.iterdir():
            if p.is_dir() and p.name != ".git":
                shutil.rmtree(p)
            elif p.is_file():
                p.unlink()
        cc.copy_fixture(FIX / "records" / "head", rec)
        commit_tree(rec)
        findings, _ = scan_records(rec, "HEAD~1", "HEAD", DEFAULT_RECORDS)
        expect(findings, FIX / "records" / "expected.txt", fails)
        if {f["path"] for f in findings if f["severity"] == "should"} != {"docs/adr/adr_d.md"}:
            fails.append("records: a bare root filename must be advisory, and only it")
    for f in fails:
        print(f"self-test: {f}")
    print("self-test: ok" if not fails else f"self-test: FAILED, {len(fails)} mismatches")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
