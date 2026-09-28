#!/usr/bin/env python3
"""Seven-rule lexical guard recogniser for the code-docs rule set.

Rules covered:
  GRD-06  the recogniser is a carve-out, never a gate. A block it flags is
          never auto-cut. A block it misses is unclassified, never safe to cut.

The seven rules: a SAFETY label, a
comparative plus a consequence, a prohibition plus a consequence or condition,
a choke point, a sentinel plus a consequence, a hedge marker, and a causal or
conditional connector plus a consequence. classify() takes comment text with
the markers stripped and returns whether any rule fires and which.

Recall is about 0.43, so a miss proves nothing. The self-test pins this
code's own counts on fixtures/guard_recogniser/labelled_blocks.jsonl: 320
blocks from agent-written codebases, 100 of them labelled guards by hand
(precision 0.652, recall 0.430). On 200 human-written blocks from eight
pre-2022 open-source repos it measured precision 0.391 and recall 0.231; that
corpus is third-party text and is not shipped. Any rule change moves the
counts, and the self-test then fails until the new counts are pinned.

Usage:
  guard_recogniser.py TEXT...            classify text (or stdin when no TEXT)
  guard_recogniser.py --root DIR         list prose blocks that fire, per file
  guard_recogniser.py --self-test

Options: --format text|json.

Exit codes: 0 success (a fire is never a failure), 2 usage or missing input.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

CONSEQUENCE = re.compile(
    r"\b(would|will|breaks?|silently|panics?|leaks?|crash(?:es)?|hangs?|"
    r"deadlocks?|corrupts?|corruption|fails?|failing|wrong|incorrect|"
    r"loses?|losing|expos(?:e|es|ed|ing)|unsound|undefined behavi\w*|ub|"
    r"races?|drops?|dropped|escapes?|stale|torn|truncat\w*|exit code|"
    r"misreads?|misdetects?|misroutes?|invalid|unrecoverable|never resolve|"
    r"no recovery)\b",
    re.IGNORECASE,
)
CONDITIONAL = re.compile(
    r"\b(if|when|unless|until|otherwise|only if|requires?|relies? on|depends? on|assumes?)\b",
    re.IGNORECASE,
)
COMPARATIVE = re.compile(r"\b(instead of|rather than|in place of)\b", re.IGNORECASE)
PROHIBITION = re.compile(
    r"\b(never|must not|do not|don't|cannot|can't|not\s+\w+ing)\b", re.IGNORECASE
)
CHOKE_POINT = re.compile(
    r"\b(single|one)\s+(choke\s*point|source of truth|place|spelling)\b"
    r"|shared so\b|the only (place|mechanism|way)\b",
    re.IGNORECASE,
)
SENTINEL = re.compile(r"\bsentinel\b|none means|-1 means|tri-state", re.IGNORECASE)
HEDGE = re.compile(r"\bcaution\b|should never happen|\bn\.b\.", re.IGNORECASE)
CAUSAL = re.compile(r"\bso that\b|\bbecause\b|\bsince\b", re.IGNORECASE)

RULES = {
    "safety-label": lambda t: bool(re.search(r"safety\s*:", t, re.IGNORECASE)),
    "comparative+consequence": lambda t: bool(COMPARATIVE.search(t) and CONSEQUENCE.search(t)),
    "prohibition+consequence": lambda t: bool(
        PROHIBITION.search(t) and (CONSEQUENCE.search(t) or CONDITIONAL.search(t))
    ),
    "choke-point": lambda t: bool(CHOKE_POINT.search(t)),
    "sentinel+consequence": lambda t: bool(SENTINEL.search(t) and CONSEQUENCE.search(t)),
    "hedge-marker": lambda t: bool(HEDGE.search(t)),
    "causal+consequence": lambda t: bool(
        (CAUSAL.search(t) or CONDITIONAL.search(t)) and CONSEQUENCE.search(t)
    ),
}


def classify(text: str) -> tuple[bool, list[str]]:
    """Return (fires, names of the rules that fired) for comment text."""
    reasons = [name for name, rule in RULES.items() if rule(text)]
    return bool(reasons), reasons


# --- self-test ----------------------------------------------------------------

FIXTURE = (
    Path(__file__).resolve().parent / "fixtures" / "guard_recogniser" / "labelled_blocks.jsonl"
)
# Measured 2026-09-27: precision 0.652 and recall 0.430 on the 320 blocks.
PINNED = {"agent": (43, 23, 57, 197)}  # TP, FP, FN, TN
ONE_PER_RULE = {
    "safety-label": "SAFETY: the pointer is valid for the whole call.",
    "comparative+consequence": "Hash the bytes instead of the name, or two files silently collide.",
    "prohibition+consequence": "This must not run until the lock is taken.",
    "choke-point": "This is the single source of truth for the cache path.",
    "sentinel+consequence": "-1 means unset; reading it as a size would corrupt the count.",
    "hedge-marker": "N.B. this branch should never happen.",
    "causal+consequence": "Sorted so that the output stays stable; unsorted, snapshots break.",
}
QUIET = ("Returns the digest for this manifest.", "Lists are always merged.")


def score(rows: list[dict]) -> tuple[int, int, int, int]:
    tp = fp = fn = tn = 0
    for r in rows:
        hit = classify(r["text"])[0]
        tp += hit and r["guard"]
        fp += hit and not r["guard"]
        fn += (not hit) and r["guard"]
        tn += (not hit) and not r["guard"]
    return tp, fp, fn, tn


def self_test() -> int:
    fails = []
    for name, text in ONE_PER_RULE.items():
        if name not in classify(text)[1]:
            fails.append(f"rule {name} did not fire on its own example")
    fails += [f"fired on a plain contract line: {t!r}" for t in QUIET if classify(t)[0]]
    rows = [json.loads(x) for x in FIXTURE.read_text(encoding="utf-8").splitlines() if x.strip()]
    for pop, want in PINNED.items():
        got = score([r for r in rows if r["pop"] == pop])
        tp, fp, fn, _ = got
        print(
            f"{pop}: TP={got[0]} FP={got[1]} FN={got[2]} TN={got[3]} precision={tp / (tp + fp):.3f} recall={tp / (tp + fn):.3f}"
        )
        if got != want:
            fails.append(f"{pop} counts {got} differ from pinned {want}")
    for f in fails:
        print("FAIL", f)
    print("self-test:", "ok" if not fails else f"{len(fails)} failures")
    return 1 if fails else 0


# --- cli ----------------------------------------------------------------------


def scan(root: Path) -> list[dict]:
    """Every prod prose block under root that fires at least one rule."""
    sys.dont_write_bytecode = True  # no __pycache__ beside an installed copy
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import comment_census as cc  # noqa: PLC0415 - only the --root mode needs the census

    out = []
    for f in cc.list_files(root):
        rel = f.relative_to(root)
        if cc.scope_of(rel) != "prod":
            continue
        lines = cc.classify(
            rel.as_posix(), f.read_text(encoding="utf-8", errors="replace"), cc.EXT_LANG[f.suffix]
        )
        for _kind, start, n in cc.blocks_of(lines):
            if lines[start].test:
                continue
            fires, reasons = classify("\n".join(lines[i].text for i in range(start, start + n)))
            if fires:
                out.append({"path": rel.as_posix(), "line": start + 1, "len": n, "rules": reasons})
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("text", nargs="*")
    ap.add_argument("--root")
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    if a.root:
        root = Path(a.root).resolve()
        if not root.is_dir():
            print(f"missing input: {root}", file=sys.stderr)
            return 2
        hits = scan(root)
        if a.format == "json":
            print(json.dumps(hits, indent=1))
        else:
            for h in hits:
                print(
                    f"{h['path']}:{h['line']}: GRD-06 carve-out x{h['len']} ({', '.join(h['rules'])})"
                )
        return 0
    text = " ".join(a.text) if a.text or sys.stdin.isatty() else sys.stdin.read()
    if not text.strip():
        print("no text given", file=sys.stderr)
        return 2
    fires, reasons = classify(text)
    print(
        json.dumps({"fires": fires, "rules": reasons})
        if a.format == "json"
        else ("FIRE " + ", ".join(reasons) if fires else "quiet")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
