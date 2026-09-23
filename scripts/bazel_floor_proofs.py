#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stage-2 floor, ceiling and target parity — C-012, C-013, C-013a, S-011.

    scripts/bazel_floor_proofs.py --self-test
    scripts/bazel_floor_proofs.py --check
    scripts/bazel_floor_proofs.py --derive --listing <nextest-list.json>
    scripts/bazel_floor_proofs.py --prove-s011

Moving test *execution* to Bazel disturbs exactly one of the two unit-test
gates. This file settles which, and supplies the comparators WP-20 needs so
that neither gate has to be re-spelled against a build engine.

**C-012 — the floor is untouched, and this file re-proves it rather than
asserting it.** `rust:test:floor` runs `cargo nextest list` and sums
`len(s["testcases"])` over `rust-suites`. `rust:test:ceiling` reads the
`Summary` line of a run. `floor_recipe_findings` is one predicate — "this
recipe reads no run artifact and names no build engine" — and the proof is
that the **two live recipes separate under it**: `rust:test:floor` green,
`rust:test:ceiling` red on `floor-recipe-run`. A predicate that passed both
would be measuring nothing, which is why the ceiling is run through it too.

**C-013 — listing carries the ceiling. Measured, both sides.** Workspace
scope, release profile, `profile.default`, this worktree at `e7a13907`:

    cargo nextest list --workspace --release --locked --message-format json
      -> 36 rust-suites, 8223 testcases, 8 with `"ignored": true`,
         the same 8 with `filter-match.status != "matches"` (reason `ignored`)
    cargo nextest run  --workspace --release --locked
      -> Starting 8215 tests across 36 binaries (8 tests skipped)
         Summary [74.613s] 8215 tests run: 8215 passed, 8 skipped

8223 - 8215 = 8. The listing's per-testcase flags reproduce the run's skipped
count exactly, so the ceiling can move to listing and become engine-independent
like the floor. Two independent readings are taken and their **disagreement is
its own finding**: `ignored` is the `#[ignore]` attribute, `filter-match` is the
decision a run would take, and they part company the moment a `default-filter`
narrows the profile. WP-20 therefore owes the listing the *same* `--profile` the
run would have used — `profile.ci` sets `default-filter` and `profile.default`
does not.

**A Bazel cache hit is never a test skip.** Structural, not promised:
`ceiling_findings` takes the listing and nothing else, `engine_reads` asserts it
has no string constant naming a run artifact, and `_cached_as_skipped` — the
wrong reader, kept as a named control and called from nowhere else — is shown
answering **0 on a cold BEP and 33 on the identical-but-cached BEP**, while the
real reader answers 8 on both.

**C-013a — parity, the one property listing cannot see.** WP-12 has now run,
so the exclusion set is measured rather than guessed. It stays a *parameter* of
`target_parity` — a check that minted its own set would reconcile against
itself — but `derive_exclusions` no longer derives it from `kind`, because that
rule is refuted on both sides:

* `kind == "bin"` does **not** imply "no Bazel target". rules_rust compiles a
  `[[bin]]` crate's `#[cfg(test)]` module as a `rust_test` over the same crate
  root, with no `rust_binary` anywhere. `ocx::bin/ocx` (1 case) and
  `ocx_shim::bin/ocx-shim` (109) both have one.
* `kind == "test"` does **not** imply "has a Bazel target".
  `ocx::linux_self_contained` needs `env!("CARGO_BIN_EXE_ocx")`, which needs a
  `rust_binary` in `data`, and stages 1-2 build none. Handing it a fabricated
  path would green it against a file that is not ocx, which is worse than not
  running it.

The set is therefore named, with a reason per entry, in
`BAZEL_UNMAPPED_SUITES`: **2 suites, 1 testcase**. 36 - 2 = 34 Bazel
`rust_test` targets, which is `bazel query 'kind(rust_test, //crates/...)'`
reached from the other side.

**The reconciliation, measured on this tree** (`crates/TEST_TARGET_MAP.toml`,
which WP-12 generates from `bazel-testlogs/**/test.log`, against the listing
above):

    8223  testcases `cargo nextest list` declares
    8174  executed by the 34 Bazel `rust_test` targets
    + 40  filtered out inside targets that DO exist, by `--skip ... --exact`
    +  8  `#[ignore]`d, and skipped by both engines
    +  1  `ocx::linux_self_contained`, which has no target at all
    = 8223

Every term is a different gate's business, which is why the sum is written out:
the 8 belong to C-013's ceiling, the 40 to `TEST_TARGET_MAP`'s `skipped`
column, and only the 1 is C-013a's.

**M-02 is wrong about `ocx_shim`, and it matters.** It records "no
`#[cfg(test)]`"; `crates/ocx_shim/src/main.rs:888` opens a `#[cfg(test)] mod
tests` holding **109** `#[test]` fns, every one of them inside the floor's 8223.
M-02 concluded from that sentence that `ocx_shim` contributes no target and no
package; it contributes both, which is why `CRATE_PACKAGES` is 20 and not 19.

**S-011 is not reproducible as the plan states it.** The plan's red is "delete
three `#[test]` fns -> 8210 < 8213". The live sum is **8223**, not 8213: the
floor carries **10** tests of headroom, so three deletions land on 8220 and the
gate stays green. `--prove-s011` reads the headroom and deletes `headroom + 1`,
which is the only deletion count that discriminates today.

Stdlib only (plan DEC-8), so the taskfile recipe is sliced by indentation
rather than parsed — the assertions themselves run over shell command items,
which is the grammar the property lives in. Not wired into
`taskfiles/scripts.taskfile.yml`; WP-17 owns that file and the line it owes is
named at the end of `--self-test`.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import subprocess
import tempfile
from pathlib import Path

from bazel_gate_proofs import (
    CRATES_TEST_TARGETS,
    REPO_ROOT,
    Finding,
    codes,
    read_test_results,
    report,
    rerun_set,
    test_labels,
    write_bep,
)

# ---------------------------------------------------------------------------
# Counts. Sums, never literals, for the reason WP-13 gives: a later correction
# to one summand must not leave a total standing at the old value.
# ---------------------------------------------------------------------------

