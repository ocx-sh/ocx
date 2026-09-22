#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`.bazelignore` is honoured by the live graph — C-006's declared red state.

    scripts/bazel_ignore_check.py [--bazel bazel]
    scripts/bazel_ignore_check.py --buildfiles FILE   # judge a captured query
    scripts/bazel_ignore_check.py --self-test

**Why this exists as its own gate.** C-006 declares the check in as many words —
*"`bazel query 'buildfiles(//...)'` must not list any ignored directory"* — and
until this file, nothing in the tree ran it. The entries were verified once by
hand (DX-14), and a one-off measurement is not a carrier.

**And `bazel build --nobuild //...` is not a substitute.** `.bazelignore`'s own
comment records why: dropping `test/manual/.ocx-home` produces forty `ERROR`
lines on every `//...` command and they are **non-fatal**, so the nearest live
gate stays green while the ignore set silently degrades. The failure mode is a
real ocx home — with the store's `refs/symlinks` back-edges — or a parallel agent
checkout of this same repository entering the build graph.

**Two readings, because one of them cannot fail on its own.**

* The **finding**: a `buildfiles(//...)` path that lies under a listed directory.
* The **floor**: how much was read. A `bazel query` whose universe fails to load
  prints a traceback on stderr, writes nothing to stdout and *exits 0* — so an
  empty answer is a clean answer to a reader that only looks for violations. Both
  floors here are counts: buildfiles read, and entries parsed out of
  `.bazelignore`. An empty ignore list would make the sweep vacuous in the other
  direction, and that state is a red of its own.

**Path semantics, measured rather than assumed.** `.bazelignore` takes
workspace-relative *directory paths*, not globs, and does not depth-match: a bare
`target` line ignores the top-level `target/` only. So a violation is a
buildfile whose workspace-relative path is the entry itself or starts with
`<entry>/` — never a substring test, which would call `targets/x/BUILD` a
violation of `target`.

`bazel query`'s labels come back as `//pkg:file` or `@@repo//pkg:file`. Only the
main repository is judged: an external repository's files live outside the
workspace and `.bazelignore` says nothing about them.

**C-029.** `bazel query` is a loading-phase command: it takes no build options,
so `.bazelrc`'s `build --remote_cache=` line does not apply to it. No action is
executed and no result is fetched or uploaded.

Every mutation in `--self-test` is proven to have landed before its result is
trusted. Fixtures are the live query's own bytes, mutated in memory; the tracked
`.bazelignore` is never written to, and nothing is restored with
`git checkout --`, which restores from the index.
"""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path

from bazel_gate_proofs import CRATE_PACKAGES, REPO_ROOT, Finding, codes, report

BAZELIGNORE = REPO_ROOT / ".bazelignore"

IGNORE_ENTRY_FLOOR = 16
"""The 16 directories `.bazelignore` lists today.

The floor is what makes an emptied or unreadable file a red rather than a sweep
with nothing to sweep for. It only ever rises, in the commit that adds the
entry."""

ROOT_BUILD_FILES = 1
"""`//:BUILD.bazel`."""

SUITE_BUILD_FILES = 4
"""`//test:BUILD.bazel` + `//test:bazel.bzl`, `//test/doc_scripts:BUILD.bazel` +
`//test/doc_scripts:cast.bzl`. `buildfiles()` answers the BUILD files **and the
`.bzl` files they load**, which is why a package can contribute more than one."""

BUILDFILES_FLOOR = ROOT_BUILD_FILES + CRATE_PACKAGES + SUITE_BUILD_FILES  # 25
"""Main-repository buildfiles in `//...`, measured: 25.

Written as a sum so a crate added without its floor rising cannot hide in a
total. External repositories are excluded before this count — `@@bazel_features+//…`
and friends push the raw answer to 179 and say nothing about `.bazelignore`.

