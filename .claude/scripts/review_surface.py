#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pygments>=2.19"]
# ///
"""Generate a clickable review page that sorts a diff by what actually moves a contract.

Motivation, measured on ocx#227: 9241 lines changed, of which 384 (4.2%) were
production logic. 2816 of the 4069 added Rust lines lived inside `#[cfg(test)]`
regions *of production files*, so any file-level view — GitHub's included —
structurally cannot show which files carry real changes.

Tiers are therefore assigned PER LINE:

    T0 WIRE   serde / OCI wire / lock format — breaks other programs
    T1 CLI    flags, `--format json` shapes, exit codes — what callers branch on
    T2 API    new public types and changed signatures
    T3 LOGIC  no contract signal; read anyway
    T4 DOC / T5 TEST / T6 SCAFFOLD  — counted, not ranked

T3 is enumerated by file and never collapsed to a count. It carries no syntactic
contract signal, which is exactly why hiding it would steer a reviewer away from
the largest change in a PR while looking authoritative. On ocx#227 the single
biggest production file (oci/index/local_index.rs, 47 lines) is a T3.

Dependencies are declared inline (PEP 723) and resolved by `uv run --script`, so
there is no venv to create and nothing to add to a pyproject. `uvx` is
`uv tool run` — it runs PyPI tools, not local scripts, and refuses this path.

Usage:
    task claude:review-surface -- 227                        # a PR
    uv run --script .claude/scripts/review_surface.py 227
    uv run --script .claude/scripts/review_surface.py --base main
    uv run --script .claude/scripts/review_surface.py 227 --no-open

Output: out/review-<slug>.html (gitignored), self-contained, opened in the
default browser. Nothing is uploaded anywhere.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

from pygments import lex
from pygments.lexers import get_lexer_for_filename
from pygments.token import Comment, Keyword, Name, Number, String
from pygments.util import ClassNotFound

REPO_ANCHOR = "crates/ocx_lib/src/oci/client.rs"
TEMPLATE = pathlib.Path(__file__).parent / "review_surface_page.html"

TIERS = {
    "T0": ("WIRE", "serde / OCI wire / lock format — breaks other programs"),
    "T1": ("CLI & EXIT", "what callers type, parse and branch on"),
    "T2": ("API", "new types and changed public signatures"),
    "T3": ("LOGIC", "no contract signal — read anyway"),
    "T4": ("DOC", "rustdoc, website, markdown"),
    "T5": ("TEST", "Rust #[cfg(test)], integration tests, pytest, fixtures"),
    "T6": ("SCAFFOLD", ".claude / CI / taskfiles"),
}
ORDER = ["T0", "T1", "T2", "T3", "T4", "T5", "T6"]
PROD = {"T0", "T1", "T2", "T3"}

WIRE_FILES = re.compile(
    r"crates/ocx_lib/src/oci/(index/(wire|wire_writer|ocx_index|oci_index)\.rs|manifest\.rs)"
    r"|crates/ocx_lib/src/project/(config|lock)\.rs"
    r"|crates/ocx_lib/src/package/metadata"
    r"|crates/ocx_schema/"
    r"|fixtures/index_wire/"
)
CLI_FILES = re.compile(r"crates/ocx_cli/src/")
EXIT_FILES = re.compile(r"(cli/exit_code\.rs|cli/classify\.rs|/error\.rs$)")
TESTY = re.compile(r"crates/[^/]+/tests/|^test/|fixtures/")
SCAFFOLD = re.compile(r"^\.claude/|^\.agents/|^taskfiles/|^\.github/")


def require_repo_root() -> None:
    """Refuse to run anywhere else.

    `cfg_test_ranges` reads each changed file from the working tree by relative
    path. From the wrong directory every read raises, zero ranges are produced,
    and every test line silently reclassifies as production — the tool then
    reports ~6x the real number and looks entirely plausible. This bit the
    author twice while establishing the baseline, so it is a hard precondition
    rather than a warning.
    """
    if not pathlib.Path(REPO_ANCHOR).exists():
        sys.exit(
            f"refusing to run: {REPO_ANCHOR} not found, so this is not the repo root.\n"
            "Classification would silently count test code as production logic.\n"
            "cd to the repository root and retry."
        )


def fetch_diff(pr: int | None, base: str | None) -> str:
    """The unified diff, as text.

    `git diff` is unusable in this environment — the RTK proxy returns empty
    output for every form of it, which reads as "no changes" rather than as an
    error. For a PR we go straight at the GitHub API with curl; for a local
    range we shell out to `git` with the proxy bypassed via `git --no-pager`
    plus an explicit read of stdout, and validate the result is non-empty.
    """
    if pr is not None:
        token = subprocess.run(
            ["gh", "auth", "token"], capture_output=True, text=True, check=True
        ).stdout.strip()
        slug = subprocess.run(
            ["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        out = subprocess.run(
            [
                "curl", "-fsSL",
                "-H", f"Authorization: Bearer {token}",
                "-H", "Accept: application/vnd.github.v3.diff",
                f"https://api.github.com/repos/{slug}/pulls/{pr}",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    else:
        merge_base = subprocess.run(
            ["git", "merge-base", base, "HEAD"], capture_output=True, text=True, check=True
        ).stdout.strip()
        out = subprocess.run(
            ["git", "--no-pager", "diff", "--no-color", f"{merge_base}...HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout

    if not out.strip():
        sys.exit(
            "the diff came back empty.\n"
            "If this was a local range, `git diff` is being swallowed by the RTK proxy — "
            "pass a PR number instead, which goes through the GitHub API."
        )
    return out


def cfg_test_ranges(path: str) -> list[tuple[int, int]]:
    """Line ranges of `#[cfg(test)]` modules in the file as it stands at HEAD."""
    try:
        src = pathlib.Path(path).read_text(errors="replace").splitlines()
    except OSError:
        return []
    out: list[tuple[int, int]] = []
    i = 0
    while i < len(src):
        if "#[cfg(test)]" in src[i] or "#[cfg(any(test," in src[i]:
            depth, start, seen, j = 0, i + 1, False, i
            while j < len(src):
                depth += src[j].count("{") - src[j].count("}")
                if "{" in src[j]:
                    seen = True
                if seen and depth <= 0:
                    break
                j += 1
            out.append((start, j + 1))
            i = j
        i += 1
    return out


def tier_of(path: str, body: str, in_test: bool) -> str:
    """First match wins. Order encodes review priority, not specificity."""
    if SCAFFOLD.search(path):
        return "T6"
    if in_test or TESTY.search(path):
        return "T5"
    if body.startswith(("///", "//!")) or path.startswith("website/") or path.endswith(".md"):
        return "T4"
    if not path.endswith(".rs"):
        return "T6"
    if WIRE_FILES.search(path) or re.search(r"#\[serde\(|deny_unknown_fields", body):
        return "T0"
    if CLI_FILES.search(path) or EXIT_FILES.search(path) or "try_downcast!" in body:
        return "T1"
    if re.match(r"^\s*pub(\([^)]*\))?\s+(fn|struct|enum|trait|type|const)\b", body) or re.match(
        r"^\s*impl\b.*\bfor\b", body
    ):
        return "T2"
    return "T3"


def classify(diff: str) -> dict:
    files: dict[str, dict] = {}
    path: str | None = None
    ranges: list[tuple[int, int]] = []
    lineno = 0
    hunk: dict | None = None
    # Total churn counts every added AND deleted line, including blank and
    # comment lines, so the headline matches the number the forge shows. The
    # tier totals deliberately do not: comparing "what the forge counts" against
    # "what is worth reading" is the entire point of the page, and quietly
    # redefining the denominator would make the ratio flattering rather than true.
    churn = 0
    seen_files: set[str] = set()

    for line in diff.splitlines():
        if line.startswith("+++ b/"):
            path = line[6:]
            ranges = cfg_test_ranges(path) if path.endswith(".rs") else []
            files.setdefault(path, {"path": path, "hunks": [], "tiers": {}})
            seen_files.add(path)
            continue
        if line.startswith("@@"):
            m = re.search(r"\+(\d+)", line)
            lineno = int(m.group(1)) if m else 0
            hunk = {"header": line[:200], "lines": []}
            if path:
                files[path]["hunks"].append(hunk)
            continue
        if not path or hunk is None:
            continue

        if line.startswith("+") and not line.startswith("+++"):
            churn += 1
            body = line[1:].strip()
            in_test = any(a <= lineno <= b for a, b in ranges)
            if not body:
                tier = "blank"
            elif body.startswith("//") and not body.startswith(("///", "//!")):
                tier = "comment"
            else:
                tier = tier_of(path, body, in_test)
            # 4th element is the NEW-file line number, so the page can offer a
            # copyable `path:line` reference. Deleted lines get 0 — they have no
            # line in the file you would open.
            hunk["lines"].append(["+", line[1:], tier, lineno])
            if tier not in ("blank", "comment"):
                files[path]["tiers"][tier] = files[path]["tiers"].get(tier, 0) + 1
            lineno += 1
        elif line.startswith("-") and not line.startswith("---"):
            churn += 1
            hunk["lines"].append(["-", line[1:], "del", 0])
        elif not line.startswith("\\"):
            hunk["lines"].append([" ", line[1:] if line else "", "ctx", lineno])
            lineno += 1

    out_files = []
    for f in files.values():
        if not f["tiers"]:
            continue
        top = next((t for t in ORDER if f["tiers"].get(t)), "T6")
        prod = sum(f["tiers"].get(t, 0) for t in PROD)
        # Only hunks carrying a production line are worth expanding: a production
        # file here is mostly test hunks, and shipping those would reproduce the
        # very problem this page exists to solve (and quadruple the payload).
        hunks = (
            [h for h in f["hunks"] if any(l[2] in PROD for l in h["lines"])]
            if top in PROD
            else []
        )
        for h in hunks:
            idx = [i for i, l in enumerate(h["lines"]) if l[0] in "+-"]
            if idx:
                lo, hi = max(0, idx[0] - 3), min(len(h["lines"]), idx[-1] + 4)
                h["lines"] = h["lines"][lo:hi]
        out_files.append(
            {
                "path": f["path"],
                "tier": top,
                "counts": f["tiers"],
                "prod": prod,
                "total": sum(f["tiers"].values()),
                "hunks": hunks,
            }
        )

    out_files.sort(key=lambda f: (ORDER.index(f["tier"]), -f["prod"]))
    totals = {t: sum(f["counts"].get(t, 0) for f in out_files) for t in ORDER}
    return {
        "tiers": {t: {"name": TIERS[t][0], "blurb": TIERS[t][1], "lines": totals[t]} for t in ORDER},
        "files": out_files,
        "movers": sorted([f for f in out_files if f["prod"]], key=lambda f: -f["prod"])[:6],
        "churn": churn,
        "filecount": len(seen_files),
    }


def esc(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def split_path(path: str) -> tuple[str, str]:
    i = path.rfind("/")
    return ("", path) if i < 0 else (path[: i + 1], path[i + 1 :])


_LEXER_CACHE: dict[str, object] = {}


def lexer_for(path: str):
    """A Pygments lexer for the file, or None when it has no sensible one.

    Cached per extension: `get_lexer_for_filename` is not free, and this runs
    over thousands of lines.
    """
    ext = pathlib.PurePath(path).suffix
    if ext not in _LEXER_CACHE:
        try:
            # `ensurenl` must stay on. With `ensurenl=False` the Rust lexer's
            # `//` rule never matches, because it is terminated by a newline —
            # every single-line comment silently degrades to Operator + Name
            # tokens and renders as ordinary code. The trailing newline is
            # stripped back off in `highlight_line`.
            _LEXER_CACHE[ext] = get_lexer_for_filename(path, stripnl=False)
        except ClassNotFound:
            _LEXER_CACHE[ext] = None
    return _LEXER_CACHE[ext]


# Deliberately coarse. An IDE's full token rainbow fights the tier colouring
# that is the actual signal here, so only a few roles get a hue and everything
# else stays at plain ink.
#
# ORDER IS LOAD-BEARING — first match wins, and two Pygments classifications are
# counter-intuitive for Rust:
#   * `#[serde(...)]` lexes as Comment.Preproc, NOT as an attribute. Left to
#     fall into `Comment` it would be greyed out — and it is the single
#     highest-signal line on the page, since T0 keys on exactly that. It gets
#     its own emphasised class instead.
#   * `/// docs` lexes as String.Doc, NOT as Comment. Left to fall into `String`
#     it would be coloured like a string literal rather than receding.
_TOKEN_CLASS = (
    (Comment.Preproc, "a"),  # before Comment
    (String.Doc, "c"),  # before String
    (Comment, "c"),
    (String, "s"),
    (Keyword, "k"),
    (Number, "n"),
    (Name.Function, "f"),
    (Name.Class, "f"),
    # Punctuation and operators are deliberately NOT tokenised. They were, and
    # cost 10859 spans — a third of the page weight — to buy a slight opacity
    # change nobody would miss.
)


def highlight_line(text: str, lexer) -> str:
    """Escaped HTML for one source line, with token spans.

    Lexed per line rather than per hunk, so a block comment or a multi-line
    string that starts outside the visible window is not carried in. In a diff
    view that is the right trade: hunks are already fragments, and a lexer state
    machine primed on partial code produces confident nonsense.

    ponytail: per-line lexing. Cheap and stable; if block comments spanning a
    hunk boundary ever matter, lex the reconstructed hunk instead.
    """
    if lexer is None:
        return esc(text)
    out = []
    for token, value in lex(text, lexer):
        if not value:
            continue
        chunk = esc(value)
        for token_type, name in _TOKEN_CLASS:
            if token in token_type:
                out.append(f'<span class="tk-{name}">{chunk}</span>')
                break
        else:
            out.append(chunk)
    return "".join(out).rstrip("\n")


def render(data: dict, title: str, subtitle: str, link: str, ledger_cmd: str) -> str:
    """Build the page as static HTML.

    Rendered here rather than in the browser on purpose: a page that assembles
    itself in JavaScript shows an empty shell anywhere scripts do not run — an
    IDE preview pane, a locked-down browser — which is indistinguishable from
    "the diff was empty". Content is markup; JavaScript only adds collapsing and
    click-to-copy. It also removes the injection hazard of embedding raw diff
    text inside a <script> block, where a changed line containing `</script>`
    would silently truncate the page.
    """
    tpl = TEMPLATE.read_text()
    tiers, files, movers = data["tiers"], data["files"], data["movers"]
    classified = sum(tiers[t]["lines"] for t in ORDER) or 1
    churn = data["churn"] or classified
    prod = sum(tiers[t]["lines"] for t in PROD)
    pct = 100 * prod / churn if churn else 0

    bar = "".join(
        f'<span style="width:{tiers[t]["lines"] / classified * 100:.2f}%;'
        f'background:var(--{t.lower()});opacity:{1 if t in PROD else 0.34}"></span>'
        for t in ORDER
    )
    barkey = "".join(
        f'<span><i style="background:var(--{t.lower()});opacity:{1 if t in PROD else 0.34}"></i>'
        f'{t} {tiers[t]["name"]} {tiers[t]["lines"]:,}</span>'
        for t in ORDER
    )
    top = movers[0] if movers else None
    if pct >= 40:
        note = (
            f"Production logic is <strong>{pct:.0f}% of this diff</strong>. There is not much "
            "noise to filter — the tiers below are a reading order, not a rescue."
        )
    else:
        note = (
            f"This is a <strong>~{round(prod, -2):,}-line change wearing a "
            f"{churn:,}-line coat</strong>. Tests are {100 * tiers['T5']['lines'] / classified:.0f}% "
            "of it, and rustdoc in this repo is design record rather than padding — but neither "
            f"belongs at the same visual weight as the {top['prod'] if top else 0} lines in "
            f"<strong>{esc(split_path(top['path'])[1]) if top else '—'}</strong>."
        )

    mover_html = "".join(
        f'<div class="mrow"><span class="mnum">{f["prod"]}</span>'
        f'<button class="mpath" type="button" data-copy="{esc(f["path"])}" '
        f'title="Copy {esc(f["path"])}">{esc(split_path(f["path"])[0])}'
        f'<b>{esc(split_path(f["path"])[1])}</b></button></div>'
        for f in movers
    )

    sections = []
    for tier in ("T0", "T1", "T2", "T3"):
        rows = [f for f in files if f["tier"] == tier]
        if not rows:
            continue
        body = []
        for f in rows:
            dirname, base = split_path(f["path"])
            lexer = lexer_for(f["path"])
            hunks = []
            for h in f["hunks"]:
                lines = []
                for glyph, text, kind, no in h["lines"]:
                    cls = "add" if glyph == "+" else "del" if glyph == "-" else "ctx"
                    mark = " p" if kind in PROD else ""
                    gutter = (
                        f'<button class="lno" type="button" data-copy="{esc(f["path"])}:{no}" '
                        f'title="Copy {esc(f["path"])}:{no}">{no}</button>'
                        if no
                        else '<button class="lno" type="button" disabled aria-hidden="true"></button>'
                    )
                    # No +/- glyph: the tint already says added or deleted, and
                    # dropping it keeps the code column aligned with the file.
                    # Three non-chromatic cues survive, so this is not
                    # colour-only encoding: a deleted line has an empty gutter
                    # (it has no line number in the file you would open), added
                    # lines carry full-strength ink, and context is dimmed.
                    lines.append(
                        f'<span class="ln {cls}{mark}">{gutter}'
                        f'<span class="code">{highlight_line(text, lexer)}</span></span>'
                    )
                hunks.append(f'<div class="hhead">{esc(h["header"])}</div><pre>{"".join(lines)}</pre>')
            extra = f' <em>/ {f["total"]} total</em>' if f["total"] > f["prod"] else ""
            body.append(
                f'<div class="file">'
                f'<div class="frow">'
                f'<button class="fbtn" type="button" aria-expanded="true">'
                f'<span class="chev">▶</span>'
                f'<span class="fpath"><i>{esc(dirname)}</i><b>{esc(base)}</b></span>'
                f'<span class="fcount">{f["prod"]} prod{extra}</span>'
                f'<span class="pill">{tier}</span></button>'
                f'<button class="fcopy" type="button" data-copy="{esc(f["path"])}" '
                f'title="Copy {esc(f["path"])}">copy</button></div>'
                f'<details open><summary></summary><div class="hunks">'
                f'{"".join(hunks) or "<div class=\"hhead\">no expandable hunk</div>"}'
                f"</div></details></div>"
            )
        sections.append(
            f'  <section class="tier" style="--tc:var(--{tier.lower()})">\n'
            f'    <div class="thead"><span class="tid">{tier}</span>'
            f'<span class="tname">{tiers[tier]["name"]}</span>'
            f'<span class="tcount">{tiers[tier]["lines"]} lines · {len(rows)} '
            f'file{"s" if len(rows) != 1 else ""}</span></div>\n'
            f'    <p class="tblurb">{tiers[tier]["blurb"]}</p>\n'
            f'    {"".join(body)}\n  </section>'
        )

    ledger = "".join(
        f'<div class="lrow"><span class="lid">{t}</span>'
        f'<span class="lname">{tiers[t]["name"]}</span>'
        f'<span class="lblurb">{tiers[t]["blurb"]}</span>'
        f'<span class="lnum">{tiers[t]["lines"]:,}</span></div>'
        for t in ("T4", "T5", "T6")
    )

    sub = f"{churn:,} lines changed across {data['filecount']} files"
    if link:
        sub += f' · <a href="{esc(link)}" target="_blank" rel="noopener">open on GitHub ↗</a>'

    out = tpl
    for key, value in {
        "__TITLE__": esc(title),
        "__EYEBROW__": esc("Review surface" + (f" · {subtitle}" if subtitle else "")),
        "__SUB__": sub,
        "__CHURN__": f"{churn:,}",
        "__PROD__": f"{prod:,}",
        "__PCT__": f"{pct:.1f}",
        "__BAR__": bar,
        "__BARKEY__": barkey,
        "__NOTE__": note,
        "__MOVERS__": mover_html,
        "__TIERS__": "\n".join(sections),
        "__LEDGER__": ledger,
        "__LEDGERCMD__": esc(ledger_cmd),
    }.items():
        if key not in out:
            sys.exit(f"template placeholder {key} missing from {TEMPLATE}")
        out = out.replace(key, value)
    return out


def open_locally(path: pathlib.Path) -> None:
    """Open in the user's own browser. Nothing is ever uploaded.

    `explorer.exe` is not on PATH in this WSL shell, and `wslview`/`xdg-open`
    are not installed, so the absolute interop path is the one that works.
    """
    win = "/mnt/c/Windows/explorer.exe"
    try:
        if pathlib.Path(win).exists():
            winpath = subprocess.run(
                ["wslpath", "-w", str(path.resolve())], capture_output=True, text=True, check=True
            ).stdout.strip()
            subprocess.run([win, winpath], capture_output=True)  # returns nonzero on success
        else:
            subprocess.run(["xdg-open", str(path)], capture_output=True)
    except Exception as exc:  # noqa: BLE001 — opening is a convenience, never fatal
        print(f"could not open a browser ({exc}); the file is ready at {path}", file=sys.stderr)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("pr", nargs="?", type=int, help="pull request number")
    ap.add_argument("--base", help="local base ref instead of a PR (e.g. main)")
    ap.add_argument("--diff-file", help="read a saved unified diff instead of fetching")
    ap.add_argument("--out", help="output path (default out/review-<slug>.html)")
    ap.add_argument("--no-open", action="store_true", help="write the file but do not open it")
    args = ap.parse_args()

    require_repo_root()

    if args.diff_file:
        diff, slug, link = pathlib.Path(args.diff_file).read_text(errors="replace"), "diff", ""
        title, subtitle = "Review surface", pathlib.Path(args.diff_file).name
    elif args.pr:
        diff = fetch_diff(args.pr, None)
        slug = f"pr{args.pr}"
        repo = subprocess.run(
            ["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        head = subprocess.run(
            ["gh", "pr", "view", str(args.pr), "--json", "title", "-q", ".title"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        title, subtitle = f"PR #{args.pr} — {head}", repo
        link = f"https://github.com/{repo}/pull/{args.pr}"
    else:
        base = args.base or "main"
        diff = fetch_diff(None, base)
        slug = "branch"
        branch = subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"], capture_output=True, text=True, check=True
        ).stdout.strip()
        title, subtitle, link = f"{branch} vs {base}", "local branch", ""

    data = classify(diff)
    if not data["files"]:
        sys.exit(
            "the diff parsed but classified nothing — no page written.\n"
            "Either there really are no changes, or the diff format was not understood."
        )
    ledger_cmd = (
        f"git diff main...HEAD -- test/ .claude/" if not args.pr else f"gh pr diff {args.pr} -- test/ .claude/"
    )
    out = pathlib.Path(args.out) if args.out else pathlib.Path("out") / f"review-{slug}.html"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(render(data, title, subtitle, link, ledger_cmd))

    prod = sum(data["tiers"][t]["lines"] for t in PROD)
    total = data["churn"]
    pct = (100 * prod / total) if total else 0
    print(f"{total} lines changed → {prod} production ({pct:.1f}%) across {data['filecount']} files")
    for t in ORDER:
        print(f"  {t} {TIERS[t][0]:<10} {data['tiers'][t]['lines']:>5}")
    print(f"\n{out}")
    if not args.no_open:
        open_locally(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