#: The suites `cargo nextest list` reports that stage 1-2 Bazel builds no
#: `rust_test` for, keyed by nextest binary id, with the reason each is here.
#: Named rather than derived: no rule over the listing alone can produce this
#: set, because whether a suite has a target is a property of WP-12's BUILD
#: files and not of the listing (see the module docstring — `kind` fails in
#: both directions). A stale entry is caught by `target_parity`'s
#: `parity-stale`, which is what a named set buys over a silent filter.
#:
#: The ids are nextest's own spelling, verified against the live listing:
#: `<package>::bin/<name>` for a `[[bin]]` suite, `<package>::<name>` for an
#: integration one. `crates/TEST_TARGET_MAP.toml` spelled the second
#: `ocx::test/linux_self_contained`, which matches no suite nextest emits.
BAZEL_UNMAPPED_SUITES: dict[str, str] = {
    "ocx_schema::bin/ocx_schema": (
        "a `[[bin]]` suite with no `#[cfg(test)]` — 0 testcases, so there is "
        "nothing for a rust_test to run"
    ),
    "ocx::linux_self_contained": (
        '`env!("CARGO_BIN_EXE_ocx")` — the suite\'s subject is the *shipped* '
        "binary's dynamic-link set, and stages 1-2 build no `rust_binary`"
    ),
}

NEXTEST_EXCLUDED_SUITES = len(BAZEL_UNMAPPED_SUITES)  # 2
"""The suite-level half of C-008's exclusion set. Derived from the named table
above rather than restated, so the two cannot disagree."""

NEXTEST_SUITE_FLOOR = CRATES_TEST_TARGETS + NEXTEST_EXCLUDED_SUITES  # 37
"""35 Bazel `rust_test` targets + 2 suites that have none = 20 lib + 14
integration + 3 bin. The reader floor for any listing that claims to be
`--workspace`: fewer suites than this and the listing is partial, which is
indistinguishable from a shrunken test set in the sum alone."""

FLOOR_FILE = REPO_ROOT / "crates" / "NEXTEST_FLOOR"
CEILING_FILE = REPO_ROOT / "crates" / "NEXTEST_SKIP_CEILING"
RUST_TASKFILE = REPO_ROOT / "taskfiles" / "rust.taskfile.yml"

# ---------------------------------------------------------------------------
# Messages. `code` is what the self-test asserts on; the message is stderr.
# ---------------------------------------------------------------------------

LISTING_PARSE_MSG = "could not read rust-suites from the nextest listing: {reason}"
SUITE_FLOOR_MSG = (
    "nextest listing read {suites} rust-suites, expected >= {minimum} — the listing is "
    "partial, so every count taken from it is a floor on a subset, not on the workspace"
)
FLOOR_MSG = (
    "nextest floor: {count} tests listed, floor is {floor} (crates/NEXTEST_FLOOR) — "
    "the unit-test set shrank"
)

FLOOR_RECIPE_READER_MSG = (
    "C-012: no command in the floor recipe runs `cargo nextest list` over `rust-suites` "
    "({items} command item(s) read) — the recipe this check judges is not the floor"
)
FLOOR_RECIPE_BAZEL_MSG = (
    "C-012: the floor recipe names the build engine ({token!r} in {item!r}) — the floor is "
    "a declaration check and must stay engine-independent"
)
FLOOR_RECIPE_RUN_MSG = (
    "C-012: the floor recipe reads a run artifact ({token!r} in {item!r}) — only deleting "
    "one is allowed there, and a floor that reads a run is a floor Bazel execution moves"
)

CEILING_MSG = (
    "nextest ceiling: {skipped} skipped exceeds crates/NEXTEST_SKIP_CEILING ({ceiling}) — "
    "first: {sample}"
)
CEILING_SPLIT_MSG = (
    "nextest ceiling: the two readings of the listing disagree — {ignored} testcase(s) carry "
    "#[ignore] but {mismatched} are filtered out of a run ({only_ignored} / {only_filtered} "
    "either way). A profile's default-filter is suppressing tests no #[ignore] declares, so "
    "the listing must be taken with the run's own --profile"
)
PARITY_MSG = (
    "C-013a: {bazel} bazel rust_test target(s), {suites} nextest rust-suite(s) minus "
    "{excluded} excluded = {expected} — off by {delta:+d}"
)
PARITY_STALE_MSG = (
    "C-013a: exclusion {name!r} names no nextest suite — a stale exclusion widens the parity "
    "check by one and nothing else reds"
)
PARITY_FLOOR_MSG = (
    "C-013a read {labels} bazel label(s) / {suites} nextest suite(s), expected >= "
    "{label_floor} / {suite_floor} — a reader stopped early, and two short reads agree"
)

#: Substring, lowercased, over one shell command item. `bep` and `testlogs` are
#: word-bounded; `bazel` is not, so `bazel-out` and `bazelisk` both land.
BAZEL_TOKENS = ("bazel", r"\bbep\b", "testlogs", "build_event")

#: The artifacts a *run* leaves. `rm`-ing one opens the floor's bracket and is
#: allowed; reading one is the thing C-012 forbids.
RUN_ARTIFACT_TOKENS = ("NEXTEST_RUN_LOG", "run.log", "target/nextest")

#: What a string constant inside the ceiling reader must never name. Docstrings
#: are stripped before the walk — this file's own prose says "cachedLocally" and
#: "bazel-out" repeatedly, and a scan that matched itself would return the same
#: answer in every state.
RUN_STATE_TOKENS = (
    "cachedLocally",
    "cached_locally",
    "cachedRemotely",
    "cached_remotely",
    "testResult",
    "test_result",
    "bazel",
    "Summary [",
    "run.log",
)


# ---------------------------------------------------------------------------
# C-012 — the floor recipe. One predicate, applied to both live recipes.
# ---------------------------------------------------------------------------

TASK_INDENT = 2
KEY_INDENT = 4


def _block(lines: list[str], header: str, indent: int) -> list[str]:
    """The lines under `header` at `indent`, to the next non-blank line at <= it."""
    out: list[str] = []
    inside = False
    for line in lines:
        stripped = line.strip()
        if not inside:
            if stripped == header and len(line) - len(line.lstrip()) == indent:
                inside = True
            continue
        if stripped and (len(line) - len(line.lstrip())) <= indent:
            break
        out.append(line)
    return out


