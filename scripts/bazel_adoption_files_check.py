#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Gate the two WP-00 artifacts — C-001's decision file and C-002's migration plan.

    scripts/bazel_adoption_files_check.py --check
    scripts/bazel_adoption_files_check.py --self-test

Both contracts are shaped by the same failure: a check whose green cannot be
told from its never having run (`quality-core.md` § Unchecked Green, BZL-CORE-02).

**C-001.** Its exit check is `go-no-go.md`'s own, run **verbatim** rather than
re-spelled — the plan records that a weaker spelling greened on an empty middle
cell, mutation-proven. The shipped command is a two-stage shell pipe, and its
pass condition is **empty stdout**, not exit 0: `grep` exits 1 when it matches
nothing, which is exactly the passing state. A gate that tolerated a *range* of
exit codes here would be the cheapest tell of all.

That command alone is vacuous on an empty file — no rows, no empty cells, green.
So C-001 pairs it with a **reader floor**: the file must name all six signals by
identity. Without that, a deleted table and a filled one are the same green.

**C-002.** SKILL.md's own check is `grep -cE '<four alternatives>' ; count >= 4`,
which passes when one required section is missing and another appears twice. The
plan upgrades it to **all five sections present by identity**, so that is what
this asserts. Red-proven one section at a time, five times.

Every mutation in `--self-test` is proven to have landed before its result is
trusted — a mutation that quietly missed is indistinguishable from a check that
cannot red. Mutations run on copies under the repo's own `.tmp/`; the originals
are never touched, and nothing is ever restored with `git checkout`, which
restores from the index and would make the whole run vacuous.
"""

from __future__ import annotations

import argparse
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ARTIFACTS = REPO_ROOT / ".claude" / "artifacts"
DECISION = ARTIFACTS / "decision_bazel_adoption.md"
MIGRATION = ARTIFACTS / "migration_plan_bazel.md"

#: `references/go-no-go.md:207`, verbatim, with `<decision-file>` the one
#: substitution. Kept as a literal so the gate does not depend on the skill
#: bundle at runtime; `--self-test` asserts it is still byte-identical to the
#: line that ships, so a drift in either spelling is loud rather than silent.
GO_NO_GO = REPO_ROOT / ".claude" / "skills" / "bazel-adopt" / "references" / "go-no-go.md"
EXIT_CHECK = "grep -nE '\\|[[:space:]]*\\|' <decision-file> | grep -v -- '---'"

#: C-001's six signal rows, by identity. This list is the reader floor: it is
#: what makes a deleted table distinguishable from a filled one.
SIGNALS = (
    "Cheaper fix tried and named",
    "Median whole-repo CI wall-clock",
    "Largest CI job is a build job",
    "Generator maturity per language",
    "Cross-repo coupling to model",
    "Build owner after adoption",
)

#: C-002's five sections, by identity — not a count, which is the skill's own
#: weaker form and cannot tell a missing section from a duplicated one.
SECTIONS = (
    "### Order",
    "### Pins",
    "### Lock mechanism per language",
    "### First CI lane",
    "### Deferred, with the condition that reopens it",
)


def empty_cell_rows(path: Path) -> list[str]:
    """`go-no-go.md`'s shipped check, run as shipped. Each line is one bad row."""
    command = EXIT_CHECK.replace("<decision-file>", shlex.quote(str(path)))
    # `check=False` is the contract, not a shortcut: the pipe's passing state is
    # `grep` matching nothing, which exits **1**. Reading the exit code here —
    # or tolerating a range of them — would invert the gate.
    result = subprocess.run(["/bin/bash", "-c", command], capture_output=True, text=True, check=False)
    return result.stdout.splitlines()


