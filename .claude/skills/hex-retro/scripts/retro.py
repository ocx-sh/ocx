#!/usr/bin/env python3
"""retro.py -- hex-retro inbox/ledger CLI.

Single-file, stdlib-only implementation (Python >= 3.11; uses stdlib
`tomllib` to read `grimoire.lock`) of the hex-retro entry/mine/fold/import
pipeline described in the sibling `../SKILL.md`. Never touches the network,
never imports a third-party module.

Usage:
    python3 retro.py where
    python3 retro.py read
    python3 retro.py mine [--transcripts DIR]
    python3 retro.py fold DECISIONS [--wall-min N]
    python3 retro.py import ENTRIES
    python3 retro.py selftest

Exit codes:
    0 - ok
    1 - selftest failure
    2 - invalid input (nothing written)
    3 - environment error (not a git work tree, unsafe home, thresholds
        table not found)

Refusals and environment errors are reported on stderr as one line:
"Error: <reason>".
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from types import SimpleNamespace
from typing import NoReturn

# Exit codes (C-1429).
EXIT_OK = 0
EXIT_SELFTEST_FAIL = 1
EXIT_INVALID_INPUT = 2
EXIT_ENV = 3

# C-1424 names only -- the values live in ../SKILL.md, never here.
THRESHOLD_NAMES = ("nudge-entries", "bar-cost-fraction", "bar-occurrences", "bar-folds",
                   "proposal-cap", "mine-slow-min", "mine-errors")
KINDS = ("slow", "inconvenient", "pitfall", "defect")
SEVERITIES = ("low", "medium", "high")
NAME_RE = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}")
ID_RE = re.compile(r"[a-z0-9][a-z0-9-]{2,79}")
MAX_ENTRY, MAX_TEXT, MAX_DEPTH = 16 * 1024, 2000, 32
CLIENT_DIRS = {".claude", ".cursor", ".codex", ".github", ".gemini", ".opencode", ".kiro",
               ".junie", ".windsurf", ".zed", ".amp", ".cline", ".clinerules", ".goose",
               ".roo", ".continue", ".agent", ".vscode", ".idea"}


def die(code: int, msg: str) -> NoReturn:
    print(f"Error: {msg}", file=sys.stderr)
    raise SystemExit(code)


def emit(obj: object) -> int:
    print(json.dumps(obj, indent=2))
    return EXIT_OK


def load_thresholds(skill_md: Path | None = None) -> dict[str, float]:
    """Parse the SKILL.md § Thresholds table (C-1424); exit 3 when unusable."""
    skill_md = skill_md or Path(__file__).resolve().parent.parent / "SKILL.md"
    try:
        text = skill_md.read_text(encoding="utf-8")
    except OSError:
        text = ""
    section = re.search(r"^#+ [^\n]*Thresholds[^\n]*$(.*?)(?=^#|\Z)", text, re.M | re.S)
    rows = dict(re.findall(r"^\|\s*`([a-z0-9-]+)`\s*\|\s*([^|\n]*?)\s*\|",
                           section.group(1) if section else "", re.M))
    try:
        return {n: float(rows[n]) for n in THRESHOLD_NAMES}
    except (KeyError, ValueError):
        die(EXIT_ENV, "thresholds table not found")


# --- paths (C-1426) --------------------------------------------------------

def git(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)


def resolve() -> SimpleNamespace:
    top = git("rev-parse", "--show-toplevel")
    if top.returncode:
        die(EXIT_ENV, "not a git work tree")
    toplevel = Path(top.stdout.strip())
    porcelain = git("worktree", "list", "--porcelain", cwd=toplevel).stdout
    trees = [Path(ln[9:]) for ln in porcelain.splitlines() if ln.startswith("worktree ")]
    main = trees[0] if trees else toplevel  # a bare first entry is still used
    home = ".agents/retro/"
    cwd = Path.cwd()  # E11: nearest hex.md from cwd up to <toplevel>, like the memory search
    ups = [d for d in (cwd, *cwd.parents) if d == toplevel or toplevel in d.parents] or [toplevel]
    hex_md = next((h for d in ups if (h := d / ".agents" / "memory" / "hex.md").is_file()), None)
    if hex_md:
        m = re.search(r"^\s*-\s*Retro:\s*`([^`]+)`", hex_md.read_text(encoding="utf-8"), re.M)
        home = m.group(1) if m else home
    parts = PurePosixPath(home).parts
    if home.startswith(("/", "~", "\\")) or Path(home).is_absolute() or ".." in parts:
        die(EXIT_ENV, f"unsafe retro home {home!r}: absolute or contains ..")
    if (CLIENT_DIRS | {".git"}) & {x.lower() for x in parts}:  # E13: case-folded; .git refused
        die(EXIT_ENV, f"unsafe retro home {home!r}: inside a client config dir or .git")
    # Only components below <main>/<toplevel>: /tmp -> /private/tmp, WSL mounts stay legal.
    for base, rels in ((main, ("inbox/consumed", "inbox/rejected")), (toplevel, ("ledger", "reports"))):
        for rel in rels:
            p = base
            for part in (*parts, *rel.split("/")):
                p = p / part
                if p.is_symlink():
                    die(EXIT_ENV, f"unsafe retro home: {p} is a symlink")
    inbox = main / home / "inbox"
    return SimpleNamespace(main=main, toplevel=toplevel, trees=trees or [toplevel], home=home,
                           inbox=inbox, ledger=toplevel / home / "ledger",
                           reports=toplevel / home / "reports")


def ensure_inbox(ctx: SimpleNamespace) -> None:
    for sub in ("consumed", "rejected"):
        (ctx.inbox / sub).mkdir(parents=True, exist_ok=True)
    if not os.path.lexists(ctx.inbox / ".gitignore"):
        write_atomic(ctx.inbox / ".gitignore", b"*\n")  # ignores itself and everything below


# --- writes (C-1427, C-1429) -----------------------------------------------

def write_atomic(path: Path, data: bytes) -> None:
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
        mask = os.umask(0)
        os.umask(mask)
        os.chmod(tmp, 0o666 & ~mask)  # mkstemp gives 0600; match a plain open()
        os.replace(tmp, path)
    finally:
        if os.path.lexists(tmp):
            os.unlink(tmp)


def publish(dirp: Path, name: str, data: bytes) -> bool:
    """Deterministic-name publish: link never replaces; False when `name` exists."""
    tmp, final = dirp / f".{name}.{os.getpid()}.{secrets.token_hex(4)}.tmp", dirp / name
    tmp.write_bytes(data)
    try:
        os.link(tmp, final)
        return True
    except FileExistsError:
        return False
    except OSError:
        # ponytail: os.link unsupported (FUSE/DrvFs) -> exists-check + rename; a racing
        # identical writer may replace an identical file (same key, same bytes).
        if os.path.lexists(final):
            return False
        os.rename(tmp, final)
        return True
    finally:
        if os.path.lexists(tmp):
            os.unlink(tmp)


def dump(obj: object) -> bytes:
    return (json.dumps(obj, sort_keys=True, indent=2) + "\n").encode("utf-8")


def sha8(key: str) -> str:
    return hashlib.sha256(key.encode("utf-8")).hexdigest()[:8]


# --- entries (C-1425, C-1428) ----------------------------------------------

def parse_ts(value: object) -> datetime | None:
    if not isinstance(value, str) or not value.endswith(("Z", "+00:00")):
        return None
    try:
        ts = datetime.fromisoformat(value)
    except ValueError:
        return None
    return ts if ts.tzinfo else None


def iso(ts: datetime, fmt: str = "%Y-%m-%dT%H:%M:%SZ") -> str:
    return ts.astimezone(timezone.utc).strftime(fmt)


def nesting(o: object) -> int:
    """Container depth of a parsed JSON value, walked level by level (never RecursionError)."""
    n, level = 0, [o]
    while level := [x for x in level if isinstance(x, (dict, list))]:
        n, level = n + 1, [v for x in level for v in (x.values() if isinstance(x, dict) else x)]
    return n


def entry_problem(e: object) -> str | None:
    if not isinstance(e, dict):
        return "not a JSON object"
    if nesting(e) > MAX_DEPTH:  # E13: parses on 3.12+, but emit/dump would raise RecursionError
        return f"nested over {MAX_DEPTH} levels"
    v = e.get("v")
    if type(v) is not int or v != 1:
        return "unsupported v" if type(v) is int and v > 1 else "missing or invalid v"
    source = e.get("source", "self")
    checks = {
        "ts": parse_ts(e.get("ts")) is not None,
        "kind": e.get("kind") in KINDS,
        "source": source in ("self", "trajectory", "seed"),
        "scope": e.get("scope") in ("harness", "project")
                 or (e.get("scope") == "unknown" and source == "trajectory"),
        "artifact": isinstance(e.get("artifact"), str) and bool(NAME_RE.fullmatch(e["artifact"])),
        "severity": e.get("severity", "low") in SEVERITIES,
        "cost_min": "cost_min" not in e or (type(e["cost_min"]) in (int, float) and e["cost_min"] >= 0),
    }
    for k in ("what", "tell"):
        checks[k] = isinstance(e.get(k), str) and 0 < len(e[k].strip()) and len(e[k]) <= MAX_TEXT
    for k in ("proposed_change", "evidence", "role", "version"):
        checks[k] = k not in e or (isinstance(e[k], str) and len(e[k]) <= MAX_TEXT)
    bad = [k for k, ok in checks.items() if not ok]
    return f"missing or invalid {bad[0]}" if bad else None


def read_inbox(ctx: SimpleNamespace) -> tuple[dict[str, dict], list[dict], list[str]]:
    """Valid entries by name, skipped items with reasons, and malformed names."""
    entries: dict[str, dict] = {}
    skipped: list[dict] = []
    malformed: list[str] = []
    if not ctx.inbox.is_dir():
        return entries, skipped, malformed
    tracked = {p.name for p in tracked_paths(ctx) if p.parent == ctx.inbox}
    for p in sorted(ctx.inbox.iterdir()):
        if not p.name.endswith(".json") or p.name.startswith("."):
            continue
        entry = None
        if p.is_symlink():
            reason = "symlink"
        elif not p.is_file():
            continue
        elif p.name in tracked:
            reason = "tracked in git (planted)"
        else:
            reason, entry = parse_entry(p.read_bytes())
        if reason is None:
            entries[p.name] = entry
            continue
        skipped.append({"file": p.name, "reason": reason})
        if not reason.startswith(("tracked", "unsupported v")):
            malformed.append(p.name)
    return entries, skipped, malformed


def tracked_paths(ctx: SimpleNamespace) -> set[Path]:
    """Every git-tracked (committed or planted) path below the inbox."""
    listed = git("ls-files", "-z", "--", str(ctx.inbox), cwd=ctx.main).stdout.split("\0")
    return {ctx.main / p for p in listed if p}


def parse_entry(data: bytes) -> tuple[str | None, dict | None]:
    if len(data) > MAX_ENTRY:
        return "over 16 KiB", None
    try:
        entry = json.loads(data.decode("utf-8"))
    except (ValueError, RecursionError):  # E13: deep nesting overflows json on 3.11/3.12
        return "bad JSON", None
    reason = entry_problem(entry)
    return reason, None if reason else entry


def load_json_arg(path: str) -> object:
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError, RecursionError) as exc:
        die(EXIT_INVALID_INPUT, f"{path}: unreadable JSON ({type(exc).__name__})")


def load_state(inbox: Path) -> dict:
    path = inbox / ".mine-state.json"
    try:
        state = json.loads(path.read_text(encoding="utf-8")) if not path.is_symlink() else {}
    except (OSError, ValueError, RecursionError):
        state = {}
    # E13: a list/string state never raises; a deep-nested one is corrupt (dump would overflow)
    state = state if isinstance(state, dict) and nesting(state) <= MAX_DEPTH else {}
    files, written = state.get("files"), state.get("written")
    return {"files": {k: v for k, v in files.items() if isinstance(v, dict)} if isinstance(files, dict) else {},
            "written": [w for w in written if isinstance(w, str)] if isinstance(written, list) else []}


def load_lock(toplevel: Path) -> dict[str, str]:
    try:
        lock = tomllib.loads((toplevel / "grimoire.lock").read_text(encoding="utf-8"))
    except (OSError, ValueError, RecursionError):
        return {}
    out: dict[str, str] = {}
    for kind in ("skill", "rule", "agent"):  # E13: an ill-typed row is skipped, never raised on
        rows = lock.get(kind)
        for e in rows if isinstance(rows, list) else []:
            pin = next((v for v in (e.get("pinned"), e.get("hash")) if isinstance(v, str) and v), None) \
                if isinstance(e, dict) and isinstance(e.get("name"), str) else None
            if pin:
                out[e["name"]] = pin
    return out


def stamp_version(entry: dict, lock: dict[str, str]) -> str | None:
    """C-1436: entry version, else grimoire.lock pin/hash, else `unknown`; project -> none."""
    if entry.get("artifact") == "project":
        return None
    return entry.get("version") or lock.get(entry.get("artifact"), "unknown")


# --- subcommands -----------------------------------------------------------

def cmd_where(args: argparse.Namespace) -> int:
    """C-1430: resolve inbox/ledger/reports paths, create missing dirs."""
    ctx = resolve()
    ensure_inbox(ctx)
    # E13: 1 = a trackable, unignored inbox -> refused; 128 only off a bare <main> (no index can
    # track it) -- a corrupt index or submodule home is refused, never read as trusted
    rc = git("check-ignore", "-q", str(ctx.inbox / "x.json"), cwd=ctx.main).returncode
    ignored = rc == 0 or rc == 128 and \
        git("rev-parse", "--is-bare-repository", cwd=ctx.main).stdout.strip() == "true"
    if not ignored:  # E13: a committed inbox is refused, never reported and carried on
        die(EXIT_ENV, f"retro inbox {ctx.inbox} is not gitignored: restore its .gitignore to `*` "
                      f"and untrack it (git rm -r --cached)")
    return emit({"main": str(ctx.main), "toplevel": str(ctx.toplevel), "home": ctx.home,
                 "inbox": str(ctx.inbox), "ledger": str(ctx.ledger),
                 "reports": str(ctx.reports), "ignored": ignored})


def cmd_read(args: argparse.Namespace) -> int:
    """C-1431: validate + print inbox entries, sorted by filename."""
    entries, skipped, _ = read_inbox(resolve())
    return emit({"entries": [{"file": n, "entry": e} for n, e in entries.items()],
                 "skipped": skipped})


# Miner (C-1432). Transcript text reaches output ONLY through call_key():
# program basename + one lowercase subcommand, or the tool name.
SHELL_TOOLS = {"Bash"}
META_TYPES = {"summary", "system", "attachment", "file-history-snapshot", "file-history-delta",
              "mode", "permission-mode", "atis-latch", "bridge-session", "last-prompt",
              "ai-title", "custom-title", "agent-name", "queue-operation", "cost-state",
              "pr-link", "frame-link", "history-suppression", "fork-context-ref", "progress",
              "artifact-comment-monitor", "artifact-autoreact-ledger"}
BLOCK_TYPES = {"text", "thinking", "redacted_thinking", "tool_use", "tool_result", "image",
               "document", "server_tool_use", "web_search_tool_result"}
ASSIGN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=")
WRAPPERS, WRAP_ARG_RE = {"timeout", "time", "nice", "env", "sudo", "rtk"}, re.compile(r"\d+(?:\.\d+)?[smhd]?")
# E13: a program starts lowercase, has no capitals, is <= 24 chars and at most 1/3 digits
# (`python3.11`, `x86_64-linux-gnu-gcc`, `g++` pass; `AKIA…`, `x9f3k2m8…` -> tool name)
PROG_RE, TOOL_RE = re.compile(r"[a-z][a-z0-9._+-]{0,23}"), re.compile(r"[A-Za-z0-9_.:+-]{1,64}")
# ponytail: a lowercase dictionary word in second position survives (`cd secretdir`,
# `git mysecret`); an empty assignment (`PW= hunter2 psql`) keys the tool name, so a
# legitimate `VAR= cmd` loses its program key; upgrade: a per-program subcommand
# allow-list and a known-program list.
SUB_RE = re.compile(r"[a-z][a-z-]{0,15}")
VERSION_RE = re.compile(r"[0-9A-Za-z.+-]{1,32}")


def call_key(tool: object, inp: object) -> str:
    tool = tool if isinstance(tool, str) and TOOL_RE.fullmatch(tool) else "tool"
    cmd = inp.get("command") if isinstance(inp, dict) else None
    if tool not in SHELL_TOOLS or not isinstance(cmd, str):
        return tool
    lex = shlex.shlex(cmd, posix=True, punctuation_chars=True)
    lex.whitespace_split = True
    try:
        toks = list(lex)  # quoted values stay one token: a secret never splits into the key
    except ValueError:
        return tool
    while toks and toks[0] in ("cd", "export"):  # leading `cd …`/`export …` segments
        sep = next((k for k, t in enumerate(toks) if t in ("&&", ";")), None)
        if sep is None:
            break
        toks = toks[sep + 1:]
    i = 0
    while i < len(toks):
        if ASSIGN_RE.fullmatch(toks[i]):  # E13: `X= <secret> cmd` puts the secret in program position
            return tool
        if ASSIGN_RE.match(toks[i]):
            i += 1
        elif toks[i] in WRAPPERS:
            # only `rtk proxy` (E8), `timeout <n>`, `nice -n <n>`, `env -i`; any other
            # wrapper option form (`env -S '…'`, `timeout -s KILL`) -> tool name (E11)
            w, arg = toks[i], toks[i + 1:i + 3]
            i += 1
            if (w, arg[:1]) in (("rtk", ["proxy"]), ("env", ["-i"])) \
                    or (w == "timeout" and arg and WRAP_ARG_RE.fullmatch(arg[0])):
                i += 1
            elif w == "nice" and arg[:1] == ["-n"] and len(arg) == 2 and re.fullmatch(r"-?\d+", arg[1]):
                i += 2
            if i < len(toks) and toks[i].startswith("-"):
                return tool
        else:
            break
    prog = toks[i] if i < len(toks) else ""
    if "://" in prog or "=" in prog:  # E11: a URL or option tail never becomes a key
        return tool
    prog = os.path.basename(prog)
    if not PROG_RE.fullmatch(prog) or 3 * sum(c.isdigit() for c in prog) > len(prog):
        return tool
    nxt = toks[i + 1] if i + 1 < len(toks) else ""
    return f"{prog} {nxt}" if SUB_RE.fullmatch(nxt) else prog


def read_records(path: Path) -> list[object]:
    out: list[object] = []
    with path.open(encoding="utf-8", errors="replace") as fh:
        for line in fh:
            if line.strip():
                try:
                    out.append(json.loads(line))
                except (ValueError, RecursionError):
                    out.append(None)
    return out


def scan(records: list[object], roots: list[str], skill: str) -> dict:
    """Replay one transcript: paired-call totals per `<artifact>|<key>` + shape stats."""
    uses: dict[str, tuple[datetime, str | None]] = {}
    totals: dict[str, dict] = {}
    failed: set[str] = set()
    prev: tuple[str, str] | None = None
    res = {"totals": totals, "counted": 0, "pairs": 0, "versions": set(), "ts": [], "skills": []}
    for rec in records:
        if not isinstance(rec, dict) or not isinstance(rec.get("type"), str):  # E11: never raises
            res["counted"] += 1
            continue
        typ, ts = rec["type"], parse_ts(rec.get("timestamp"))
        if typ in META_TYPES:
            continue
        if ts is None:
            res["counted"] += 1
            continue
        msg = rec.get("message")
        content = msg.get("content") if isinstance(msg, dict) else None
        if typ not in ("user", "assistant") or isinstance(content, str):
            continue
        if isinstance(rec.get("version"), str) and VERSION_RE.fullmatch(rec["version"]):
            res["versions"].add(rec["version"])
        cwd = rec.get("cwd")
        in_scope = isinstance(cwd, str) and any(cwd == r or cwd.startswith(r + "/") for r in roots)
        if in_scope:
            res["ts"].append(ts)
        blocks = content if isinstance(content, list) else [None]
        if any(not isinstance(b, dict) or not isinstance(b.get("type"), str)
               or b["type"] not in BLOCK_TYPES for b in blocks):
            res["counted"] += 1
        for b in blocks:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "tool_use" and typ == "assistant" and isinstance(b.get("id"), str):
                akey = f"{skill}|{call_key(b.get('name'), b.get('input'))}" if in_scope else None
                uses[b["id"]] = (ts, akey)
                if akey and prev and prev[1] == akey and prev[0] in failed:
                    totals.setdefault(akey, {"ms": 0, "errors": 0, "ts": ts, "retries": 0})["retries"] += 1
                prev = (b["id"], akey) if akey else prev
                inp = b.get("input")
                if b.get("name") == "Skill" and isinstance(inp, dict) and isinstance(inp.get("skill"), str):
                    skill = inp["skill"] if NAME_RE.fullmatch(inp["skill"]) else "project"
                    res["skills"].append((ts, skill))
            elif b.get("type") == "tool_result" and typ == "user" and isinstance(b.get("tool_use_id"), str):
                use_ts, akey = uses.pop(b["tool_use_id"], (None, None))
                if use_ts is None:
                    continue
                res["pairs"] += 1
                if akey is None:
                    continue
                t = totals.setdefault(akey, {"ms": 0, "errors": 0, "ts": ts, "retries": 0})
                t["ms"] += max(0, round((ts - use_ts).total_seconds() * 1000))
                t["ts"] = max(t["ts"], ts)
                if b.get("is_error") is True:  # null/absent = unknown, never an error
                    t["errors"] += 1
                    failed.add(b["tool_use_id"])
    return res


def mined(akey: str, new: dict, old: dict, th: dict, sid: str) -> list[tuple[str, dict]]:
    """(counter, entry) per threshold the delta crosses; totals below stay pending."""
    artifact, key = akey.split("|", 1)
    d_ms, d_err = new["ms"] - old["ms"], new["errors"] - old["errors"]
    cost = round(d_ms / 60000, 1)
    cost = int(cost) if cost == int(cost) else cost
    base = {"v": 1, "ts": iso(new["ts"]), "scope": "unknown", "artifact": artifact,
            "source": "trajectory", "evidence": f"session {sid}"}
    out = []
    if d_ms >= th["mine-slow-min"] * 60000:
        out.append(("ms", dict(base, kind="slow", cost_min=cost,
                               what=f"`{key}` spent {cost} min of tool-busy time",
                               tell="paired tool_use/tool_result timing in the transcript")))
    if d_err >= th["mine-errors"]:  # count only: cost lives in `slow`, never double counted
        out.append(("errors", dict(base, kind="pitfall", cost_min=0,
                                   what=f"`{key}` failed {d_err} times "
                                        f"({new['retries']} immediate same-key retries)",
                                   tell="tool_result is_error in the transcript")))
    return out


def settle(ctx: SimpleNamespace, state: dict, lock: dict, th: dict, f: Path, sid: str,
           res: dict, stored: dict) -> int:
    """Publish each crossed delta for one transcript; advance its stored totals by it."""
    written, keep = 0, {}
    for akey in sorted(set(res["totals"]) | set(stored)):
        new = res["totals"].get(akey, {"ms": 0, "errors": 0})
        prior = stored.get(akey) if isinstance(stored.get(akey), dict) else {}
        old = {k: v if type(v := prior.get(k, 0)) is int else 0 for k in ("ms", "errors")}  # E13
        if new["ms"] < old["ms"] or new["errors"] < old["errors"]:  # file rewritten: reset
            old = {"ms": new["ms"], "errors": new["errors"]}
        else:
            for field, entry in mined(akey, new, old, th, sid):
                if (ver := stamp_version(entry, lock)) is not None:
                    entry["version"] = ver
                digest = sha8(f"{f}|{akey}|{entry['kind']}|{new[field]}")
                name = f"{iso(new['ts'], '%Y%m%dT%H%M%SZ')}-{digest}.json"
                data, final = dump(entry), ctx.inbox / name
                fresh = publish(ctx.inbox, name, data)
                # a crash-replayed identical file is ours; anything else keeps the delta pending
                if fresh or (not final.is_symlink() and final.is_file() and final.read_bytes() == data):
                    written += fresh
                    old[field] = new[field]
                    if name not in state["written"]:
                        state["written"].append(name)
        if old["ms"] or old["errors"]:
            keep[akey] = old
    state["files"][str(f)] = {"totals": keep}
    return written


def cmd_mine(args: argparse.Namespace) -> int:
    """C-1432: mine Claude Code transcripts for slow/pitfall entries.

    ponytail: cost is tool-busy time -- parallel calls sum, background commands
    undercount; the state file is machine-local, another machine's transcripts are
    mined there; `written` grows with every entry ever mined (like consumed/).
    """
    th, ctx = args.th, resolve()
    cfg = os.environ.get("CLAUDE_CONFIG_DIR") or str(Path.home() / ".claude")
    root = Path(args.transcripts) if args.transcripts else Path(cfg) / "projects"
    roots = [str(t) for t in ctx.trees]
    prefixes = tuple(re.sub(r"[^A-Za-z0-9]", "-", r)[:200] for r in roots)
    dirs = sorted(d for d in root.iterdir() if d.is_dir() and d.name.startswith(prefixes)) \
        if root.is_dir() else []
    if not dirs:
        return emit({"written": 0, "span_min": 0,
                     "degraded": [f"no Claude Code transcripts for {ctx.main}"]})
    ensure_inbox(ctx)
    state, lock = load_state(ctx.inbox), load_lock(ctx.toplevel)
    cache: dict[Path, dict] = {}

    def session_scan(path: Path) -> dict:
        if path not in cache:
            cache[path] = scan(read_records(path), roots, "project")
        return cache[path]

    written, bad_files, scanned, pairs, versions, stamps = 0, 0, 0, 0, set(), []
    for f in (f for d in dirs for f in [*sorted(d.glob("*.jsonl")),
                                        *sorted(d.glob("*/subagents/agent-*.jsonl"))]):
        mtime, prev = f.stat().st_mtime_ns, state["files"].get(str(f), {})
        if prev.get("mtime") == mtime:
            continue
        sub = f.parent.name == "subagents"
        sid = f.parent.parent.name if sub else f.stem
        sid = sid if re.fullmatch(r"[A-Za-z0-9-]{1,64}", sid) else "unknown"
        if sub:
            records, parent, skill = read_records(f), f.parent.parent.parent / f"{sid}.jsonl", "project"
            first = next((t for r in records if isinstance(r, dict)
                          for t in [parse_ts(r.get("timestamp"))] if t), None)
            if parent.is_file() and first:
                skill = next((s for t, s in reversed(session_scan(parent)["skills"]) if t < first), skill)
            res = scan(records, roots, skill)
        else:
            res = session_scan(f)
        scanned, pairs = scanned + 1, pairs + res["pairs"]
        if res["counted"]:  # E5: a tool-free file is normal; only unrecognised records degrade
            bad_files, versions = bad_files + 1, versions | res["versions"]
        stamps += res["ts"]
        stored = prev.get("totals")
        written += settle(ctx, state, lock, th, f, sid, res, stored if isinstance(stored, dict) else {})
        state["files"][str(f)]["mtime"] = mtime  # replay, not a watermark: any change re-reads whole
        # E13: persisted per file -- a crash on a later file never re-emits this one's deltas.
        # ponytail: rewrites the whole state per changed transcript, O(n^2) bytes on a first
        # run over ~1,000 transcripts; upgrade: write every N files and on exception.
        write_atomic(ctx.inbox / ".mine-state.json", dump(state))
    degraded = [f"transcript shape: {bad_files} file(s) with unrecognised records "
                f"(record versions: {', '.join(sorted(versions)) or 'none'})"] if bad_files else []
    if scanned and not pairs:
        degraded.append(f"transcript shape: no tool call paired in {scanned} scanned file(s)")
    span = (max(stamps) - min(stamps)).total_seconds() / 60 if stamps else 0
    return emit({"written": written, "span_min": round(span, 1), "degraded": degraded})


def row_problem(r: dict) -> str | None:
    """First field of a v 1 ledger row that fold cannot rely on, else None."""
    def strs(v: object) -> bool:
        return isinstance(v, list) and all(isinstance(x, str) for x in v)

    def ts_or_none(k: str) -> bool:
        return k in r and (r[k] is None or parse_ts(r[k]) is not None)

    fixed = r.get("fixed", 0)
    checks = {
        "status": r.get("status") in ("open", "deferred", "fixed", "reopened"),
        "entries": strs(r.get("entries")),
        "versions": strs(r.get("versions")),
        "occurrences": type(r.get("occurrences")) is int,
        "folds": type(r.get("folds")) is int,
        "cost_min": type(r.get("cost_min")) in (int, float),
        "severity": "severity" in r and r["severity"] in (None, *SEVERITIES),
        "first_seen": ts_or_none("first_seen"),
        "last_seen": ts_or_none("last_seen"),
        "fixed": fixed is None or (isinstance(fixed, dict) and parse_ts(fixed.get("at")) is not None
                                   and strs(fixed.get("stale_versions", []))),
        "declined_at": "declined_at" not in r or parse_ts(r["declined_at"]) is not None,
    }
    bad = [k for k, ok in checks.items() if not ok]
    return f"missing or invalid {bad[0]}" if bad else None


def _items(dec: dict, key: str, fields: tuple[str, ...]) -> list[dict]:
    items = dec.get(key, [])
    if not isinstance(items, list) or not all(
            isinstance(i, dict) and all(isinstance(i.get(f), str) for f in fields) for i in items):
        die(EXIT_INVALID_INPUT, f"decisions `{key}` must be a list of objects with string "
                                f"{', '.join(fields)}")
    return items


def cmd_fold(args: argparse.Namespace) -> int:
    """C-1433: apply a decisions.json to the ledger; validates all before writing anything."""
    th, ctx = args.th, resolve()
    dec = load_json_arg(args.decisions)
    if not isinstance(dec, dict) or type(dec.get("v", 1)) is not int or dec.get("v", 1) != 1:
        die(EXIT_INVALID_INPUT, "decisions must be an object with v 1")
    rows: dict[str, dict] = {}
    for p in sorted(ctx.ledger.glob("*.json")) if ctx.ledger.is_dir() else []:
        try:
            row = None if p.is_symlink() else json.loads(p.read_text(encoding="utf-8"))
        except (ValueError, RecursionError):
            row = None
        if not isinstance(row, dict) or type(row.get("v")) is not int or row["v"] != 1:
            die(EXIT_INVALID_INPUT, f"ledger row {p.name} is not a v 1 row this retro.py can rewrite")
        if problem := row_problem(row):
            die(EXIT_INVALID_INPUT, f"ledger row {p.name}: {problem}")
        rows[p.stem] = row
    before = {rid: dump(r) for rid, r in rows.items()}
    entries, _, malformed = read_inbox(ctx)
    consumed_dir = ctx.inbox / "consumed"
    creates = _items(dec, "create", ("id", "title", "scope", "artifact", "kind"))
    assigns, ignores = _items(dec, "assign", ("file", "id")), _items(dec, "ignore", ("file", "reason"))
    statuses = _items(dec, "status", ("id", "to"))
    for c in creates:
        if not ID_RE.fullmatch(c["id"]) or c["id"] in rows:
            die(EXIT_INVALID_INPUT, f"create {c['id']!r}: invalid or existing id")
        if not (0 < len(c["title"]) <= 120 and c["scope"] in ("harness", "project", "unknown")
                and NAME_RE.fullmatch(c["artifact"]) and c["kind"] in KINDS):
            die(EXIT_INVALID_INPUT, f"create {c['id']!r}: invalid title, scope, artifact or kind")
        rows[c["id"]] = {"v": 1, "id": c["id"], "title": c["title"], "scope": c["scope"],
                         "artifact": c["artifact"], "kind": c["kind"], "severity": None,
                         "status": "open", "entries": [], "occurrences": 0, "folds": 0,
                         "first_seen": None, "last_seen": None, "folded_at": None, "cost_min": 0,
                         "versions": [], "fixed": None}
    recorded = {f for r in rows.values() for f in r.get("entries", [])}
    seen: set[str] = set()
    from_consumed: dict[str, dict] = {}
    for item, rid in [(a, a["id"]) for a in assigns] + [(i, None) for i in ignores]:
        f = item["file"]
        if not f or "/" in f or "\\" in f or ".." in f or f.startswith("."):
            die(EXIT_INVALID_INPUT, f"file {f!r} is not a bare entry basename")
        if f in seen:
            die(EXIT_INVALID_INPUT, f"file {f} is referenced twice")
        seen.add(f)
        cpath = consumed_dir / f
        in_consumed = cpath.is_file() and not cpath.is_symlink()
        if f not in entries and not in_consumed:
            die(EXIT_INVALID_INPUT, f"file {f} is neither a valid inbox entry nor in inbox/consumed/")
        if rid is not None and rid not in rows:
            die(EXIT_INVALID_INPUT, f"assign {f}: unknown id {rid!r}")
        if rid is not None and f not in entries and f not in recorded:
            reason, from_consumed[f] = parse_entry(cpath.read_bytes())
            if reason:
                die(EXIT_INVALID_INPUT, f"consumed entry {f}: {reason}")
    for s in statuses:
        if s["id"] not in rows:
            die(EXIT_INVALID_INPUT, f"status: unknown id {s['id']!r}")
        if s["to"] not in ("open", "deferred", "fixed"):
            die(EXIT_INVALID_INPUT, f"status {s['id']}: `to` must be open, deferred or fixed")
        if s["to"] == "fixed" and not (isinstance(s.get("by"), str) and s["by"].strip()):
            die(EXIT_INVALID_INPUT, f"status {s['id']}: fixed needs `by`")
        if not isinstance(s.get("shipped", False), bool):
            die(EXIT_INVALID_INPUT, f"status {s['id']}: `shipped` must be true or false")
        if s.get("shipped") is True and s["to"] != "fixed":  # E10
            die(EXIT_INVALID_INPUT, f"status {s['id']}: `shipped` only with `to: fixed`")
        if not isinstance(s.get("declined", False), bool):
            die(EXIT_INVALID_INPUT, f"status {s['id']}: `declined` must be true or false")
        if s.get("declined") is True and s["to"] != "deferred":  # E15
            die(EXIT_INVALID_INPUT, f"status {s['id']}: `declined` only with `to: deferred`")

    # -- validated; apply in memory, then write rows, then move files --
    now = datetime.now(timezone.utc)
    # ponytail: trusted cost authenticates the mine-written filename, not its bytes -- an
    # untracked local edit of the inbox or `.mine-state.json` still forges cost (local write
    # access is outside the threat model); a tracked (committed or planted) state file or
    # consumed entry is refused (E13). Upgrade: a per-entry byte digest in the state file.
    planted = tracked_paths(ctx)
    lock = load_lock(ctx.toplevel)
    trusted = set() if ctx.inbox / ".mine-state.json" in planted else set(load_state(ctx.inbox)["written"])
    trusted -= {p.name for p in planted if p.parent == consumed_dir}
    gained: set[str] = set()
    for a in assigns:
        f, row = a["file"], rows[a["id"]]
        if f in recorded:  # never assigned twice: crash re-runs cannot double count
            continue
        recorded.add(f)
        gained.add(a["id"])
        e = entries.get(f) or from_consumed[f]
        row["entries"] = sorted({*row["entries"], f})
        row["occurrences"] = len(row["entries"])
        if e.get("severity"):
            row["severity"] = max(filter(None, (row["severity"], e["severity"])), key=SEVERITIES.index)
        ts = parse_ts(e["ts"])
        if not row["first_seen"] or ts < parse_ts(row["first_seen"]):
            row["first_seen"] = e["ts"]
        if not row["last_seen"] or ts > parse_ts(row["last_seen"]):
            row["last_seen"] = e["ts"]
        if "declined_at" in row and ts > parse_ts(row["declined_at"]):
            del row["declined_at"]  # E15: an occurrence after the decline is new evidence
        ver = stamp_version(e, lock)
        if ver is not None:
            row["versions"] = sorted({*row["versions"], ver})
        if f in trusted and e.get("source") == "trajectory":  # C-1437: forged cost buys nothing
            row["cost_min"] = round(row["cost_min"] + e.get("cost_min", 0), 2)
        fixed = row.get("fixed")
        if (row["status"] == "fixed" and fixed and ts > parse_ts(fixed["at"])
                and ver not in fixed.get("stale_versions", [])):
            row["status"] = "reopened"  # mechanical only; `fixed` kept as the last fix
    for rid in gained:
        rows[rid]["folds"] += 1
    for s in statuses:
        row = rows[s["id"]]
        row["status"] = s["to"]
        if s.get("declined"):  # E15: a human "no" -- held until an occurrence after it
            row["declined_at"] = iso(now)
        elif s["to"] != "deferred":
            row.pop("declined_at", None)
        if s["to"] == "fixed":
            # E6: `shipped` = the fix is already in the stamped versions -> none is stale
            stale = [] if s.get("shipped") else sorted(set(row["versions"]) - {"unknown"})
            row["fixed"] = {"at": iso(now), "by": s["by"], "stale_versions": stale}
    changed = [rid for rid, r in rows.items() if dump(r) != before.get(rid)]
    if changed:
        ctx.ledger.mkdir(parents=True, exist_ok=True)
    for rid in changed:
        if rid in gained:  # added an entry this fold; reopening only ever happens as part of that
            rows[rid]["folded_at"] = iso(now)
        write_atomic(ctx.ledger / f"{rid}.json", dump(rows[rid]))
    # ponytail: consumed/ grows unbounded (gitignored); add a TTL when it matters.
    moves = [(f, "consumed") for f in seen if os.path.lexists(ctx.inbox / f)]
    moves += [(f, "rejected") for f in malformed if f not in seen]
    for f, dest in moves:
        (ctx.inbox / dest).mkdir(exist_ok=True)
        os.replace(ctx.inbox / f, ctx.inbox / dest / f)
    wall = args.wall_min

    def big(r: dict) -> bool:
        return bool((wall and wall > 0 and r["cost_min"] >= th["bar-cost-fraction"] * wall)
                    or (r["occurrences"] >= th["bar-occurrences"] and r["folds"] >= th["bar-folds"]))

    # E10: changed rows plus every big or reopened row, touched or not
    listed = sorted({*changed, *(rid for rid, r in rows.items() if big(r) or r["status"] == "reopened")})
    return emit({"at": iso(now), "rows": [{"id": rid, "status": rows[rid]["status"],
                           "occurrences": rows[rid]["occurrences"], "folds": rows[rid]["folds"],
                           "cost_min": rows[rid]["cost_min"], "big": big(rows[rid]),
                           "reopened": rows[rid]["status"] == "reopened",
                           "declined": "declined_at" in rows[rid]} for rid in listed],
                 "consumed": sum(d == "consumed" for _, d in moves),
                 "rejected": sum(d == "rejected" for _, d in moves)})


def cmd_import(args: argparse.Namespace) -> int:
    """C-1454: import model-written seed entries from entries.json."""
    ctx = resolve()
    doc = load_json_arg(args.entries)
    items = doc.get("entries") if isinstance(doc, dict) and doc.get("v") == 1 else None
    if not isinstance(items, list) or not items:
        die(EXIT_INVALID_INPUT, f"{args.entries} holds no entries to import")
    pubs: list[tuple[str, bytes]] = []
    for i, it in enumerate(items):
        key, e = (it.get("key"), it.get("entry")) if isinstance(it, dict) else (None, None)
        problem = "key must be a non-empty string" if not (isinstance(key, str) and key) \
            else entry_problem(e) or (None if e.get("source") == "seed" else "source must be seed")
        data = b"" if problem else dump(e)  # E13: a too-deep entry never reaches dump
        if problem or len(data) > MAX_ENTRY:
            die(EXIT_INVALID_INPUT, f"entries[{i}]: {problem or 'over 16 KiB'}")
        pubs.append((f"{iso(parse_ts(e['ts']), '%Y%m%dT%H%M%SZ')}-{sha8(key)}.json", data))
    ensure_inbox(ctx)
    names = [p.name for d in (ctx.inbox, ctx.inbox / "consumed") for p in d.iterdir()]
    for p in ctx.ledger.glob("*.json") if ctx.ledger.is_dir() else []:
        try:
            names += json.loads(p.read_text(encoding="utf-8")).get("entries", [])
        except (ValueError, AttributeError, TypeError, RecursionError):
            pass
    # dedupe on the `-<sha8(key)>.json` suffix: a moved `ts` fallback never re-imports (E2)
    taken = {n[-14:] for n in names if isinstance(n, str)}
    written = 0
    for name, data in pubs:
        if name[-14:] not in taken and publish(ctx.inbox, name, data):
            written += 1
        taken.add(name[-14:])
    return emit({"written": written, "skipped": len(pubs) - written})


# --- selftest (C-1434) ---------------------------------------------------
#
# Each case runs in a temp git repo against a copy of retro.py placed
# beside a fixture SKILL.md holding the C-1424 table, so it passes
# without the bundle sources. Cases are black-box: they drive the copy by
# subprocess and assert only on exit codes, stdout JSON and on-disk files.

_ST_THRESHOLDS = {
    "nudge-entries": "5",
    "bar-cost-fraction": "0.10",
    "bar-occurrences": "3",
    "bar-folds": "2",
    "proposal-cap": "10",
    "mine-slow-min": "10",
    "mine-errors": "2",
}


def _st_skill_md(rows: dict[str, str] | None = _ST_THRESHOLDS) -> str:
    """Fixture SKILL.md; `rows=None` omits the § Thresholds table."""
    head = "---\nname: hex-retro\n---\n\n# hex-retro\n\n"
    if rows is None:
        return head + "No thresholds here.\n"
    table = "".join(f"| `{k}` | {v} |\n" for k, v in rows.items())
    return head + "## Thresholds\n\n| Name | Value |\n|---|---|\n" + table


class _StFail(Exception):
    pass


def _st_check(cond: object, why: str) -> None:
    if not cond:
        raise _StFail(why)


def _st_entry(**over: object) -> dict:
    """A valid C-1425 self entry; keyword overrides replace fields."""
    entry = {
        "v": 1,
        "ts": "2026-01-01T00:00:00Z",
        "kind": "pitfall",
        "scope": "harness",
        "artifact": "hex-plan",
        "what": "what happened",
        "tell": "the observable tell",
    }
    entry.update(over)
    return entry


def _st_put(dirp: Path, name: str, obj: object) -> Path:
    path = dirp / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(obj if isinstance(obj, str) else json.dumps(obj), encoding="utf-8")
    return path


def _st_tree(*roots: Path) -> dict[str, bytes | None]:
    """Snapshot every dir, file and symlink below `roots` (no link follow)."""
    snap: dict[str, bytes | None] = {}
    for root in roots:
        for dirpath, dirnames, filenames in os.walk(root):
            for d in dirnames:
                p = Path(dirpath, d)
                snap[str(p)] = os.readlink(p).encode() if p.is_symlink() else None
            for f in filenames:
                p = Path(dirpath, f)
                snap[str(p)] = os.readlink(p).encode() if p.is_symlink() else p.read_bytes()
    return snap


class _StSandbox:
    """Temp git repo + a copy of this script beside a fixture SKILL.md."""

    def __init__(self, skill_md: str) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="retro-selftest-")).resolve()
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        scripts = self.tmp / "skill" / "scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / "retro.py"
        shutil.copy2(Path(__file__).resolve(), self.script)
        self.skill_md = self.tmp / "skill" / "SKILL.md"
        self.skill_md.write_text(skill_md, encoding="utf-8")
        self.cfg = self.tmp / "cfg"
        self.cfg.mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        env.update(CLAUDE_CONFIG_DIR=str(self.cfg), PYTHONDONTWRITEBYTECODE="1")
        self.env = env
        self.n = 0
        self.git("init", "-q")
        self.git("config", "user.name", "retro selftest")
        self.git("config", "user.email", "selftest@example.invalid")
        self.git("config", "commit.gpgsign", "false")

    def git(self, *args: str) -> None:
        subprocess.run(
            ["git", *args], cwd=self.repo, env=self.env, check=True,
            capture_output=True, text=True, timeout=60,
        )

    def run(self, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(self.script), *args], cwd=cwd or self.repo, env=self.env,
            capture_output=True, text=True, timeout=120,
        )

    def expect(self, code: int, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
        proc = self.run(*args, cwd=cwd)
        tail = (proc.stderr.strip().splitlines() or [""])[-1]
        _st_check(
            proc.returncode == code,
            f"{args[0]}: exit {proc.returncode}, want {code} ({tail[:160]})",
        )
        return proc

    def json(self, *args: str, cwd: Path | None = None) -> dict:
        proc = self.expect(EXIT_OK, *args, cwd=cwd)
        try:
            out = json.loads(proc.stdout)
        except ValueError:
            raise _StFail(f"{args[0]}: stdout is not JSON: {proc.stdout[:120]!r}")
        _st_check(isinstance(out, dict), f"{args[0]}: stdout is not a JSON object")
        return out

    def where(self) -> dict:
        w = self.json("where")
        self.inbox, self.ledger = Path(w["inbox"]), Path(w["ledger"])
        return w

    def scratch(self, obj: object) -> str:
        """Write `obj` as JSON outside the repo; return its path."""
        self.n += 1
        return str(_st_put(self.tmp, f"scratch-{self.n}.json", obj))

    def fold(self, decisions: dict, code: int = EXIT_OK) -> subprocess.CompletedProcess[str]:
        return self.expect(code, "fold", self.scratch(decisions))

    def row(self, rid: str) -> dict:
        path = self.ledger / f"{rid}.json"
        _st_check(path.is_file(), f"ledger row {rid} missing")
        return json.loads(path.read_text(encoding="utf-8"))


def _st_run(body: Callable[[_StSandbox], None], skill_md: str | None = None) -> tuple[bool, str]:
    sb = None
    try:
        sb = _StSandbox(_st_skill_md() if skill_md is None else skill_md)
        body(sb)
        return True, ""
    except _StFail as exc:
        return False, str(exc)
    except Exception as exc:  # a crash is a FAIL line, never a traceback
        return False, f"{type(exc).__name__}: {exc}"
    finally:
        if sb is not None:
            shutil.rmtree(sb.tmp, ignore_errors=True)


def _case_malformed() -> tuple[bool, str]:
    """C-1434 malformed; C-1428, C-1431, C-1433 (rejected/), S-1424.

    Bad JSON, deep-nested JSON and a valid entry with a 2000-deep extra key
    (E13), missing field, tracked (planted) file, symlink and `"v": 2` ->
    `read` exits 0 with 7 skipped-with-reason;
    `fold` (over a non-object `.mine-state.json`, E13) moves the malformed
    ones to rejected/, leaves the tracked and `v: 2` files in inbox/ (v: 2
    never in rejected/). A subdirectory hex.md `Retro:` row is honoured
    from that subdirectory only (E11); a `.Claude/` or `.git/` home and an
    inbox that is not gitignored -> `where` exit 3, a worktree of a bare repo
    -> `where` exit 0, a corrupt index (check-ignore 128, not bare) -> exit 3 (E13).
    """

    def body(sb: _StSandbox) -> None:
        sb.where()
        inbox = sb.inbox
        good = "20260101T000000Z-0000000a.json"
        bad_json, missing = "20260101T000000Z-0000000b.json", "20260101T000000Z-0000000c.json"
        tracked, link = "20260101T000000Z-0000000d.json", "20260101T000000Z-0000000e.json"
        v2, deep = "20260101T000000Z-0000000f.json", "20260101T000000Z-00000010.json"
        _st_put(inbox, good, _st_entry())
        _st_put(inbox, bad_json, "{not json")
        _st_put(inbox, deep, "[" * 8000)  # E13: RecursionError on 3.11/3.12, under 16 KiB
        # E13: parses on 3.12+, then `read`'s emit overflowed (3.12) or it was read as valid
        nest = "20260101T000000Z-00000011.json"
        _st_put(inbox, nest, json.dumps(_st_entry())[:-1] + ', "x": ' + "[" * 2000 + "]" * 2000 + "}")
        no_tell = _st_entry()
        del no_tell["tell"]
        _st_put(inbox, missing, no_tell)
        _st_put(inbox, tracked, _st_entry())
        sb.git("add", "-f", str(inbox / tracked))
        sb.git("commit", "-q", "--no-verify", "-m", "plant")
        (inbox / link).symlink_to(_st_put(sb.tmp, "outside.json", _st_entry()))
        _st_put(inbox, v2, _st_entry(v=2))

        out = sb.json("read")
        files = [e.get("file") for e in out.get("entries", [])]
        _st_check(files == [good], f"read entries {files}, want [{good}]")
        skipped = {s.get("file"): s.get("reason") for s in out.get("skipped", [])}
        want = {bad_json, deep, nest, missing, tracked, link, v2}
        _st_check(set(skipped) == want and len(out["skipped"]) == 7,
                  f"read skipped {sorted(skipped)}, want the 7 bad files")
        _st_check(all(isinstance(r, str) and r.strip() for r in skipped.values()),
                  "a skipped item has no reason")
        _st_check("unsupported v" in skipped[v2], f"v: 2 reason {skipped[v2]!r}")

        _st_put(inbox, ".mine-state.json", [1])  # E13: a non-object state never raises
        sb.fold({"v": 1})
        rejected = inbox / "rejected"
        _st_check((inbox / v2).is_file(), "v: 2 entry left inbox/ after fold")
        _st_check(not (rejected / v2).exists(), "v: 2 entry moved to rejected/")
        for name in (bad_json, deep, nest, missing):
            _st_check((rejected / name).is_file() and not (inbox / name).exists(),
                      f"malformed {name} not moved to rejected/")
        _st_check((inbox / tracked).is_file() and not (rejected / tracked).exists(),
                  "tracked entry not left in place")
        _st_check((inbox / good).is_file(), "empty decisions moved a valid entry")

        subdir = sb.repo / "pkg" / "deep"
        _st_put(sb.repo / "pkg" / ".agents" / "memory", "hex.md", "## Pointers\n\n- Retro: `pkg-retro/`\n")
        subdir.mkdir()
        got = sb.json("where", cwd=subdir)
        _st_check(got.get("home") == "pkg-retro/" and got.get("inbox") == str(sb.repo / "pkg-retro" / "inbox"),
                  f"subdirectory hex.md ignored: home {got.get('home')!r}, inbox {got.get('inbox')!r}")
        _st_check(sb.json("where").get("home") == ".agents/retro/", "subdirectory hex.md leaked to the root")

        for n, home in enumerate((".Claude/rules/", ".git/retro/")):  # E13: case-folded, .git refused
            d = sb.repo / f"unsafe{n}"
            _st_put(d / ".agents" / "memory", "hex.md", f"## Pointers\n\n- Retro: `{home}`\n")
            proc = sb.expect(EXIT_ENV, "where", cwd=d)
            _st_check(proc.stderr.startswith("Error: unsafe retro home"), f"{home}: stderr {proc.stderr[:120]!r}")
            _st_check(not (sb.repo / home).exists(), f"{home}: where created the unsafe home")
        (inbox / ".gitignore").write_text("", encoding="utf-8")  # E13: unignored inbox is exit 3
        proc = sb.expect(EXIT_ENV, "where")
        _st_check(proc.stderr.startswith("Error:") and "not gitignored" in proc.stderr,
                  f"unignored inbox stderr {proc.stderr[:120]!r}")
        bare, wt = sb.tmp / "bare.git", sb.tmp / "wt"  # E13: bare <main> -> check-ignore 128, not (k)
        sb.git("clone", "-q", "--bare", str(sb.repo), str(bare))
        sb.git("-C", str(bare), "worktree", "add", "-q", str(wt))
        got = sb.json("where", cwd=wt)
        _st_check(got.get("main") == str(bare) and got.get("inbox") == str(bare / ".agents/retro/inbox"),
                  f"bare-repo worktree where {got}")
        (inbox / ".gitignore").write_text("*\n", encoding="utf-8")  # E13: 128 off a bare repo is refused
        (sb.repo / ".git" / "index").write_text("corrupt\n", encoding="utf-8")
        proc = sb.expect(EXIT_ENV, "where")
        _st_check("not gitignored" in proc.stderr, f"corrupt-index where stderr {proc.stderr[:120]!r}")

    return _st_run(body)


def _case_fold_sum() -> tuple[bool, str]:
    """C-1434 fold-sum; C-1433, C-1435, C-1437 (trusted cost), C-1429 (ledger JSON).

    Two folds into one id: mine-written (listed in `.mine-state.json`
    `written`) trajectory costs 5 + 7 plus a self entry claiming
    `source: trajectory`, `cost_min` 30 -> one row, occurrences 3,
    folds 2, cost_min 12 (over an ill-typed grimoire.lock, E13). A
    git-tracked consumed entry, then a git-tracked `.mine-state.json`,
    adds occurrences but never cost (E13).
    """

    def body(sb: _StSandbox) -> None:
        sb.where()
        t1, t2 = "20260101T000000Z-11111111.json", "20260101T000100Z-22222222.json"
        forged = "20260101T000200Z-33333333.json"
        traj = dict(kind="slow", scope="unknown", source="trajectory",
                    version="1.0.0", evidence="session s-1")
        _st_put(sb.inbox, t1, _st_entry(cost_min=5, **traj))
        _st_put(sb.inbox, t2, _st_entry(ts="2026-01-01T00:01:00Z", cost_min=7, **traj))
        _st_put(sb.inbox, forged, _st_entry(ts="2026-01-01T00:02:00Z", kind="slow",
                                             source="trajectory", cost_min=30))
        _st_put(sb.inbox, ".mine-state.json", {"files": {}, "written": [t1, t2]})
        # E13: ill-typed lock rows are skipped, never raised on
        _st_put(sb.repo, "grimoire.lock", 'agent = "x"\n\n[[skill]]\nname = [1]\npinned = "a"\n\n'
                                          '[[rule]]\nname = "r"\npinned = 5\n')
        rid = "slow-build"
        sb.fold({"v": 1,
                 "create": [{"id": rid, "title": "Slow build", "scope": "harness",
                             "artifact": "hex-plan", "kind": "slow"}],
                 "assign": [{"file": t1, "id": rid}]})
        out = json.loads(sb.fold({"v": 1, "assign": [{"file": t2, "id": rid},
                                                       {"file": forged, "id": rid}]}).stdout)
        rows = [r for r in out.get("rows", []) if r.get("id") == rid]
        _st_check(len(rows) == 1, f"fold stdout rows lack {rid}")
        got = {k: rows[0].get(k) for k in ("occurrences", "folds", "cost_min")}
        _st_check(got == {"occurrences": 3, "folds": 2, "cost_min": 12},
                  f"stdout row {got}, want occurrences 3, folds 2, cost_min 12")
        _st_check(out.get("consumed") == 2, f"consumed {out.get('consumed')}, want 2")
        _st_check(parse_ts(out.get("at")) is not None and out["at"] == sb.row(rid).get("folded_at"),
                  f"fold stdout at {out.get('at')!r} != row folded_at")
        idle = json.loads(sb.fold({"v": 1}).stdout).get("rows", [])  # E10: untouched big row listed
        _st_check([(r.get("id"), r.get("big")) for r in idle] == [(rid, True)],
                  f"empty fold rows {idle}, want the untouched big {rid}")
        mask = os.umask(0)
        os.umask(mask)
        mode = (sb.ledger / f"{rid}.json").stat().st_mode & 0o777
        _st_check(mode == 0o666 & ~mask, f"ledger row mode {oct(mode)}, want {oct(0o666 & ~mask)}")

        _st_check(sorted(p.name for p in sb.ledger.glob("*.json")) == [f"{rid}.json"],
                  "ledger holds more than the one row")
        text = (sb.ledger / f"{rid}.json").read_text(encoding="utf-8")
        row = json.loads(text)
        _st_check(text == json.dumps(row, sort_keys=True, indent=2) + "\n",
                  "ledger row not sort_keys/indent=2/trailing newline")
        got = {k: row.get(k) for k in ("v", "status", "occurrences", "folds", "cost_min")}
        _st_check(got == {"v": 1, "status": "open", "occurrences": 3, "folds": 2,
                          "cost_min": 12}, f"ledger row {got}")
        _st_check(row.get("entries") == sorted([t1, t2, forged]),
                  f"ledger entries {row.get('entries')}")
        for name in (t1, t2, forged):
            _st_check((sb.inbox / "consumed" / name).is_file()
                      and not (sb.inbox / name).exists(), f"{name} not consumed")

        # E13: a tracked consumed entry, then a tracked state file, never buys trusted cost
        t3, t4 = "20260101T000300Z-44444444.json", "20260101T000400Z-55555555.json"
        _st_put(sb.inbox / "consumed", t4, _st_entry(ts="2026-01-01T00:04:00Z", cost_min=4, **traj))
        _st_put(sb.inbox, ".mine-state.json", {"files": {}, "written": [t1, t2, t3, t4]})
        sb.git("add", "-f", str(sb.inbox / "consumed" / t4))
        sb.git("commit", "-q", "--no-verify", "-m", "plant consumed")
        sb.fold({"v": 1, "assign": [{"file": t4, "id": rid}]})
        _st_check(sb.row(rid).get("cost_min") == 12, f"tracked consumed cost summed: {sb.row(rid).get('cost_min')}")
        _st_put(sb.inbox, t3, _st_entry(ts="2026-01-01T00:03:00Z", cost_min=9, **traj))
        sb.git("add", "-f", str(sb.inbox / ".mine-state.json"))
        sb.git("commit", "-q", "--no-verify", "-m", "plant state")
        sb.fold({"v": 1, "assign": [{"file": t3, "id": rid}]})
        got = {k: sb.row(rid).get(k) for k in ("occurrences", "cost_min")}
        _st_check(got == {"occurrences": 5, "cost_min": 12}, f"tracked state cost summed: {got}")

    return _st_run(body)


def _case_fold_idempotent() -> tuple[bool, str]:
    """C-1434 fold-idempotent; C-1433 (never assigned twice), C-1454, C-1453, C-1427.

    After a fold: re-folding a consumed filename, assigning a file already
    in another row's `entries`, and re-importing an already-folded seed
    key each exit 0 and leave inbox + ledger bytes unchanged.
    """

    def body(sb: _StSandbox) -> None:
        sb.where()
        e1 = "20260101T000000Z-aaaaaaaa.json"
        _st_put(sb.inbox, e1, _st_entry())
        key = "notes.md#x1"
        seeds = sb.scratch({"v": 1, "entries": [{"key": key, "entry": _st_entry(
            ts="2026-02-03T04:05:06Z", kind="inconvenient", source="seed", evidence=key)}]})
        out = sb.json("import", seeds)
        _st_check(out.get("written") == 1, f"first import written {out.get('written')}, want 1")
        seed = f"20260203T040506Z-{hashlib.sha256(key.encode()).hexdigest()[:8]}.json"
        _st_check((sb.inbox / seed).is_file(), f"import did not publish {seed}")

        create = [{"id": rid, "title": rid, "scope": "harness", "artifact": "hex-plan",
                   "kind": "pitfall"} for rid in ("alpha-row", "beta-row")]
        sb.fold({"v": 1, "create": create,
                 "assign": [{"file": e1, "id": "alpha-row"}, {"file": seed, "id": "alpha-row"}]})
        _st_check(sb.row("alpha-row").get("entries") == sorted([e1, seed]),
                  "first fold did not record both entries")
        before = _st_tree(sb.inbox, sb.ledger)

        sb.fold({"v": 1, "assign": [{"file": e1, "id": "alpha-row"}]})
        _st_check(_st_tree(sb.inbox, sb.ledger) == before, "re-folding a consumed file changed state")
        sb.fold({"v": 1, "assign": [{"file": e1, "id": "beta-row"}]})
        _st_check(_st_tree(sb.inbox, sb.ledger) == before,
                  "assigning a file already in another row changed state")
        out = sb.json("import", seeds)
        _st_check(out.get("written") == 0, f"re-import written {out.get('written')}, want 0")
        moved = sb.scratch({"v": 1, "entries": [{"key": key, "entry": _st_entry(
            ts="2026-05-06T07:08:09Z", kind="inconvenient", source="seed", evidence=key)}]})
        out = sb.json("import", moved)
        _st_check(out.get("written") == 0, f"re-import with moved ts written {out.get('written')}, want 0")
        _st_check(_st_tree(sb.inbox, sb.ledger) == before, "re-import changed state")

    return _st_run(body)


def _case_reopen() -> tuple[bool, str]:
    """C-1434 reopen; C-1436 (lifecycle, stale_versions), S-1421.

    Fixed rows then a later fold: later ts + new version -> reopened;
    later ts + stale version -> fixed; earlier ts -> fixed; a row whose
    only versions are `unknown` (no grimoire.lock) reopens on ts alone and
    `unknown` never appears in `stale_versions`; a `shipped` fix (E6) keeps
    `stale_versions` empty, so a later ts on the same version -> reopened. A
    status-only fold leaves `folded_at` unchanged and its stdout `rows`
    omits a row it did not touch; a later empty fold still lists every
    untouched reopened row and omits the untouched non-big ones (E10).
    """

    def body(sb: _StSandbox) -> None:
        sb.where()
        late, early, old = "2099-01-01T00:00:00Z", "2000-01-01T00:00:00Z", "2020-01-01T00:00:00Z"
        # id -> (first entry, later entry, expected status); ts vs fixed.at = fold time
        cases = {
            "new-version": (_st_entry(ts=old, version="1.0.0"),
                            _st_entry(ts=late, version="2.0.0"), "reopened"),
            "stale-version": (_st_entry(ts=old, version="1.0.0"),
                              _st_entry(ts=late, version="1.0.0"), "fixed"),
            "earlier-ts": (_st_entry(ts=old, version="1.0.0"),
                           _st_entry(ts=early, version="2.0.0"), "fixed"),
            "only-unknown": (_st_entry(ts=old), _st_entry(ts=late), "reopened"),
            "shipped-fix": (_st_entry(ts=old, version="1.0.0"),
                            _st_entry(ts=late, version="1.0.0"), "reopened"),
        }
        first, second = {}, {}
        for i, (rid, (a, b, _)) in enumerate(cases.items()):
            first[rid] = _st_put(sb.inbox, f"20260101T00000{i}Z-0000000{i}.json", a).name
        sb.fold({"v": 1,
                 "create": [{"id": rid, "title": rid, "scope": "harness",
                             "artifact": "hex-plan", "kind": "defect"} for rid in cases]
                           + [{"id": "cold-row", "title": "cold-row", "scope": "harness",
                               "artifact": "hex-plan", "kind": "defect"}],
                 "assign": [{"file": f, "id": rid} for rid, f in first.items()]})
        folded_after_assign = {rid: sb.row(rid).get("folded_at") for rid in cases}
        _st_check(all(parse_ts(v) is not None for v in folded_after_assign.values()),
                  f"folded_at not set by the assigning fold: {folded_after_assign}")
        status_out = json.loads(sb.fold({"v": 1, "status": [
            {"id": rid, "to": "fixed", "by": "abc1234",
             **({"shipped": True} if rid == "shipped-fix" else {})} for rid in cases]}).stdout)
        _st_check({r.get("id") for r in status_out.get("rows", [])} == set(cases),
                  f"status-only fold stdout rows {sorted(r.get('id') for r in status_out.get('rows', []))}"
                  f", want exactly {sorted(cases)} (cold-row untouched, must be omitted)")
        for rid in cases:
            _st_check(sb.row(rid).get("folded_at") == folded_after_assign[rid],
                      f"{rid}: folded_at changed by a status-only fold")
        for rid in cases:
            row = sb.row(rid)
            fixed = row.get("fixed") or {}
            want = [] if rid in ("only-unknown", "shipped-fix") else ["1.0.0"]
            _st_check(row.get("status") == "fixed", f"{rid}: status {row.get('status')} after fix")
            _st_check(fixed.get("stale_versions") == want,
                      f"{rid}: stale_versions {fixed.get('stale_versions')}, want {want}")
        _st_check(sb.row("only-unknown").get("versions") == ["unknown"],
                  f"only-unknown versions {sb.row('only-unknown').get('versions')}")

        for i, (rid, (_, b, _)) in enumerate(cases.items()):
            second[rid] = _st_put(sb.inbox, f"20260102T00000{i}Z-1000000{i}.json", b).name
        out = json.loads(sb.fold({"v": 1, "assign": [{"file": f, "id": rid}
                                                      for rid, f in second.items()]}).stdout)
        rows = {r.get("id"): r for r in out.get("rows", [])}
        for rid, (_, _, want) in cases.items():
            row = sb.row(rid)
            _st_check(row.get("status") == want, f"{rid}: ledger status {row.get('status')}, want {want}")
            _st_check((row.get("fixed") or {}).get("by") == "abc1234", f"{rid}: last fix not kept")
            _st_check("unknown" not in row["fixed"].get("stale_versions", []),
                      f"{rid}: unknown in stale_versions")
            got = rows.get(rid, {})
            _st_check(got.get("status") == want and bool(got.get("reopened")) == (want == "reopened"),
                      f"{rid}: stdout row {got}, want status {want}")
        idle = {r.get("id") for r in json.loads(sb.fold({"v": 1}).stdout).get("rows", [])}
        want_ids = {rid for rid, (_, _, st) in cases.items() if st == "reopened"}
        _st_check(idle == want_ids, f"empty fold rows {sorted(idle)}, want the reopened {sorted(want_ids)}")

    return _st_run(body)


def _case_decline() -> tuple[bool, str]:
    """E15 (issue #11): a declined row is never re-asked without new evidence.

    A big row given `deferred` + `"declined": true` records `declined_at` and
    fold stdout `declined: true`; a re-fold re-assigning a recorded entry
    (crash replay) still lists it (big) with `declined: true`; a plain
    `deferred` keeps the decline; an occurrence dated before it keeps it; one
    dated after it drops `declined_at` (re-ask); a re-decline holds again;
    `fixed` and `open` drop it; `declined` on a non-`deferred` op, or not a
    bool, exits 2.
    """

    def body(sb: _StSandbox) -> None:
        sb.where()
        rid = "declined-row"
        files = [_st_put(sb.inbox, f"20260101T00000{i}Z-d000000{i}.json", _st_entry(ts=ts)).name
                 for i, ts in enumerate(["2020-01-01T00:00:00Z"] * 3
                                        + ["2000-01-01T00:00:00Z", "2099-01-01T00:00:00Z"])]
        sb.fold({"v": 1, "create": [{"id": rid, "title": rid, "scope": "harness",
                                     "artifact": "hex-plan", "kind": "defect"}],
                 "assign": [{"file": f, "id": rid} for f in files[:2]]})
        sb.fold({"v": 1, "assign": [{"file": files[2], "id": rid}]})  # occurrences 3, folds 2 -> big

        def declined(label: str, decisions: dict) -> object:
            rows = {r.get("id"): r for r in json.loads(sb.fold(decisions).stdout).get("rows", [])}
            _st_check(rows.get(rid, {}).get("big") is True, f"{label}: {rid} not listed as big: {rows}")
            return rows[rid].get("declined")

        _st_check(declined("decline", {"v": 1, "status": [{"id": rid, "to": "deferred", "declined": True}]})
                  is True, "decline: stdout declined not true")
        at = sb.row(rid).get("declined_at")
        _st_check(parse_ts(at) is not None and sb.row(rid).get("status") == "deferred", f"declined_at {at!r}")
        _st_check(declined("rerun", {"v": 1, "assign": [{"file": files[2], "id": rid}]}) is True,
                  "crash re-run of a recorded entry: re-asked")
        _st_check(declined("plain deferred", {"v": 1, "status": [{"id": rid, "to": "deferred"}]}) is True
                  and sb.row(rid).get("declined_at") == at, "plain deferred dropped the decline")
        _st_check(declined("old occurrence", {"v": 1, "assign": [{"file": files[3], "id": rid}]}) is True,
                  "occurrence dated before the decline re-asked")
        _st_check(declined("new occurrence", {"v": 1, "assign": [{"file": files[4], "id": rid}]}) is False
                  and "declined_at" not in sb.row(rid), "new occurrence: decline still stands")
        decline = {"v": 1, "status": [{"id": rid, "to": "deferred", "declined": True}]}
        for to in ("fixed", "open"):
            _st_check(declined(f"re-decline before {to}", decline) is True, "re-decline: not held")
            _st_check(declined(to, {"v": 1, "status": [{"id": rid, "to": to, "by": "x"}]}) is False
                      and "declined_at" not in sb.row(rid), f"{to} kept declined_at")
        for bad in ({"to": "open", "declined": True}, {"to": "fixed", "by": "x", "declined": True},
                    {"to": "deferred", "declined": "yes"}):
            before = _st_tree(sb.inbox, sb.ledger)
            sb.fold({"v": 1, "status": [{"id": rid, **bad}]}, code=EXIT_INVALID_INPUT)
            _st_check(_st_tree(sb.inbox, sb.ledger) == before, f"{bad}: state changed")

    return _st_run(body)


def _case_invalid_decisions() -> tuple[bool, str]:
    """C-1434 invalid-decisions; C-1433 (validate first, path safety), C-1429 (exit 2, Error:).

    Unknown id; `file` `../x.json`, `a/b.json`, `.x.json` (each resolves
    to a valid entry below inbox/consumed/, so only the basename rule can
    refuse it); a v 1 ledger row with a missing or ill-typed field; an
    existing ledger row with `"v": 2`; `shipped` on a non-`fixed` op (E10) -> each exits 2 with one `Error:`
    line, ledger and inbox bytes unchanged even though every decisions
    file also carries a valid create + assign.
    """

    def body(sb: _StSandbox) -> None:
        sb.where()
        e1, e2 = "20260101T000000Z-bbbbbbb1.json", "20260101T000000Z-bbbbbbb2.json"
        _st_put(sb.inbox, e1, _st_entry())
        sb.fold({"v": 1, "create": [{"id": "good-row", "title": "good", "scope": "harness",
                                     "artifact": "hex-plan", "kind": "pitfall"}],
                 "assign": [{"file": e1, "id": "good-row"}]})
        _st_put(sb.inbox, e2, _st_entry())
        consumed = sb.inbox / "consumed"  # `f` resolves as consumed/<f>
        _st_put(sb.inbox, "x.json", _st_entry())  # consumed/../x.json
        _st_put(consumed / "a", "b.json", _st_entry())
        _st_put(consumed, ".x.json", _st_entry())
        fresh = {"id": "fresh-row", "title": "fresh", "scope": "harness",
                 "artifact": "hex-plan", "kind": "pitfall"}
        valid = {"file": e2, "id": "good-row"}

        def refused(label: str, decisions: dict) -> None:
            before = _st_tree(sb.inbox, sb.ledger)
            proc = sb.fold(decisions, code=EXIT_INVALID_INPUT)
            err = proc.stderr.strip().splitlines()
            _st_check(len(err) == 1 and err[0].startswith("Error:"), f"{label}: stderr {err[:2]}")
            _st_check(_st_tree(sb.inbox, sb.ledger) == before, f"{label}: state changed")

        refused("shipped not bool", {"v": 1, "create": [fresh], "assign": [valid],
                                     "status": [{"id": "good-row", "to": "fixed", "by": "x",
                                                 "shipped": "yes"}]})
        for to in ("open", "deferred"):
            refused(f"shipped with to {to}", {"v": 1, "create": [fresh], "assign": [valid],
                                              "status": [{"id": "good-row", "to": to, "shipped": True}]})
        refused("unknown id", {"v": 1, "create": [fresh],
                               "assign": [{"file": e2, "id": "no-such-row"}]})
        for bad in ("../x.json", "a/b.json", ".x.json"):
            refused(f"file {bad}", {"v": 1, "create": [fresh],
                                    "assign": [valid, {"file": bad, "id": "good-row"}]})
        good = sb.row("good-row")
        broken = {"entries": dict(good, entries=None), "status": dict(good, status=5),
                  "first_seen": dict(good, first_seen="yesterday"),
                  "fixed.at": dict(good, fixed={"at": "never", "by": "x"}),
                  "declined_at": dict(good, declined_at="never"),
                  "no first_seen": {k: v for k, v in good.items() if k != "first_seen"}}
        for label, row in broken.items():
            path = _st_put(sb.ledger, "broken-row.json", row)
            refused(f"v 1 row {label}", {"v": 1, "create": [fresh], "assign": [valid]})
            path.unlink()
        _st_put(sb.ledger, "future-row.json", {"v": 2, "id": "future-row"})
        refused("v: 2 row", {"v": 1, "create": [fresh], "assign": [valid]})

    return _st_run(body)


def _case_miner() -> tuple[bool, str]:
    """C-1434 miner; C-1432 (keys, attribution, shape guard, replay), C-1427, C-1425, S-1420.

    Synthetic session + two subagent transcripts: keys `cargo test` (both
    cargo lines), `some-cli`, `curl`, `echo`; quoted secrets in env/`cd`/
    `export` segments -> `curl`, `psql`, `cargo test`, `make`; attributed
    to the invoked skill only (a non-Skill `input.skill` ignored; subagents
    inherit); entries carry `version`; no secret, input
    or result text in any written byte (entries, `.mine-state.json`);
    `summary` record not counted, a tool-free session file adds no degraded
    item (E5), one unknown-shape block -> exactly one degraded item;
    `rtk proxy git status` -> `git status` (E8); an unsupported wrapper option
    (`env -S '<url>'`, `env -u <secret>`), a URL program, or an uppercase or
    digit-heavy program token, any token after an empty assignment, or a
    program token over 24 chars (E13) failing -> key `Bash`, no secret
    written; a deep-nested line degrades, never raises, and ill-typed or
    deep-nested `.mine-state.json` rows read as fresh (E13); a
    non-string `type`/`timestamp` record counts as degraded, never raises
    (E11: run 1 now expects one degraded file); unchanged rerun writes 0;
    a tool_result appended after run 1 is counted exactly once; a record
    outside the repo is ignored.
    """

    def body(sb: _StSandbox) -> None:
        main = sb.where()["main"]
        sid = "5e55a0f1-0000-4000-8000-000000000001"
        proj = sb.cfg / "projects" / "".join(c if c.isalnum() and c.isascii() else "-" for c in main)
        session = proj / f"{sid}.jsonl"
        sub = proj / sid / "subagents" / "agent-a1b2c3d4.jsonl"
        sub2 = sub.parent / "agent-e5f6a7b8.jsonl"
        sub.parent.mkdir(parents=True)

        def rec(typ: str, hhmmss: str, content: object, cwd: str = main, **extra: object) -> dict:
            return {"type": typ, "timestamp": f"2026-09-01T{hhmmss}.000Z", "cwd": cwd,
                    "sessionId": sid, "version": "2.0.0",
                    "message": {"role": typ, "content": content}, **extra}

        def use(hhmmss: str, tid: str, name: str, inp: dict, **kw: object) -> dict:
            return rec("assistant", hhmmss, [{"type": "tool_use", "id": tid, "name": name,
                                              "input": inp}], **kw)

        def res(hhmmss: str, tid: str, err: bool = False, **kw: object) -> dict:
            text = "RESULT-TEXT-MARKER AKIAIOSFODNN7EXAMPLE sk_live_X hunter2 SECRET"
            return rec("user", hhmmss, [{"type": "tool_result", "tool_use_id": tid,
                                         "is_error": err, "content": text}], **kw)

        def bash(hhmmss: str, tid: str, cmd: str, **kw: object) -> dict:
            return use(hhmmss, tid, "Bash", {"command": cmd, "description": "INPUT-DESC-MARKER"}, **kw)

        elsewhere = str(sb.tmp / "elsewhere")
        lines = [
            {"type": "summary", "summary": "a session title", "leafUuid": "u-1"},
            rec("user", "10:00:00", "run the build"),
            use("10:00:01", "s0", "Skill", {"skill": "hex-plan"}),
            res("10:00:02", "s0"),
            use("10:00:03", "s1", "Task", {"skill": "not-a-skill-call"}),
            res("10:00:04", "s1"),
            bash("10:01:00", "c1", "cd x && timeout 60 rtk cargo test --token=SECRET"),
            res("10:12:00", "c1"),
            bash("10:13:00", "c2", "export T=x; cargo test"),
            res("10:24:00", "c2"),
            bash("10:25:00", "c3", "some-cli AKIAIOSFODNN7EXAMPLE"),
            res("10:36:00", "c3"),
            use("10:37:00", "r1", "Read", {"file_path": "/INPUT-PATH-MARKER"}),
            res("10:37:01", "r1", err=True),
            use("10:37:02", "r2", "Read", {"file_path": "/INPUT-PATH-MARKER"}),
            res("10:37:03", "r2", err=True),
            bash("10:38:00", "p1", "pytest -x"),  # left unpaired in run 1
            bash("10:39:00", "m1", "make build", cwd=elsewhere),
            res("11:09:00", "m1", cwd=elsewhere),
        ]
        session.write_text("".join(json.dumps(r) + "\n" for r in lines), encoding="utf-8")
        wt = f"{main}/.agents/worktrees/gone"  # removed worktree (S-1420)
        sub_lines = [
            rec("user", "10:02:00", "sub task", cwd=wt, isSidechain=True),
            bash("10:02:10", "a1", "curl sk_live_ABC123secret", cwd=wt, isSidechain=True),
            res("10:13:10", "a1", cwd=wt, isSidechain=True),
            bash("10:14:00", "a2", "echo hunter2", cwd=wt, isSidechain=True),
            res("10:25:00", "a2", cwd=wt, isSidechain=True),
        ]
        sub.write_text("".join(json.dumps(r) + "\n" for r in sub_lines), encoding="utf-8")
        quoted = [('TOKEN="a sk_live_ABC123secret b" curl https://x', "curl"),
                  ("PGPASSWORD='x AKIAIOSFODNN7EXAMPLE y' psql", "psql"),
                  ('export T="x;ghp_abcDEF1234567890 y"; cargo test', "cargo test"),
                  ('cd "/p;AKIAIOSFODNN7EXAMPLE q" && make', "make"),
                  ("rtk proxy git status", "git status")]
        sub2_lines = [rec("user", "10:03:00", "quoted", isSidechain=True)]
        for n, (cmd, _) in enumerate(quoted):
            sub2_lines += [bash(f"1{n}:04:00", f"q{n}", cmd, isSidechain=True),
                           res(f"1{n}:15:00", f"q{n}", isSidechain=True)]
        wrapped = "env -S 'curl https://example.invalid/sk_live_ABC123secret'"
        sub2_lines += [bash("15:00:00", "w1", wrapped, isSidechain=True),
                       res("15:00:01", "w1", err=True, isSidechain=True),
                       bash("15:00:02", "w2", "env -u sk_live_ABC123secret make", isSidechain=True),
                       res("15:00:03", "w2", err=True, isSidechain=True),
                       bash("15:00:04", "w3", "https://example.invalid/sk_live_ABC123secret", isSidechain=True),
                       res("15:00:05", "w3", err=True, isSidechain=True),
                       {"type": []}, rec("user", "15:00:06", "odd", timestamp=["x"])]
        # E13: a secret or digit-heavy token in program position -> key `Bash`, twice each so a
        # leaked key would cross mine-errors and be written
        for n, cmd in enumerate(["AWS_ACCESS_KEY_ID= AKIAIOSFODNN7EXAMPLE aws s3 ls", "PW= hunter2 psql",
                                 "x9f3k2m8q7z1 --flag", "abcdefghijklmnopqrstuvwxyz --flag"] * 2):
            sub2_lines += [bash(f"15:01:{2 * n:02}", f"d{n}", cmd, isSidechain=True),
                           res(f"15:01:{2 * n + 1:02}", f"d{n}", err=True, isSidechain=True)]
        sub2.write_text("".join(json.dumps(r) + "\n" for r in sub2_lines) + "[" * 100000 + "\n",
                        encoding="utf-8")  # E13: a deep-nested line is degraded, never a crash
        idle = proj / "5e55a0f1-0000-4000-8000-000000000002.jsonl"  # tool-free session (E5)
        idle.write_text("".join(json.dumps(r) + "\n" for r in (
            rec("user", "10:05:00", "just a question"),
            rec("assistant", "10:05:10", [{"type": "text", "text": "an answer"}]))), encoding="utf-8")
        state_path = sb.inbox / ".mine-state.json"
        forbidden = [b"AKIA", b"x9f3k2m8q7z1", b"abcdefghijklmnopqrstuvwxyz", b"sk_live", b"ghp_",
                     b"hunter2", b"SECRET", b"RESULT-TEXT-MARKER", b"INPUT-DESC-MARKER",
                     b"INPUT-PATH-MARKER", b"not-a-skill-call"]

        def inbox_json() -> set[str]:
            return {p.name for p in sb.inbox.glob("*.json") if not p.name.startswith(".")}

        def totals(files: tuple[Path, ...] = (session, sub)) -> dict[str, dict]:
            state = json.loads(state_path.read_text(encoding="utf-8"))
            merged: dict[str, dict] = {}
            for f in (state.get("files", {}).get(str(p), {}) for p in files):
                for k, v in f.get("totals", {}).items():
                    m = merged.setdefault(k, {"ms": 0, "errors": 0})
                    m["ms"] += v.get("ms", 0)
                    m["errors"] += v.get("errors", 0)
            return merged

        def no_leak(label: str) -> None:
            for path, data in _st_tree(sb.inbox).items():
                for bad in forbidden:
                    _st_check(data is None or bad not in data,
                              f"{label}: {bad.decode()} written to {Path(path).name}")

        # E13: ill-typed state rows (non-object file, non-object totals) read as fresh, never raise
        # and a deep-nested `files` row (kept by the row filter) reads as corrupt state -- it
        # parses on 3.12+, then dump overflowed (E14; red on 3.12 with the depth guard reverted)
        _st_put(sb.inbox, ".mine-state.json", json.dumps(
            {"files": {str(session): 1, str(sub): {"totals": [1]}, "/gone.jsonl": "DEEP"}})
            .replace('"DEEP"', '{"x": ' + "[" * 2000 + "]" * 2000 + "}"))
        # run 1 -- default transcripts dir from CLAUDE_CONFIG_DIR
        out = sb.json("mine")
        _st_check(isinstance(out.get("span_min"), (int, float)), "span_min missing")
        _st_check([d[:28] for d in out.get("degraded", [])] == ["transcript shape: 1 file(s) "],
                  f"run 1 degraded {out.get('degraded')}, want only sub2's malformed records "
                  f"(summary or tool-free file counted?)")
        _st_check(out.get("written") == 11, f"run 1 written {out.get('written')}, want 11")
        names = inbox_json()
        state = json.loads(state_path.read_text(encoding="utf-8"))
        _st_check(set(state.get("written", [])) == names, "state `written` != published names")
        t = totals()
        for key in ("cargo test", "some-cli", "curl", "echo", "Read"):
            _st_check(f"hex-plan|{key}" in t, f"key hex-plan|{key} missing from {sorted(t)}")
        _st_check([k for k in t if "cargo" in k] == ["hex-plan|cargo test"],
                  f"cargo keys {[k for k in t if 'cargo' in k]}")
        _st_check(t["hex-plan|cargo test"].get("ms") == 22 * 60000,
                  "cargo test ms != both lines (22 min)")
        _st_check(t["hex-plan|Read"].get("errors") == 2, "Read errors != 2")
        _st_check(not any(v["ms"] for k, v in t.items() if "make" in k or "pytest" in k),
                  "outside-cwd or unpaired call counted")
        want = {f"hex-plan|{k}" for _, k in quoted} | {"hex-plan|Bash"}
        _st_check(totals((sub2,)).get("hex-plan|Bash", {}).get("errors") == 11, "wrapper/URL/secret keys not Bash")
        _st_check(set(totals((sub2,))) == want, f"quoted keys {sorted(totals((sub2,)))}, want {sorted(want)}")
        read = sb.json("read")
        _st_check(not read.get("skipped") and len(read.get("entries", [])) == 11,
                  f"mined entries fail read validation: {read.get('skipped')}")
        entries = [e["entry"] for e in read["entries"]]
        for e in entries:
            _st_check(e.get("artifact") == "hex-plan", f"attributed to {e.get('artifact')}")
            _st_check(e.get("source") == "trajectory" and e.get("scope") == "unknown",
                      "mined entry not source trajectory / scope unknown")
            _st_check(isinstance(e.get("version"), str) and e["version"], "mined entry lacks version")
            _st_check(e.get("evidence") == f"session {sid}", f"evidence {e.get('evidence')!r}")
        kinds = sorted((e["kind"], e.get("cost_min", 0)) for e in entries)
        _st_check(kinds == [("pitfall", 0), ("pitfall", 0), *[("slow", 11)] * 8, ("slow", 22)],
                  f"mined kinds/costs {kinds}")
        no_leak("run 1")

        # run 2 -- unchanged files
        out = sb.json("mine", "--transcripts", str(sb.cfg / "projects"))
        _st_check(out.get("written") == 0 and inbox_json() == names,
                  f"unchanged rerun written {out.get('written')}")

        # run 3 -- late tool_result for p1 plus one unknown-shape assistant block
        with session.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(res("11:10:00", "p1")) + "\n")
            fh.write(json.dumps(rec("assistant", "11:10:30",
                                    [{"type": "hologram", "data": "x"}])) + "\n")
        st = session.stat()
        os.utime(session, ns=(st.st_atime_ns, st.st_mtime_ns + 2_000_000_000))
        out = sb.json("mine", "--transcripts", str(sb.cfg / "projects"))
        _st_check(out.get("written") == 1, f"run 3 written {out.get('written')}, want 1 (pytest)")
        _st_check(len(out.get("degraded", [])) == 1, f"run 3 degraded {out.get('degraded')}, want 1")
        t = totals()
        _st_check(t.get("hex-plan|pytest", {}).get("ms") == 32 * 60000, "late pair not counted once")
        _st_check(t["hex-plan|cargo test"].get("ms") == 22 * 60000, "replay double counted cargo test")
        new = inbox_json() - names
        _st_check(len(new) == 1, f"run 3 new files {sorted(new)}")
        e = json.loads((sb.inbox / new.pop()).read_text(encoding="utf-8"))
        _st_check(e.get("kind") == "slow" and e.get("cost_min") == 32, f"late entry {e}")
        no_leak("run 3")

        # run 4 -- nothing new after the pair was counted
        out = sb.json("mine", "--transcripts", str(sb.cfg / "projects"))
        _st_check(out.get("written") == 0, f"run 4 written {out.get('written')}, want 0")

    return _st_run(body)


def _case_thresholds() -> tuple[bool, str]:
    """C-1434 thresholds; C-1424, C-1429 (parse first, exit 3).

    Fixture SKILL.md without the table -> `where` exits 3 with
    `Error: thresholds table not found` and creates nothing; same for a
    table missing one C-1424 name and one with an unparseable value.
    """

    def body(sb: _StSandbox) -> None:
        missing = {k: v for k, v in _ST_THRESHOLDS.items() if k != "mine-errors"}
        garbled = dict(_ST_THRESHOLDS, **{"bar-folds": "many"})
        for label, md in (("no table", _st_skill_md(None)),
                          ("name absent", _st_skill_md(missing)),
                          ("unparseable", _st_skill_md(garbled))):
            sb.skill_md.write_text(md, encoding="utf-8")
            proc = sb.expect(EXIT_ENV, "where")
            _st_check("Error: thresholds table not found" in proc.stderr,
                      f"{label}: stderr {proc.stderr.strip()[:120]!r}")
            _st_check(not (sb.repo / ".agents").exists(), f"{label}: where created files")

    return _st_run(body, _st_skill_md(None))


SELFTEST_CASES: list[tuple[str, Callable[[], tuple[bool, str]]]] = [
    ("malformed", _case_malformed),
    ("fold-sum", _case_fold_sum),
    ("fold-idempotent", _case_fold_idempotent),
    ("reopen", _case_reopen),
    ("decline", _case_decline),
    ("invalid-decisions", _case_invalid_decisions),
    ("miner", _case_miner),
    ("thresholds", _case_thresholds),
]


def cmd_selftest(args: argparse.Namespace) -> int:
    """C-1434: run every case, print ok/FAIL per case, exit 1 on any FAIL."""
    all_ok = True
    for name, case in SELFTEST_CASES:
        passed, why = case()
        if passed:
            print(f"ok {name}")
        else:
            print(f"FAIL {name}: {why}")
            all_ok = False
    return EXIT_OK if all_ok else EXIT_SELFTEST_FAIL


class _Parser(argparse.ArgumentParser):
    def error(self, message: str) -> NoReturn:  # one `Error:` line, exit 2
        die(EXIT_INVALID_INPUT, message)


def build_parser() -> argparse.ArgumentParser:
    parser = _Parser(
        prog="retro.py", description="hex-retro inbox/ledger CLI"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("where", help="print resolved inbox/ledger/reports paths").set_defaults(
        func=cmd_where
    )
    sub.add_parser("read", help="validate + list inbox entries").set_defaults(func=cmd_read)

    p_mine = sub.add_parser("mine", help="mine transcripts for slow/pitfall entries")
    p_mine.add_argument("--transcripts", help="transcripts dir override")
    p_mine.set_defaults(func=cmd_mine)

    p_fold = sub.add_parser("fold", help="apply a decisions.json to the ledger")
    p_fold.add_argument("decisions")
    p_fold.add_argument("--wall-min", type=float)
    p_fold.set_defaults(func=cmd_fold)

    p_import = sub.add_parser("import", help="import seed entries from entries.json")
    p_import.add_argument("entries")
    p_import.set_defaults(func=cmd_import)

    sub.add_parser("selftest", help="run the built-in self-test").set_defaults(
        func=cmd_selftest
    )

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.func is not cmd_selftest:
        args.th = load_thresholds()  # C-1429: before anything else touches the repo
    try:
        return args.func(args)
    # UnicodeDecodeError is a ValueError subclass, not OSError; E13: RecursionError is neither
    except (UnicodeDecodeError, ValueError, KeyError, TypeError, RecursionError) as exc:
        die(EXIT_INVALID_INPUT, " ".join(f"{type(exc).__name__}: {exc}".split()))
    except OSError as exc:
        die(EXIT_ENV, " ".join(f"{type(exc).__name__}: {exc}".split()))


if __name__ == "__main__":
    sys.exit(main())