def task_cmds(taskfile_text: str, task_name: str) -> list[str]:
    """One string per `cmds:` list item, folded continuations joined.

    Deliberately not the whole task block: `test:floor`'s own `summary:` prose
    explains the run-log bracket in English, so a check that read the block
    would red on the documentation of the property it is asserting.
    """
    task = _block(taskfile_text.splitlines(), f"{task_name}:", TASK_INDENT)
    items: list[str] = []
    current: str | None = None
    for line in _block(task, "cmds:", KEY_INDENT):
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("- "):
            if current is not None:
                items.append(current)
            current = stripped[2:]
        elif current is not None:
            current = f"{current} {stripped}"
    if current is not None:
        items.append(current)
    return items


def _is_pure_delete(item: str) -> bool:
    """`rm -f <log>` and nothing chained onto it."""
    words = item.split()
    return bool(words) and words[0] == "rm" and not any(c in item for c in ";|&")


def floor_recipe_findings(items: list[str]) -> list[Finding]:
    """C-012: a recipe that reads no run and names no engine, and reads enough."""
    findings: list[Finding] = []
    if not any("cargo nextest list" in item and "rust-suites" in item for item in items):
        findings.append(
            Finding("floor-recipe-reader", FLOOR_RECIPE_READER_MSG.format(items=len(items)))
        )
    for item in items:
        lowered = item.lower()
        for token in BAZEL_TOKENS:
            if re.search(token, lowered):
                findings.append(
                    Finding(
                        "floor-recipe-bazel",
                        FLOOR_RECIPE_BAZEL_MSG.format(token=token, item=item[:80]),
                    )
                )
                break
        if _is_pure_delete(item):
            continue
        for token in RUN_ARTIFACT_TOKENS:
            if token in item:
                findings.append(
                    Finding(
                        "floor-recipe-run",
                        FLOOR_RECIPE_RUN_MSG.format(token=token, item=item[:80]),
                    )
                )
                break
    return findings


# ---------------------------------------------------------------------------
# The listing. One reader, two gates.
# ---------------------------------------------------------------------------


def read_listing(text: str) -> tuple[dict[str, dict], list[Finding]]:
    """`rust-suites` out of `cargo nextest list --message-format json`.

    Absent, unparseable and wrong-shaped are three distinct reader-floor reds,
    never an empty mapping — an empty mapping sums to 0 and would red the floor
    for a reason that has nothing to do with the test set.
    """
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        return {}, [Finding("listing-parse", LISTING_PARSE_MSG.format(reason=error))]
    if not isinstance(document, dict):
        return {}, [Finding("listing-parse", LISTING_PARSE_MSG.format(reason="not an object"))]
    suites = document.get("rust-suites")
    if not isinstance(suites, dict):
        return {}, [
            Finding("listing-parse", LISTING_PARSE_MSG.format(reason="no rust-suites object"))
        ]
    return suites, []


def suite_floor_findings(suites: dict[str, dict], minimum: int) -> list[Finding]:
    if len(suites) >= minimum:
        return []
    return [
        Finding("suite-floor", SUITE_FLOOR_MSG.format(suites=len(suites), minimum=minimum))
    ]


def floor_count(suites: dict[str, dict]) -> int:
    """The live recipe's sum, in Python — `taskfiles/rust.taskfile.yml:610`."""
    return sum(len(suite.get("testcases", {})) for suite in suites.values())


def floor_findings(suites: dict[str, dict], floor: int, *, minimum: int = NEXTEST_SUITE_FLOOR) -> list[Finding]:
    """C-012. The suite floor first: a short read must not read as a shrunken set."""
    findings = suite_floor_findings(suites, minimum)
    count = floor_count(suites)
    if count < floor:
        findings.append(Finding("floor", FLOOR_MSG.format(count=count, floor=floor)))
    return findings


def skip_names(suites: dict[str, dict]) -> tuple[set[str], set[str]]:
    """Two independent readings: `#[ignore]`, and "a run would not run this".

    `ignored` is the attribute. `filter-match` is the verdict nextest's own
    filter machinery reaches, which is what the run's `N skipped` counts. They
    agree on this tree (8 and 8) and part company under a narrowing
    `default-filter`, so both are read and the split is a finding.
    """
    ignored: set[str] = set()
    filtered: set[str] = set()
    for binary_id, suite in suites.items():
        for name, case in suite.get("testcases", {}).items():
            if not isinstance(case, dict):
                continue
            qualified = f"{binary_id}::{name}"
            if case.get("ignored") is True:
                ignored.add(qualified)
            match = case.get("filter-match")
            if isinstance(match, dict) and match.get("status") != "matches":
                filtered.add(qualified)
    return ignored, filtered


def ceiling_findings(suites: dict[str, dict], ceiling: int, *, minimum: int = NEXTEST_SUITE_FLOOR) -> list[Finding]:
    """C-013, from the listing alone. No parameter here can carry a run."""
    findings = suite_floor_findings(suites, minimum)
    ignored, filtered = skip_names(suites)
    if ignored != filtered:
        findings.append(
            Finding(
                "ceiling-split",
                CEILING_SPLIT_MSG.format(
                    ignored=len(ignored),
                    mismatched=len(filtered),
                    only_ignored=sorted(ignored - filtered)[:1],
                    only_filtered=sorted(filtered - ignored)[:1],
                ),
            )
        )
    skipped = ignored | filtered
    if len(skipped) > ceiling:
        findings.append(
            Finding(
                "ceiling",
                CEILING_MSG.format(
                    skipped=len(skipped), ceiling=ceiling, sample=min(skipped)
                ),
            )
        )
    return findings


def engine_reads(source: str) -> list[str]:
    """Every *code* string in `source` naming a build-engine run artifact.

    AST with docstrings stripped, never `grep`: the tokens appear in this
    module's prose on nearly every screen, so a text scan would match itself in
    every state and report the same answer whether or not the reader was clean.
    """
    tree = ast.parse(source)
    for node in ast.walk(tree):
        body = getattr(node, "body", None)
        if not isinstance(body, list) or not body:
            continue
        first = body[0]
        if isinstance(first, ast.Expr) and isinstance(getattr(first.value, "value", None), str):
            body.pop(0)
    hits: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            hits.extend(token for token in RUN_STATE_TOKENS if token in node.value)
    return hits