def check_decision(path: Path = DECISION) -> int:
    """C-001. Exit 0 silent; exit 1 naming what is missing or empty."""
    if not path.is_file():
        print(f"decision file: {path} does not exist — C-001 is unsatisfied", file=sys.stderr)
        return 1
    text = path.read_text(encoding="utf-8")
    if not text.strip():
        print(f"decision file: {path} is empty — C-001 is unsatisfied", file=sys.stderr)
        return 1
    missing = [signal for signal in SIGNALS if signal not in text]
    if missing:
        print(
            f"decision file: names {len(SIGNALS) - len(missing)} of {len(SIGNALS)} signals by "
            f"identity, missing {missing}. This is the reader floor, not a style rule: "
            "`go-no-go.md`'s own check prints nothing for a file with no table in it, so "
            "without this assertion a deleted table and a filled one are the same green.",
            file=sys.stderr,
        )
        return 1
    rows = empty_cell_rows(path)
    if rows:
        print(
            f"decision file: {len(rows)} row(s) carry an empty cell, so the verdict is not "
            "writable yet (`go-no-go.md:210-211`). Measure the cell or delete the row — a "
            "signal recalled, estimated or inferred from a badge is not a decision:",
            file=sys.stderr,
        )
        for row in rows:
            print(f"  {row}", file=sys.stderr)
        return 1
    return 0


def check_migration(path: Path = MIGRATION) -> int:
    """C-002. All five sections by identity, never a count."""
    if not path.is_file():
        print(f"migration plan: {path} does not exist — C-002 is unsatisfied", file=sys.stderr)
        return 1
    lines = path.read_text(encoding="utf-8").splitlines()
    headings = [line.rstrip() for line in lines if line.startswith("### ")]
    missing = [section for section in SECTIONS if section not in headings]
    if missing:
        print(
            f"migration plan: {len(missing)} required section(s) missing or renamed — "
            f"{missing}. All five must be present by identity (C-002); a count >= 4 is "
            "SKILL.md's weaker form and greens when one section is absent and another "
            f"is duplicated. Present: {headings}",
            file=sys.stderr,
        )
        return 1
    return 0


def expect(cond: bool, problem: str) -> None:
    """A failed check is a loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not cond:
        raise SystemExit(f"bazel adoption files self-test: {problem}")


def blank_measured_value(text: str, signal: str) -> str:
    """Empty one signal row's middle cell, leaving `\\|`-escaped pipes alone."""
    pattern = re.compile(rf"^(\| {re.escape(signal)} \|)(?:\\\||[^|\n])*(\|)", re.MULTILINE)
    mutated, count = pattern.subn(r"\1  \2", text)
    expect(count == 1, f"blanking {signal!r} matched {count} row(s), expected exactly 1")
    return mutated


def drop_section(text: str, heading: str) -> str:
    """Delete a `###` section — its heading and its body up to the next one."""
    pattern = re.compile(rf"^{re.escape(heading)}$.*?(?=^### |\Z)", re.MULTILINE | re.DOTALL)
    mutated, count = pattern.subn("", text)
    expect(count == 1, f"dropping {heading!r} matched {count} section(s), expected exactly 1")
    return mutated


