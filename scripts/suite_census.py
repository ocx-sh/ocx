#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Pin the shape of the acceptance suite, so a `passed` that moved for the
right reason cannot cover one that moved for the wrong reason (DEC-38).

    scripts/suite_census.py --check
    scripts/suite_census.py --print
    scripts/suite_census.py --self-test

The crate split's acceptance invariant is that `skipped` and `xfailed` never
move and no test is added, removed, renamed or re-marked. `passed` *does* move:
`test/tests/test_logging.py` parametrizes over `LIBRARY_TARGETS`, so it went
3643 -> 3645 in batch B5 as `ocx_util` and `ocx_console` became library
targets, and twelve library targets remain.

That is the whole problem. A `+2` from a parametrized structural census
gaining two targets and a `+2` from two new behavioural tests are
**byte-identical in the summary line**. Until this script, the only thing
separating them was a census one reviewer chose to run once, by hand — an
invariant that depends on somebody remembering is the same shape as a check
whose green cannot be told from never having run. Twelve targets left is
twelve chances to not remember.

So the four numbers are pinned here and compared on every full verify. A
behavioural test smuggled in under cover of a census bump moves `TESTS` and
reds; reconciling it — in `plan_crate_split_workspace.md`, in the commit that
causes it — is what turns it green again.

**Scope.** Every `.py` under `test/` whose path contains no dot-prefixed
directory. That exclusion is load-bearing rather than tidy: `test/.venv` holds
1069 files and counting them reports 3409 tests, so a walk that took the whole
tree would be measuring uv's virtualenv and would move whenever a dependency
did. The self-test proves the exclusion by planting a file under a dot
directory and asserting the walk does not see it.

**Definitions**, stated because a count nobody can reproduce is not evidence:

- `TESTS` — `def`/`async def` whose name starts with `test_`, anywhere in the
  module (a method of a `Test*` class included).
- `SKIPPED` — decorators on a function or a class whose text contains
  `mark.skip`, which is both `skip` and `skipif`.
- `XFAILED` — decorators whose text contains `xfail`.
- `PARAMETRIZED` — decorators whose text contains `parametrize`.

Decorators only. A `pytest.skip()` inside a body is a runtime decision the
suite's own `skipped` count already ratchets through `test/SKIP_CEILING`, and
a module-level `pytestmark` applies to a whole file rather than to a test.

**The unit is the decorator, not the thing decorated.** The distinction is
invisible until a function carries two of the same mark, and then it is the
whole count: per-decorator this tree is 105 / 150, per-node it is 104 / 144.
Seven functions account for both gaps. Six carry two `@parametrize` each --
`test_exec_modes.py::test_all_surfaces_carry_self_flag`,
`test_shell_reconcile.py::test_shell_state_output_is_never_eval_able`,
`test_toolchain_activate.py::test_the_activate_pinned_matrix_emits_the_contracted_set`,
and three in `test_sign_platforms.py`
(`test_push_sign_writes_one_signature_per_platform_then_verify_per_platform`,
`test_sign_tags_file_over_an_index_then_verify_on_the_tag`,
`test_sign_dash_p_narrows_to_that_platform`) -- so 150 - 144 = 6. One carries
two skip decorators, `test_assembly.py::test_shared_layer_disk_usage_is_not_doubled`,
so 105 - 104 = 1.

Per-decorator is the unit this instrument needs, and not as a tiebreak:
**adding a second `parametrize` to an existing function is a census change
that a per-node count cannot see.** It multiplies the collected total while
leaving every per-node figure identical -- which is exactly the shape DEC-38
exists to close, a `passed` that moved for a reason the census does not
record. Per-node answers "how many tests carry this mark"; only per-decorator
answers "how many ways is the suite's shape declared".