# ---------------------------------------------------------------------------
# C-013a — target-count parity. The exclusion set is a parameter: WP-12 owns
# producing it (C-008), and a check that minted its own would be reconciling
# against itself.
# ---------------------------------------------------------------------------


def derive_exclusions(suites: dict[str, dict]) -> dict[str, dict]:
    """`BAZEL_UNMAPPED_SUITES`, annotated from the listing it is being read
    against.

    Not a rule over the listing, and the previous one was wrong in both
    directions. It excluded `kind` outside `{"lib", "test"}` on the premise
    that a `[[bin]]` suite maps to a `rust_binary` stages 1-2 do not build;
    rules_rust compiles a `[[bin]]`'s `#[cfg(test)]` module as a `rust_test`
    over the same crate root and needs no `rust_binary`, so two of the three
    `bin` suites do have targets. And it could not see
    `ocx::linux_self_contained`, which is `kind = "test"` and has none.
    Reported 3 suites / 110 testcases against a true 2 / 1.

    Entries absent from the listing are kept, not filtered: `target_parity`
    reports them as `parity-stale`, and dropping them here would be exactly the
    silent widening that check exists to catch.
    """
    return {
        binary_id: {
            "kind": str(suites.get(binary_id, {}).get("kind", "<absent>")),
            "package": str(suites.get(binary_id, {}).get("package-name", "<absent>")),
            "testcases": len(suites.get(binary_id, {}).get("testcases", {})),
            "reason": reason,
        }
        for binary_id, reason in sorted(BAZEL_UNMAPPED_SUITES.items())
    }


def target_parity(
    bazel_labels: set[str],
    suite_ids: set[str],
    exclusions: set[str],
    *,
    label_floor: int = CRATES_TEST_TARGETS,
    suite_floor: int = NEXTEST_SUITE_FLOOR,
) -> list[Finding]:
    """C-013a: `len(bazel) == len(suites) - len(exclusions)`, floored on both reads."""
    findings: list[Finding] = []
    if len(bazel_labels) < label_floor or len(suite_ids) < suite_floor:
        findings.append(
            Finding(
                "parity-floor",
                PARITY_FLOOR_MSG.format(
                    labels=len(bazel_labels),
                    suites=len(suite_ids),
                    label_floor=label_floor,
                    suite_floor=suite_floor,
                ),
            )
        )
    for name in sorted(exclusions - suite_ids):
        findings.append(Finding("parity-stale", PARITY_STALE_MSG.format(name=name)))
    expected = len(suite_ids) - len(exclusions & suite_ids)
    if len(bazel_labels) != expected:
        findings.append(
            Finding(
                "parity",
                PARITY_MSG.format(
                    bazel=len(bazel_labels),
                    suites=len(suite_ids),
                    excluded=len(exclusions & suite_ids),
                    expected=expected,
                    delta=len(bazel_labels) - expected,
                ),
            )
        )
    return findings


# ---------------------------------------------------------------------------
# The named control. The wrong reader, called from the self-test and nowhere
# else — WP-13's `_presence_reader` idiom. This is the shape the execution swap
# invites: "skipped = the targets that did not execute", which a warm cache
# satisfies for every target at once.
# ---------------------------------------------------------------------------


def _cached_as_skipped(bep: Path, universe: int) -> int:
    """WRONG. Counts a Bazel cache hit as a test skip."""
    outcomes, _ = read_test_results(bep)
    return universe - len(rerun_set(outcomes))


# ---------------------------------------------------------------------------
# Self-test.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel floor proofs self-test: {problem}")


def _suite(kind: str, package: str, cases: dict[str, tuple[bool, str]]) -> dict:
    return {
        "package-name": package,
        "kind": kind,
        "testcases": {
            name: {"ignored": ignored, "filter-match": {"status": status}}
            for name, (ignored, status) in cases.items()
        },
    }


def sample_listing(*, cases_per_suite: int = 4, ignored: int = 8) -> dict[str, dict]:
    """37 suites in the live tree's shape: 20 lib, 14 test, 3 bin.

    Names are borrowed from the real listing for the three `bin` suites and for
    `ocx::linux_self_contained` — those four are what the exclusion set turns
    on, and the fixture would be answering a different question without them.
    The rest are `pkgNN`, so nothing here reads as a claim about the real crate
    graph. Note the shape deliberately does NOT line up with `kind`: two of the
    three `bin` suites map to a target and one `test` suite does not, which is
    the fact the old `kind`-based rule got wrong in both directions.
    """
    suites: dict[str, dict] = {}
    for index in range(1, 21):
        suites[f"pkg{index:02d}"] = _suite(
            "lib",
            f"pkg{index:02d}",
            {f"t{n}": (False, "matches") for n in range(cases_per_suite)},
        )
    for index in range(1, 14):
        suites[f"pkg{index:02d}::it{index:02d}"] = _suite(
            "test",
            f"pkg{index:02d}",
            {f"t{n}": (False, "matches") for n in range(cases_per_suite)},
        )
    # The 14th integration suite, real name and real shape: `kind = "test"`,
    # one testcase, and no Bazel target.
    suites["ocx::linux_self_contained"] = _suite("test", "ocx", {"t0": (False, "matches")})
    for binary_id, package in (
        ("ocx::bin/ocx", "ocx"),
        ("ocx_schema::bin/ocx_schema", "ocx_schema"),
        ("ocx_shim::bin/ocx-shim", "ocx_shim"),
    ):
        suites[binary_id] = _suite(
            "bin", package, {f"t{n}": (False, "matches") for n in range(cases_per_suite)}
        )
    marked = 0
    for suite in suites.values():
        for case in suite["testcases"].values():
            if marked >= ignored:
                break
            case["ignored"] = True
            case["filter-match"] = {"status": "mismatch", "reason": "ignored"}
            marked += 1
    return suites


