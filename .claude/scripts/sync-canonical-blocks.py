#!/usr/bin/env python3
"""Propagate the canonical Review-Fix Loop block from `workflow-swarm.md`
to `workflow-bugfix.md` and `workflow-refactor.md`.

Compression (e.g. `caveman:compress`) rewrites prose independently per file
and breaks the byte-identity contract enforced by
`adr_ai_config_review_loop_dedup.md` and `test_review_fix_loop_parity`.

Run this after any pass that may have touched one of the three carriers.
"""

from pathlib import Path
import sys

BEGIN = "<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->"
END = "<!-- REVIEW_FIX_LOOP_CANONICAL_END -->"

REPO = Path(__file__).resolve().parents[2]
SOURCE = REPO / ".claude/rules/workflow-swarm.md"
TARGETS = [
    REPO / ".claude/rules/workflow-bugfix.md",
    REPO / ".claude/rules/workflow-refactor.md",
]


def extract_block(text: str, path: Path) -> str:
    if BEGIN not in text or END not in text:
        sys.exit(f"missing canonical markers in {path}")
    b = text.index(BEGIN)
    e = text.index(END) + len(END)
    return text[b:e]


def main() -> int:
    src_text = SOURCE.read_text()
    canonical = extract_block(src_text, SOURCE)
    changed = 0
    for target in TARGETS:
        text = target.read_text()
        existing = extract_block(text, target)
        if existing == canonical:
            continue
        b = text.index(BEGIN)
        e = text.index(END) + len(END)
        target.write_text(text[:b] + canonical + text[e:])
        print(f"synced: {target.relative_to(REPO)}")
        changed += 1
    if changed == 0:
        print("already in sync")
    return 0


if __name__ == "__main__":
    sys.exit(main())