`review-b5r` ran an independent census at `a6296b8a` and got 2857 / 115 / 4 /
150. Three of the four agree exactly with this script, which is the
corroboration that matters: the scope and the `test_`, `xfail` and
`parametrize` rules are the same two ways round. Its 115 does not reproduce
under any decorator rule recoverable from the report -- function decorators
give 105, adding module-level `pytestmark` gives 127, and `marks=` inside
`parametrize` params adds none -- so 105 is pinned under the definition above
rather than a number this script cannot produce. The 104 / 144 pair is a
different thing and reproduces exactly: one census under two stated rules, one
decorator apart.
"""

from __future__ import annotations

import argparse
import ast
import sys
import tempfile
from collections import Counter
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SUITE = REPO_ROOT / "test"

#: The census as of the ocx#477–#494 batch: `tests` 2859 -> 2904 with the
#: batch's 45 rows, `skipped` 108 -> 110 with two more POSIX-only gates
#: (`flock(2)` advisory contract, SIGKILL mid-add). 2904 -> 2906 with the two
#: rows of `lint/test_patch_global_slot.py`, which keep every patch tier off
#: the bare registry's `global` slot outside its xdist group. 2906 -> 2915
#: with the eight rows `tests/test_exec_forwarding.py` gains for the bare
#: `--env NAME` pass-through and for the four ambient-only resolution knobs
#: a `--clean` child now inherits, plus the project-tier pass-through row
#: added beside the retargeted refusal row in `tests/test_project_env.py`.
#: 2927 / 151 as the tree stood before C-020 (the floors had not been raised
#: past 2915 / 150); 2922 / 150 once `tests/test_schema_generation.py` (five
#: test functions, one `parametrize`) was ported into
#: `crates/ocx_schema/tests/schema_outputs.rs` and deleted
#: (plan_test_speed_tiers.md C-020). 2885 / 147 once the WP-13 pilot ported
#: 37 test functions (three of them parametrized) out of `test_status.py`,
#: `test_config_test.py` and `test_platform_pairs.py` into `ocx_cli` and
#: `ocx_package` crate tests and deleted them (C-025).
PINNED = {"tests": 2885, "skipped": 110, "xfailed": 4, "parametrized": 147}

#: Which way each number may move on its own. DEC-38 pinned all four to
#: equality for the duration of the crate split, so that a refactor could not
#: change the suite underneath itself. That split has landed, and equality had
#: outlived it: it refused a test *added*, which is ordinary work.
#:
#: What it is still worth refusing is a test going away or going quiet —
#: deleting, skipping or xfailing a test is how a red is silenced, and the
#: summary line cannot tell that from honest work. So: counts that should only
#: grow are floors, counts that should only shrink are ceilings, and a
#: deliberate move is recorded by editing PINNED in the same commit.
DIRECTION = {"tests": "floor", "parametrized": "floor", "skipped": "ceiling", "xfailed": "ceiling"}

#: A floor on the walk itself. A scope that stopped matching reports zero of
#: everything, which looks like a suite that vanished rather than like a
#: scanner that did — and the `--check` below would then red for the wrong
#: reason, or a future `>=` reading of it would not red at all.
MIN_FILES = 100


def census(suite: Path = SUITE) -> tuple[Counter[str], list[Path]]:
    """The four counts, and the files they were taken over."""
    files = [
        path
        for path in sorted(suite.rglob("*.py"))
        if not any(part.startswith(".") for part in path.relative_to(suite).parts)
    ]
    counts: Counter[str] = Counter({key: 0 for key in PINNED})
    for path in files:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                continue
            if not isinstance(node, ast.ClassDef) and node.name.startswith("test_"):
                counts["tests"] += 1
            for decorator in node.decorator_list:
                text = ast.unparse(decorator)
                if "mark.skip" in text:
                    counts["skipped"] += 1
                if "xfail" in text:
                    counts["xfailed"] += 1
                if "parametrize" in text:
                    counts["parametrized"] += 1
    return counts, files


def check(suite: Path = SUITE, pinned: dict[str, int] | None = None) -> int:
    pinned = PINNED if pinned is None else pinned
    counts, files = census(suite)
    if len(files) < MIN_FILES:
        print(
            f"suite census: walked {len(files)} .py file(s) under {suite} — the scope stopped "
            f"matching, and a census over nothing is not a census",
            file=sys.stderr,
        )
        return 1
    moved = {
        key: (pinned[key], counts[key])
        for key in pinned
        if (counts[key] < pinned[key] if DIRECTION[key] == "floor" else counts[key] > pinned[key])
    }
    if moved:
        print(
            f"suite census: {len(moved)} of {len(pinned)} number(s) moved the wrong way over "
            f"{len(files)} file(s) — {{key: (pinned, live)}} {moved}.\n"
            "`tests` and `parametrized` are floors: the suite may grow, never shrink. `skipped` "
            "and `xfailed` are ceilings: a test may be un-skipped, never quietly skipped. A move "
            "that is the work, not a silenced red, is recorded by editing PINNED in this file in "
            "the same commit that causes it.",
            file=sys.stderr,
        )
        return 1
    print(f"suite census: {dict(sorted(counts.items()))} over {len(files)} file(s), as pinned")
    return 0


def expect(cond: bool, problem: str) -> None:
    """A failed check is a loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not cond:
        raise SystemExit(f"suite census self-test: {problem}")


