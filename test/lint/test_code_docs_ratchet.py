"""Code-docs comment ratchets: over-cap comment lines (prod and test scope), bare IDs, dead pointers.

One test, one census scan, so `-n auto` pays for the tree walk once. Each baseline only
falls: after a cleanup lands, lower all three from the repository root with

    python3 test/lint/test_code_docs_ratchet.py --update

A rise refuses to write; `--allow-regression` accepts it and needs a reviewer.
"""

from __future__ import annotations

import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LENGTH = ROOT / ".code-docs-length.json"
LENGTH_TEST = ROOT / ".code-docs-length-test.json"
LINKAGE = ROOT / ".code-docs-linkage.json"
LINKAGE_CONFIG = ROOT / ".code-docs-linkage-config.json"

# Reader floors, about half of what the tree read when set (2026-09-29): 39307 census blocks
# (22995 test, 14930 prod) over 634 prod files; 40294 linkage blocks over 1035 files. A parser that
# reads nothing passes every ratchet, so each scan must prove it read a tree-sized input.
FLOOR_CENSUS_BLOCKS = 19000
FLOOR_CENSUS_TEST_BLOCKS = 11000
FLOOR_CENSUS_PROD_BLOCKS = 7000
FLOOR_CENSUS_FILES = 300
FLOOR_LINKAGE_BLOCKS = 20000
FLOOR_LINKAGE_FILES = 500

sys.path.insert(0, str(ROOT / ".claude/rules/code-docs/checks"))
_bytecode, sys.dont_write_bytecode = sys.dont_write_bytecode, True
# No __pycache__ beside the vendored checks: their own cleanup check would read it.
import comment_census as cc
import linkage_check as lc

sys.dont_write_bytecode = _bytecode


def _test_scope(sc: cc.Scan) -> cc.Scan:
    # The census ratchet counts only rows labelled prod, so test rows are relabelled. Measured
    # must be every listed file: a file whose test comments all went reads as unmeasured otherwise.
    rows = [
        (rel, "prod", pkg, lib, b)
        for rel, scope, pkg, lib, b in sc.rows
        if scope == "test"
    ]
    measured = {f.relative_to(ROOT).as_posix() for f in cc.list_files(ROOT)}
    return cc.Scan(rows, measured, sc.aggs)


def run(update: bool = False, allow: bool = False) -> list[str]:
    """Every ratchet's failure lines; empty when all hold. With update, writes what may be written."""
    if not update:
        missing = [p.name for p in (LENGTH, LENGTH_TEST, LINKAGE, LINKAGE_CONFIG) if not p.is_file()]
        if missing:
            return [f"missing baseline: {', '.join(missing)}"]
    out: list[str] = []
    sc = cc.scan(ROOT, cc.load_config(ROOT, None))
    test_blocks = sum(1 for r in sc.rows if r[1] == "test")
    for what, got, floor in (
        ("census blocks", len(sc.rows), FLOOR_CENSUS_BLOCKS),
        ("census test blocks", test_blocks, FLOOR_CENSUS_TEST_BLOCKS),
        ("census prod blocks", len(sc.rows) - test_blocks, FLOOR_CENSUS_PROD_BLOCKS),
        ("census prod files", len(sc.measured), FLOOR_CENSUS_FILES),
    ):
        if got < floor:
            out.append(f"reader floor: {what} read {got}, floor {floor}")
    for path, scan in ((LENGTH, sc), (LENGTH_TEST, _test_scope(sc))):
        code, findings, notices = cc.ratchet(ROOT, scan, path, update, allow)
        lines = [
            f"{f['path']}:{f['line']}: {f['rule']} {f['message']}" for f in findings
        ]
        if code:
            out += [f"{path.name}:", *lines, *notices]
        elif update:
            print(path.name, *notices, sep="\n")
    # An explicit path: load_config reads a missing default as {}, which would shrink the families.
    cfg = lc.load_config(ROOT, str(LINKAGE_CONFIG))
    # Both passes walk the same comment blocks; reading the tree once keeps the tier in budget.
    blocks, walk = list(lc.comment_blocks(ROOT)), lc.comment_blocks
    for what, got, floor in (
        ("linkage blocks", len(blocks), FLOOR_LINKAGE_BLOCKS),
        ("linkage files", len({path for path, _ in blocks}), FLOOR_LINKAGE_FILES),
    ):
        if got < floor:
            out.append(f"reader floor: {what} read {got}, floor {floor}")
    lc.comment_blocks = lambda _root: blocks
    try:
        passes = (
            ("ids", lc.scan_ids(ROOT, cfg)[0]),
            ("pointers", lc.scan_pointers(ROOT, cfg)[0]),
        )
    finally:
        lc.comment_blocks = walk
    for cmd, findings in passes:
        counts = Counter(f["key"] for f in findings if f["key"])
        rises, _, risen = lc.ratchet(counts, LINKAGE, lc.OWNED[cmd], update, allow)
        if rises and not (update and allow):
            shown = [
                f"{f['path']}:{f['line']}: {f['rule']} {f['message']}"
                for f in findings
                if f["path"] in risen
            ]
            out += [f"{LINKAGE.name} {cmd}:", *shown, *(f"rise {r}" for r in rises)]
    return out


def test_code_docs_ratchets_hold() -> None:
    failures = run()
    assert not failures, "\n".join(failures)


if __name__ == "__main__":
    args = set(sys.argv[1:])
    if not args <= {"--update", "--allow-regression"} or args == {"--allow-regression"}:
        sys.exit("usage: test_code_docs_ratchet.py [--update [--allow-regression]]")
    failed = run(update="--update" in args, allow="--allow-regression" in args)
    print(*failed, sep="\n")
    sys.exit(1 if failed else 0)