A universe that failed to load answers 0 and exits 0, which is the state this
floor exists to separate from a clean one."""

IGNORE_VIOLATION_MSG = (
    "bazel ignore check: `buildfiles(//...)` lists {label} — under the ignored directory "
    "{entry!r} (C-006). A package there is in the build graph: an agent worktree is a "
    "parallel checkout of this same repository, and `test/manual/.ocx-home` is a real ocx "
    "store whose refs/symlinks back-edges Bazel's directory walk reads as infinite symlink "
    "expansion. Neither fails `bazel build --nobuild //...`, which is why this gate is not it"
)
IGNORE_QUERY_MSG = (
    "bazel ignore check: `bazel query 'buildfiles(//...)'` exited {rc} — nothing was read, so "
    "the sweep below is vacuous. On a fresh checkout or a linked worktree the cause is "
    "crate_universe's gitignored Cargo.bazel.lock.json; `task bazel:bootstrap` repins it. "
    "stderr: {detail}"
)
IGNORE_READER_MSG = (
    "bazel ignore check read {read} of an expected >= {minimum} buildfiles in //... — the "
    "reader stopped early, and a zero-match query exits 0, so the count is what gates"
)
IGNORE_ENTRIES_MSG = (
    "bazel ignore check parsed {read} of an expected >= {minimum} directory entries out of "
    "{path} — an empty ignore list makes every violation check above it vacuous"
)


def read_entries(text: str) -> list[str]:
    """Workspace-relative directory paths, comments and blanks dropped."""
    entries = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        entries.append(stripped.rstrip("/"))
    return entries


def workspace_paths(query_stdout: str) -> list[str]:
    """`//pkg:file` -> `pkg/file`, main repository only.

    An `@@repo//...` label names a file outside the workspace, where
    `.bazelignore` has nothing to say; `//:file` is a root-package file and
    becomes a bare `file`.
    """
    paths = []
    for line in query_stdout.splitlines():
        label = line.strip()
        if not label.startswith("//"):
            continue
        package, _, name = label[2:].partition(":")
        paths.append(f"{package}/{name}" if package else name)
    return paths


def ignore_findings(*, paths: list[str], entries: list[str]) -> list[Finding]:
    """C-006's red state, plus the two floors that make its green mean anything."""
    findings: list[Finding] = []

    if len(entries) < IGNORE_ENTRY_FLOOR:
        findings.append(
            Finding(
                "ignore-entries-floor",
                IGNORE_ENTRIES_MSG.format(
                    read=len(entries), minimum=IGNORE_ENTRY_FLOOR, path=BAZELIGNORE
                ),
            )
        )
    if len(paths) < BUILDFILES_FLOOR:
        findings.append(
            Finding(
                "ignore-reader-floor",
                IGNORE_READER_MSG.format(read=len(paths), minimum=BUILDFILES_FLOOR),
            )
        )

    for path in sorted(paths):
        for entry in entries:
            # Prefix on a path BOUNDARY, never a substring: `target` must not
            # match `targets/x/BUILD`, and `.bazelignore` does not depth-match,
            # so the entry is the directory itself and nothing deeper is implied.
            if path == entry or path.startswith(entry + "/"):
                findings.append(
                    Finding(
                        "ignore-violated",
                        IGNORE_VIOLATION_MSG.format(label=path, entry=entry),
                    )
                )
                break
    return findings