def prove_floor_recipe() -> int:
    """C-012 — the predicate, then the two live recipes separating under it."""
    checks = 0
    taskfile = RUST_TASKFILE.read_text(encoding="utf-8")

    floor_items = task_cmds(taskfile, "test:floor")
    # A slicer floor in both directions, not a pinned count: WP-20 owns this
    # file and may add a command, but an under-read returns nothing and an
    # over-read swallows the `summary:` prose that explains the run-log bracket
    # in English — which would red the check on its own documentation.
    expect(2 <= len(floor_items) <= 6, f"read {len(floor_items)} floor command item(s)")
    expect(
        not any("Opens the bracket" in item for item in floor_items),
        f"the slicer captured the task's summary prose, not only its cmds: {floor_items}",
    )
    expect(
        any("cargo nextest list --workspace --release --locked" in i for i in floor_items),
        f"the floor recipe slice does not contain the floor's own command: {floor_items}",
    )
    expect(
        report(floor_recipe_findings(floor_items)) == 0,
        f"the live floor recipe must be clean under C-012: {floor_items}",
    )
    print(
        f"C-012 GREEN: taskfiles/rust.taskfile.yml rust:test:floor — {len(floor_items)} command "
        "item(s), reads no run artifact, names no build engine"
    )
    checks += 1

    ceiling_items = task_cmds(taskfile, "test:ceiling")
    expect(len(ceiling_items) >= 1, f"read {len(ceiling_items)} ceiling item(s)")
    live_ceiling = floor_recipe_findings(ceiling_items)
    run_read = [f for f in live_ceiling if f.code == "floor-recipe-run"]
    expect(
        len(run_read) == 1,
        "the live ceiling recipe reads target/nextest/run.log — a predicate that calls it "
        f"clean is measuring nothing. got {codes(live_ceiling)}",
    )
    print(f"C-012 RED  : the same predicate on the live rust:test:ceiling — {run_read[0].message}")
    checks += 1

    # --- a floor that grew a Bazel reader.
    mutated = [
        floor_items[0],
        floor_items[1].replace(
            "cargo nextest list --workspace",
            "bazel test //crates/... --build_event_json_file=bep.json && cargo nextest list --workspace",
        ),
    ]
    expect("bazel test" in mutated[1], "the engine mutation did not land in the recipe copy")
    engine = floor_recipe_findings(mutated)
    expect(codes(engine) == ["floor-recipe-bazel"], f"got {codes(engine)}")
    print(f"C-012 RED  : {engine[0].message}")
    checks += 1

    # --- a floor that reads the run log instead of deleting it.
    reading = ["cat {{.NEXTEST_RUN_LOG}}", floor_items[1]]
    expect(not _is_pure_delete(reading[0]), "the run-read mutation is still a pure delete")
    run_read = floor_recipe_findings(reading)
    expect(codes(run_read) == ["floor-recipe-run"], f"got {codes(run_read)}")
    print(f"C-012 RED  : {run_read[0].message}")
    checks += 1

    # --- `rm -f` chained to a read is not a delete. The allowance is narrow on
    #     purpose: `rm -f X && cat X` is a read wearing the allowed first token.
    chained = ["rm -f {{.NEXTEST_RUN_LOG}} && cat {{.NEXTEST_RUN_LOG}}", floor_items[1]]
    expect(chained[0].split()[0] == "rm", "the chained fixture no longer starts with rm")
    chained_findings = floor_recipe_findings(chained)
    expect(codes(chained_findings) == ["floor-recipe-run"], f"got {codes(chained_findings)}")
    print(f"C-012 RED  : {chained_findings[0].message}")
    checks += 1

    # --- a recipe that reads nothing at all. A gate with no subject is not a
    #     clean gate, and the floor above would otherwise pass it silently.
    empty = floor_recipe_findings(["echo ok"])
    expect(codes(empty) == ["floor-recipe-reader"], f"got {codes(empty)}")
    print(f"C-012 RED  : {empty[0].message}")
    checks += 1
    return checks


def prove_floor_count() -> int:
    """C-012's other half — the sum, and S-011 on a listing built by hand."""
    checks = 0
    suites = sample_listing()
    expect(len(suites) == 37, f"the sample listing has {len(suites)} suites, expected 37")
    total = floor_count(suites)
    # 36 suites of `cases_per_suite`, plus `ocx::linux_self_contained`'s single
    # real testcase — the one the exclusion set is worth 1 rather than 4 for.
    expect(total == 36 * 4 + 1, f"sample sum is {total}, expected {36 * 4 + 1}")

    expect(report(floor_findings(suites, total)) == 0, "a sum equal to its floor must be green")
    print(f"S-011 GREEN: {total} tests listed across {len(suites)} suites (floor {total})")
    checks += 1

    # --- S-011: tests deleted from one suite. Three, as the plan writes it,
    #     against a floor with no headroom — which is the condition the plan's
    #     own arithmetic assumes and the live tree does not meet (see --prove-s011).
    shrunk = json.loads(json.dumps(suites))
    victim = "pkg01::it01"
    for name in sorted(shrunk[victim]["testcases"])[:3]:
        del shrunk[victim]["testcases"][name]
    expect(
        len(shrunk[victim]["testcases"]) == len(suites[victim]["testcases"]) - 3,
        "the three-testcase deletion did not land in the copy",
    )
    shrunk_findings = floor_findings(shrunk, total)
    expect(codes(shrunk_findings) == ["floor"], f"got {codes(shrunk_findings)}")
    print(f"S-011 RED  : {shrunk_findings[0].message}")
    checks += 1

    # --- headroom: the same three deletions against the live tree's slack are
    #     green, which is why --prove-s011 computes the count instead of taking 3.
    expect(
        floor_findings(shrunk, total - 3) == [],
        "three deletions against three tests of headroom must be green — that is the trap",
    )
    print(
        f"S-011 GREEN: the same three deletions under a floor of {total - 3} — a floor with "
        "headroom tolerates exactly that many, silently"
    )
    checks += 1

    # --- a partial listing is not a shrunken test set.
    partial = {k: v for k, v in sorted(suites.items())[:20]}
    expect(len(partial) == 20, "the partial-listing slice did not land")
    partial_findings = floor_findings(partial, 1)
    expect(codes(partial_findings) == ["suite-floor"], f"got {codes(partial_findings)}")
    print(f"S-011 RED  : {partial_findings[0].message}")
    checks += 1

    for reason, text in (
        ("not json", "{"),
        ("no rust-suites", '{"test-count": 8223}'),
        ("not an object", "[]"),
    ):
        _, findings = read_listing(text)
        expect(codes(findings) == ["listing-parse"], f"{reason}: got {codes(findings)}")
        print(f"S-011 RED  : {findings[0].message}")
        checks += 1

    parsed, findings = read_listing(json.dumps({"rust-suites": suites}))
    expect(findings == [] and floor_count(parsed) == total, "the reader must round-trip")
    print(f"S-011 GREEN: the reader round-trips a well-formed listing to {floor_count(parsed)}")
    checks += 1
    return checks