def self_test() -> int:
    checks = 0
    with tempfile.TemporaryDirectory() as directory:
        suite = Path(directory)
        (suite / "tests").mkdir()
        (suite / "tests" / "test_a.py").write_text(
            "import pytest\n"
            "\n"
            "@pytest.mark.parametrize('n', [1, 2])\n"
            "def test_one(n):\n"
            "    assert n\n"
            "\n"
            "@pytest.mark.skipif(True, reason='x')\n"
            "def test_two():\n"
            "    pass\n"
            "\n"
            "@pytest.mark.xfail\n"
            "async def test_three():\n"
            "    pass\n"
            "\n"
            "class TestGroup:\n"
            "    def test_method(self):\n"
            "        pass\n"
            "\n"
            "def helper():\n"
            "    pytest.skip('runtime')\n",
            encoding="utf-8",
        )
        counts, files = census(suite)
        expect(
            counts == Counter({"tests": 4, "skipped": 1, "xfailed": 1, "parametrized": 1}),
            f"the four definitions do not hold on an owned module: {dict(counts)}",
        )
        expect(len(files) == 1, f"the walk found {len(files)} file(s), not 1")
        checks += 1

        # THE EXCLUSION, shown rather than asserted in prose: `test/.venv` holds
        # more than four times as many `.py` files as the suite does, so a walk
        # that took the whole tree would be counting uv's virtualenv and would
        # move whenever a dependency did.
        (suite / ".venv").mkdir()
        (suite / ".venv" / "test_vendored.py").write_text(
            "def test_from_a_dependency():\n    pass\n", encoding="utf-8"
        )
        after, files_after = census(suite)
        expect(
            after == counts and len(files_after) == len(files),
            f"a file under a dot directory entered the census: {dict(after)} over {len(files_after)} file(s)",
        )
        checks += 1

        # `check` red and green against a pinned set it owns, and the floor that
        # stops an empty scope from reading as a clean suite.
        pinned = dict(counts)
        expect(check(suite, pinned) == 1, "a suite under MIN_FILES must red however well it matches")
        for index in range(MIN_FILES):
            (suite / "tests" / f"test_pad_{index}.py").write_text("def test_pad():\n    pass\n", encoding="utf-8")
        pinned["tests"] += MIN_FILES
        expect(check(suite, pinned) == 0, "a matching census over a real-sized suite must pass")
        (suite / "tests" / "test_smuggled.py").write_text(
            "def test_added_behaviour():\n    assert True\n", encoding="utf-8"
        )
        expect(check(suite, pinned) == 0, "a test ADDED is ordinary work and must pass the floor")
        # The floor bites the other way: the suite may never shrink.
        (suite / "tests" / "test_smuggled.py").unlink()
        (suite / "tests" / "test_pad_0.py").unlink()
        expect(check(suite, pinned) == 1, "a test removed must red — `tests` is a floor")
        (suite / "tests" / "test_pad_0.py").write_text("def test_pad():\n    pass\n", encoding="utf-8")
        expect(check(suite, pinned) == 0, "restoring it must green again")
        # And the ceiling catches the thing the floor cannot: a test that is
        # still there and no longer runs.
        (suite / "tests" / "test_smuggled.py").write_text(
            "import pytest\n\n@pytest.mark.skip\ndef test_added_behaviour():\n    assert True\n",
            encoding="utf-8",
        )
        expect(check(suite, pinned) == 1, "a newly skipped test must red on `skipped` — the ceiling")
        checks += 1
    print(f"suite census self-test: {checks} checks passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="fail when any of the four numbers moved")
    mode.add_argument("--print", action="store_true", help="print the live census, for recording a move")
    mode.add_argument("--self-test", action="store_true", help="prove the definitions and the gate inline")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.print:
        counts, files = census()
        print(f"{dict(sorted(counts.items()))} over {len(files)} file(s)")
        return 0
    return check()


if __name__ == "__main__":
    raise SystemExit(main())