def run_query(bazel: str) -> tuple[str, int, str]:
    """`(stdout, rc, stderr)`. An unreachable binary reads as rc 127 with a reason.

    No exception escapes: a missing `bazel` and a failed load must both arrive at
    the reader floor as "nothing was read", which is the state that gates.
    """
    try:
        result = subprocess.run(
            [bazel, "query", "buildfiles(//...)"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            cwd=REPO_ROOT,
            timeout=900,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return "", 127, f"{type(error).__name__}: {error}"
    return result.stdout, result.returncode, result.stderr


def run_check(*, bazel: str, buildfiles: Path | None = None) -> tuple[list[Finding], int, int]:
    """`(findings, buildfiles read, ignore entries parsed)`."""
    findings: list[Finding] = []
    if buildfiles is not None:
        try:
            text = buildfiles.read_text(encoding="utf-8")
        except OSError as error:
            text = ""
            findings.append(Finding("ignore-query-failed", IGNORE_QUERY_MSG.format(rc="-", detail=error)))
    else:
        text, rc, stderr = run_query(bazel)
        if rc != 0:
            findings.append(
                Finding(
                    "ignore-query-failed",
                    IGNORE_QUERY_MSG.format(
                        rc=rc, detail=" | ".join(stderr.strip().splitlines()[-3:])
                    ),
                )
            )

    try:
        entries = read_entries(BAZELIGNORE.read_text(encoding="utf-8"))
    except OSError as error:
        entries = []
        findings.append(Finding("ignore-file-unreadable", f"cannot read {BAZELIGNORE}: {error}"))

    paths = workspace_paths(text)
    findings.extend(ignore_findings(paths=paths, entries=entries))
    return findings, len(paths), len(entries)


# ---------------------------------------------------------------------------
# Self-test.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel ignore check self-test: {problem}")


def self_test(bazel: str) -> int:
    """Every finding red and green, on the live query's own bytes."""
    checks = 0

    entries = read_entries(BAZELIGNORE.read_text(encoding="utf-8"))
    expect(
        len(entries) >= IGNORE_ENTRY_FLOOR,
        f"{BAZELIGNORE} lists {len(entries)} directories, below the declared floor "
        f"{IGNORE_ENTRY_FLOOR}",
    )
    expect(
        all(not entry.startswith("/") and "*" not in entry for entry in entries),
        f"an entry is absolute or a glob — `.bazelignore` takes workspace-relative "
        f"directory paths only: {entries}",
    )

    stdout, rc, stderr = run_query(bazel)
    expect(
        rc == 0,
        f"`bazel query 'buildfiles(//...)'` exited {rc}. This gate's subject is the live "
        "graph, so a self-test that greened without reading it would be the vacuous green "
        "this plan exists to prevent. stderr: "
        + " | ".join(stderr.strip().splitlines()[-4:]),
    )
    live = workspace_paths(stdout)
    expect(len(live) >= BUILDFILES_FLOOR, f"the live query read {len(live)} buildfiles")

    green = ignore_findings(paths=live, entries=entries)
    expect(green == [], f"the live graph must be silent, got {[f.message for f in green]}")
    print(
        f"C-006 GREEN: {len(live)} buildfiles in //..., none under any of the {len(entries)} "
        f"directories {BAZELIGNORE.name} lists"
    )
    checks += 1

    # --- red 1: a package appears under an ignored directory.
    planted = f"{entries[0]}/probe/BUILD.bazel"
    mutated = [*live, planted]
    expect(planted not in live, "the planted path is already in the live answer")
    findings = ignore_findings(paths=mutated, entries=entries)
    expect(codes(findings) == ["ignore-violated"], f"got {codes(findings)}")
    expect(planted in findings[0].message, "the finding does not name the path")
    print(f"C-006 RED  : {findings[0].message}")
    checks += 1

    # --- red 2: the directory ITSELF, with no package under it.
    findings = ignore_findings(paths=[*live, f"{entries[0]}/BUILD.bazel"], entries=entries)
    expect(codes(findings) == ["ignore-violated"], f"got {codes(findings)}")
    checks += 1

    # --- the control: a prefix that is not a path boundary must NOT fire.
    sibling = f"{entries[0]}s/probe/BUILD.bazel"
    findings = ignore_findings(paths=[*live, sibling], entries=entries)
    expect(
        findings == [],
        f"{sibling!r} shares a string prefix with {entries[0]!r} and is a different "
        f"directory — a substring test would call it a violation, got {codes(findings)}",
    )
    print(
        f"C-006 CONTROL: {sibling} is silent — the match is on a path boundary, so a "
        f"substring reader's false positive is refused"
    )
    checks += 1

    # --- red 3: the reader stopped. A zero-match query exits 0.
    findings = ignore_findings(paths=[], entries=entries)
    expect(codes(findings) == ["ignore-reader-floor"], f"got {codes(findings)}")
    print(f"C-006 RED  : {findings[0].message}")
    checks += 1

    # --- red 4: an emptied ignore list — the sweep has nothing to sweep for.
    findings = ignore_findings(paths=live, entries=[])
    expect(codes(findings) == ["ignore-entries-floor"], f"got {codes(findings)}")
    print(f"C-006 RED  : {findings[0].message}")
    checks += 1

    # --- red 5: the entry that `bazel build --nobuild //...` does not catch.
    #     `.bazelignore`'s own comment measures it: forty non-fatal ERROR lines.
    manual = "test/manual/.ocx-home"
    expect(manual in entries, f"{manual} is no longer in {BAZELIGNORE.name}")
    findings = ignore_findings(paths=[*live, f"{manual}/BUILD.bazel"], entries=entries)
    expect(codes(findings) == ["ignore-violated"], f"got {codes(findings)}")
    print(f"C-006 RED  : {findings[0].message}")
    checks += 1

    # And dropping the entry does not escape the gate the other way: the entry
    # floor is what catches a deletion, which is the regression `.bazelignore`'s
    # own comment says `bazel build --nobuild //...` reports as non-fatal noise.
    thinned = [entry for entry in entries if entry != manual]
    findings = ignore_findings(paths=[*live, f"{manual}/BUILD.bazel"], entries=thinned)
    expect(
        codes(findings) == ["ignore-entries-floor"],
        f"deleting an entry must red on the entry floor, got {codes(findings)}",
    )
    print(f"C-006 RED  : dropping {manual!r} from the list — {findings[0].message}")
    checks += 1

    expect(
        read_entries(BAZELIGNORE.read_text(encoding="utf-8")) == entries,
        "the self-test modified .bazelignore in place — every mutation here is in memory",
    )
    print(
        f"bazel ignore check self-test: {checks} checks passed — C-006's declared red state "
        "(a buildfile under an ignored directory), both reader floors, and the path-boundary "
        "control, all against the live `buildfiles(//...)` answer"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--self-test", action="store_true", help="every finding red and green")
    parser.add_argument(
        "--bazel",
        default="bazel",
        help="the bazel binary to query with (default: resolved from PATH, as the per-prompt "
        "hook and CI's setup-ocx action both leave it)",
    )
    parser.add_argument(
        "--buildfiles",
        type=Path,
        default=None,
        help="judge a captured `bazel query 'buildfiles(//...)'` instead of running one",
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test(args.bazel)

    findings, read, entries = run_check(bazel=args.bazel, buildfiles=args.buildfiles)
    # Said out loud on every run: a green is only as wide as what ran.
    print(f"bazel ignore check: {read} buildfiles in //... against {entries} ignored directories")
    return report(findings)


if __name__ == "__main__":
    raise SystemExit(main())