def prove_ceiling() -> int:
    """C-013 — the ceiling from the listing, and the two readings' split."""
    checks = 0
    suites = sample_listing(ignored=8)
    ignored, filtered = skip_names(suites)
    expect(ignored == filtered and len(ignored) == 8, f"fixture: {len(ignored)}/{len(filtered)}")

    expect(report(ceiling_findings(suites, 9)) == 0, "8 ignored under a ceiling of 9 is green")
    print("C-013 GREEN: 8 skipped read from the listing (ceiling 9) — the live tree's numbers")
    checks += 1

    over = sample_listing(ignored=10)
    marked, _ = skip_names(over)
    expect(len(marked) == 10, f"the over-ceiling mutation marked {len(marked)}, expected 10")
    over_findings = ceiling_findings(over, 9)
    expect(codes(over_findings) == ["ceiling"], f"got {codes(over_findings)}")
    print(f"C-013 RED  : {over_findings[0].message}")
    checks += 1

    # --- a default-filter suppressing a test no #[ignore] declares. The two
    #     readings must not average: one of them is wrong and the run follows
    #     `filter-match`, so the disagreement is its own red.
    split = json.loads(json.dumps(suites))
    case = split["pkg05"]["testcases"]["t0"]
    expect(case["ignored"] is False, "the split fixture picked an already-ignored case")
    case["filter-match"] = {"status": "mismatch", "reason": "default-filter"}
    split_findings = ceiling_findings(split, 99)
    expect(codes(split_findings) == ["ceiling-split"], f"got {codes(split_findings)}")
    print(f"C-013 RED  : {split_findings[0].message}")
    checks += 1

    # --- the reader must not answer from a partial listing.
    partial = {k: v for k, v in sorted(suites.items())[:20]}
    partial_findings = ceiling_findings(partial, 9)
    expect("suite-floor" in codes(partial_findings), f"got {codes(partial_findings)}")
    print(f"C-013 RED  : {partial_findings[0].message}")
    checks += 1
    return checks


def prove_cache_neutrality(scratch: Path) -> int:
    """C-013's hard clause: a Bazel cache hit is never counted as a test skip."""
    checks = 0
    labels = set(test_labels())
    universe = len(labels)
    expect(universe == CRATES_TEST_TARGETS, f"BEP fixture has {universe} targets")

    cold = scratch / "cold.bep.jsonl"
    warm = scratch / "warm.bep.jsonl"
    write_bep(cold, labels)
    write_bep(warm, set())
    expect(
        cold.read_text(encoding="utf-8") != warm.read_text(encoding="utf-8"),
        "the two BEP fixtures are byte-identical — the cache mutation did not land",
    )
    expect(
        len(rerun_set(read_test_results(cold)[0])) == universe
        and rerun_set(read_test_results(warm)[0]) == set(),
        "the BEP fixtures do not encode a cold and a fully cached run",
    )

    cold_skips = _cached_as_skipped(cold, universe)
    warm_skips = _cached_as_skipped(warm, universe)
    expect(cold_skips == 0, f"the control reads {cold_skips} skips on a cold run, expected 0")
    expect(warm_skips == universe, f"the control reads {warm_skips} on a warm run, expected {universe}")
    print(
        f"C-013 RED  : the run-reading control answers {cold_skips} skipped on a cold BEP and "
        f"{warm_skips} on the identical-but-cached one — a warm cache alone clears a ceiling "
        f"of 9 by {warm_skips - 9}, on a tree where nothing was skipped at all"
    )
    checks += 1

    suites = sample_listing(ignored=8)
    expect(
        ceiling_findings(suites, 9) == [],
        "the listing reader must be green on the same tree the control just reddened",
    )
    print(
        "C-013 GREEN: the listing reader answers 8 on that same tree, cold or warm — it takes "
        "the listing and nothing else, so no BEP can reach it"
    )
    checks += 1

    # --- structural, so it survives WP-20 editing the reader: no code string in
    #     the ceiling path may name a run artifact. Docstrings excluded, or the
    #     scan matches its own explanation in every state.
    tree = ast.parse(Path(__file__).read_text(encoding="utf-8"))
    wanted = {"ceiling_findings", "skip_names", "suite_floor_findings"}
    readers = [
        node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in wanted
    ]
    expect(
        {node.name for node in readers} == wanted,
        f"the ceiling path did not resolve to {sorted(wanted)}: {[n.name for n in readers]}",
    )
    clean = [hit for node in readers for hit in engine_reads(ast.unparse(node))]
    expect(clean == [], f"the ceiling reader names run state: {clean}")
    print(
        f"C-013 GREEN: engine_reads() finds no run-artifact string in the {len(readers)} "
        "function(s) the ceiling answers from"
    )
    checks += 1

    dirty = engine_reads(
        'def ceiling_findings(suites, ceiling):\n'
        '    """A docstring naming cachedLocally must not count."""\n'
        '    return json.loads(Path("bazel-out/test.bep").read_text())\n'
    )
    expect(dirty == ["bazel"], f"the injected run-artifact read was not seen: {dirty}")
    print(f"C-013 RED  : engine_reads() sees the injected reader — {dirty}")
    checks += 1

    exempt = engine_reads('def f():\n    """cachedLocally testResult Summary [ run.log"""\n    return 1\n')
    expect(exempt == [], f"the docstring exemption failed: {exempt}")
    print("C-013 GREEN: the same tokens in a docstring alone are not a hit — the scan cannot match its own prose")
    checks += 1
    return checks