def self_test() -> int:
    """Both red/green pairs, on copies, with every mutation proven to have landed."""
    checks = 0
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)

    # The shipped spelling of C-001's exit check has not drifted from ours.
    if GO_NO_GO.is_file():
        shipped = GO_NO_GO.read_text(encoding="utf-8").splitlines()
        expect(
            EXIT_CHECK in shipped,
            f"EXIT_CHECK is no longer a verbatim line of {GO_NO_GO} — re-copy it, never re-spell it",
        )
        print("  C-001 exit check: verbatim from go-no-go.md, byte-identical")
        checks += 1
    else:
        print(f"  C-001 exit check: {GO_NO_GO} absent, verbatim-equality not asserted this run")

    original_decision = DECISION.read_text(encoding="utf-8")
    original_migration = MIGRATION.read_text(encoding="utf-8")

    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)

        # --- C-001, green half first, so the red half is a change from a known state.
        decision = work / "decision_bazel_adoption.md"
        decision.write_text(original_decision, encoding="utf-8")
        expect(check_decision(decision) == 0, "the shipped decision file must pass C-001")
        expect(empty_cell_rows(decision) == [], "the shipped decision file must print no rows")
        print("  C-001 GREEN: the shipped decision file, check prints nothing")
        checks += 1

        # --- C-001, red half: blank each signal row's middle cell in turn.
        for signal in SIGNALS:
            mutated = blank_measured_value(original_decision, signal)
            decision.write_text(mutated, encoding="utf-8")
            landed = decision.read_text(encoding="utf-8")
            # Prove the mutation landed in the file the check actually reads.
            expect(f"| {signal} |  |" in landed, f"mutation for {signal!r} did not land on disk")
            expect(landed != original_decision, f"mutation for {signal!r} was a no-op")
            rows = empty_cell_rows(decision)
            expect(len(rows) == 1, f"blanking {signal!r} printed {len(rows)} row(s), expected 1")
            expect(signal in rows[0], f"the printed row does not name {signal!r}: {rows[0]}")
            expect(check_decision(decision) == 1, f"blanking {signal!r} must exit 1")
            print(f"  C-001 RED  : {rows[0].strip()}")
            checks += 1
            # Restore from our own saved bytes — never `git checkout --`, which
            # restores from the index and would make this whole run vacuous.
            decision.write_text(original_decision, encoding="utf-8")
            expect(
                decision.read_text(encoding="utf-8") == original_decision,
                f"restore after {signal!r} did not land",
            )
            expect(check_decision(decision) == 0, f"restoring after {signal!r} must green again")

        # --- C-001's reader floor: the grep alone is green on a file with no table.
        decision.write_text("# Bazel adoption decision\n", encoding="utf-8")
        expect(empty_cell_rows(decision) == [], "the shipped grep is green on a table-less file")
        expect(check_decision(decision) == 1, "the reader floor must red on a table-less file")
        print("  C-001 RED  : a decision file with no table — the grep alone greens on it")
        checks += 1
        decision.write_text(original_decision, encoding="utf-8")
        expect(check_decision(decision) == 0, "restoring the decision file must green again")

        # --- C-002, green half.
        migration = work / "migration_plan_bazel.md"
        migration.write_text(original_migration, encoding="utf-8")
        expect(check_migration(migration) == 0, "the shipped migration plan must pass C-002")
        print("  C-002 GREEN: the shipped migration plan, all five sections by identity")
        checks += 1

        # --- C-002, red half: delete each of the five sections in turn.
        for heading in SECTIONS:
            mutated = drop_section(original_migration, heading)
            migration.write_text(mutated, encoding="utf-8")
            landed = migration.read_text(encoding="utf-8")
            expect(
                heading not in landed.splitlines(),
                f"deletion of {heading!r} did not land on disk",
            )
            expect(len(landed) < len(original_migration), f"deleting {heading!r} was a no-op")
            expect(check_migration(migration) == 1, f"deleting {heading!r} must exit 1")
            print(f"  C-002 RED  : missing or renamed — {heading!r}")
            checks += 1
            migration.write_text(original_migration, encoding="utf-8")
            expect(
                migration.read_text(encoding="utf-8") == original_migration,
                f"restore after {heading!r} did not land",
            )
            expect(check_migration(migration) == 0, f"restoring after {heading!r} must green again")

    # The originals were never written to; say so with a read, not with a claim.
    expect(
        DECISION.read_text(encoding="utf-8") == original_decision
        and MIGRATION.read_text(encoding="utf-8") == original_migration,
        "the self-test modified an artifact in place — it must only ever touch copies",
    )
    print(f"bazel adoption files self-test: {checks} checks passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="gate the two shipped artifacts")
    mode.add_argument("--self-test", action="store_true", help="prove both gates red and green")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    return check_decision() or check_migration()


if __name__ == "__main__":
    raise SystemExit(main())