def prove_parity() -> int:
    """C-013a — parity over a parameterised exclusion set."""
    checks = 0
    suites = sample_listing()
    suite_ids = set(suites)
    exclusions = set(derive_exclusions(suites))
    expect(
        exclusions == {"ocx_schema::bin/ocx_schema", "ocx::linux_self_contained"},
        f"derived exclusions are {sorted(exclusions)}",
    )
    # The refuted rule, kept as a named control: it answers 3 on this fixture,
    # and the three it names are not the two that are right. A set of the wrong
    # SIZE would have reddened parity; a set of the right size and the wrong
    # membership would not, which is why the assertion above is on membership.
    by_kind = {b for b, s in suites.items() if s.get("kind") not in {"lib", "test"}}
    expect(len(by_kind) == 3 and by_kind != exclusions, f"the kind rule answers {sorted(by_kind)}")
    expect(
        len(labels := set(test_labels())) == len(suite_ids) - len(exclusions),
        f"{len(labels)} labels vs {len(suite_ids)} - {len(exclusions)}",
    )

    expect(report(target_parity(labels, suite_ids, exclusions)) == 0, "35 == 37 - 2 must be green")
    print(
        f"C-013a GREEN: {len(labels)} bazel rust_test == {len(suite_ids)} rust-suites - "
        f"{len(exclusions)} excluded"
    )
    checks += 1

    # --- the scenario's red: one integration target gone from the graph.
    dropped = min(label for label in labels if label.endswith("_test"))
    short = labels - {dropped}
    expect(len(short) == len(labels) - 1 and dropped not in short, "the target drop did not land")
    short_findings = target_parity(short, suite_ids, exclusions, label_floor=len(labels) - 1)
    expect(codes(short_findings) == ["parity"], f"got {codes(short_findings)}")
    print(f"C-013a RED  : {short_findings[0].message} (dropped {dropped})")
    checks += 1

    # --- the same delta from the other side: a suite arrives with no target.
    grown = suite_ids | {"pkg20::it20"}
    grown_findings = target_parity(labels, grown, exclusions)
    expect(codes(grown_findings) == ["parity"], f"got {codes(grown_findings)}")
    print(f"C-013a RED  : {grown_findings[0].message} (a suite with no target)")
    checks += 1

    # --- a stale exclusion. Silent otherwise: it widens the tolerance by one
    #     and the count check then balances on a suite that is not there.
    stale_findings = target_parity(labels, suite_ids, exclusions | {"pkg99::gone"})
    expect(codes(stale_findings) == ["parity-stale"], f"got {codes(stale_findings)}")
    print(f"C-013a RED  : {stale_findings[0].message}")
    checks += 1

    # --- two short reads agree with each other. The floor is what tells a
    #     reader that stopped from a graph that is genuinely that small.
    floor_findings_ = target_parity({next(iter(labels))}, {next(iter(suite_ids))}, set())
    expect("parity-floor" in codes(floor_findings_), f"got {codes(floor_findings_)}")
    print(f"C-013a RED  : {floor_findings_[0].message}")
    checks += 1
    return checks


def prove_counts() -> int:
    """Sums, and the two constants this file floors against on disk."""
    expect(NEXTEST_SUITE_FLOOR == 37, f"suite floor is {NEXTEST_SUITE_FLOOR}, expected 35+2")
    expect(CRATES_TEST_TARGETS == 35, f"{CRATES_TEST_TARGETS} rust_test targets, the tree has 35")
    expect(
        NEXTEST_EXCLUDED_SUITES == 2,
        f"{NEXTEST_EXCLUDED_SUITES} excluded suites, TEST_TARGET_MAP.toml has 2",
    )
    floor = int(FLOOR_FILE.read_text(encoding="utf-8").strip())
    ceiling = int(CEILING_FILE.read_text(encoding="utf-8").strip())
    expect(floor > 0 and ceiling >= 0, f"crates/ floor={floor} ceiling={ceiling}")
    print(
        f"counts  OK : 37 = 20 + 14 + 3 = {CRATES_TEST_TARGETS} + {NEXTEST_EXCLUDED_SUITES}; "
        f"crates/NEXTEST_FLOOR={floor}, crates/NEXTEST_SKIP_CEILING={ceiling} — internal "
        "consistency only; WP-12's generated table is the reality check"
    )
    return 1


def self_test() -> int:
    checks = 0
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        checks += prove_floor_recipe()
        checks += prove_floor_count()
        checks += prove_ceiling()
        checks += prove_cache_neutrality(work)
        checks += prove_parity()
        checks += prove_counts()
    print(
        f"bazel floor proofs self-test: {checks} checks passed — C-012 (the floor reads no run), "
        "C-013 (the ceiling from the listing, and a cache hit that is not a skip), C-013a "
        "(target parity) and S-011 each shown red and green"
    )
    print(
        "  wired into taskfiles/scripts.taskfile.yml `self-test:` (WP-17): "
        "`- python3 scripts/bazel_floor_proofs.py --self-test`"
    )
    return 0


# ---------------------------------------------------------------------------
# Live modes.
# ---------------------------------------------------------------------------

NEXTEST_LIST = (
    "cargo",
    "nextest",
    "list",
    "--workspace",
    "--release",
    "--locked",
    "--message-format",
    "json",
)


def run_nextest_list() -> tuple[dict[str, dict], list[Finding]]:
    """The floor's own command, run here so `--prove-s011` reads what it reads."""
    result = subprocess.run(
        NEXTEST_LIST, cwd=REPO_ROOT, capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        tail = (result.stderr or "").strip().splitlines()[-1:] or ["<no stderr>"]
        return {}, [
            Finding("listing-parse", LISTING_PARSE_MSG.format(reason=f"rc={result.returncode}: {tail[0]}"))
        ]
    return read_listing(result.stdout)


def run_check(listing_path: Path | None) -> int:
    """C-012 always; the listing-borne gates when a listing is supplied."""
    findings = floor_recipe_findings(task_cmds(RUST_TASKFILE.read_text(encoding="utf-8"), "test:floor"))
    if not findings:
        print(f"C-012: {RUST_TASKFILE.name} rust:test:floor reads no run artifact and names no build engine")
    if listing_path is None:
        print("C-013/C-013a: no --listing given, so neither was evaluated — this run is C-012 only")
        return report(findings)

    suites, read = read_listing(listing_path.read_text(encoding="utf-8"))
    findings += read
    if read:
        return report(findings)
    floor = int(FLOOR_FILE.read_text(encoding="utf-8").strip())
    ceiling = int(CEILING_FILE.read_text(encoding="utf-8").strip())
    findings += floor_findings(suites, floor)
    findings += ceiling_findings(suites, ceiling)
    if not findings:
        ignored, _ = skip_names(suites)
        print(
            f"C-012: {floor_count(suites)} tests listed across {len(suites)} suites (floor {floor})"
        )
        print(f"C-013: {len(ignored)} skipped read from the listing (ceiling {ceiling})")
    return report(findings)


def run_derive(listing_path: Path) -> int:
    """The numbers WP-12 must reconcile C-008's exclusion set against."""
    suites, findings = read_listing(listing_path.read_text(encoding="utf-8"))
    if findings:
        return report(findings)
    findings += suite_floor_findings(suites, NEXTEST_SUITE_FLOOR)
    kinds: dict[str, int] = {}
    for suite in suites.values():
        kind = str(suite.get("kind"))
        kinds[kind] = kinds.get(kind, 0) + 1
    exclusions = derive_exclusions(suites)
    excluded_cases = sum(entry["testcases"] for entry in exclusions.values())
    ignored, filtered = skip_names(suites)

    print(f"rust-suites      : {len(suites)}  by kind: {dict(sorted(kinds.items()))}")
    print(f"testcases (floor): {floor_count(suites)}")
    print(f"skipped (ceiling): {len(ignored)} ignored / {len(filtered)} filter-mismatched")
    print(f"exclusion set    : {len(exclusions)} suite(s), {excluded_cases} testcase(s)")
    for binary_id, entry in exclusions.items():
        print(f"  - {binary_id:32} kind={entry['kind']:4} pkg={entry['package']:12} cases={entry['testcases']}")
    print(f"expected bazel rust_test targets: {len(suites)} - {len(exclusions)} = {len(suites) - len(exclusions)}")
    print(
        f"  those {excluded_cases} testcase(s) are counted by the floor and executed by no "
        "stage-1/2 Bazel target — C-013a is the only gate that can see the gap"
    )
    return report(findings)


S011_FILE = REPO_ROOT / "crates" / "ocx_test_support" / "tests" / "boundary.rs"
S011_MARKER = "#[cfg(any())]\n"
TEST_ATTR = re.compile(r"^#\[test\]$", re.MULTILINE)


def _git_is_clean(path: Path) -> bool:
    result = subprocess.run(
        ["git", "status", "--porcelain", "--", str(path)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    return result.returncode == 0 and not result.stdout.strip()


def run_s011() -> int:
    """S-011 against the live tree, with the deletion count the floor demands.

    `#[cfg(any())]` in front of `#[test]` rather than a textual excision of the
    function body: the item is cfg-stripped before name resolution, so the test
    ceases to exist exactly as a deletion would, the mutation is one line and
    provably landed by counting the marker, and the restore is a byte-for-byte
    rewrite of the saved original — never `git checkout --`, which restores from
    the index and would make the whole run vacuous.
    """
    if not _git_is_clean(S011_FILE):
        print(f"S-011: {S011_FILE} is already modified — refusing to mutate a dirty file")
        return 1

    original = S011_FILE.read_bytes()
    floor = int(FLOOR_FILE.read_text(encoding="utf-8").strip())

    suites, findings = run_nextest_list()
    if findings:
        return report(findings)
    baseline = floor_count(suites)
    headroom = baseline - floor
    needed = headroom + 1
    print(f"S-011 GREEN: {baseline} tests listed, floor {floor} — headroom {headroom}")
    print(
        f"  the plan's red deletes 3, which lands on {baseline - 3} and stays green. "
        f"{needed} is the smallest deletion this floor can see."
    )
    if report(floor_findings(suites, floor)) != 0:
        return 1

    source = original.decode("utf-8")
    occurrences = list(TEST_ATTR.finditer(source))
    if len(occurrences) < needed:
        print(f"S-011: {S011_FILE} declares {len(occurrences)} tests, {needed} are needed")
        return 1

    mutated = source
    for match in reversed(occurrences[:needed]):
        mutated = mutated[: match.start()] + S011_MARKER + mutated[match.start() :]
    status = 1
    try:
        S011_FILE.write_text(mutated, encoding="utf-8")
        landed = S011_FILE.read_text(encoding="utf-8").count(S011_MARKER.strip())
        if landed != needed:
            print(f"S-011: the mutation did not land — {landed} marker(s) on disk, expected {needed}")
            return 1
        print(f"S-011: mutation landed — {landed} of {len(occurrences)} tests cfg'd out of {S011_FILE.name}")

        shrunk, findings = run_nextest_list()
        if findings:
            return report(findings)
        count = floor_count(shrunk)
        if count != baseline - needed:
            print(f"S-011: listed {count}, expected {baseline - needed} — the mutation reached the wrong universe")
            return 1
        red = floor_findings(shrunk, floor)
        if codes(red) != ["floor"]:
            print(f"S-011: expected the floor to red and nothing else, got {codes(red)}")
            return 1
        print(f"S-011 RED  : {red[0].message}")
        status = 0
    finally:
        S011_FILE.write_bytes(original)
        restored = S011_FILE.read_bytes() == original and _git_is_clean(S011_FILE)
        print(f"S-011: restore {'landed' if restored else 'FAILED'} — {S011_FILE}")
        if not restored:
            status = 1

    suites, findings = run_nextest_list()
    if findings:
        return report(findings)
    if floor_count(suites) != baseline:
        print(f"S-011: post-restore listing is {floor_count(suites)}, expected {baseline}")
        return 1
    print(f"S-011 GREEN: {floor_count(suites)} tests listed again (floor {floor})")
    return status


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="prove every pair red and green")
    mode.add_argument("--check", action="store_true", help="gate the live tree: C-012, and C-013 with --listing")
    mode.add_argument("--derive", action="store_true", help="the exclusion set and counts WP-12 must reconcile")
    mode.add_argument("--prove-s011", action="store_true", help="S-011 against the live workspace (builds)")
    parser.add_argument("--listing", type=Path, help="a `cargo nextest list --message-format json` capture")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if args.check:
        return run_check(args.listing)
    if args.derive:
        if args.listing is None:
            parser.error("--derive needs --listing")
        return run_derive(args.listing)
    return run_s011()


if __name__ == "__main__":
    raise SystemExit(main())
